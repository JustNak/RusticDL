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
    NullDacl,
    /// ACCESS_ALLOWED ACE for a concrete SID.
    #[allow(dead_code)] // asserted by the DACL regression test; installed by the follow-up fix
    AccessAllowed(LocalPipeAceSid),
}

/// SIDs that may appear in the rusticdl.v1 DACL.
#[cfg(any(windows, test))]
#[allow(dead_code)] // asserted by the DACL regression test; installed by the follow-up fix
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
        // Current production token: NULL DACL (every local caller).
        Self {
            tokens: &[LocalPipeDaclToken::NullDacl],
        }
    }

    fn contains_null_dacl(&self) -> bool {
        self.tokens
            .iter()
            .any(|token| matches!(token, LocalPipeDaclToken::NullDacl))
    }

    #[allow(dead_code)] // asserted by the DACL regression test
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

/// Security descriptor that grants local same-user / browser native-host processes
/// access to the named pipe. A NULL DACL means "everyone has access" (local only;
/// we still set PIPE_REJECT_REMOTE_CLIENTS).
#[cfg(windows)]
struct PipeSecurity {
    descriptor: windows::Win32::Security::SECURITY_DESCRIPTOR,
    attributes: windows::Win32::Security::SECURITY_ATTRIBUTES,
}

#[cfg(windows)]
impl PipeSecurity {
    fn new_allow_local() -> Result<Self, String> {
        use windows::Win32::Security::{
            InitializeSecurityDescriptor, SetSecurityDescriptorDacl, PSECURITY_DESCRIPTOR,
            SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
        };
        use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;

        let dacl = LocalPipeDacl::new_allow_local();

        // SAFETY: SECURITY_DESCRIPTOR is a plain C struct; we fully initialize it
        // via Win32 APIs before use.
        let mut descriptor = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };
        unsafe {
            InitializeSecurityDescriptor(
                PSECURITY_DESCRIPTOR(&mut descriptor as *mut _ as *mut _),
                SECURITY_DESCRIPTOR_REVISION,
            )
            .map_err(|error| format!("InitializeSecurityDescriptor failed: {error}"))?;
            if dacl.contains_null_dacl() {
                // bDaclPresent=true, pDacl=null → NULL DACL → full access for local clients.
                SetSecurityDescriptorDacl(
                    PSECURITY_DESCRIPTOR(&mut descriptor as *mut _ as *mut _),
                    true,
                    None,
                    false,
                )
                .map_err(|error| format!("SetSecurityDescriptorDacl failed: {error}"))?;
            } else {
                return Err(
                    "explicit same-user DACL tokens are not installed on the named pipe yet".into(),
                );
            }
        }

        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&raw const descriptor) as *mut _,
            bInheritHandle: false.into(),
        };

        Ok(Self {
            descriptor,
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

    // Create the pipe with a permissive local DACL, then drop security state
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

        let user_sid = current_process_user_sid_for_test();
        let ace_count = unsafe { (*dacl).AceCount };
        let mut saw_same_user = false;
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
            if unsafe { EqualSid(ace_sid, user_sid).is_ok() } {
                saw_same_user = true;
            }
        }
        assert!(
            saw_same_user,
            "missing same-user ACE: installed DACL has no ACCESS_ALLOWED ACE for the current process user SID"
        );
    }

    #[cfg(windows)]
    fn current_process_user_sid_for_test() -> windows::Win32::Security::PSID {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::Security::{
            CopySid, GetLengthSid, GetTokenInformation, TokenUser, PSID, TOKEN_QUERY, TOKEN_USER,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        // Leaked on purpose: the test process exits immediately after.
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
                .expect("OpenProcessToken");
            let mut needed = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
            let mut info = vec![0u8; needed as usize];
            GetTokenInformation(
                token,
                TokenUser,
                Some(info.as_mut_ptr().cast()),
                needed,
                &mut needed,
            )
            .expect("GetTokenInformation");
            let _ = CloseHandle(token);
            let token_user = &*(info.as_ptr() as *const TOKEN_USER);
            let sid_len = GetLengthSid(token_user.User.Sid);
            let mut sid = vec![0u8; sid_len as usize].into_boxed_slice();
            CopySid(sid_len, PSID(sid.as_mut_ptr().cast()), token_user.User.Sid).expect("CopySid");
            let psid = PSID(sid.as_mut_ptr().cast());
            std::mem::forget(sid);
            std::mem::forget(info);
            psid
        }
    }
}
