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

        // SAFETY: SECURITY_DESCRIPTOR is a plain C struct; we fully initialize it
        // via Win32 APIs before use.
        let mut descriptor = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };
        unsafe {
            InitializeSecurityDescriptor(
                PSECURITY_DESCRIPTOR(&mut descriptor as *mut _ as *mut _),
                SECURITY_DESCRIPTOR_REVISION,
            )
            .map_err(|error| format!("InitializeSecurityDescriptor failed: {error}"))?;
            // bDaclPresent=true, pDacl=null → NULL DACL → full access for local clients.
            SetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(&mut descriptor as *mut _ as *mut _),
                true,
                None,
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
            .reject_remote_clients(true)
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
