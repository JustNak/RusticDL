//! Resolution-aware native window placement helpers.
//!
//! GPUI's `Bounds::centered` + Windows `SetWindowPlacement` path can leave
//! newly created windows (especially `WindowKind::PopUp`) at the cascade /
//! corner position chosen by `CreateWindowEx(CW_USEDEFAULT)`. These helpers
//! re-center using the monitor work area in physical screen coordinates, which
//! is correct across DPI scales and multi-monitor layouts.

use gpui::Window;

/// Center a GPUI window on the most appropriate monitor work area.
///
/// Prefer the monitor under the mouse cursor (where the user is looking /
/// interacting). Fall back to the monitor that currently hosts the window,
/// then to the primary display.
pub fn center_window(window: &Window) {
    #[cfg(windows)]
    {
        if let Some(hwnd) = hwnd_of(window) {
            center_hwnd(hwnd);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

/// Center a raw Win32 window handle on the cursor/host/primary work area.
#[cfg(windows)]
pub fn center_hwnd(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    };

    unsafe {
        if hwnd.0.is_null() {
            return;
        }

        let mut window_rect = RECT::default();
        if GetWindowRect(hwnd, &mut window_rect).is_err() {
            return;
        }

        let width = window_rect.right - window_rect.left;
        let height = window_rect.bottom - window_rect.top;
        if width <= 0 || height <= 0 {
            return;
        }

        let monitor = monitor_for_centering(hwnd, &window_rect);
        if monitor.0.is_null() {
            return;
        }

        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }

        // `rcWork` excludes the taskbar / docked bars — better than full `rcMonitor`.
        let work = info.rcWork;
        let work_w = work.right - work.left;
        let work_h = work.bottom - work.top;
        if work_w <= 0 || work_h <= 0 {
            return;
        }

        let x = work.left + (work_w - width) / 2;
        let y = work.top + (work_h - height) / 2;

        let _ = SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

#[cfg(windows)]
fn hwnd_of(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;

    let handle = <Window as HasWindowHandle>::window_handle(window).ok()?;
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return None;
    };
    Some(HWND(win32.hwnd.get() as *mut core::ffi::c_void))
}

/// Pick the monitor to center on: cursor → window host → primary.
#[cfg(windows)]
unsafe fn monitor_for_centering(
    hwnd: windows::Win32::Foundation::HWND,
    window_rect: &windows::Win32::Foundation::RECT,
) -> windows::Win32::Graphics::Gdi::HMONITOR {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        MonitorFromPoint, MonitorFromRect, MonitorFromWindow, MONITOR_DEFAULTTONEAREST,
        MONITOR_DEFAULTTOPRIMARY,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut cursor = POINT::default();
    if GetCursorPos(&mut cursor).is_ok() {
        let from_cursor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        if !from_cursor.0.is_null() {
            return from_cursor;
        }
    }

    let from_window = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if !from_window.0.is_null() {
        return from_window;
    }

    let from_rect = MonitorFromRect(window_rect, MONITOR_DEFAULTTONEAREST);
    if !from_rect.0.is_null() {
        return from_rect;
    }

    MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY)
}
