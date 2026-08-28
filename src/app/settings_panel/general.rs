use std::path::PathBuf;

use gpui::{
    div, prelude::FluentBuilder, px, Context, IntoElement, ParentElement, SharedString, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
    group_box::{GroupBox, GroupBoxVariants},
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable,
};

use super::super::widgets::{
    browse_directory, field_hint, settings_choice_row, settings_field_label,
    settings_input_with_reset, settings_subgroup, shorten_path_display,
};
use super::super::DownloadApp;
use crate::download::{reveal_in_folder, FileTypeKind};
use crate::settings::UpdateChannel;

impl DownloadApp {
    pub(super) fn render_settings_general(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let data_dir = self.paths.root.display().to_string();
        let update_channel = self.settings.update_channel;
        let update_busy = self.update_busy;
        let update_label = self.update_action_label();
        let organize = self.settings.organize_by_file_type;
        let preview_root = PathBuf::from(self.dir_input.read(cx).value().to_string());
        let audio_idx = FileTypeKind::Audio.index();
        let audio_folder = self.category_folder_inputs[audio_idx].read(cx).value();
        let audio_folder = if audio_folder.trim().is_empty() {
            FileTypeKind::Audio.default_folder_name().to_string()
        } else {
            audio_folder.to_string()
        };
        let example_path = shorten_path_display(
            &preview_root
                .join(audio_folder)
                .join("song.mp3")
                .to_string_lossy(),
        );

        GroupBox::new().outline().child(
            v_flex()
                .gap_4()
                .child(settings_subgroup("Downloads", false, cx))
                .child(
                    v_flex()
                        .gap_1p5()
                        .child(settings_field_label("Download directory", cx))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Input::new(&self.dir_input).w_full().flex_1())
                                .child(
                                    Button::new("browse-settings-dir")
                                        .label("Browse...")
                                        .icon(IconName::FolderOpen)
                                        .outline()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            browse_directory(
                                                this.dir_input.clone(),
                                                cx.entity().clone(),
                                                window,
                                                cx,
                                            );
                                        })),
                                ),
                        ),
                )
                .child(settings_choice_row(
                    "Organize by type",
                    Some("New files go in a type folder. Existing downloads stay put."),
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("organize-type-off")
                                .label("Off")
                                .when(!organize, |b| b.primary())
                                .when(organize, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_organize_by_file_type(false, window, cx);
                                })),
                        )
                        .child(
                            Button::new("organize-type-on")
                                .label("On")
                                .when(organize, |b| b.primary())
                                .when(!organize, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_organize_by_file_type(true, window, cx);
                                })),
                        ),
                    cx,
                ))
                .child(field_hint(
                    format!("Example: {example_path}"),
                    cx,
                ))
                .child(settings_subgroup("Type folders", true, cx))
                .children(FileTypeKind::ALL.into_iter().map(|kind| {
                    let i = kind.index();
                    let enabled = self.settings.category_folders.get(kind).enabled;
                    let current = self.category_folder_inputs[i].read(cx).value().to_string();
                    let default_name = kind.default_folder_name();
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(
                            Icon::empty()
                                .path(kind.icon_path())
                                .with_size(px(14.))
                                .text_color(theme.muted_foreground),
                        )
                        .child(
                            div()
                                .w(px(96.))
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(theme.foreground)
                                .child(kind.label()),
                        )
                        .child(
                            settings_input_with_reset(
                                format!("category-folder-reset-{}", kind.label()),
                                &self.category_folder_inputs[i],
                                &current,
                                default_name,
                                default_name,
                                cx.entity().clone(),
                                !organize,
                            )
                            .flex_1(),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .flex_shrink_0()
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "category-off-{}",
                                        kind.label()
                                    )))
                                    .label("Off")
                                    .small()
                                    .disabled(!organize)
                                    .when(!enabled, |b| b.primary())
                                    .when(enabled, |b| b.outline())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.set_category_enabled(kind, false, window, cx);
                                    })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "category-on-{}",
                                        kind.label()
                                    )))
                                    .label("On")
                                    .small()
                                    .disabled(!organize)
                                    .when(enabled, |b| b.primary())
                                    .when(!enabled, |b| b.outline())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.set_category_enabled(kind, true, window, cx);
                                    })),
                                ),
                        )
                }))
                .child(settings_subgroup("Updates", true, cx))
                .child(settings_choice_row(
                    "Check for updates",
                    Some("Same check as the brand menu and About dialog."),
                    Button::new("settings-check-updates")
                        .outline()
                        .label(update_label)
                        .disabled(update_busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_update_action(window, cx);
                        })),
                    cx,
                ))
                .child(settings_choice_row(
                    "Update channel",
                    Some(
                        "Each channel follows its own stream. Switching installs that stream’s current build, even if the version number is lower.",
                    ),
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("update-channel-stable")
                                .label(UpdateChannel::Stable.label())
                                .when(update_channel == UpdateChannel::Stable, |b| b.primary())
                                .when(update_channel != UpdateChannel::Stable, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_update_channel(UpdateChannel::Stable, window, cx);
                                })),
                        )
                        .child(
                            Button::new("update-channel-nightly")
                                .label(UpdateChannel::Nightly.label())
                                .when(update_channel == UpdateChannel::Nightly, |b| b.primary())
                                .when(update_channel != UpdateChannel::Nightly, |b| b.outline())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_update_channel(UpdateChannel::Nightly, window, cx);
                                })),
                        ),
                    cx,
                ))
                .child(settings_subgroup("App data", true, cx))
                .child(
                    v_flex()
                        .gap_1p5()
                        .child(settings_field_label("App data directory", cx))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_x_hidden()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(data_dir.clone()),
                                )
                                .child(
                                    Clipboard::new("copy-data-dir")
                                        .value(SharedString::from(data_dir)),
                                )
                                .child(
                                    Button::new("open-data-dir")
                                        .outline()
                                        .small()
                                        .icon(IconName::FolderOpen)
                                        .label("Open")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Err(msg) = reveal_in_folder(&this.paths.root) {
                                                this.show_toast(msg, cx);
                                            }
                                        })),
                                ),
                        )
                        .child(field_hint("settings.json and state.json live here.", cx)),
                ),
        )
    }
}
