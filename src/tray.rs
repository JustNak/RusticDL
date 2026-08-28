//! System tray (notification area / overflow) icon for Windows.
//! Provides a tray icon with a context menu so the user can restore the main
//! window or fully quit while the app is hidden via "close to tray".
//! Balloon notifications (`NIF_INFO`) are shown only on the tray message thread:
//! the UI calls [`SystemTray::show_notification`], which enqueues a payload and
//! posts a wake-up to the tray HWND. Ownership of balloon payloads always lives
//! in the shared queue (never only in a discarded PostMessage `LPARAM`).

use crate::branding::APP_NAME;

/// Severity icon for a tray balloon (`NIIF_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyLevel {
    Info,
    #[allow(dead_code)] // reserved for future policy levels
    Warning,
    Error,
}

/// Events the tray message thread posts back to the GPUI UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    ShowWindow,
    /// Fully quit the application.
    Exit,
    /// User clicked the balloon; `context_id` is the token from
    /// [`SystemTray::show_notification`] (policy layer maps the click).
    BalloonUserClick {
        context_id: u64,
    },
}

/// Maximum UTF-16 code units for balloon title (`szInfoTitle` is 64 including NUL).
pub const BALLOON_TITLE_MAX_UTF16: usize = 63;
/// Maximum UTF-16 code units for balloon body (`szInfo` is 256 including NUL).
pub const BALLOON_BODY_MAX_UTF16: usize = 255;

/// Truncate `s` so its UTF-16 encoding fits in `max_units` code units.
///
/// Used for `NOTIFYICONDATAW` string fields that require a trailing NUL.
pub fn truncate_utf16_units(s: &str, max_units: usize) -> &str {
    if max_units == 0 {
        return "";
    }
    let mut units = 0usize;
    let mut end = 0usize;
    for (i, ch) in s.char_indices() {
        let u = ch.len_utf16();
        if units + u > max_units {
            break;
        }
        units += u;
        end = i + ch.len_utf8();
    }
    &s[..end]
}

/// RAII handle for the background tray icon. Dropping it removes the icon.
pub struct SystemTray {
    #[cfg(windows)]
    thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    hwnd: std::sync::Arc<std::sync::atomic::AtomicIsize>,
    /// Pending balloon payloads owned by the queue (not PostMessage LPARAM).
    #[cfg(windows)]
    pending_balloons: windows_impl::PendingBalloons,
}

impl SystemTray {
    ///
    /// Returns `None` on non-Windows platforms or if creation fails.
    pub fn start(event_tx: async_channel::Sender<TrayEvent>) -> Option<Self> {
        #[cfg(windows)]
        {
            windows_impl::start(event_tx)
        }
        #[cfg(not(windows))]
        {
            let _ = event_tx;
            None
        }
    }

    ///
    /// Enqueues the payload and posts a wake-up to the tray message thread
    /// (never calls `Shell_NotifyIconW` from the caller). No-op if the tray
    /// HWND is not ready (`hwnd == 0`).
    ///
    /// `context_id` is stored for the active balloon (after a successful
    /// `NIM_MODIFY`) and echoed on [`TrayEvent::BalloonUserClick`].
    pub fn show_notification(&self, title: &str, body: &str, level: NotifyLevel, context_id: u64) {
        #[cfg(windows)]
        {
            windows_impl::show_notification(
                &self.hwnd,
                &self.pending_balloons,
                title,
                body,
                level,
                context_id,
            );
        }
        #[cfg(not(windows))]
        {
            let _ = (title, body, level, context_id);
        }
    }
}

impl Drop for SystemTray {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

            let raw = self.hwnd.swap(0, std::sync::atomic::Ordering::SeqCst);
            if raw != 0 {
                let hwnd = HWND(raw as *mut core::ffi::c_void);
                let _ = unsafe {
                    PostMessageW(Some(hwnd), WM_CLOSE, WPARAM::default(), LPARAM::default())
                };
            }
            if let Some(handle) = self.thread.take() {
                let (done_tx, done_rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = handle.join();
                    let _ = done_tx.send(());
                });
                let _ = done_rx.recv_timeout(std::time::Duration::from_millis(750));
            }
            if let Ok(mut q) = self.pending_balloons.lock() {
                q.clear();
            }
        }
    }
}

/// Capture the Win32 HWND for a GPUI window (0 if unavailable).
///
/// Stored so tray / IPC can restore the window without waiting for the next
/// GPUI render frame (hidden windows often stop painting).
pub fn main_window_hwnd(window: &gpui::Window) -> isize {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return 0;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return 0;
        };
        win32.hwnd.get() as isize
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        0
    }
}

/// Hide a GPUI window from the taskbar (true tray hide).
pub fn hide_main_window(window: &gpui::Window) {
    #[cfg(windows)]
    {
        let hwnd = main_window_hwnd(window);
        if hwnd != 0 {
            show_hwnd(hwnd, false);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

pub fn show_main_window(window: &gpui::Window) {
    #[cfg(windows)]
    {
        let hwnd = main_window_hwnd(window);
        if hwnd != 0 {
            show_hwnd(hwnd, true);
        }
    }
    window.activate_window();
}

/// Restore/show a main window by raw HWND (safe without a GPUI `Window`).
///
/// Used when the UI is hidden to tray and may not paint until the HWND is shown
/// again — tray and second-instance activate must not wait on `Render`.
pub fn show_main_window_hwnd(hwnd: isize) {
    #[cfg(windows)]
    {
        if hwnd != 0 {
            show_hwnd(hwnd, true);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
    }
}

#[cfg(windows)]
fn show_hwnd(hwnd_raw: isize, show: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, SetForegroundWindow, ShowWindow, SW_HIDE, SW_RESTORE, SW_SHOW,
    };

    let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
    unsafe {
        if show {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            let _ = SetForegroundWindow(hwnd);
        } else {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::{
        truncate_utf16_units, NotifyLevel, SystemTray, TrayEvent, APP_NAME, BALLOON_BODY_MAX_UTF16,
        BALLOON_TITLE_MAX_UTF16,
    };
    use crate::branding::APP_ICON_ICO;
    use std::collections::VecDeque;
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO,
        NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_BALLOONTIMEOUT,
        NIN_BALLOONUSERCLICK, NOTIFYICONDATAW, NOTIFYICONDATAW_0, NOTIFYICON_VERSION,
        NOTIFY_ICON_INFOTIP_FLAGS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
        DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, LoadImageW,
        PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW,
        TrackPopupMenu, TranslateMessage, UnregisterClassW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
        GWLP_USERDATA, HICON, HWND_MESSAGE, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, MF_STRING,
        MSG, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE,
        WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP,
        WNDCLASSW,
    };

    const TRAY_UID: u32 = 1;
    const ID_TRAY_SHOW: usize = 1001;
    const ID_TRAY_EXIT: usize = 1002;
    /// Custom callback message delivered to our message-only window.
    const WM_TRAYICON: u32 = WM_APP + 40;
    /// UI → tray thread: drain pending balloon queue and apply.
    /// `LPARAM` is unused — payloads live in [`PendingBalloons`].
    const WM_SHOW_BALLOON: u32 = WM_APP + 41;

    /// Shared queue of balloon payloads. Always owns the heap data; `PostMessage`
    /// is only a wake-up so DestroyWindow cannot leak discarded LPARAMs.
    pub(super) type PendingBalloons = Arc<Mutex<VecDeque<BalloonRequest>>>;

    pub(super) struct BalloonRequest {
        title: String,
        body: String,
        level: NotifyLevel,
        context_id: u64,
    }

    struct TrayState {
        event_tx: async_channel::Sender<TrayEvent>,
        icon: HICON,
        pending_balloons: PendingBalloons,
        /// Context id of the balloon currently shown (if any).
        /// Only set after a successful `NIM_MODIFY`.
        active_balloon_context_id: Option<u64>,
    }

    pub(super) fn start(event_tx: async_channel::Sender<TrayEvent>) -> Option<SystemTray> {
        let hwnd_slot = Arc::new(AtomicIsize::new(0));
        let hwnd_for_thread = hwnd_slot.clone();
        let pending_balloons: PendingBalloons = Arc::new(Mutex::new(VecDeque::new()));
        let pending_for_thread = pending_balloons.clone();

        let thread = thread::Builder::new()
            .name("rusticdl-tray".into())
            .spawn(move || {
                if let Err(err) = run_tray_loop(event_tx, hwnd_for_thread, pending_for_thread) {
                    eprintln!("[rusticdl] tray: {err}");
                }
            })
            .ok()?;

        for _ in 0..50 {
            if hwnd_slot.load(Ordering::SeqCst) != 0 {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }

        Some(SystemTray {
            thread: Some(thread),
            hwnd: hwnd_slot,
            pending_balloons,
        })
    }

    pub(super) fn show_notification(
        hwnd_slot: &AtomicIsize,
        pending: &PendingBalloons,
        title: &str,
        body: &str,
        level: NotifyLevel,
        context_id: u64,
    ) {
        let raw = hwnd_slot.load(Ordering::SeqCst);
        if raw == 0 {
            return;
        }
        let req = BalloonRequest {
            title: truncate_utf16_units(title, BALLOON_TITLE_MAX_UTF16).to_string(),
            body: truncate_utf16_units(body, BALLOON_BODY_MAX_UTF16).to_string(),
            level,
            context_id,
        };
        if let Ok(mut q) = pending.lock() {
            q.push_back(req);
        } else {
            return;
        }
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        let _ = unsafe {
            PostMessageW(
                Some(hwnd),
                WM_SHOW_BALLOON,
                WPARAM::default(),
                LPARAM::default(),
            )
        };
    }

    fn drain_pending(pending: &PendingBalloons) -> Vec<BalloonRequest> {
        pending
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    fn run_tray_loop(
        event_tx: async_channel::Sender<TrayEvent>,
        hwnd_slot: Arc<AtomicIsize>,
        pending_balloons: PendingBalloons,
    ) -> Result<(), String> {
        unsafe {
            let module = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandle: {e}"))?;
            let hinstance = HINSTANCE(module.0);
            let class_name = w!("RusticDLTrayMessageWindow");

            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: hinstance,
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let icon = load_tray_icon().unwrap_or(HICON(std::ptr::null_mut()));
            let state = Box::new(TrayState {
                event_tx,
                icon,
                pending_balloons: pending_balloons.clone(),
                active_balloon_context_id: None,
            });
            let state_ptr = Box::into_raw(state);

            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("RusticDL Tray"),
                WINDOW_STYLE::default(),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                Some(HWND_MESSAGE),
                None,
                Some(hinstance),
                Some(state_ptr as *const core::ffi::c_void),
            ) {
                Ok(h) => h,
                Err(e) => {
                    drop(Box::from_raw(state_ptr));
                    let _ = drain_pending(&pending_balloons);
                    return Err(format!("CreateWindowEx: {e}"));
                }
            };

            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);

            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_UID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_TRAYICON,
                hIcon: icon,
                ..Default::default()
            };
            write_utf16_buf(&mut nid.szTip, APP_NAME);

            if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                let _ = DestroyWindow(hwnd);
                let _ = drain_pending(&pending_balloons);
                return Err("Shell_NotifyIcon NIM_ADD failed".into());
            }

            let ver = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_UID,
                Anonymous: NOTIFYICONDATAW_0 {
                    uVersion: NOTIFYICON_VERSION,
                },
                ..Default::default()
            };
            if !Shell_NotifyIconW(NIM_SETVERSION, &ver).as_bool() {
                eprintln!(
                    "[rusticdl] tray: NIM_SETVERSION failed; balloon click callbacks may be unreliable"
                );
            }

            hwnd_slot.store(hwnd.0 as isize, Ordering::SeqCst);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            hwnd_slot.store(0, Ordering::SeqCst);
            let _ = drain_pending(&pending_balloons);

            let del = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_UID,
                ..Default::default()
            };
            let _ = Shell_NotifyIconW(NIM_DELETE, &del);

            let _ = UnregisterClassW(class_name, Some(hinstance));
            Ok(())
        }
    }

    unsafe extern "system" fn tray_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe {
            match msg {
                WM_DESTROY => {
                    let nid = NOTIFYICONDATAW {
                        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                        hWnd: hwnd,
                        uID: TRAY_UID,
                        ..Default::default()
                    };
                    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);

                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayState;
                    if !ptr.is_null() {
                        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                        let state = Box::from_raw(ptr);
                        let _ = drain_pending(&state.pending_balloons);
                        if !state.icon.0.is_null() {
                            let _ = DestroyIcon(state.icon);
                        }
                    }
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                WM_CLOSE => {
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                WM_SHOW_BALLOON => {
                    apply_pending_balloons(hwnd);
                    LRESULT(0)
                }
                WM_TRAYICON => {
                    let notify = lparam.0 as u32;
                    match notify {
                        WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                            send_event(hwnd, TrayEvent::ShowWindow);
                        }
                        WM_RBUTTONUP => {
                            show_context_menu(hwnd);
                        }
                        NIN_BALLOONUSERCLICK => {
                            if let Some(context_id) = take_active_balloon_context(hwnd) {
                                send_event(hwnd, TrayEvent::BalloonUserClick { context_id });
                            }
                        }
                        // Clear only on timeout. Do not clear on NIN_BALLOONHIDE:
                        NIN_BALLOONTIMEOUT => {
                            clear_active_balloon_context(hwnd);
                        }
                        _ => {}
                    }
                    LRESULT(0)
                }
                WM_COMMAND => {
                    let id = wparam.0 & 0xFFFF;
                    match id {
                        ID_TRAY_SHOW => send_event(hwnd, TrayEvent::ShowWindow),
                        ID_TRAY_EXIT => send_event(hwnd, TrayEvent::Exit),
                        _ => {}
                    }
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
    }

    unsafe fn apply_pending_balloons(hwnd: HWND) {
        unsafe {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayState;
            if ptr.is_null() {
                return;
            }
            let state = &mut *ptr;
            let pending = drain_pending(&state.pending_balloons);
            for req in pending {
                apply_balloon(hwnd, state, req);
            }
        }
    }

    unsafe fn apply_balloon(hwnd: HWND, state: &mut TrayState, req: BalloonRequest) {
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_UID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_INFO,
                uCallbackMessage: WM_TRAYICON,
                hIcon: state.icon,
                dwInfoFlags: level_to_flags(req.level),
                ..Default::default()
            };
            write_utf16_buf(&mut nid.szTip, APP_NAME);
            write_utf16_buf(&mut nid.szInfoTitle, &req.title);
            write_utf16_buf(&mut nid.szInfo, &req.body);
            if Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() {
                state.active_balloon_context_id = Some(req.context_id);
            } else {
                eprintln!(
                    "[rusticdl] tray: NIM_MODIFY (balloon) failed for context_id={}",
                    req.context_id
                );
            }
        }
    }

    fn level_to_flags(level: NotifyLevel) -> NOTIFY_ICON_INFOTIP_FLAGS {
        match level {
            NotifyLevel::Info => NIIF_INFO,
            NotifyLevel::Warning => NIIF_WARNING,
            NotifyLevel::Error => NIIF_ERROR,
        }
    }

    unsafe fn take_active_balloon_context(hwnd: HWND) -> Option<u64> {
        unsafe {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayState;
            if ptr.is_null() {
                return None;
            }
            let state = &mut *ptr;
            state.active_balloon_context_id.take()
        }
    }

    unsafe fn clear_active_balloon_context(hwnd: HWND) {
        unsafe {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayState;
            if ptr.is_null() {
                return;
            }
            (*ptr).active_balloon_context_id = None;
        }
    }

    unsafe fn send_event(hwnd: HWND, event: TrayEvent) {
        unsafe {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayState;
            if ptr.is_null() {
                return;
            }
            let state = &*ptr;
            let _ = state.event_tx.send_blocking(event);
        }
    }

    unsafe fn show_context_menu(hwnd: HWND) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            let show_label = wide_null(&format!("Show {APP_NAME}"));
            let exit_label = wide_null("Exit");
            let _ = AppendMenuW(menu, MF_STRING, ID_TRAY_SHOW, PCWSTR(show_label.as_ptr()));
            let _ = AppendMenuW(menu, MF_STRING, ID_TRAY_EXIT, PCWSTR(exit_label.as_ptr()));

            let mut pt = windows::Win32::Foundation::POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(
                menu,
                TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                Some(0),
                hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
        }
    }

    fn load_tray_icon() -> Option<HICON> {
        let path = resolve_icon_path()?;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            LoadImageW(
                None,
                PCWSTR(wide.as_ptr()),
                IMAGE_ICON,
                0,
                0,
                LR_LOADFROMFILE | LR_DEFAULTSIZE,
            )
        }
        .ok()?;
        Some(HICON(handle.0))
    }

    fn resolve_icon_path() -> Option<PathBuf> {
        let candidates = [
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("assets").join(APP_ICON_ICO))),
            Some(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("assets")
                    .join(APP_ICON_ICO),
            ),
        ];
        candidates.into_iter().flatten().find(|p| p.exists())
    }

    fn write_utf16_buf(buf: &mut [u16], text: &str) {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        if wide.len() >= buf.len() {
            wide.truncate(buf.len() - 1);
        }
        buf.fill(0);
        buf[..wide.len()].copy_from_slice(&wide);
    }

    fn wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{truncate_utf16_units, BALLOON_BODY_MAX_UTF16, BALLOON_TITLE_MAX_UTF16};

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate_utf16_units("hello", 63), "hello");
    }

    #[test]
    fn truncate_title_to_63_units() {
        let s: String = "a".repeat(100);
        let out = truncate_utf16_units(&s, BALLOON_TITLE_MAX_UTF16);
        assert_eq!(out.encode_utf16().count(), BALLOON_TITLE_MAX_UTF16);
        assert_eq!(out.len(), BALLOON_TITLE_MAX_UTF16);
    }

    #[test]
    fn truncate_body_to_255_units() {
        let s: String = "b".repeat(300);
        let out = truncate_utf16_units(&s, BALLOON_BODY_MAX_UTF16);
        assert_eq!(out.encode_utf16().count(), BALLOON_BODY_MAX_UTF16);
    }

    #[test]
    fn truncate_respects_multibyte_utf16() {
        let s = "😀😀😀";
        let out = truncate_utf16_units(s, 4);
        assert_eq!(out, "😀😀");
        assert_eq!(out.encode_utf16().count(), 4);
        let out2 = truncate_utf16_units(s, 3);
        assert_eq!(out2, "😀");
    }

    #[test]
    fn truncate_zero_is_empty() {
        assert_eq!(truncate_utf16_units("abc", 0), "");
    }
}
