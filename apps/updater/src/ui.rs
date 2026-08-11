//! Minimal Win32 progress UI for the updater process.

use std::sync::mpsc;
use std::thread;

/// Progress callbacks from the worker thread.
pub trait ProgressSink: Send {
    fn set_status(&self, text: String);
    fn set_progress_percent(&self, percent: u32);
    fn set_progress_unknown(&self);
}

/// Run `work` on a background thread while a progress window pumps messages.
pub fn run_with_progress_window<F, T>(title: &str, work: F) -> T
where
    F: FnOnce(&dyn ProgressSink) -> T + Send + 'static,
    T: Send + 'static,
{
    #[cfg(windows)]
    {
        windows_ui::run(title, work)
    }
    #[cfg(not(windows))]
    {
        let _ = title;
        struct Noop;
        impl ProgressSink for Noop {
            fn set_status(&self, _text: String) {}
            fn set_progress_percent(&self, _percent: u32) {}
            fn set_progress_unknown(&self) {}
        }
        work(&Noop)
    }
}

/// Modal error dialog; optional “open release page” Yes/No.
pub fn show_error_message(title: &str, message: &str, release_page: Option<&str>) {
    #[cfg(windows)]
    {
        windows_ui::error_dialog(title, message, release_page);
    }
    #[cfg(not(windows))]
    {
        let _ = (title, message, release_page);
        eprintln!("{title}: {message}");
    }
}

#[cfg(windows)]
mod windows_ui {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::{Arc, Mutex};

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        GetStockObject, UpdateWindow, DEFAULT_GUI_FONT, HBRUSH, WHITE_BRUSH,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Controls::{
        InitCommonControlsEx, ICC_PROGRESS_CLASS, INITCOMMONCONTROLSEX, PBM_SETMARQUEE, PBM_SETPOS,
        PBM_SETRANGE, PBS_MARQUEE, PBS_SMOOTH, PROGRESS_CLASS,
    };
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetWindowLongPtrW, LoadCursorW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
        SendMessageW, SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, CS_HREDRAW,
        CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, HMENU, IDC_ARROW, IDYES, MB_ICONERROR, MB_OK,
        MB_YESNO, MESSAGEBOX_RESULT, MSG, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE,
        WM_CREATE, WM_DESTROY, WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_OVERLAPPED,
        WS_SYSMENU, WS_VISIBLE,
    };

    const WM_UPD_STATUS: u32 = WM_APP + 1;
    const WM_UPD_PROGRESS: u32 = WM_APP + 2;
    const WM_UPD_DONE: u32 = WM_APP + 3;
    /// wparam for WM_UPD_PROGRESS: 0–100 = percent, 101 = marquee/unknown
    const PROGRESS_UNKNOWN: usize = 101;

    const IDC_STATUS: isize = 1001;
    const IDC_BAR: isize = 1002;

    struct SharedState {
        status: Mutex<String>,
        /// HWND stored as isize so SharedState is Send + Sync.
        hwnd: AtomicIsize,
    }

    struct UiSink {
        state: Arc<SharedState>,
    }

    impl ProgressSink for UiSink {
        fn set_status(&self, text: String) {
            if let Ok(mut g) = self.state.status.lock() {
                *g = text;
            }
            post(self.state.hwnd.load(Ordering::SeqCst), WM_UPD_STATUS, 0);
        }

        fn set_progress_percent(&self, percent: u32) {
            post(
                self.state.hwnd.load(Ordering::SeqCst),
                WM_UPD_PROGRESS,
                percent.min(100) as usize,
            );
        }

        fn set_progress_unknown(&self) {
            post(
                self.state.hwnd.load(Ordering::SeqCst),
                WM_UPD_PROGRESS,
                PROGRESS_UNKNOWN,
            );
        }
    }

    fn post(hwnd_raw: isize, msg: u32, wparam: usize) {
        if hwnd_raw == 0 {
            return;
        }
        let hwnd = HWND(hwnd_raw as *mut _);
        unsafe {
            let _ = PostMessageW(Some(hwnd), msg, WPARAM(wparam), LPARAM(0));
        }
    }

    struct WindowData {
        status_hwnd: HWND,
        bar_hwnd: HWND,
        shared: Arc<SharedState>,
        marquee: bool,
    }

    pub fn run<F, T>(title: &str, work: F) -> T
    where
        F: FnOnce(&dyn ProgressSink) -> T + Send + 'static,
        T: Send + 'static,
    {
        unsafe {
            let _ = SetCurrentProcessExplicitAppUserModelID(w!("com.rusticdl.updater"));
        }

        let shared = Arc::new(SharedState {
            status: Mutex::new("Starting update…".into()),
            hwnd: AtomicIsize::new(0),
        });

        let (tx, rx) = mpsc::channel::<T>();
        let sink_shared = Arc::clone(&shared);
        thread::spawn(move || {
            // Give the UI thread a moment to create the window.
            thread::sleep(std::time::Duration::from_millis(80));
            let sink = UiSink { state: sink_shared };
            let result = work(&sink);
            post(sink.state.hwnd.load(Ordering::SeqCst), WM_UPD_DONE, 0);
            let _ = tx.send(result);
        });

        unsafe {
            pump_window(title, Arc::clone(&shared));
        }

        rx.recv().expect("updater worker ended without a result")
    }

    pub fn error_dialog(title: &str, message: &str, release_page: Option<&str>) {
        let title_w = wide(title);
        let body = if release_page.is_some() {
            format!("{message}\n\nOpen the release page in your browser?")
        } else {
            message.to_string()
        };
        let body_w = wide(&body);
        let flags = if release_page.is_some() {
            MB_ICONERROR | MB_YESNO
        } else {
            MB_ICONERROR | MB_OK
        };
        let result = unsafe {
            MessageBoxW(
                None,
                PCWSTR(body_w.as_ptr()),
                PCWSTR(title_w.as_ptr()),
                flags,
            )
        };
        if release_page.is_some() && result == MESSAGEBOX_RESULT(IDYES.0 as i32) {
            if let Some(url) = release_page {
                let _ = open::that(url);
            }
        }
    }

    unsafe fn pump_window(title: &str, shared: Arc<SharedState>) {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_PROGRESS_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);

        let instance = GetModuleHandleW(None).expect("GetModuleHandleW");
        let class_name = w!("RusticDLUpdaterWindow");

        let brush = GetStockObject(WHITE_BRUSH);
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(brush.0),
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let title_w = wide(title);
        let width = 420;
        let height = 160;

        // Pass Arc via boxed raw pointer for WM_CREATE.
        let create_param = Box::into_raw(Box::new(shared.clone()));

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            PCWSTR(title_w.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            Some(create_param as *const std::ffi::c_void),
        )
        .expect("CreateWindowExW");

        shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let create =
                    &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
                let shared = *Box::from_raw(create.lpCreateParams as *mut Arc<SharedState>);
                let instance = GetModuleHandleW(None).unwrap_or_default();

                let status = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Starting update…"),
                    WS_CHILD | WS_VISIBLE,
                    20,
                    24,
                    360,
                    40,
                    Some(hwnd),
                    Some(HMENU(IDC_STATUS as *mut _)),
                    Some(instance.into()),
                    None,
                )
                .unwrap_or_default();

                let bar_style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | PBS_SMOOTH | PBS_MARQUEE);
                let bar = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    PROGRESS_CLASS,
                    PCWSTR::null(),
                    bar_style,
                    20,
                    72,
                    360,
                    22,
                    Some(hwnd),
                    Some(HMENU(IDC_BAR as *mut _)),
                    Some(instance.into()),
                    None,
                )
                .unwrap_or_default();

                let font = GetStockObject(DEFAULT_GUI_FONT);
                if !font.is_invalid() {
                    let _ = SendMessageW(
                        status,
                        WM_SETFONT,
                        Some(WPARAM(font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                    let _ = SendMessageW(
                        bar,
                        WM_SETFONT,
                        Some(WPARAM(font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }

                // range 0..100 packed in LPARAM: low = min, high = max
                let _ = SendMessageW(
                    bar,
                    PBM_SETRANGE,
                    Some(WPARAM(0)),
                    Some(LPARAM(make_lparam(0, 100))),
                );
                let _ = SendMessageW(bar, PBM_SETMARQUEE, Some(WPARAM(1)), Some(LPARAM(30)));

                let data = Box::new(WindowData {
                    status_hwnd: status,
                    bar_hwnd: bar,
                    shared,
                    marquee: true,
                });
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(data) as isize);
                LRESULT(0)
            }
            WM_UPD_STATUS => {
                if let Some(data) = data_of(hwnd) {
                    if let Ok(text) = data.shared.status.lock() {
                        let w = wide(&text);
                        let _ = SetWindowTextW(data.status_hwnd, PCWSTR(w.as_ptr()));
                    }
                }
                LRESULT(0)
            }
            WM_UPD_PROGRESS => {
                if let Some(data) = data_of(hwnd) {
                    let value = wparam.0;
                    if value == PROGRESS_UNKNOWN {
                        if !data.marquee {
                            let _ = SendMessageW(
                                data.bar_hwnd,
                                PBM_SETMARQUEE,
                                Some(WPARAM(1)),
                                Some(LPARAM(30)),
                            );
                            data.marquee = true;
                        }
                    } else {
                        if data.marquee {
                            let _ = SendMessageW(
                                data.bar_hwnd,
                                PBM_SETMARQUEE,
                                Some(WPARAM(0)),
                                Some(LPARAM(0)),
                            );
                            data.marquee = false;
                        }
                        let _ = SendMessageW(
                            data.bar_hwnd,
                            PBM_SETPOS,
                            Some(WPARAM(value)),
                            Some(LPARAM(0)),
                        );
                    }
                }
                LRESULT(0)
            }
            WM_UPD_DONE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_CLOSE => {
                // Ignore close while updating (prevents abort mid-install).
                LRESULT(0)
            }
            WM_DESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if ptr != 0 {
                    drop(Box::from_raw(ptr as *mut WindowData));
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn data_of<'a>(hwnd: HWND) -> Option<&'a mut WindowData> {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if ptr == 0 {
            None
        } else {
            Some(&mut *(ptr as *mut WindowData))
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Pack two 16-bit values into an LPARAM (Win32 MAKELPARAM).
    const fn make_lparam(low: u16, high: u16) -> isize {
        ((high as isize) << 16) | (low as isize & 0xFFFF)
    }
}
