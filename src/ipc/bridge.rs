//! Shared IPC bridge state between the UI and the named-pipe server.

use std::collections::{HashSet, VecDeque};
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
        overwrite: bool,
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
    pub(crate) jobs: Arc<Vec<Job>>,
    /// FIFO of browser prompts waiting for UI (or currently shown).
    pub(crate) prompt_queue: VecDeque<BrowserPrompt>,
    /// Prompt ids currently shown in an ask dialog (more than one HUD can be open).
    pub(crate) active_prompt_ids: HashSet<String>,
    /// Set by `show_window` IPC; UI polls and activates the main window.
    pub(crate) show_window_requested: bool,
    /// Job ids from browser handoff that should open the progress HUD.
    pub(crate) pending_progress_job_ids: VecDeque<String>,
    /// Job ids to watch for Complete re-open (even if Progress HUD owns them).
    pub(crate) pending_progress_watch_ids: VecDeque<String>,
    /// Job ids already bound to an open capture HUD (avoid double windows).
    pub(crate) progress_hud_owned_jobs: HashSet<String>,
    /// URLs where Confirm morph is waiting to bind a newly enqueued job.
    pub(crate) progress_hud_waiting_urls: HashSet<String>,
    /// Completed jobs for which the Complete HUD was already shown (once per id).
    pub(crate) complete_hud_shown: HashSet<String>,
    /// Bumped when the main window hides to tray so leftover HUDs can close.
    pub(crate) capture_close_epoch: u64,
}

impl IpcBridge {
    pub fn new(engine: EngineHandle, settings: &Settings, paths: AppPaths) -> Self {
        Self {
            inner: Arc::new(Mutex::new(IpcState {
                download_directory: settings.download_directory.clone(),
                extension_settings: settings.extension.clone(),
                settings: settings.clone(),
                jobs: Arc::new(Vec::new()),
                prompt_queue: VecDeque::new(),
                active_prompt_ids: HashSet::new(),
                show_window_requested: false,
                pending_progress_job_ids: VecDeque::new(),
                pending_progress_watch_ids: VecDeque::new(),
                progress_hud_owned_jobs: HashSet::new(),
                progress_hud_waiting_urls: HashSet::new(),
                complete_hud_shown: HashSet::new(),
                capture_close_epoch: 0,
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

    pub fn update_jobs(&self, jobs: Arc<Vec<Job>>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.jobs = jobs;
        }
    }

    /// Pull extension settings that may have been saved by the extension bridge.
    pub fn extension_settings(&self) -> Option<ExtensionIntegrationSettings> {
        self.inner
            .lock()
            .ok()
            .map(|guard| guard.extension_settings.clone())
    }

    /// Claim the next unshown prompt and return a UI view.
    ///
    /// Multiple confirm HUDs can be open at once; already-claimed ids stay in
    /// the queue until resolved and are skipped here.
    pub fn claim_next_prompt_for_ui(&self) -> Option<BrowserPromptView> {
        let mut guard = self.inner.lock().ok()?;
        // Drop timed-out prompts still sitting in the queue.
        let now = Instant::now();
        while let Some(front) = guard.prompt_queue.front() {
            if now.duration_since(front.created_at) > DOWNLOAD_PROMPT_TIMEOUT {
                if let Some(stale) = guard.prompt_queue.pop_front() {
                    guard.active_prompt_ids.remove(&stale.id);
                    let _ = stale.reply.send(PromptDecision::Dismiss);
                }
            } else {
                break;
            }
        }
        let index = guard
            .prompt_queue
            .iter()
            .position(|p| !guard.active_prompt_ids.contains(&p.id))?;
        let name = {
            let prompt = &guard.prompt_queue[index];
            prompt
                .suggested_filename
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| crate::download::derive_filename_from_url(&prompt.url))
                .unwrap_or_else(|| "download.bin".into())
        };
        let dir = guard.settings.resolve_save_directory(&name, None);
        let view = guard.prompt_queue[index].to_view(dir);
        guard.active_prompt_ids.insert(view.id.clone());
        Some(view)
    }

    /// Resolve a claimed or queued prompt with the user's decision.
    pub fn resolve_prompt(&self, prompt_id: &str, decision: PromptDecision) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        let Some(index) = guard.prompt_queue.iter().position(|p| p.id == prompt_id) else {
            return false;
        };
        let prompt = guard.prompt_queue.remove(index).expect("index checked");
        guard.active_prompt_ids.remove(prompt_id);
        prompt.reply.send(decision).is_ok()
    }

    /// Whether a prompt is still waiting (used to close stale dialogs after IPC timeout).
    #[allow(dead_code)] // used by unit tests; HUD timeout close can consult this
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
    ) -> Option<(
        PathBuf,
        ExtensionIntegrationSettings,
        Settings,
        Arc<Vec<Job>>,
    )> {
        let guard = self.inner.lock().ok()?;
        Some((
            guard.download_directory.clone(),
            guard.extension_settings.clone(),
            guard.settings.clone(),
            Arc::clone(&guard.jobs),
        ))
    }

    /// Queue a browser-handoff job for the floating progress HUD (deduped, capped).
    pub fn enqueue_progress_job(&self, job_id: String) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        if !guard
            .pending_progress_watch_ids
            .iter()
            .any(|id| id == &job_id)
        {
            if guard.pending_progress_watch_ids.len() >= 40 {
                let _ = guard.pending_progress_watch_ids.pop_front();
            }
            guard.pending_progress_watch_ids.push_back(job_id.clone());
        }
        if guard
            .pending_progress_job_ids
            .iter()
            .any(|id| id == &job_id)
        {
            return;
        }
        // Cap so a runaway extension cannot grow unbounded.
        if guard.pending_progress_job_ids.len() >= 20 {
            let _ = guard.pending_progress_job_ids.pop_front();
        }
        guard.pending_progress_job_ids.push_back(job_id);
    }

    /// Drain job ids that should be watched for Complete re-open.
    pub fn take_progress_watch_jobs(&self) -> Vec<String> {
        self.inner
            .lock()
            .ok()
            .map(|mut guard| guard.pending_progress_watch_ids.drain(..).collect())
            .unwrap_or_default()
    }

    /// Drain pending progress job ids that still need a new HUD window.
    ///
    /// Skips jobs already owned by a HUD and jobs whose URL is being bound by
    /// a Confirm→Progress morph (those windows claim the job themselves).
    /// `max` caps how many ready ids leave the queue (1 = one window per tick).
    pub fn take_pending_progress_jobs(&self) -> Vec<String> {
        self.take_pending_progress_jobs_n(usize::MAX)
    }

    pub fn take_pending_progress_jobs_n(&self, max: usize) -> Vec<String> {
        let Ok(mut guard) = self.inner.lock() else {
            return Vec::new();
        };
        if max == 0 {
            return Vec::new();
        }
        let mut open = Vec::new();
        let mut keep = VecDeque::new();
        while let Some(id) = guard.pending_progress_job_ids.pop_front() {
            if guard.progress_hud_owned_jobs.contains(&id) {
                continue;
            }
            // Job not in snapshot yet (JobsChanged lag): retry next poll so we do not
            // open a Progress HUD while Confirm morph still waits with job_id: None.
            let Some(job) = guard.jobs.iter().find(|j| j.id == id) else {
                keep.push_back(id);
                continue;
            };
            if guard.progress_hud_waiting_urls.contains(&job.url) {
                // Confirm morph will bind; leave id for a later claim if morph abandons.
                keep.push_back(id);
                continue;
            }
            if open.len() < max {
                open.push(id);
            } else {
                keep.push_back(id);
            }
        }
        guard.pending_progress_job_ids = keep;
        open
    }

    /// Look up one job without cloning the whole snapshot.
    pub fn job_by_id(&self, id: &str) -> Option<Job> {
        self.inner
            .lock()
            .ok()
            .and_then(|guard| guard.jobs.iter().find(|j| j.id == id).cloned())
    }

    /// Shared snapshot of current jobs (cheap Arc clone).
    pub fn jobs_snapshot(&self) -> Arc<Vec<Job>> {
        self.inner
            .lock()
            .ok()
            .map(|guard| Arc::clone(&guard.jobs))
            .unwrap_or_default()
    }

    pub fn note_progress_waiting_url(&self, url: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.progress_hud_waiting_urls.insert(url.to_string());
        }
    }

    pub fn clear_progress_waiting_url(&self, url: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.progress_hud_waiting_urls.remove(url);
        }
    }

    /// Mark a job as shown in a progress HUD. Returns false if already owned.
    pub fn try_own_progress_job(&self, job_id: &str) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        if guard.progress_hud_owned_jobs.contains(job_id) {
            return false;
        }
        guard.progress_hud_owned_jobs.insert(job_id.to_string());
        // Drop from pending queue if present.
        guard.pending_progress_job_ids.retain(|id| id != job_id);
        true
    }

    pub fn release_progress_job(&self, job_id: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.progress_hud_owned_jobs.remove(job_id);
        }
    }

    pub fn is_progress_hud_owned(&self, job_id: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .is_some_and(|g| g.progress_hud_owned_jobs.contains(job_id))
    }

    /// Open capture HUDs (confirm + progress/complete) currently tracked.
    ///
    /// Used to cascade newly opened windows so they do not stack on one pixel.
    pub fn capture_window_count(&self) -> usize {
        self.inner.lock().ok().map_or(0, |g| {
            g.active_prompt_ids.len() + g.progress_hud_owned_jobs.len()
        })
    }

    /// Returns true the first time Complete should be shown for this job.
    pub fn try_claim_complete_hud(&self, job_id: &str) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        if guard.complete_hud_shown.contains(job_id) {
            return false;
        }
        // Cap memory for long-running sessions.
        if guard.complete_hud_shown.len() >= 200 {
            guard.complete_hud_shown.clear();
        }
        guard.complete_hud_shown.insert(job_id.to_string());
        true
    }

    /// Undo a Complete claim when the window failed to open.
    pub fn release_complete_hud(&self, job_id: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.complete_hud_shown.remove(job_id);
        }
    }

    /// Close leftover Confirm/Progress HUDs when the main window hides to tray.
    ///
    /// Dismisses open confirm prompts, releases Progress ownership, and bumps
    /// the close epoch. Leaves `complete_hud_shown` (and its cap) intact so a
    /// finished job does not reopen Complete on restore.
    pub fn request_close_capture_windows(&self) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.capture_close_epoch = guard.capture_close_epoch.wrapping_add(1);
        let active_ids = std::mem::take(&mut guard.active_prompt_ids);
        let mut keep = VecDeque::new();
        while let Some(prompt) = guard.prompt_queue.pop_front() {
            if active_ids.contains(&prompt.id) {
                let _ = prompt.reply.send(PromptDecision::Dismiss);
            } else {
                keep.push_back(prompt);
            }
        }
        guard.prompt_queue = keep;
        guard.progress_hud_owned_jobs.clear();
    }

    /// Tray-hide close generation. Tests and HUD teardown compare against this.
    pub fn capture_close_epoch(&self) -> u64 {
        self.inner
            .lock()
            .ok()
            .map(|guard| guard.capture_close_epoch)
            .unwrap_or(0)
    }

    pub fn show_progress_after_handoff(&self) -> bool {
        self.inner
            .lock()
            .ok()
            .is_some_and(|g| g.extension_settings.show_progress_after_handoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::{EngineHandle, Job};
    use crate::persistence::AppPaths;
    use crate::settings::Settings;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_bridge() -> IpcBridge {
        let root = std::env::temp_dir().join(format!("rusticdl-ipc-test-{}", uuid::Uuid::new_v4()));
        IpcBridge::new(
            EngineHandle::stub(),
            &Settings::default(),
            AppPaths {
                settings: root.join("settings.json"),
                state: root.join("state.json"),
                pending_whats_new: root.join("pending_whats_new.json"),
                root,
            },
        )
    }

    fn test_prompt(id: &str, url: &str) -> (BrowserPrompt, oneshot::Receiver<PromptDecision>) {
        let (reply, rx) = oneshot::channel();
        (
            BrowserPrompt {
                id: id.to_string(),
                url: url.to_string(),
                suggested_filename: Some(format!("{id}.bin")),
                total_bytes: Some(1),
                browser: "chrome".into(),
                entry_point: "popup".into(),
                page_title: None,
                created_at: Instant::now(),
                reply,
            },
            rx,
        )
    }

    fn test_job(id: &str, url: &str) -> Job {
        let mut job = Job::new(
            url.to_string(),
            format!("{id}.bin"),
            PathBuf::from(format!("C:\\tmp\\{id}.bin")),
            PathBuf::from(format!("C:\\tmp\\{id}.bin.part")),
        );
        job.id = id.to_string();
        job
    }

    #[test]
    fn claim_next_prompt_allows_multiple_open_huds() {
        let ipc = test_bridge();
        let (prompt_a, _rx_a) = test_prompt("a", "https://example.com/a");
        let (prompt_b, _rx_b) = test_prompt("b", "https://example.com/b");
        ipc.enqueue_prompt(prompt_a).expect("enqueue a");
        ipc.enqueue_prompt(prompt_b).expect("enqueue b");

        let first = ipc.claim_next_prompt_for_ui().expect("first prompt");
        let second = ipc.claim_next_prompt_for_ui().expect("second prompt");
        assert_eq!(first.id, "a");
        assert_eq!(second.id, "b");
        assert!(ipc.claim_next_prompt_for_ui().is_none());
        assert_eq!(ipc.capture_window_count(), 2);

        assert!(ipc.resolve_prompt("a", PromptDecision::Dismiss));
        assert_eq!(ipc.capture_window_count(), 1);
        assert!(ipc.is_prompt_pending("b"));
        assert!(!ipc.is_prompt_pending("a"));
    }

    #[test]
    fn take_pending_progress_jobs_n_leaves_the_rest() {
        let ipc = test_bridge();
        ipc.update_jobs(Arc::new(vec![
            test_job("j1", "https://example.com/1"),
            test_job("j2", "https://example.com/2"),
            test_job("j3", "https://example.com/3"),
        ]));
        ipc.enqueue_progress_job("j1".into());
        ipc.enqueue_progress_job("j2".into());
        ipc.enqueue_progress_job("j3".into());

        assert_eq!(ipc.take_pending_progress_jobs_n(1), vec!["j1".to_string()]);
        assert_eq!(ipc.take_pending_progress_jobs_n(1), vec!["j2".to_string()]);
        assert_eq!(ipc.take_pending_progress_jobs(), vec!["j3".to_string()]);
        assert!(ipc.take_pending_progress_jobs_n(1).is_empty());
    }

    #[test]
    fn request_close_capture_windows_dismisses_open_huds_and_keeps_caps() {
        let ipc = test_bridge();
        let (prompt_open, mut rx_open) = test_prompt("open", "https://example.com/open");
        let (prompt_queued, mut rx_queued) = test_prompt("queued", "https://example.com/queued");
        ipc.enqueue_prompt(prompt_open).expect("enqueue open");
        ipc.enqueue_prompt(prompt_queued).expect("enqueue queued");
        assert_eq!(ipc.claim_next_prompt_for_ui().unwrap().id, "open");

        assert!(ipc.try_own_progress_job("progress-1"));
        assert!(ipc.try_claim_complete_hud("done-1"));
        assert_eq!(ipc.capture_window_count(), 2);
        assert_eq!(ipc.capture_close_epoch(), 0);

        ipc.request_close_capture_windows();

        assert_eq!(ipc.capture_close_epoch(), 1);
        assert_eq!(ipc.capture_window_count(), 0);
        assert!(!ipc.is_progress_hud_owned("progress-1"));
        assert!(
            !ipc.try_claim_complete_hud("done-1"),
            "complete cap must stay"
        );
        assert!(matches!(rx_open.try_recv(), Ok(PromptDecision::Dismiss)));
        assert!(
            ipc.is_prompt_pending("queued"),
            "unshown prompts stay queued"
        );
        assert!(rx_queued.try_recv().is_err());

        ipc.request_close_capture_windows();
        assert_eq!(ipc.capture_close_epoch(), 2);
    }
}
