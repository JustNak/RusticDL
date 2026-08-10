//! Named-pipe IPC server for the browser extension native messaging host.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use url::Url;

use crate::appearance::appearance_settings_dto;
use crate::download::{
    EngineCommand, EngineHandle, EnqueueOutcome, EnqueueStatus, HandoffAuth, HandoffAuthHeader,
    Job, JobState,
};
use crate::extension_settings::ExtensionIntegrationSettings;
use crate::persistence::{save_settings, AppPaths};
use crate::settings::Settings;

pub use crate::branding::PIPE_NAME;
pub const PROTOCOL_VERSION: u32 = 1;

const MAX_REQUEST_ID_LENGTH: usize = 128;
const MAX_URL_LENGTH: usize = 2048;
const MAX_METADATA_LENGTH: usize = 512;
const SIDE_EFFECT_REQUEST_LIMIT: usize = 30;
const SIDE_EFFECT_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(10);
const MAX_PIPE_REQUEST_BYTES: usize = 1024 * 1024;
const PIPE_READ_TIMEOUT: Duration = Duration::from_secs(5);
const PIPE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const PIPE_MAX_INSTANCES: usize = 4;
const ENQUEUE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the desktop waits for the user to accept/dismiss a browser handoff prompt.
pub const DOWNLOAD_PROMPT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

static SIDE_EFFECT_REQUEST_TIMES: OnceLock<Mutex<VecDeque<Instant>>> = OnceLock::new();

/// User decision from the ask-mode dialog.
#[derive(Debug, Clone)]
pub enum PromptDecision {
    Accept {
        filename: Option<String>,
        directory: Option<PathBuf>,
    },
    Dismiss,
}

/// A browser handoff waiting for the user (or timeout).
#[derive(Debug)]
pub struct BrowserPrompt {
    pub id: String,
    pub url: String,
    pub suggested_filename: Option<String>,
    pub total_bytes: Option<u64>,
    pub browser: String,
    pub entry_point: String,
    pub page_title: Option<String>,
    pub created_at: Instant,
    reply: oneshot::Sender<PromptDecision>,
}

/// UI-facing view of a pending prompt (no reply channel).
#[derive(Debug, Clone)]
pub struct BrowserPromptView {
    pub id: String,
    pub url: String,
    pub suggested_filename: Option<String>,
    pub total_bytes: Option<u64>,
    pub browser: String,
    pub entry_point: String,
    pub page_title: Option<String>,
    pub default_directory: PathBuf,
}

impl BrowserPrompt {
    fn to_view(&self, default_directory: PathBuf) -> BrowserPromptView {
        BrowserPromptView {
            id: self.id.clone(),
            url: self.url.clone(),
            suggested_filename: self.suggested_filename.clone(),
            total_bytes: self.total_bytes,
            browser: self.browser.clone(),
            entry_point: self.entry_point.clone(),
            page_title: self.page_title.clone(),
            default_directory,
        }
    }
}

/// Shared snapshot the IPC server reads; the UI keeps it fresh.
#[derive(Clone)]
pub struct IpcBridge {
    inner: Arc<Mutex<IpcState>>,
    engine: EngineHandle,
    paths: AppPaths,
}

struct IpcState {
    download_directory: PathBuf,
    extension_settings: ExtensionIntegrationSettings,
    settings: Settings,
    jobs: Vec<Job>,
    /// FIFO of browser prompts waiting for UI (or currently shown).
    prompt_queue: VecDeque<BrowserPrompt>,
    /// Prompt id currently shown in the ask dialog (if any).
    active_prompt_id: Option<String>,
    /// Set by `show_window` IPC; UI polls and activates the main window.
    show_window_requested: bool,
}

impl IpcBridge {
    pub fn new(engine: EngineHandle, settings: &Settings, paths: AppPaths) -> Self {
        Self {
            inner: Arc::new(Mutex::new(IpcState {
                download_directory: settings.download_directory.clone(),
                extension_settings: settings.extension.clone(),
                settings: settings.clone(),
                jobs: Vec::new(),
                prompt_queue: VecDeque::new(),
                active_prompt_id: None,
                show_window_requested: false,
            })),
            engine,
            paths,
        }
    }

    /// Request that the main window be focused/restored (extension "Open app").
    pub fn request_show_window(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.show_window_requested = true;
        }
    }

    /// Consume a pending show-window request. Returns true once per request.
    pub fn take_show_window_request(&self) -> bool {
        self.inner
            .lock()
            .ok()
            .map(|mut guard| {
                let requested = guard.show_window_requested;
                guard.show_window_requested = false;
                requested
            })
            .unwrap_or(false)
    }

    pub fn update_settings(&self, settings: &Settings) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.download_directory = settings.download_directory.clone();
            guard.extension_settings = settings.extension.clone();
            guard.settings = settings.clone();
        }
    }

    pub fn update_jobs(&self, jobs: &[Job]) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.jobs = jobs.to_vec();
        }
    }

    /// Pull extension settings that may have been saved by the extension bridge.
    pub fn extension_settings(&self) -> Option<ExtensionIntegrationSettings> {
        self.inner
            .lock()
            .ok()
            .map(|guard| guard.extension_settings.clone())
    }

    /// If no dialog is open and a prompt is waiting, mark it active and return a UI view.
    pub fn claim_next_prompt_for_ui(&self) -> Option<BrowserPromptView> {
        let mut guard = self.inner.lock().ok()?;
        if guard.active_prompt_id.is_some() {
            return None;
        }
        // Drop timed-out prompts still sitting in the queue.
        let now = Instant::now();
        while let Some(front) = guard.prompt_queue.front() {
            if now.duration_since(front.created_at) > DOWNLOAD_PROMPT_TIMEOUT {
                if let Some(stale) = guard.prompt_queue.pop_front() {
                    let _ = stale.reply.send(PromptDecision::Dismiss);
                }
            } else {
                break;
            }
        }
        let dir = guard.download_directory.clone();
        let view = guard.prompt_queue.front()?.to_view(dir);
        guard.active_prompt_id = Some(view.id.clone());
        Some(view)
    }

    /// Resolve the active prompt (or any matching id) with the user's decision.
    pub fn resolve_prompt(&self, prompt_id: &str, decision: PromptDecision) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        let Some(index) = guard.prompt_queue.iter().position(|p| p.id == prompt_id) else {
            return false;
        };
        let prompt = guard.prompt_queue.remove(index).expect("index checked");
        if guard.active_prompt_id.as_deref() == Some(prompt_id) {
            guard.active_prompt_id = None;
        }
        prompt.reply.send(decision).is_ok()
    }

    /// Whether a prompt is still waiting (used to close stale dialogs after IPC timeout).
    pub fn is_prompt_pending(&self, prompt_id: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .is_some_and(|guard| guard.prompt_queue.iter().any(|p| p.id == prompt_id))
    }

    fn enqueue_prompt(&self, prompt: BrowserPrompt) -> Result<(), oneshot::Sender<PromptDecision>> {
        let Ok(mut guard) = self.inner.lock() else {
            return Err(prompt.reply);
        };
        // Cap queue depth so a runaway extension cannot OOM the app.
        if guard.prompt_queue.len() >= 20 {
            return Err(prompt.reply);
        }
        guard.prompt_queue.push_back(prompt);
        Ok(())
    }

    fn snapshot(&self) -> Option<(PathBuf, ExtensionIntegrationSettings, Settings, Vec<Job>)> {
        let guard = self.inner.lock().ok()?;
        Some((
            guard.download_directory.clone(),
            guard.extension_settings.clone(),
            guard.settings.clone(),
            guard.jobs.clone(),
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostRequest {
    protocol_version: u32,
    request_id: String,
    #[serde(rename = "type")]
    message_type: String,
    #[serde(default)]
    payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnqueueSource {
    entry_point: String,
    browser: String,
    extension_version: String,
    page_url: Option<String>,
    page_title: Option<String>,
    referrer: Option<String>,
    #[allow(dead_code)]
    incognito: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnqueuePayload {
    url: String,
    source: EnqueueSource,
    suggested_filename: Option<String>,
    #[allow(dead_code)]
    total_bytes: Option<u64>,
    handoff_auth: Option<RawHandoffAuth>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHandoffAuth {
    headers: Vec<RawHandoffAuthHeader>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHandoffAuthHeader {
    name: String,
    value: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HostResponse {
    ok: bool,
    request_id: String,
    #[serde(rename = "type")]
    message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl HostResponse {
    fn ready(
        request_id: String,
        settings: &Settings,
        extension: &ExtensionIntegrationSettings,
        jobs: &[Job],
    ) -> Self {
        Self {
            ok: true,
            request_id,
            message_type: "ready".into(),
            payload: Some(json!({
                "appState": "running",
                "appVersion": env!("CARGO_PKG_VERSION"),
                "connectionState": "connected",
                "queueSummary": queue_summary(jobs),
                "extensionSettings": extension.to_protocol_json(),
                "appearanceSettings": appearance_settings_dto(settings),
            })),
            code: None,
            message: None,
        }
    }

    fn enqueue_result(request_id: String, outcome: EnqueueOutcome) -> Self {
        Self {
            ok: true,
            request_id,
            message_type: outcome.status.as_protocol().into(),
            payload: Some(json!({
                "jobId": outcome.job_id,
                "filename": outcome.filename,
                "status": outcome.status.as_protocol(),
            })),
            code: None,
            message: None,
        }
    }

    fn prompt_dismissed(request_id: String) -> Self {
        Self {
            ok: true,
            request_id,
            message_type: "prompt_dismissed".into(),
            payload: Some(json!({
                "status": "dismissed",
            })),
            code: None,
            message: None,
        }
    }

    fn error(request_id: String, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            request_id,
            message_type: "rejected".into(),
            payload: None,
            code: Some(code),
            message: Some(message.into()),
        }
    }
}

fn queue_summary(jobs: &[Job]) -> Value {
    let mut queued = 0u32;
    let mut downloading = 0u32;
    let mut completed = 0u32;
    let mut failed = 0u32;
    let mut attention = 0u32;
    for job in jobs {
        match job.state {
            JobState::Queued | JobState::Starting | JobState::Paused => queued += 1,
            JobState::Downloading => downloading += 1,
            JobState::Completed => completed += 1,
            JobState::Failed => {
                failed += 1;
                attention += 1;
            }
            JobState::Canceled => {}
        }
    }
    let active = queued + downloading;
    json!({
        "total": jobs.len(),
        "active": active,
        "attention": attention,
        "queued": queued,
        "downloading": downloading,
        "completed": completed,
        "failed": failed,
    })
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

async fn handle_request(bridge: &IpcBridge, request: HostRequest) -> HostResponse {
    if let Err(response) = validate_host_request(&request) {
        return response;
    }

    if is_side_effect_rate_limited(&request.message_type) {
        return HostResponse::error(
            request.request_id,
            "RATE_LIMITED",
            "Too many extension bridge requests. Try again shortly.",
        );
    }

    match request.message_type.as_str() {
        "ping" | "get_status" => {
            let Some((_, extension, settings, jobs)) = bridge.snapshot() else {
                return HostResponse::error(
                    request.request_id,
                    "INTERNAL_ERROR",
                    "Could not read app state.",
                );
            };
            HostResponse::ready(request.request_id, &settings, &extension, &jobs)
        }
        "show_window" => {
            bridge.request_show_window();
            let Some((_, extension, settings, jobs)) = bridge.snapshot() else {
                return HostResponse::error(
                    request.request_id,
                    "INTERNAL_ERROR",
                    "Could not read app state.",
                );
            };
            HostResponse::ready(request.request_id, &settings, &extension, &jobs)
        }
        "enqueue_download" => match parse_enqueue_payload(&request.request_id, &request.payload) {
            Ok(payload) => enqueue_download(bridge, request.request_id, payload).await,
            Err(response) => response,
        },
        "prompt_download" => match parse_enqueue_payload(&request.request_id, &request.payload) {
            Ok(payload) => prompt_download(bridge, request.request_id, payload).await,
            Err(response) => response,
        },
        "save_extension_settings" => {
            save_extension_settings(bridge, request.request_id, &request.payload)
        }
        _ => HostResponse::error(
            request.request_id,
            "INVALID_PAYLOAD",
            "Unsupported request type.",
        ),
    }
}

fn save_extension_settings(
    bridge: &IpcBridge,
    request_id: String,
    payload: &Value,
) -> HostResponse {
    match ExtensionIntegrationSettings::from_protocol_json(payload) {
        Ok(extension) => {
            if let Ok(mut guard) = bridge.inner.lock() {
                guard.extension_settings = extension.clone();
                guard.settings.extension = extension.clone();
                let _ = save_settings(&bridge.paths, &guard.settings);
            }
            let Some((_, extension, settings, jobs)) = bridge.snapshot() else {
                return HostResponse::error(
                    request_id,
                    "INTERNAL_ERROR",
                    "Could not read app state.",
                );
            };
            HostResponse::ready(request_id, &settings, &extension, &jobs)
        }
        Err(message) => HostResponse::error(request_id, "INVALID_PAYLOAD", message),
    }
}

fn parse_handoff_auth(raw: Option<RawHandoffAuth>) -> Option<HandoffAuth> {
    raw.map(|auth| HandoffAuth {
        headers: auth
            .headers
            .into_iter()
            .filter(|h| !h.name.trim().is_empty() && !h.value.is_empty())
            .take(32)
            .map(|h| HandoffAuthHeader {
                name: h.name.chars().take(MAX_METADATA_LENGTH).collect(),
                value: h.value.chars().take(16 * 1024).collect(),
            })
            .collect(),
    })
    .filter(|auth| !auth.headers.is_empty())
}

async fn enqueue_download(
    bridge: &IpcBridge,
    request_id: String,
    payload: EnqueuePayload,
) -> HostResponse {
    let Some((directory, _, _, jobs)) = bridge.snapshot() else {
        return HostResponse::error(request_id, "INTERNAL_ERROR", "Could not read app state.");
    };

    if directory.as_os_str().is_empty() {
        return HostResponse::error(
            request_id,
            "DESTINATION_NOT_CONFIGURED",
            "Download directory is not configured.",
        );
    }

    // Active duplicate: same URL still in queue / downloading / paused.
    if let Some(existing) = find_active_duplicate(&jobs, &payload.url) {
        return HostResponse::enqueue_result(
            request_id,
            EnqueueOutcome {
                job_id: existing.id.clone(),
                filename: existing.filename.clone(),
                status: EnqueueStatus::DuplicateExistingJob,
            },
        );
    }

    let handoff_auth = parse_handoff_auth(payload.handoff_auth);
    engine_enqueue(
        bridge,
        request_id,
        payload.url,
        payload.suggested_filename,
        directory,
        handoff_auth,
    )
    .await
}

async fn prompt_download(
    bridge: &IpcBridge,
    request_id: String,
    payload: EnqueuePayload,
) -> HostResponse {
    let Some((directory, _, _, jobs)) = bridge.snapshot() else {
        return HostResponse::error(request_id, "INTERNAL_ERROR", "Could not read app state.");
    };

    if directory.as_os_str().is_empty() {
        return HostResponse::error(
            request_id,
            "DESTINATION_NOT_CONFIGURED",
            "Download directory is not configured.",
        );
    }

    // Still short-circuit exact active duplicates without bothering the user.
    if let Some(existing) = find_active_duplicate(&jobs, &payload.url) {
        return HostResponse::enqueue_result(
            request_id,
            EnqueueOutcome {
                job_id: existing.id.clone(),
                filename: existing.filename.clone(),
                status: EnqueueStatus::DuplicateExistingJob,
            },
        );
    }

    let handoff_auth = parse_handoff_auth(payload.handoff_auth);
    let (reply_tx, reply_rx) = oneshot::channel();
    let prompt_id = uuid::Uuid::new_v4().to_string();
    let prompt = BrowserPrompt {
        id: prompt_id.clone(),
        url: payload.url.clone(),
        suggested_filename: payload.suggested_filename.clone(),
        total_bytes: payload.total_bytes,
        browser: payload.source.browser.clone(),
        entry_point: payload.source.entry_point.clone(),
        page_title: payload.source.page_title.clone(),
        created_at: Instant::now(),
        reply: reply_tx,
    };

    if bridge.enqueue_prompt(prompt).is_err() {
        return HostResponse::error(
            request_id,
            "RATE_LIMITED",
            "Too many pending download prompts. Accept or dismiss existing ones first.",
        );
    }

    let decision = match tokio::time::timeout(DOWNLOAD_PROMPT_TIMEOUT, reply_rx).await {
        Ok(Ok(decision)) => decision,
        Ok(Err(_)) => PromptDecision::Dismiss,
        Err(_) => {
            // Timed out waiting for the user — remove from queue so the dialog can close.
            let _ = bridge.resolve_prompt(&prompt_id, PromptDecision::Dismiss);
            PromptDecision::Dismiss
        }
    };

    match decision {
        PromptDecision::Dismiss => HostResponse::prompt_dismissed(request_id),
        PromptDecision::Accept {
            filename,
            directory: dir_override,
        } => {
            let directory = dir_override.unwrap_or(directory);
            let filename = filename.or(payload.suggested_filename);
            engine_enqueue(
                bridge,
                request_id,
                payload.url,
                filename,
                directory,
                handoff_auth,
            )
            .await
        }
    }
}

fn find_active_duplicate<'a>(jobs: &'a [Job], url: &str) -> Option<&'a Job> {
    jobs.iter().find(|job| {
        job.url == url
            && matches!(
                job.state,
                JobState::Queued | JobState::Starting | JobState::Downloading | JobState::Paused
            )
    })
}

async fn engine_enqueue(
    bridge: &IpcBridge,
    request_id: String,
    url: String,
    filename: Option<String>,
    directory: PathBuf,
    handoff_auth: Option<HandoffAuth>,
) -> HostResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    bridge.engine.send(EngineCommand::Add {
        url,
        filename,
        directory,
        handoff_auth,
        reply: Some(reply_tx),
    });

    match tokio::time::timeout(ENQUEUE_REPLY_TIMEOUT, reply_rx).await {
        Ok(Ok(outcome)) => HostResponse::enqueue_result(request_id, outcome),
        Ok(Err(_)) => HostResponse::error(
            request_id,
            "INTERNAL_ERROR",
            "Download engine closed before accepting the job.",
        ),
        Err(_) => HostResponse::error(
            request_id,
            "INTERNAL_ERROR",
            "Timed out waiting for the download engine.",
        ),
    }
}

fn parse_enqueue_payload(
    request_id: &str,
    payload: &Value,
) -> Result<EnqueuePayload, HostResponse> {
    let mut parsed: EnqueuePayload = serde_json::from_value(payload.clone()).map_err(|error| {
        HostResponse::error(
            request_id.to_string(),
            "INVALID_PAYLOAD",
            format!("Payload could not be parsed: {error}"),
        )
    })?;

    parsed.url = validate_http_url(request_id, &parsed.url)?;
    validate_source(request_id, &parsed.source)?;
    if let Some(name) = parsed.suggested_filename.as_deref() {
        if name.len() > MAX_METADATA_LENGTH {
            return Err(HostResponse::error(
                request_id.to_string(),
                "METADATA_TOO_LARGE",
                "suggestedFilename exceeds limit.",
            ));
        }
    }
    // Source metadata is only logged conceptually; keep for future UI labels.
    let _ = (
        &parsed.source.page_url,
        &parsed.source.page_title,
        &parsed.source.referrer,
        &parsed.source.extension_version,
        &parsed.source.browser,
        &parsed.source.entry_point,
    );
    Ok(parsed)
}

fn validate_source(request_id: &str, source: &EnqueueSource) -> Result<(), HostResponse> {
    if !matches!(
        source.entry_point.as_str(),
        "context_menu" | "popup" | "browser_download"
    ) {
        return Err(HostResponse::error(
            request_id.to_string(),
            "INVALID_PAYLOAD",
            "Source entry point is not supported.",
        ));
    }
    if !matches!(source.browser.as_str(), "chrome" | "edge" | "firefox") {
        return Err(HostResponse::error(
            request_id.to_string(),
            "INVALID_PAYLOAD",
            "Browser is not supported.",
        ));
    }
    for (field, value) in [
        ("extensionVersion", Some(source.extension_version.as_str())),
        ("pageUrl", source.page_url.as_deref()),
        ("pageTitle", source.page_title.as_deref()),
        ("referrer", source.referrer.as_deref()),
    ] {
        if value.is_some_and(|v| v.len() > MAX_METADATA_LENGTH) {
            return Err(HostResponse::error(
                request_id.to_string(),
                "METADATA_TOO_LARGE",
                format!("{field} exceeds limit."),
            ));
        }
    }
    Ok(())
}

fn validate_http_url(request_id: &str, raw_url: &str) -> Result<String, HostResponse> {
    let trimmed = raw_url.trim();
    if trimmed.len() > MAX_URL_LENGTH {
        return Err(HostResponse::error(
            request_id.to_string(),
            "URL_TOO_LONG",
            format!("URL exceeds {MAX_URL_LENGTH} characters."),
        ));
    }
    let parsed = Url::parse(trimmed).map_err(|_| {
        HostResponse::error(request_id.to_string(), "INVALID_URL", "URL is not valid.")
    })?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        _ => Err(HostResponse::error(
            request_id.to_string(),
            "UNSUPPORTED_SCHEME",
            "Only http and https URLs are supported.",
        )),
    }
}

fn validate_host_request(request: &HostRequest) -> Result<(), HostResponse> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(HostResponse::error(
            request.request_id.clone(),
            "HOST_PROTOCOL_MISMATCH",
            format!(
                "Expected protocol version {}, got {}.",
                PROTOCOL_VERSION, request.protocol_version
            ),
        ));
    }
    if !is_valid_request_id(&request.request_id) {
        return Err(HostResponse::error(
            request.request_id.clone(),
            "INVALID_PAYLOAD",
            "Request id is not supported.",
        ));
    }
    if !matches!(
        request.message_type.as_str(),
        "ping"
            | "get_status"
            | "show_window"
            | "enqueue_download"
            | "prompt_download"
            | "save_extension_settings"
    ) {
        return Err(HostResponse::error(
            request.request_id.clone(),
            "INVALID_PAYLOAD",
            "Unsupported request type.",
        ));
    }
    Ok(())
}

fn is_valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty()
        && request_id.len() <= MAX_REQUEST_ID_LENGTH
        && request_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

fn is_side_effect_rate_limited(message_type: &str) -> bool {
    if !matches!(
        message_type,
        "enqueue_download" | "prompt_download" | "save_extension_settings"
    ) {
        return false;
    }
    let times = SIDE_EFFECT_REQUEST_TIMES.get_or_init(|| Mutex::new(VecDeque::new()));
    let Ok(mut guard) = times.lock() else {
        return false;
    };
    let now = Instant::now();
    while guard
        .front()
        .is_some_and(|t| now.duration_since(*t) > SIDE_EFFECT_RATE_LIMIT_WINDOW)
    {
        guard.pop_front();
    }
    if guard.len() >= SIDE_EFFECT_REQUEST_LIMIT {
        return true;
    }
    guard.push_back(now);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_scheme() {
        let err = validate_http_url("r1", "ftp://example.com/a").unwrap_err();
        assert_eq!(err.code, Some("UNSUPPORTED_SCHEME"));
    }

    #[test]
    fn accepts_https() {
        let url = validate_http_url("r1", "https://example.com/file.zip").unwrap();
        assert!(url.starts_with("https://"));
    }
}
