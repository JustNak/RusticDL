//! Resolution-aware native window placement helpers.
//!
//! GPUI's `Bounds::centered` + Windows `SetWindowPlacement` path can leave
//! newly created windows (especially `WindowKind::PopUp`) at the cascade /
//! corner position chosen by `CreateWindowEx(CW_USEDEFAULT)`. These helpers
//! re-center using the monitor work area in physical screen coordinates, which
//! is correct across DPI scales and multi-monitor layouts.

use gpui::Window;

/// Pixel step used to stagger stacked capture HUDs so the Nth window is visible.
pub const CASCADE_STEP_PX: i32 = 36;

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

/// Center, then offset by `index * CASCADE_STEP_PX` so multiple HUDs do not stack.
pub fn cascade_window(window: &Window, index: usize) {
    #[cfg(windows)]
    {
        if let Some(hwnd) = hwnd_of(window) {
            cascade_hwnd(hwnd, index);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (window, index);
    }
}

/// Center a raw Win32 window handle on the cursor/host/primary work area.
#[cfg(windows)]
pub fn center_hwnd(hwnd: windows::Win32::Foundation::HWND) {
    cascade_hwnd(hwnd, 0);
}

/// Center, then stagger by `index` cascade steps, clamped to the work area.
#[cfg(windows)]
pub fn cascade_hwnd(hwnd: windows::Win32::Foundation::HWND, index: usize) {
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
        let (x, y) = cascade_origin(
            work.left,
            work.top,
            work.right - work.left,
            work.bottom - work.top,
            width,
            height,
            index,
        );

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

/// Centered origin plus cascade offset, clamped so the window stays in the work area.
pub fn cascade_origin(
    work_left: i32,
    work_top: i32,
    work_w: i32,
    work_h: i32,
    width: i32,
    height: i32,
    index: usize,
) -> (i32, i32) {
    if work_w <= 0 || work_h <= 0 || width <= 0 || height <= 0 {
        return (work_left, work_top);
    }
    let (dx, dy) = cascade_delta(index);
    let centered_x = work_left + (work_w - width) / 2;
    let centered_y = work_top + (work_h - height) / 2;
    let max_x = work_left + (work_w - width).max(0);
    let max_y = work_top + (work_h - height).max(0);
    (
        (centered_x + dx).clamp(work_left, max_x),
        (centered_y + dy).clamp(work_top, max_y),
    )
}

/// Offset from the centered origin for the Nth stacked HUD.
pub fn cascade_delta(index: usize) -> (i32, i32) {
    let step = CASCADE_STEP_PX.saturating_mul(index as i32);
    (step, step)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_window_is_centered() {
        assert_eq!(cascade_delta(0), (0, 0));
        assert_eq!(cascade_origin(0, 0, 1000, 800, 480, 320, 0), (260, 240));
    }

    #[test]
    fn later_windows_step_down_and_right() {
        assert_eq!(cascade_delta(1), (CASCADE_STEP_PX, CASCADE_STEP_PX));
        assert_eq!(cascade_delta(2), (CASCADE_STEP_PX * 2, CASCADE_STEP_PX * 2));
        let (x0, y0) = cascade_origin(0, 0, 1000, 800, 480, 320, 0);
        let (x1, y1) = cascade_origin(0, 0, 1000, 800, 480, 320, 1);
        assert_eq!(x1 - x0, CASCADE_STEP_PX);
        assert_eq!(y1 - y0, CASCADE_STEP_PX);
    }

    #[test]
    fn cascade_stays_inside_work_area() {
        let (x, y) = cascade_origin(0, 0, 500, 400, 480, 320, 20);
        assert!(x >= 0 && x + 480 <= 500);
        assert!(y >= 0 && y + 320 <= 400);
    }
}
