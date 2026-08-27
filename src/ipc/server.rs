//! Windows named-pipe server for the extension native-messaging host.

use std::time::Duration;

use super::bridge::IpcBridge;
use super::handlers::handle_request;
use super::protocol::HostRequest;
use crate::branding::PIPE_NAME;

const MAX_PIPE_REQUEST_BYTES: usize = 1024 * 1024;
const PIPE_READ_TIMEOUT: Duration = Duration::from_secs(5);
const PIPE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const PIPE_MAX_INSTANCES: usize = 4;

/// Remote SMB clients stay rejected even after the local DACL is tightened.
#[cfg(any(windows, test))]
const REJECT_REMOTE_NAMED_PIPE_CLIENTS: bool = true;

/// One ACE / control token installed by [`PipeSecurity::new_allow_local`].
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalPipeDaclToken {
    /// `SetSecurityDescriptorDacl(present=true, dacl=NULL)`.
    /// Windows treats this as a NULL DACL: every local process gets full access.
    /// Forbidden token: kept so the regression test can name it.
    #[allow(dead_code)]
    NullDacl,
    /// ACCESS_ALLOWED ACE for a concrete SID.
    AccessAllowed(LocalPipeAceSid),
}

/// SIDs that may appear in the rusticdl.v1 DACL.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalPipeAceSid {
    /// TokenUser SID of the current process (same Windows account).
    CurrentProcessUser,
    /// `NT AUTHORITY\SYSTEM` (`S-1-5-18`).
    LocalSystem,
}

/// DACL tokens [`PipeSecurity::new_allow_local`] applies to `\\.\pipe\rusticdl.v1`.
#[cfg(any(windows, test))]
struct LocalPipeDacl {
    tokens: &'static [LocalPipeDaclToken],
}

#[cfg(any(windows, test))]
impl LocalPipeDacl {
    fn new_allow_local() -> Self {
        Self {
            tokens: &[
                LocalPipeDaclToken::AccessAllowed(LocalPipeAceSid::CurrentProcessUser),
                LocalPipeDaclToken::AccessAllowed(LocalPipeAceSid::LocalSystem),
            ],
        }
    }

    fn contains_null_dacl(&self) -> bool {
        self.tokens
            .iter()
            .any(|token| matches!(token, LocalPipeDaclToken::NullDacl))
    }

    #[cfg(test)]
    fn has_same_user_ace(&self) -> bool {
        self.tokens.iter().any(|token| {
            matches!(
                token,
                LocalPipeDaclToken::AccessAllowed(LocalPipeAceSid::CurrentProcessUser)
            )
        })
    }
}

/// Start the Windows named-pipe listener (no-op on other platforms).
pub fn start_ipc_server(bridge: IpcBridge) {
    #[cfg(windows)]
    {
        tokio::spawn(async move {
            let mut first_pipe_instance = true;
            loop {
                if let Err(error) =
                    accept_single_connection(bridge.clone(), first_pipe_instance).await
                {
                    eprintln!("[ipc] named pipe listener error: {error}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                } else {
                    first_pipe_instance = false;
                }
            }
        });
    }
    #[cfg(not(windows))]
    {
        let _ = bridge;
        eprintln!("[ipc] named pipe server is only available on Windows");
    }
}

/// Same-user DACL for `\\.\pipe\rusticdl.v1`.
///
/// ACCESS_ALLOWED GENERIC_ALL for the current process user SID (native-host and
/// `single_instance` `show_window`) plus Local System. Remote clients are still
/// rejected via `reject_remote_clients`.
#[cfg(windows)]
struct PipeSecurity {
    descriptor: windows::Win32::Security::SECURITY_DESCRIPTOR,
    /// Owns the ACL bytes pointed to by `descriptor`.
    #[allow(dead_code)]
    acl: Vec<u8>,
    attributes: windows::Win32::Security::SECURITY_ATTRIBUTES,
}

#[cfg(windows)]
struct OwnedSid(Vec<u8>);

#[cfg(windows)]
impl OwnedSid {
    fn as_psid(&self) -> windows::Win32::Security::PSID {
        windows::Win32::Security::PSID(self.0.as_ptr() as *mut _)
    }
}

#[cfg(windows)]
fn current_process_user_sid() -> Result<OwnedSid, String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        CopySid, GetLengthSid, GetTokenInformation, TokenUser, PSID, TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|error| format!("OpenProcessToken failed: {error}"))?;

        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            let _ = CloseHandle(token);
            return Err("GetTokenInformation did not return a TokenUser size".into());
        }

        let mut info = vec![0u8; needed as usize];
        let queried = GetTokenInformation(
            token,
            TokenUser,
            Some(info.as_mut_ptr().cast()),
            needed,
            &mut needed,
        );
        let _ = CloseHandle(token);
        queried.map_err(|error| format!("GetTokenInformation failed: {error}"))?;

        let token_user = &*(info.as_ptr() as *const TOKEN_USER);
        let sid_len = GetLengthSid(token_user.User.Sid);
        let mut sid = vec![0u8; sid_len as usize];
        CopySid(sid_len, PSID(sid.as_mut_ptr().cast()), token_user.User.Sid)
            .map_err(|error| format!("CopySid failed: {error}"))?;
        Ok(OwnedSid(sid))
    }
}

#[cfg(windows)]
fn well_known_sid(kind: windows::Win32::Security::WELL_KNOWN_SID_TYPE) -> Result<OwnedSid, String> {
    use windows::Win32::Security::{CreateWellKnownSid, PSID};

    unsafe {
        let mut sid_len = 0u32;
        let _ = CreateWellKnownSid(kind, None, None, &mut sid_len);
        if sid_len == 0 {
            return Err("CreateWellKnownSid did not return a SID size".into());
        }
        let mut sid = vec![0u8; sid_len as usize];
        CreateWellKnownSid(
            kind,
            None,
            Some(PSID(sid.as_mut_ptr().cast())),
            &mut sid_len,
        )
        .map_err(|error| format!("CreateWellKnownSid failed: {error}"))?;
        Ok(OwnedSid(sid))
    }
}

#[cfg(windows)]
fn build_access_allowed_acl(sids: &[OwnedSid]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::GENERIC_ALL;
    use windows::Win32::Security::{
        AddAccessAllowedAce, GetLengthSid, InitializeAcl, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION,
    };

    let mut acl_len = std::mem::size_of::<ACL>();
    for sid in sids {
        let sid_len = unsafe { GetLengthSid(sid.as_psid()) } as usize;
        acl_len += std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<u32>() + sid_len;
    }

    let mut acl = vec![0u8; acl_len];
    unsafe {
        InitializeAcl(acl.as_mut_ptr().cast(), acl_len as u32, ACL_REVISION)
            .map_err(|error| format!("InitializeAcl failed: {error}"))?;
        for sid in sids {
            AddAccessAllowedAce(
                acl.as_mut_ptr().cast(),
                ACL_REVISION,
                GENERIC_ALL.0,
                sid.as_psid(),
            )
            .map_err(|error| format!("AddAccessAllowedAce failed: {error}"))?;
        }
    }
    Ok(acl)
}

#[cfg(windows)]
impl PipeSecurity {
    fn new_allow_local() -> Result<Self, String> {
        use windows::Win32::Security::{
            InitializeSecurityDescriptor, SetSecurityDescriptorDacl, WinLocalSystemSid,
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
        };
        use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;

        let policy = LocalPipeDacl::new_allow_local();
        if policy.contains_null_dacl() {
            return Err("NULL DACL is not allowed on \\\\.\\pipe\\rusticdl.v1".into());
        }

        let mut sids = Vec::new();
        for token in policy.tokens {
            match token {
                LocalPipeDaclToken::NullDacl => {
                    return Err("NULL DACL is not allowed on \\\\.\\pipe\\rusticdl.v1".into());
                }
                LocalPipeDaclToken::AccessAllowed(LocalPipeAceSid::CurrentProcessUser) => {
                    sids.push(current_process_user_sid()?);
                }
                LocalPipeDaclToken::AccessAllowed(LocalPipeAceSid::LocalSystem) => {
                    sids.push(well_known_sid(WinLocalSystemSid)?);
                }
            }
        }
        if sids.is_empty() {
            return Err("named pipe DACL is missing ACCESS_ALLOWED ACEs".into());
        }

        let acl = build_access_allowed_acl(&sids)?;

        // SAFETY: SECURITY_DESCRIPTOR is a plain C struct; we fully initialize it
        // via Win32 APIs before use. The DACL pointer refers to `acl`, which is
        // stored alongside the descriptor for the lifetime of this value.
        let mut descriptor = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };
        unsafe {
            InitializeSecurityDescriptor(
                PSECURITY_DESCRIPTOR(&mut descriptor as *mut _ as *mut _),
                SECURITY_DESCRIPTOR_REVISION,
            )
            .map_err(|error| format!("InitializeSecurityDescriptor failed: {error}"))?;
            SetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(&mut descriptor as *mut _ as *mut _),
                true,
                Some(acl.as_ptr().cast()),
                false,
            )
            .map_err(|error| format!("SetSecurityDescriptorDacl failed: {error}"))?;
        }

        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&raw const descriptor) as *mut _,
            bInheritHandle: false.into(),
        };

        Ok(Self {
            descriptor,
            acl,
            attributes,
        })
    }

    fn attributes_ptr(&mut self) -> *mut std::ffi::c_void {
        // Keep descriptor alive while attributes point at it.
        self.attributes.lpSecurityDescriptor = (&raw mut self.descriptor) as *mut std::ffi::c_void;
        (&raw mut self.attributes) as *mut std::ffi::c_void
    }
}

#[cfg(windows)]
async fn accept_single_connection(
    bridge: IpcBridge,
    first_pipe_instance: bool,
) -> Result<(), String> {
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    // Create the pipe with a same-user local DACL, then drop security state
    // before any .await so the async future stays Send.
    let server = {
        let mut security = PipeSecurity::new_allow_local()?;
        let security_ptr = security.attributes_ptr();

        let mut server_options = ServerOptions::new();
        server_options
            .reject_remote_clients(REJECT_REMOTE_NAMED_PIPE_CLIENTS)
            .max_instances(PIPE_MAX_INSTANCES);
        if first_pipe_instance {
            server_options.first_pipe_instance(true);
        }

        // SAFETY: security_ptr points at a live SECURITY_ATTRIBUTES only for this create call.
        // CreateNamedPipe copies the security descriptor into the kernel object.
        let server =
            unsafe { server_options.create_with_security_attributes_raw(PIPE_NAME, security_ptr) }
                .map_err(|error| format!("Could not create named pipe server: {error}"))?;
        drop(security);
        server
    };

    server
        .connect()
        .await
        .map_err(|error| format!("Could not accept named pipe connection: {error}"))?;

    tokio::spawn(async move {
        let result: Result<(), String> = async {
            let (reader, mut writer) = tokio::io::split(server);
            let mut reader = BufReader::new(reader);
            let request_line =
                tokio::time::timeout(PIPE_READ_TIMEOUT, read_limited_request_line(&mut reader))
                    .await
                    .map_err(|_| "Timed out reading named pipe payload.".to_string())??;

            if request_line.trim().is_empty() {
                return Ok(());
            }

            let request = serde_json::from_str::<HostRequest>(&request_line)
                .map_err(|error| format!("Could not parse host request: {error}"))?;

            let response = handle_request(&bridge, request).await;
            let response_json = serde_json::to_string(&response)
                .map_err(|error| format!("Could not serialize host response: {error}"))?;

            tokio::time::timeout(PIPE_WRITE_TIMEOUT, async {
                writer
                    .write_all(response_json.as_bytes())
                    .await
                    .map_err(|error| format!("Could not write named pipe response: {error}"))?;
                writer.write_all(b"\n").await.map_err(|error| {
                    format!("Could not write named pipe response terminator: {error}")
                })?;
                writer
                    .flush()
                    .await
                    .map_err(|error| format!("Could not flush named pipe response: {error}"))
            })
            .await
            .map_err(|_| "Timed out writing named pipe response.".to_string())??;

            Ok(())
        }
        .await;

        if let Err(error) = result {
            eprintln!("[ipc] named pipe request error: {error}");
        }
    });

    Ok(())
}

#[cfg(windows)]
async fn read_limited_request_line<R>(reader: &mut R) -> Result<String, String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;

    let mut request = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| format!("Could not read named pipe payload: {error}"))?;

        if available.is_empty() {
            break;
        }

        let newline_index = available.iter().position(|byte| *byte == b'\n');
        let read_len = newline_index
            .map(|index| index.saturating_add(1))
            .unwrap_or(available.len());

        if request.len().saturating_add(read_len) > MAX_PIPE_REQUEST_BYTES {
            return Err(format!(
                "Named pipe payload exceeds {MAX_PIPE_REQUEST_BYTES} bytes."
            ));
        }

        request.extend_from_slice(&available[..read_len]);
        reader.consume(read_len);

        if newline_index.is_some() {
            break;
        }
    }

    String::from_utf8(request)
        .map_err(|error| format!("Named pipe payload was not valid UTF-8: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_security_new_allow_local_rejects_null_dacl_and_requires_same_user_ace() {
        let dacl = LocalPipeDacl::new_allow_local();
        assert!(
            !dacl.contains_null_dacl(),
            "NULL DACL (SetSecurityDescriptorDacl present=true, dacl=NULL) grants every local process full access to \\\\.\\pipe\\rusticdl.v1"
        );
        assert!(
            dacl.has_same_user_ace(),
            "missing same-user ACE: DACL must contain ACCESS_ALLOWED for the current process user SID"
        );
        assert!(
            REJECT_REMOTE_NAMED_PIPE_CLIENTS,
            "reject_remote_clients(true) must remain set on the named-pipe server"
        );

        #[cfg(windows)]
        assert_installed_descriptor_rejects_null_dacl_and_has_same_user_ace();
    }

    #[cfg(windows)]
    fn assert_installed_descriptor_rejects_null_dacl_and_has_same_user_ace() {
        use windows::Win32::Security::{
            EqualSid, GetAce, GetSecurityDescriptorDacl, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
            PSECURITY_DESCRIPTOR,
        };
        use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

        let security = PipeSecurity::new_allow_local().expect("pipe security descriptor");
        let mut dacl_present = windows::core::BOOL(0);
        let mut dacl_defaulted = windows::core::BOOL(0);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        unsafe {
            GetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(
                    (&raw const security.descriptor).cast::<std::ffi::c_void>() as *mut _
                ),
                &mut dacl_present,
                &mut dacl,
                &mut dacl_defaulted,
            )
            .expect("GetSecurityDescriptorDacl");
        }
        assert!(
            dacl_present.as_bool(),
            "DACL must be present on \\\\.\\pipe\\rusticdl.v1"
        );
        assert!(
            !dacl.is_null(),
            "NULL DACL (present=true, dacl=NULL) grants every local process full access to \\\\.\\pipe\\rusticdl.v1"
        );

        let user_sid = current_process_user_sid().expect("current process user SID");
        let system_sid =
            well_known_sid(windows::Win32::Security::WinLocalSystemSid).expect("SYSTEM SID");
        let everyone_sid =
            well_known_sid(windows::Win32::Security::WinWorldSid).expect("Everyone SID");
        let ace_count = unsafe { (*dacl).AceCount };
        let mut saw_same_user = false;
        let mut saw_system = false;
        let mut saw_everyone = false;
        for index in 0..ace_count {
            let mut ace: *mut std::ffi::c_void = std::ptr::null_mut();
            unsafe {
                GetAce(dacl, u32::from(index), &mut ace).expect("GetAce");
            }
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
                continue;
            }
            let allowed = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
            let ace_sid =
                windows::Win32::Security::PSID(std::ptr::addr_of!(allowed.SidStart) as *mut _);
            if unsafe { EqualSid(ace_sid, user_sid.as_psid()).is_ok() } {
                saw_same_user = true;
            }
            if unsafe { EqualSid(ace_sid, system_sid.as_psid()).is_ok() } {
                saw_system = true;
            }
            if unsafe { EqualSid(ace_sid, everyone_sid.as_psid()).is_ok() } {
                saw_everyone = true;
            }
        }
        assert!(
            saw_same_user,
            "missing same-user ACE: installed DACL has no ACCESS_ALLOWED ACE for the current process user SID"
        );
        assert!(
            saw_system,
            "missing SYSTEM ACE: named-pipe servers need NT AUTHORITY\\SYSTEM"
        );
        assert!(
            !saw_everyone,
            "Everyone must not have an ACE on \\\\.\\pipe\\rusticdl.v1"
        );
    }
}
