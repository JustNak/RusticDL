//! System tray (notification area / overflow) icon.
//!
//! Windows uses a Win32 notification-area icon. Linux uses a
//! StatusNotifierItem (SNI) over D-Bus via `ksni`, which Waybar (Omarchy),
//! KDE, and GNOME's AppIndicator extension all host. When no SNI watcher or
//! host is running, [`SystemTray::start`] returns `None` and the caller falls
//! back instead of hiding the window with no way back.
//!
//! Provides a tray icon with a context menu so the user can restore the main
//! window or fully quit while the app is hidden via "close to tray".
//! Balloon notifications (`NIF_INFO`) are shown only on the tray message thread:
//! the UI calls [`SystemTray::show_notification`], which enqueues a payload and
//! posts a wake-up to the tray HWND. Ownership of balloon payloads always lives
//! in the shared queue (never only in a discarded PostMessage `LPARAM`).

use crate::branding::APP_NAME;
#[cfg(target_os = "linux")]
use crate::linux_restore;

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
    #[cfg(target_os = "linux")]
    linux: linux_impl::LinuxTrayHandle,
    /// Cleared while the StatusNotifierWatcher is gone (see `watcher_offline`).
    #[cfg(target_os = "linux")]
    online: linux_impl::Liveness,
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
        #[cfg(target_os = "linux")]
        {
            linux_impl::start(event_tx)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = event_tx;
            None
        }
    }

    /// Whether the icon can currently be seen and clicked.
    ///
    /// On Linux the SNI item outlives its watcher: when the bar or watcher
    /// goes away the handle stays open but nothing displays the icon. Callers
    /// must not treat such a tray as a way to restore a hidden window.
    pub fn is_online(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.online.is_visible() && !self.linux.is_closed()
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }

    /// Whether this tray can show balloon notifications itself.
    ///
    /// SNI has no balloon API; Linux notifications go through `notify-send`.
    pub fn supports_balloons(&self) -> bool {
        cfg!(windows)
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
        #[cfg(target_os = "linux")]
        {
            self.online.stop_monitor();
            let _ = self.linux.shutdown();
        }
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
///
/// Returns whether the window actually left the screen (or was minimized
/// somewhere that honours it). `false` means it is still visible, so callers
/// must not record it as hidden or tell the user it is gone.
pub fn hide_main_window(window: &gpui::Window) -> bool {
    #[cfg(windows)]
    {
        let hwnd = main_window_hwnd(window);
        if hwnd != 0 {
            show_hwnd(hwnd, false);
            return true;
        }
        false
    }
    #[cfg(target_os = "linux")]
    {
        // Hyprland ignores minimize; a special workspace really takes the
        // window off screen. Elsewhere minimize works only on desktops that
        // implement it (tiling compositors silently drop the request).
        if crate::hyprland::hide_main_windows() {
            return true;
        }
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
        if desktop_supports_minimize(desktop.as_deref()) {
            window.minimize_window();
            return true;
        }
        false
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = window;
        false
    }
}

/// Whether the session's compositor is known to honour minimize requests.
///
/// Tiling compositors have no minimized state, so `minimize_window` is a
/// silent no-op there and the window would stay on screen. Unknown or unset
/// desktops are treated as unsupported: leaving a window visible is safe,
/// marking a visible window hidden (and dismissing its pop-ups) is not.
#[cfg(target_os = "linux")]
pub(crate) fn desktop_supports_minimize(xdg_current_desktop: Option<&str>) -> bool {
    const NO_MINIMIZE: [&str; 9] = [
        "hyprland", "sway", "niri", "river", "i3", "bspwm", "dwl", "dwm", "xmonad",
    ];
    // Awesome, Qtile and Wayfire do implement minimize.
    const MINIMIZE: [&str; 18] = [
        "gnome",
        "kde",
        "plasma",
        "xfce",
        "x-cinnamon",
        "cinnamon",
        "mate",
        "lxqt",
        "lxde",
        "budgie",
        "pantheon",
        "unity",
        "deepin",
        "dde",
        "cosmic",
        "awesome",
        "qtile",
        "wayfire",
    ];
    let Some(desktop) = xdg_current_desktop else {
        return false;
    };
    let parts: Vec<&str> = desktop
        .split([':', ';'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let matches = |list: &[&str]| {
        parts
            .iter()
            .any(|part| list.iter().any(|d| part.eq_ignore_ascii_case(d)))
    };
    !matches(&NO_MINIMIZE) && matches(&MINIMIZE)
}

/// Show the main window.
///
/// Returns whether the window is back on screen. Wayland `activate` does not
/// clear minimized, so that session returns `false` unless Hyprland actually
/// moved the window; the hidden flag then stays set until the compositor
/// reports activation.
pub fn show_main_window(window: &mut gpui::Window) -> bool {
    #[cfg(windows)]
    {
        let hwnd = main_window_hwnd(window);
        if hwnd != 0 {
            show_hwnd(hwnd, true);
        }
        window.activate_window();
        return hwnd != 0;
    }
    #[cfg(target_os = "linux")]
    {
        // Compositor-side restore (Hyprland move, KWin unminimize) runs exactly
        // once per Show, in `DownloadApp::restore_main_window_now`. This render
        // path only activates; the hidden flag is owned by that restore.
        linux_restore::activate_for_restore(window);
        return false;
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        window.activate_window();
        true
    }
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

/// Hide `hwnd` again if it is visible.
///
/// GPUI's `WM_DISPLAYCHANGE` handler calls `ShowWindow(SW_SHOWNORMAL)` when
/// the window's last monitor is gone. That unhides an `SW_HIDE` tray window.
pub fn reassert_tray_hide(hwnd: isize) {
    #[cfg(windows)]
    {
        if hwnd != 0 && hwnd_is_visible(hwnd) {
            show_hwnd(hwnd, false);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
    }
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::{SystemTray, TrayEvent, APP_NAME};
    use ksni::blocking::{Handle, TrayMethods};
    use ksni::menu::StandardItem;
    use ksni::{Category, Icon, MenuItem, OfflineReason, ToolTip, Tray};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    /// Upper bound on the D-Bus registration. zbus has no handshake timeout,
    /// so a bus that accepts but never answers would otherwise hang the UI.
    const START_TIMEOUT: Duration = Duration::from_millis(1500);

    const TRAY_ICON_PNG: &[u8] = include_bytes!("../assets/brand/icon-64.png");
    const ICON_NAME: &str = "rusticdl";

    pub(super) type LinuxTrayHandle = Handle<LinuxTray>;

    const WATCHER_DEST: &str = "org.kde.StatusNotifierWatcher";
    const WATCHER_PATH: &str = "/StatusNotifierWatcher";
    const HOST_PROPERTY: &str = "IsStatusNotifierHostRegistered";
    const HOST_POLL: Duration = Duration::from_secs(1);
    const METHOD_TIMEOUT: Duration = Duration::from_secs(2);

    /// Whether the icon can currently be seen: the watcher owns its bus name
    /// *and* it reports a registered host. A standalone watcher keeps running
    /// after its host dies, so ownership alone is not enough.
    #[derive(Clone)]
    pub(super) struct Liveness {
        watcher_up: Arc<AtomicBool>,
        host_up: Arc<AtomicBool>,
        /// Monitor shutdown: flag + condvar so the poll wait ends immediately.
        stop: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Liveness {
        fn new() -> Self {
            Self {
                watcher_up: Arc::new(AtomicBool::new(true)),
                host_up: Arc::new(AtomicBool::new(true)),
                stop: Arc::new((Mutex::new(false), Condvar::new())),
            }
        }

        pub(super) fn is_visible(&self) -> bool {
            visible(
                self.watcher_up.load(Ordering::SeqCst),
                self.host_up.load(Ordering::SeqCst),
            )
        }

        pub(super) fn stop_monitor(&self) {
            let (lock, cvar) = &*self.stop;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cvar.notify_all();
        }

        /// Sleep up to `HOST_POLL`; returns `true` when asked to stop.
        fn wait_or_stop(&self) -> bool {
            let (lock, cvar) = &*self.stop;
            let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let (guard, _) = cvar
                .wait_timeout_while(guard, HOST_POLL, |stopped| !*stopped)
                .unwrap_or_else(|e| e.into_inner());
            *guard
        }

        /// Follow `IsStatusNotifierHostRegistered` on the watcher. Failures
        /// (no property, call error) leave the last value; watcher loss is
        /// reported separately through `watcher_offline`. KDE and GNOME
        /// hard-code the property to true, so they never flip.
        fn spawn_host_monitor(&self) {
            let live = self.clone();
            let _ = std::thread::Builder::new()
                .name("rusticdl-tray-host".into())
                .spawn(move || {
                    // A hung watcher must not wedge this thread: bound each call.
                    let Ok(conn) = zbus::blocking::connection::Builder::session()
                        .map(|b| b.method_timeout(METHOD_TIMEOUT))
                        .and_then(|b| b.build())
                    else {
                        return;
                    };
                    let Ok(proxy) =
                        zbus::blocking::proxy::Builder::<'_, zbus::blocking::Proxy>::new(&conn)
                            .destination(WATCHER_DEST)
                            .and_then(|b| b.path(WATCHER_PATH))
                            .and_then(|b| b.interface(WATCHER_DEST))
                            .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
                            .and_then(|b| b.build())
                    else {
                        return;
                    };
                    loop {
                        if let Ok(registered) = proxy.get_property::<bool>(HOST_PROPERTY) {
                            live.host_up.store(registered, Ordering::SeqCst);
                        }
                        if live.wait_or_stop() {
                            break;
                        }
                    }
                });
        }
    }

    pub(super) fn visible(watcher_up: bool, host_up: bool) -> bool {
        watcher_up && host_up
    }

    enum StartSlot {
        Pending,
        Done(Result<LinuxTrayHandle, ksni::Error>),
        Abandoned,
    }

    pub(super) struct LinuxTray {
        event_tx: async_channel::Sender<TrayEvent>,
        icon: Vec<Icon>,
        online: Liveness,
    }

    impl LinuxTray {
        fn send(&self, event: TrayEvent) {
            let _ = self.event_tx.send_blocking(event);
        }
    }

    impl Tray for LinuxTray {
        fn id(&self) -> String {
            "rusticdl".into()
        }

        fn title(&self) -> String {
            APP_NAME.into()
        }

        fn category(&self) -> Category {
            Category::ApplicationStatus
        }

        fn icon_name(&self) -> String {
            ICON_NAME.into()
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            self.icon.clone()
        }

        fn tool_tip(&self) -> ToolTip {
            ToolTip {
                title: APP_NAME.into(),
                ..Default::default()
            }
        }

        fn watcher_online(&self) {
            self.online.watcher_up.store(true, Ordering::SeqCst);
        }

        fn watcher_offline(&self, _reason: OfflineReason) -> bool {
            self.online.watcher_up.store(false, Ordering::SeqCst);
            // Keep the service alive: ksni re-registers when a watcher returns.
            true
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayEvent::ShowWindow);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                StandardItem {
                    label: format!("Show {APP_NAME}"),
                    activate: Box::new(|this: &mut Self| this.send(TrayEvent::ShowWindow)),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Quit".into(),
                    icon_name: "application-exit".into(),
                    activate: Box::new(|this: &mut Self| this.send(TrayEvent::Exit)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// Decode the bundled PNG into SNI's ARGB32 (network byte order) pixmap.
    pub(super) fn argb_pixmap(png: &[u8]) -> Option<Icon> {
        let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .ok()?
            .to_rgba8();
        let (width, height) = img.dimensions();
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for px in img.pixels() {
            let [r, g, b, a] = px.0;
            data.extend_from_slice(&[a, r, g, b]);
        }
        Some(Icon {
            width: width as i32,
            height: height as i32,
            data,
        })
    }

    /// Register an SNI item. `None` when D-Bus, the watcher, or a host is
    /// missing (or answers too slowly), so callers never hide the window behind
    /// a tray that is not there.
    pub(super) fn start(event_tx: async_channel::Sender<TrayEvent>) -> Option<SystemTray> {
        let online = Liveness::new();
        let tray = LinuxTray {
            event_tx,
            icon: argb_pixmap(TRAY_ICON_PNG).into_iter().collect(),
            online: online.clone(),
        };
        let slot = Arc::new((Mutex::new(StartSlot::Pending), Condvar::new()));
        let helper_slot = Arc::clone(&slot);
        let spawned = std::thread::Builder::new()
            .name("rusticdl-tray-start".into())
            .spawn(move || {
                let result = tray.spawn();
                let (lock, cvar) = &*helper_slot;
                let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
                match (&*state, result) {
                    (StartSlot::Abandoned, Ok(handle)) => {
                        // The caller timed out; do not leave an orphan icon.
                        let _ = handle.shutdown();
                    }
                    (StartSlot::Abandoned, Err(_)) => {}
                    (_, result) => {
                        *state = StartSlot::Done(result);
                        cvar.notify_all();
                    }
                }
            });
        if spawned.is_err() {
            return None;
        }

        let (lock, cvar) = &*slot;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let (mut state, _) = cvar
            .wait_timeout_while(guard, START_TIMEOUT, |s| matches!(s, StartSlot::Pending))
            .unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *state, StartSlot::Abandoned) {
            StartSlot::Done(Ok(handle)) => {
                online.spawn_host_monitor();
                Some(SystemTray {
                    linux: handle,
                    online,
                })
            }
            StartSlot::Done(Err(error)) => {
                eprintln!("rusticdl: tray unavailable ({error})");
                None
            }
            StartSlot::Pending | StartSlot::Abandoned => {
                eprintln!("rusticdl: tray unavailable (session bus did not answer)");
                None
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn bundled_icon_decodes_to_argb32() {
            let icon = argb_pixmap(TRAY_ICON_PNG).expect("icon decodes");
            assert_eq!(icon.width, 64);
            assert_eq!(icon.height, 64);
            assert_eq!(icon.data.len(), 64 * 64 * 4);
        }

        #[test]
        fn watcher_loss_marks_tray_offline_and_recovery_restores_it() {
            let (event_tx, _event_rx) = async_channel::unbounded();
            let online = Liveness::new();
            let tray = LinuxTray {
                event_tx,
                icon: Vec::new(),
                online: online.clone(),
            };
            assert!(tray.watcher_offline(OfflineReason::No));
            assert!(!online.is_visible());
            tray.watcher_online();
            assert!(online.is_visible());
        }

        #[test]
        fn tray_is_visible_only_with_watcher_and_host() {
            assert!(visible(true, true));
            assert!(!visible(false, true));
            assert!(!visible(true, false));
            assert!(!visible(false, false));
        }

        #[test]
        fn stopping_the_monitor_wakes_the_wait_immediately() {
            let online = Liveness::new();
            let waiter = online.clone();
            let began = std::time::Instant::now();
            let handle = std::thread::spawn(move || waiter.wait_or_stop());
            std::thread::sleep(Duration::from_millis(50));
            online.stop_monitor();
            assert!(handle.join().unwrap());
            assert!(began.elapsed() < HOST_POLL);
        }

        #[test]
        fn host_loss_marks_tray_invisible_until_host_returns() {
            let online = Liveness::new();
            online.host_up.store(false, Ordering::SeqCst);
            assert!(!online.is_visible());
            online.host_up.store(true, Ordering::SeqCst);
            assert!(online.is_visible());
        }

        struct BusAddressGuard(Option<std::ffi::OsString>);

        impl BusAddressGuard {
            fn set(value: &str) -> Self {
                let previous = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
                std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value);
                Self(previous)
            }
        }

        impl Drop for BusAddressGuard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value),
                    None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
                }
            }
        }

        #[test]
        fn start_returns_within_bound_when_session_bus_never_answers() {
            let dir =
                std::env::temp_dir().join(format!("rusticdl-hung-bus-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            let path = dir.join("bus");
            let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
            let _holder = std::thread::spawn(move || {
                let conns: Vec<_> = listener.incoming().take(4).flatten().collect();
                std::thread::sleep(Duration::from_secs(8));
                drop(conns);
            });
            let _bus = BusAddressGuard::set(&format!("unix:path={}", path.display()));
            let (event_tx, _event_rx) = async_channel::unbounded();
            let began = std::time::Instant::now();
            assert!(start(event_tx).is_none());
            assert!(began.elapsed() < START_TIMEOUT + Duration::from_secs(2));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn garbage_png_is_rejected() {
            assert!(argb_pixmap(b"not a png").is_none());
        }
    }
}

#[cfg(windows)]
fn hwnd_is_visible(hwnd_raw: isize) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;

    let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
    unsafe { IsWindowVisible(hwnd).as_bool() }
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
        AppendMenuW, ChangeWindowMessageFilterEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
        DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
        GetWindowLongPtrW, KillTimer, LoadIconW, LoadImageW, PostMessageW, PostQuitMessage,
        RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
        TrackPopupMenu, TranslateMessage, UnregisterClassW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
        GWLP_USERDATA, HICON, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE,
        MF_STRING, MSG, MSGFLT_ALLOW, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK,
        WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_OVERLAPPED,
    };

    const TRAY_UID: u32 = 1;
    const ID_TRAY_SHOW: usize = 1001;
    const ID_TRAY_EXIT: usize = 1002;
    const ID_RETRY_ADD: usize = 1;
    const RETRY_ADD_INTERVAL_MS: u32 = 1000;
    /// Custom callback message delivered to our hidden tray host window.
    const WM_TRAYICON: u32 = WM_APP + 40;
    /// UI → tray thread: drain pending balloon queue and apply.
    /// `LPARAM` is unused — payloads live in [`PendingBalloons`].
    const WM_SHOW_BALLOON: u32 = WM_APP + 41;

    /// Hidden top-level window. `HWND_MESSAGE` is not a top-level window, so it
    /// never receives the `TaskbarCreated` broadcast Explorer sends after restart.
    fn tray_host_ex_style() -> WINDOW_EX_STYLE {
        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW
    }

    fn tray_host_style() -> WINDOW_STYLE {
        WS_OVERLAPPED
    }

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
        icon_owned: bool,
        pending_balloons: PendingBalloons,
        /// Context id of the balloon currently shown (if any).
        /// Only set after a successful `NIM_MODIFY`.
        active_balloon_context_id: Option<u64>,
        taskbar_created_msg: u32,
        icon_added: bool,
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
            if thread.is_finished() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }

        if hwnd_slot.load(Ordering::SeqCst) == 0 && thread.is_finished() {
            let _ = thread.join();
            return None;
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
            let class_name = w!("RusticDLTrayHostWindow");

            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: hinstance,
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
            let loaded = load_tray_icon(hinstance);
            let taskbar_created_msg = RegisterWindowMessageW(w!("TaskbarCreated"));
            let state = Box::new(TrayState {
                event_tx,
                icon: loaded.icon,
                icon_owned: loaded.owned,
                pending_balloons: pending_balloons.clone(),
                active_balloon_context_id: None,
                taskbar_created_msg,
                icon_added: false,
            });
            let state_ptr = Box::into_raw(state);

            let hwnd = match CreateWindowExW(
                tray_host_ex_style(),
                class_name,
                w!("RusticDL Tray"),
                tray_host_style(),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
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
            if taskbar_created_msg != 0 {
                let _ = ChangeWindowMessageFilterEx(hwnd, taskbar_created_msg, MSGFLT_ALLOW, None);
            }

            hwnd_slot.store(hwnd.0 as isize, Ordering::SeqCst);
            ensure_notify_icon(hwnd, &mut *state_ptr);

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
                        if state.icon_owned && !state.icon.0.is_null() {
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
                WM_TIMER => {
                    if wparam.0 == ID_RETRY_ADD {
                        apply_pending_balloons(hwnd);
                    }
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
                _ => {
                    let taskbar = with_tray_state(hwnd, |state| {
                        if state.taskbar_created_msg != 0 && msg == state.taskbar_created_msg {
                            state.icon_added = false;
                            true
                        } else {
                            false
                        }
                    });
                    if taskbar {
                        apply_pending_balloons(hwnd);
                        LRESULT(0)
                    } else {
                        DefWindowProcW(hwnd, msg, wparam, lparam)
                    }
                }
            }
        }
    }

    fn with_tray_state<T: Default>(hwnd: HWND, f: impl FnOnce(&mut TrayState) -> T) -> T {
        unsafe {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayState;
            if ptr.is_null() {
                return T::default();
            }
            f(&mut *ptr)
        }
    }

    unsafe fn ensure_notify_icon(hwnd: HWND, state: &mut TrayState) {
        if state.icon_added {
            return;
        }
        if add_notify_icon(hwnd, state.icon) {
            state.icon_added = true;
            let _ = KillTimer(Some(hwnd), ID_RETRY_ADD);
            return;
        }
        let _ = SetTimer(Some(hwnd), ID_RETRY_ADD, RETRY_ADD_INTERVAL_MS, None);
    }

    unsafe fn add_notify_icon(hwnd: HWND, icon: HICON) -> bool {
        let nid = notify_icon_data(hwnd, icon);
        if Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            set_notify_icon_version(hwnd);
            return true;
        }
        if Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() {
            set_notify_icon_version(hwnd);
            return true;
        }
        false
    }

    fn notify_icon_data(hwnd: HWND, icon: HICON) -> NOTIFYICONDATAW {
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
        nid
    }

    fn set_notify_icon_version(hwnd: HWND) {
        let ver = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_UID,
            Anonymous: NOTIFYICONDATAW_0 {
                uVersion: NOTIFYICON_VERSION,
            },
            ..Default::default()
        };
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &ver) }.as_bool() {
            eprintln!(
                "[rusticdl] tray: NIM_SETVERSION failed; balloon click callbacks may be unreliable"
            );
        }
    }

    unsafe fn apply_pending_balloons(hwnd: HWND) {
        unsafe {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayState;
            if ptr.is_null() {
                return;
            }
            let state = &mut *ptr;
            ensure_notify_icon(hwnd, state);
            if !state.icon_added {
                return;
            }
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

    struct LoadedIcon {
        icon: HICON,
        owned: bool,
    }

    fn load_tray_icon(hinstance: HINSTANCE) -> LoadedIcon {
        if let Some(icon) = load_icon_from_file() {
            return LoadedIcon { icon, owned: true };
        }
        let from_resource = unsafe {
            LoadImageW(
                Some(hinstance),
                PCWSTR(1usize as *const u16),
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE,
            )
        };
        if let Ok(handle) = from_resource {
            return LoadedIcon {
                icon: HICON(handle.0),
                owned: true,
            };
        }
        let fallback = unsafe { LoadIconW(None, IDI_APPLICATION) };
        LoadedIcon {
            icon: fallback.unwrap_or(HICON(std::ptr::null_mut())),
            owned: false,
        }
    }

    fn load_icon_from_file() -> Option<HICON> {
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

    #[cfg(test)]
    mod host_window_tests {
        use super::{tray_host_ex_style, tray_host_style};
        use std::sync::atomic::{AtomicBool, Ordering};
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, RegisterClassW,
            RegisterWindowMessageW, SendMessageTimeoutW, SetWindowLongPtrW, UnregisterClassW,
            CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, HWND_BROADCAST, HWND_MESSAGE,
            SMTO_ABORTIFHUNG, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
        };

        struct Probe {
            hwnd: HWND,
            class: PCWSTR,
            hinstance: windows::Win32::Foundation::HINSTANCE,
        }

        impl Drop for Probe {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.hwnd);
                    let _ = UnregisterClassW(self.class, Some(self.hinstance));
                }
            }
        }

        unsafe extern "system" fn probe_wnd_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProbeState;
                if !ptr.is_null() && msg == (*ptr).taskbar_created {
                    (*ptr).hit.store(true, Ordering::SeqCst);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }

        struct ProbeState {
            taskbar_created: u32,
            hit: AtomicBool,
        }

        fn create_probe(
            class: PCWSTR,
            parent: Option<HWND>,
            ex: WINDOW_EX_STYLE,
            style: WINDOW_STYLE,
        ) -> (Probe, Box<ProbeState>) {
            unsafe {
                let module = GetModuleHandleW(None).expect("GetModuleHandleW");
                let hinstance = windows::Win32::Foundation::HINSTANCE(module.0);
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(probe_wnd_proc),
                    hInstance: hinstance,
                    lpszClassName: class,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
                let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
                assert_ne!(taskbar_created, 0, "RegisterWindowMessageW(TaskbarCreated)");
                let mut state = Box::new(ProbeState {
                    taskbar_created,
                    hit: AtomicBool::new(false),
                });
                let hwnd = CreateWindowExW(
                    ex,
                    class,
                    w!("RusticDL Tray Probe"),
                    style,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    parent,
                    None,
                    Some(hinstance),
                    None,
                )
                .expect("CreateWindowExW");
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut *state as *mut ProbeState as isize);
                (
                    Probe {
                        hwnd,
                        class,
                        hinstance,
                    },
                    state,
                )
            }
        }

        fn broadcast_taskbar_created() {
            let msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
            let _ = unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    msg,
                    WPARAM(0),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG,
                    1000,
                    None,
                )
            };
        }

        #[test]
        fn message_only_window_misses_taskbar_created_broadcast() {
            let (probe, state) = create_probe(
                w!("RusticDLTrayProbeMessageOnly"),
                Some(HWND_MESSAGE),
                WINDOW_EX_STYLE::default(),
                WINDOW_STYLE::default(),
            );
            broadcast_taskbar_created();
            assert!(
                !state.hit.load(Ordering::SeqCst),
                "HWND_MESSAGE windows are not top-level and must not see TaskbarCreated"
            );
            drop(probe);
        }

        #[test]
        fn tray_host_window_receives_taskbar_created_broadcast() {
            let (probe, state) = create_probe(
                w!("RusticDLTrayProbeHost"),
                None,
                tray_host_ex_style(),
                tray_host_style(),
            );
            broadcast_taskbar_created();
            assert!(
                state.hit.load(Ordering::SeqCst),
                "tray host must be a top-level window so Explorer restart can re-add the icon"
            );
            drop(probe);
        }
    }

    #[cfg(test)]
    mod tray_hide_display_change_tests {
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, IsWindowVisible, RegisterClassW,
            ShowWindow, UnregisterClassW, CS_HREDRAW, CS_VREDRAW, SW_HIDE, SW_SHOWNORMAL,
            WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
        };

        struct HideProbe {
            hwnd: HWND,
            class: PCWSTR,
            hinstance: windows::Win32::Foundation::HINSTANCE,
        }

        impl Drop for HideProbe {
            fn drop(&mut self) {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                    let _ = DestroyWindow(self.hwnd);
                    let _ = UnregisterClassW(self.class, Some(self.hinstance));
                }
            }
        }

        unsafe extern "system" fn hide_probe_wnd_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        fn create_hidden_top_level(class: PCWSTR) -> HideProbe {
            unsafe {
                let module = GetModuleHandleW(None).expect("GetModuleHandleW");
                let hinstance = windows::Win32::Foundation::HINSTANCE(module.0);
                let wc = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(hide_probe_wnd_proc),
                    hInstance: hinstance,
                    lpszClassName: class,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    class,
                    w!("RusticDL Hide Probe"),
                    WS_OVERLAPPEDWINDOW,
                    -32000,
                    -32000,
                    160,
                    90,
                    None,
                    None,
                    Some(hinstance),
                    None,
                )
                .expect("CreateWindowExW");
                HideProbe {
                    hwnd,
                    class,
                    hinstance,
                }
            }
        }

        #[test]
        fn shownormal_unhides_sw_hide() {
            let probe = create_hidden_top_level(w!("RusticDLTrayHideShownormalProbe"));
            unsafe {
                let _ = ShowWindow(probe.hwnd, SW_HIDE);
                assert!(
                    !IsWindowVisible(probe.hwnd).as_bool(),
                    "SW_HIDE must leave the HWND invisible"
                );
                let _ = ShowWindow(probe.hwnd, SW_SHOWNORMAL);
                assert!(
                    IsWindowVisible(probe.hwnd).as_bool(),
                    "GPUI WM_DISPLAYCHANGE uses SW_SHOWNORMAL, which unhides SW_HIDE"
                );
            }
        }

        #[test]
        fn reassert_tray_hide_undoes_shownormal() {
            let probe = create_hidden_top_level(w!("RusticDLTrayHideReassertProbe"));
            unsafe {
                let _ = ShowWindow(probe.hwnd, SW_HIDE);
                let _ = ShowWindow(probe.hwnd, SW_SHOWNORMAL);
                assert!(
                    IsWindowVisible(probe.hwnd).as_bool(),
                    "precondition: SW_SHOWNORMAL made the HWND visible"
                );
            }
            super::super::reassert_tray_hide(probe.hwnd.0 as isize);
            unsafe {
                assert!(
                    !IsWindowVisible(probe.hwnd).as_bool(),
                    "tray-hidden HWND must be SW_HIDE after GPUI SW_SHOWNORMAL"
                );
            }
        }
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

#[cfg(all(test, target_os = "linux"))]
mod linux_desktop_tests {
    use super::desktop_supports_minimize;

    #[test]
    fn tiling_compositors_do_not_support_minimize() {
        for desktop in [
            "Hyprland",
            "sway",
            "niri",
            "river",
            "i3",
            "sway:wlroots",
            "sway;wlroots",
            "dwm",
            "xmonad",
        ] {
            assert!(!desktop_supports_minimize(Some(desktop)), "{desktop}");
        }
    }

    #[test]
    fn mainstream_desktops_support_minimize() {
        for desktop in [
            "GNOME",
            "KDE",
            "ubuntu:GNOME",
            "XFCE",
            "X-Cinnamon",
            "DDE",
            "awesome",
            "qtile",
            "Wayfire",
        ] {
            assert!(desktop_supports_minimize(Some(desktop)), "{desktop}");
        }
    }

    #[test]
    fn unset_or_unknown_desktops_are_not_assumed_to_minimize() {
        assert!(!desktop_supports_minimize(None));
        assert!(!desktop_supports_minimize(Some("")));
        assert!(!desktop_supports_minimize(Some("SomeNewWM")));
        assert!(!desktop_supports_minimize(Some("GNOME:sway")));
    }
}
