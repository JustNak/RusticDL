use gpui::Window;

/// Vertical offset that visually centers a gpui-component overlay dialog.
///
/// Those dialogs sit near the top of the overlay by default. This matches the
/// Add / About / What's New treatment: half the leftover viewport height,
/// floored at 24px so a short window still has a usable top inset.
pub(crate) fn dialog_margin_top(view_h: f32, est_h: f32) -> f32 {
    let max_top = (view_h - est_h - 20.0).max(24.0);
    ((view_h - est_h) * 0.5).clamp(24.0, max_top)
}

pub(crate) fn dialog_margin_top_in(window: &Window, est_h: f32) -> f32 {
    dialog_margin_top(window.viewport_size().height.to_f64() as f32, est_h)
}

#[cfg(test)]
mod tests {
    use super::dialog_margin_top;

    #[test]
    fn centers_when_window_is_tall() {
        assert_eq!(dialog_margin_top(800.0, 200.0), 300.0);
    }

    #[test]
    fn floors_at_24_when_window_is_short() {
        assert_eq!(dialog_margin_top(210.0, 200.0), 24.0);
    }

    #[test]
    fn floors_at_24_when_dialog_taller_than_window() {
        assert_eq!(dialog_margin_top(100.0, 200.0), 24.0);
    }
}
