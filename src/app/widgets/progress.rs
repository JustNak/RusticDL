use gpui::{div, px, Hsla, IntoElement, ParentElement, Styled};
use gpui_component::{h_flex, progress::Progress};

use crate::settings::ProgressStyle;

/// Progress bar variants for queue rows and settings preview.
/// `value` is 0..100.
pub(crate) fn styled_progress(value: f32, color: Hsla, style: ProgressStyle) -> impl IntoElement {
    let value = value.clamp(0.0, 100.0);
    match style {
        ProgressStyle::Solid => Progress::new()
            .value(value)
            .bg(color)
            .h(px(6.))
            .w_full()
            .rounded_full()
            .into_any_element(),
        ProgressStyle::Soft => Progress::new()
            .value(value)
            .bg(color.opacity(0.85))
            .h(px(4.))
            .w_full()
            .rounded_full()
            .into_any_element(),
        ProgressStyle::Glow => Progress::new()
            .value(value)
            .bg(color)
            .h(px(9.))
            .w_full()
            .rounded_full()
            .into_any_element(),
        ProgressStyle::Segmented => {
            const SEGMENTS: u32 = 12;
            let filled = ((value / 100.0) * SEGMENTS as f32).round() as u32;
            h_flex()
                .w_full()
                .gap_0p5()
                .h(px(8.))
                .items_center()
                .children((0..SEGMENTS).map(move |i| {
                    let on = i < filled;
                    div().flex_1().h_full().rounded(px(2.)).bg(if on {
                        color
                    } else {
                        color.opacity(0.16)
                    })
                }))
                .into_any_element()
        }
    }
}
