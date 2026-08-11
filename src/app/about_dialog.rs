//! About dialog extracted from `DownloadApp`.

use gpui::{div, px, Context, ParentElement, Styled, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    description_list::DescriptionList,
    h_flex, v_flex, ActiveTheme, Disableable, Sizable, WindowExt,
};

use super::DownloadApp;
use crate::branding::{APP_NAME, APP_VERSION};
use crate::updater::open_release_page;

impl DownloadApp {
    pub(crate) fn open_about_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app_view = cx.entity().clone();
        let update_busy = self.update_busy;
        let update_action_label = self.update_action_label();
        window.open_dialog(cx, move |dialog, window, cx| {
            let theme = cx.theme().clone();
            let muted = theme.muted_foreground;
            let app_view_check = app_view.clone();

            // Match Add download: viewport-center so the card sits mid-window, not top-biased.
            let est_h = 320.0;
            let view_h = window.viewport_size().height.to_f64() as f32;
            let max_top = (view_h - est_h - 20.0).max(24.0);
            let margin_top = ((view_h - est_h) * 0.5).clamp(24.0, max_top);

            dialog
                .title(format!("About {APP_NAME}"))
                .alert()
                // alert() disables outside-click; re-enable for light dismiss UX.
                .overlay_closable(true)
                .keyboard(true)
                .w(px(420.))
                .margin_top(px(margin_top))
                .border_color(theme.border.opacity(0.32))
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(crate::branding::APP_TAGLINE))
                        .child(
                            DescriptionList::new()
                                .columns(1)
                                .bordered(false)
                                .label_width(px(96.))
                                .item("Version", APP_VERSION, 1)
                                .item("Engine", "Single-stream + Range resume", 1)
                                .item("License", "MIT", 1)
                                .item("Updates", "GitHub Releases", 1),
                        )
                        .child(div().text_xs().text_color(muted).child(format!(
                            "Data folder: %APPDATA%\\{}\\",
                            crate::branding::APP_DATA_DIR_NAME
                        )))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("about-check-update")
                                        .outline()
                                        .small()
                                        .label(if update_busy {
                                            "Updating…".to_string()
                                        } else {
                                            update_action_label.clone()
                                        })
                                        .disabled(update_busy)
                                        .on_click(move |_, _window, cx| {
                                            app_view_check.update(cx, |app, cx| {
                                                app.begin_one_click_update(cx);
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("about-open-releases")
                                        .ghost()
                                        .small()
                                        .label("Open releases")
                                        .on_click(|_, _, _| {
                                            let _ = open_release_page();
                                        }),
                                ),
                        ),
                )
        });
    }
}
