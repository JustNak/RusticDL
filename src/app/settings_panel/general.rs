use std::path::PathBuf;

use gpui::{
    div, Context, IntoElement, ParentElement, SharedString, Styled,
};
use gpui_component::{
    button::Button,
    clipboard::Clipboard,
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable, IconName, Sizable,
};

use super::super::widgets::{
    browse_directory, field_hint, settings_bays, settings_control_row, settings_field_label,
    ExclusiveOpt, SettingsBay, SettingsExclusiveRow, SettingsToggleRow, TypeFolderStrip,
    shorten_path_display,
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
        let app = cx.entity();

        settings_bays()
            .child(
                SettingsBay::new("Downloads")
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
                    .child(
                        SettingsToggleRow::new(
                            "organize-type",
                            "Organize by type",
                            organize,
                            {
                                let app = app.clone();
                                move |on, window, cx| {
                                    app.update(cx, |this, cx| {
                                        this.set_organize_by_file_type(on, window, cx);
                                    });
                                }
                            },
                        )
                        .hint("New files go in a type folder. Existing downloads stay put."),
                    )
                    .child(field_hint(format!("Example: {example_path}"), cx)),
            )
            .child(SettingsBay::new("Type folders").children(
                FileTypeKind::ALL.into_iter().map(|kind| {
                    let i = kind.index();
                    let enabled = self.settings.category_folders.get(kind).enabled;
                    let current = self.category_folder_inputs[i].read(cx).value().to_string();
                    TypeFolderStrip::new(
                        kind,
                        &self.category_folder_inputs[i],
                        &current,
                        enabled,
                        organize,
                        app.clone(),
                        {
                            let app = app.clone();
                            move |on, window, cx| {
                                app.update(cx, |this, cx| {
                                    this.set_category_enabled(kind, on, window, cx);
                                });
                            }
                        },
                    )
                }),
            ))
            .child(
                SettingsBay::new("Updates")
                    .child(settings_control_row(
                        "Check for updates",
                        Some(
                            "Same check as the brand menu and About dialog."
                                .into(),
                        ),
                        Button::new("settings-check-updates")
                            .outline()
                            .label(update_label)
                            .disabled(update_busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_update_action(window, cx);
                            })),
                        cx,
                    ))
                    .child(
                        SettingsExclusiveRow::new(
                            "update-channel",
                            "Update channel",
                            update_channel,
                            [
                                ExclusiveOpt::new(
                                    UpdateChannel::Stable,
                                    "update-channel-stable",
                                    UpdateChannel::Stable.label(),
                                ),
                                ExclusiveOpt::new(
                                    UpdateChannel::Nightly,
                                    "update-channel-nightly",
                                    UpdateChannel::Nightly.label(),
                                ),
                            ],
                            {
                                let app = app.clone();
                                move |channel, window, cx| {
                                    app.update(cx, |this, cx| {
                                        this.set_update_channel(channel, window, cx);
                                    });
                                }
                            },
                        )
                        .hint(
                            "Each channel follows its own stream. Switching installs that stream's current build, even if the version number is lower.",
                        ),
                    ),
            )
            .child(
                SettingsBay::new("App data").child(
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
