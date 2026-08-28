use std::path::PathBuf;

pub fn apply_app_icon(window: &gpui::Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            LoadImageW, SendMessageW, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTSIZE,
            LR_LOADFROMFILE, WM_SETICON,
        };

        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let hwnd = HWND(win32.hwnd.get() as *mut _);

        let Some(ico) = resolve_icon_path() else {
            return;
        };
        let wide: Vec<u16> = ico
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let big = unsafe {
            LoadImageW(
                None,
                PCWSTR(wide.as_ptr()),
                IMAGE_ICON,
                0,
                0,
                LR_LOADFROMFILE | LR_DEFAULTSIZE,
            )
        };
        let Ok(big) = big else {
            return;
        };
        let big = HICON(big.0);

        unsafe {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(big.0 as isize)),
            );
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(big.0 as isize)),
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

fn resolve_icon_path() -> Option<PathBuf> {
    use crate::branding::APP_ICON_ICO;
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
