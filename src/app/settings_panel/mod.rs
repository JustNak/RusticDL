
mod appearance;
mod browser;
mod engine;
mod general;
mod system;

use gpui::{
    div, px, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt,
};

use super::settings_category::SettingsCategory;
use super::DownloadApp;

impl DownloadApp {
    pub(super) fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let settings_pad = self.settings.ui_density.settings_pad();
        let category = self.settings_category;

        v_flex()
            .id("settings-view")
            .size_full()
            .bg(theme.background)
            .child(
                div()
                    // Key id by category so GPUI does not reuse scroll offset
                    .id(SharedString::from(format!(
                        "settings-content-scroll-{}",
                        category.label()
                    )))
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_y_scroll()
                    .p(px(settings_pad))
                    .child(
                        v_flex()
                            .gap_5()
                            .max_w(px(880.))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Icon::new(category.icon())
                                            .with_size(px(16.))
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_bold()
                                            .text_color(theme.foreground)
                                            .child(category.panel_title()),
                                    ),
                            )
                            .child(match category {
                                SettingsCategory::General => {
                                    self.render_settings_general(cx).into_any_element()
                                }
                                SettingsCategory::DownloadEngine => {
                                    self.render_settings_engine(cx).into_any_element()
                                }
                                SettingsCategory::System => {
                                    self.render_settings_system(cx).into_any_element()
                                }
                                SettingsCategory::Browser => {
                                    self.render_settings_browser(cx).into_any_element()
                                }
                                SettingsCategory::Appearance => {
                                    self.render_settings_appearance(cx).into_any_element()
                                }
                            }),
                    ),
            )
            .child(
                h_flex()
                    .id("settings-footer")
                    .flex_shrink_0()
                    .w_full()
                    .px(px(settings_pad))
                    .py_3()
                    .gap_3()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(
                        Button::new("reset-settings-defaults")
                            .outline()
                            .label("Reset defaults")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_reset_settings_defaults(window, cx);
                            })),
                    )
                    .child(
                        Button::new("save-settings")
                            .primary()
                            .icon(IconName::Check)
                            .label("Save settings")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_settings(window, cx);
                            })),
                    ),
            )
    }
}
