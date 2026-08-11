//! System tray (notification area / overflow) icon for Windows.
//!
//! Provides a tray icon with a context menu so the user can restore the main
//! window or fully quit while the app is hidden via "close to tray".

use crate::branding::APP_NAME;

/// Events the tray message thread posts back to the GPUI UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// Show / activate the main window.
    ShowWindow,
    /// Fully quit the application.
    Exit,
}

/// RAII handle for the background tray icon. Dropping it removes the icon.
pub struct SystemTray {
    #[cfg(windows)]
    thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    hwnd: std::sync::Arc<std::sync::atomic::AtomicIsize>,
}

impl SystemTray {
    /// Spawn the tray icon on a dedicated message-loop thread.
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
}

impl Drop for SystemTray {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

            let raw = self.hwnd.load(std::sync::atomic::Ordering::SeqCst);
            if raw != 0 {
                let hwnd = HWND(raw as *mut core::ffi::c_void);
                let _ = unsafe {
                    PostMessageW(Some(hwnd), WM_CLOSE, WPARAM::default(), LPARAM::default())
                };
            }
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }
}

/// Hide a GPUI window from the taskbar (true tray hide).
pub fn hide_main_window(window: &gpui::Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

/// Show a previously hidden main window and try to bring it to the foreground.
pub fn show_main_window(window: &gpui::Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{IsIconic, ShowWindow, SW_RESTORE, SW_SHOW};

        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            window.activate_window();
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            window.activate_window();
            return;
        };
        let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
        }
    }
    window.activate_window();
}

#[cfg(windows)]
mod windows_impl {
    use super::{SystemTray, TrayEvent, APP_NAME};
    use crate::branding::APP_ICON_ICO;
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
        DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, LoadImageW,
        PostQuitMessage, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, TrackPopupMenu,
        TranslateMessage, UnregisterClassW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA,
        HICON, HWND_MESSAGE, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, MF_STRING, MSG,
        TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
        WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
    };

    const TRAY_UID: u32 = 1;
    const ID_TRAY_SHOW: usize = 1001;
    const ID_TRAY_EXIT: usize = 1002;
    /// Custom callback message delivered to our message-only window.
    const WM_TRAYICON: u32 = WM_APP + 40;

    struct TrayState {
        event_tx: async_channel::Sender<TrayEvent>,
        icon: HICON,
    }

    pub(super) fn start(event_tx: async_channel::Sender<TrayEvent>) -> Option<SystemTray> {
        let hwnd_slot = Arc::new(AtomicIsize::new(0));
        let hwnd_for_thread = hwnd_slot.clone();

        let thread = thread::Builder::new()
            .name("rusticdl-tray".into())
            .spawn(move || {
                if let Err(err) = run_tray_loop(event_tx, hwnd_for_thread) {
                    eprintln!("[rusticdl] tray: {err}");
                }
            })
            .ok()?;

        // Brief wait so Drop can target a real HWND quickly after startup.
        for _ in 0..50 {
            if hwnd_slot.load(Ordering::SeqCst) != 0 {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }

        Some(SystemTray {
            thread: Some(thread),
            hwnd: hwnd_slot,
        })
    }

    fn run_tray_loop(
        event_tx: async_channel::Sender<TrayEvent>,
        hwnd_slot: Arc<AtomicIsize>,
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
            // Class may already exist if we restarted the tray in-process.
            let _ = RegisterClassW(&wc);

            let icon = load_tray_icon().unwrap_or(HICON(std::ptr::null_mut()));
            let state = Box::new(TrayState { event_tx, icon });
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
                    return Err(format!("CreateWindowEx: {e}"));
                }
            };

            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
            hwnd_slot.store(hwnd.0 as isize, Ordering::SeqCst);

            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_UID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_TRAYICON,
                hIcon: icon,
                ..Default::default()
            };
            write_tip(&mut nid.szTip, APP_NAME);

            if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                hwnd_slot.store(0, Ordering::SeqCst);
                // DestroyWindow → WM_DESTROY frees TrayState; do not free again here.
                let _ = DestroyWindow(hwnd);
                return Err("Shell_NotifyIcon NIM_ADD failed".into());
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let del = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_UID,
                ..Default::default()
            };
            let _ = Shell_NotifyIconW(NIM_DELETE, &del);

            hwnd_slot.store(0, Ordering::SeqCst);
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
                WM_TRAYICON => {
                    let mouse = lparam.0 as u32;
                    match mouse {
                        WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                            send_event(hwnd, TrayEvent::ShowWindow);
                        }
                        WM_RBUTTONUP => {
                            show_context_menu(hwnd);
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
            // Required so the menu dismisses correctly when clicking elsewhere.
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

    fn write_tip(buf: &mut [u16; 128], tip: &str) {
        let mut wide: Vec<u16> = tip.encode_utf16().collect();
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
