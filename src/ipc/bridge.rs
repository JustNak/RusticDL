//! Shared IPC bridge state between the UI and the named-pipe server.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use crate::download::{EngineHandle, Job};
use crate::extension_settings::ExtensionIntegrationSettings;
use crate::persistence::AppPaths;
use crate::settings::Settings;

/// How long the desktop waits for the user to accept/dismiss a browser handoff prompt.
pub const DOWNLOAD_PROMPT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

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
    pub(crate) reply: oneshot::Sender<PromptDecision>,
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
    pub(crate) fn to_view(&self, default_directory: PathBuf) -> BrowserPromptView {
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
    pub(crate) inner: Arc<Mutex<IpcState>>,
    pub(crate) engine: EngineHandle,
    pub(crate) paths: AppPaths,
}

pub(crate) struct IpcState {
    pub(crate) download_directory: PathBuf,
    pub(crate) extension_settings: ExtensionIntegrationSettings,
    pub(crate) settings: Settings,
    pub(crate) jobs: Vec<Job>,
    /// FIFO of browser prompts waiting for UI (or currently shown).
    pub(crate) prompt_queue: VecDeque<BrowserPrompt>,
    /// Prompt id currently shown in the ask dialog (if any).
    pub(crate) active_prompt_id: Option<String>,
    /// Set by `show_window` IPC; UI polls and activates the main window.
    pub(crate) show_window_requested: bool,
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

    pub(crate) fn enqueue_prompt(
        &self,
        prompt: BrowserPrompt,
    ) -> Result<(), oneshot::Sender<PromptDecision>> {
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

    pub(crate) fn snapshot(
        &self,
    ) -> Option<(PathBuf, ExtensionIntegrationSettings, Settings, Vec<Job>)> {
        let guard = self.inner.lock().ok()?;
        Some((
            guard.download_directory.clone(),
            guard.extension_settings.clone(),
            guard.settings.clone(),
            guard.jobs.clone(),
        ))
    }
}
