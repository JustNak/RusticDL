use std::rc::Rc;

use gpui::{
    div, hsla, prelude::FluentBuilder, px, AnyElement, App, Context, Corner, Div, ElementId,
    Entity, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    switch::Switch,
    tooltip::Tooltip,
    v_flex, ActiveTheme, Disableable, Icon, IconName, Side, Sizable, StyledExt, Theme,
};

use super::super::DownloadApp;
use crate::download::FileTypeKind;
use crate::settings::AccentPreset;

pub(crate) fn settings_bays() -> Div {
    v_flex().w_full().gap_4()
}

pub(crate) fn field_label(text: &'static str, cx: &App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_semibold()
        .text_color(theme.foreground)
        .child(text)
}

pub(crate) fn field_hint(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_xs()
        .font_normal()
        .text_color(theme.muted_foreground.opacity(0.78))
        .child(text.into())
}

pub(crate) fn settings_field_label(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let theme = cx.theme().clone();
    div()
        .text_sm()
        .font_semibold()
        .text_color(theme.foreground)
        .child(text.into())
}

pub(crate) fn settings_input_with_reset(
    id: impl Into<SharedString>,
    input: &Entity<InputState>,
    current: &str,
    default_value: &str,
    default_label: impl Into<SharedString>,
    app: Entity<DownloadApp>,
    disabled: bool,
) -> Input {
    let dirty = current.trim() != default_value.trim();
    let default_owned = default_value.to_string();
    let tip: SharedString = format!("Reset to default ({})", default_label.into()).into();
    let reset_id = id.into();
    let input_entity = input.clone();

    Input::new(input)
        .w_full()
        .disabled(disabled)
        .when(dirty && !disabled, |inp| {
            inp.suffix(
                Button::new(reset_id)
                    .ghost()
                    .compact()
                    .icon(Icon::empty().path("icons/rotate-cw.svg"))
                    .tooltip(tip)
                    .on_click({
                        let input_entity = input_entity.clone();
                        let default_owned = default_owned.clone();
                        let app = app.clone();
                        move |_, window, cx| {
                            input_entity.update(cx, |state, cx| {
                                state.set_value(default_owned.clone(), window, cx);
                            });
                            let _ = app.update(cx, |_, cx| cx.notify());
                        }
                    }),
            )
        })
}

pub(crate) fn settings_control_row(
    label: impl Into<SharedString>,
    hint: Option<SharedString>,
    control: impl IntoElement,
    cx: &mut App,
) -> impl IntoElement {
    row_shell(label.into(), hint, control, cx)
}

fn row_shell(
    label: SharedString,
    hint: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .gap_4()
        .items_center()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(settings_field_label(label, cx))
                .when_some(hint, |el, text| el.child(field_hint(text, cx))),
        )
        .child(div().flex_shrink_0().child(control))
}

fn bay_fill(theme: &Theme) -> Hsla {
    if theme.is_dark() {
        theme.secondary.opacity(0.38)
    } else {
        theme.group_box.opacity(0.92)
    }
}

#[derive(IntoElement)]
pub(crate) struct SettingsBay {
    title: SharedString,
    children: Vec<AnyElement>,
}

impl SettingsBay {
    pub(crate) fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            children: Vec::new(),
        }
    }

    pub(crate) fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_any_element());
        self
    }

    pub(crate) fn children<I>(mut self, children: I) -> Self
    where
        I: IntoIterator,
        I::Item: IntoElement,
    {
        self.children
            .extend(children.into_iter().map(|c| c.into_any_element()));
        self
    }
}

impl RenderOnce for SettingsBay {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let bay_id: SharedString = format!("settings-bay-{}", self.title).into();

        div()
            .id(bay_id)
            .w_full()
            .rounded(theme.radius_lg)
            .bg(bay_fill(&theme))
            .child(
                v_flex()
                    .w_full()
                    .px(px(16.))
                    .py(px(14.))
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(self.title),
                    )
                    .children(self.children),
            )
    }
}

type ToggleHandler = Rc<dyn Fn(bool, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub(crate) struct SettingsToggleRow {
    id: ElementId,
    label: SharedString,
    hint: Option<SharedString>,
    checked: bool,
    disabled: bool,
    on_toggle: ToggleHandler,
}

impl SettingsToggleRow {
    pub(crate) fn new<F>(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        checked: bool,
        on_toggle: F,
    ) -> Self
    where
        F: Fn(bool, &mut Window, &mut App) + 'static,
    {
        Self {
            id: id.into(),
            label: label.into(),
            hint: None,
            checked,
            disabled: false,
            on_toggle: Rc::new(on_toggle),
        }
    }

    pub(crate) fn hint(mut self, text: impl Into<SharedString>) -> Self {
        self.hint = Some(text.into());
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for SettingsToggleRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let on_toggle = self.on_toggle.clone();
        let switch = Switch::new(self.id)
            .checked(self.checked)
            .disabled(self.disabled)
            .on_click(move |next, window, cx| on_toggle(*next, window, cx));
        row_shell(self.label, self.hint, switch, cx)
    }
}

#[derive(Clone)]
pub(crate) struct ExclusiveOpt<V: Copy> {
    pub value: V,
    #[allow(dead_code)]
    pub id: SharedString,
    pub label: SharedString,
    pub icon: Option<IconName>,
}

impl<V: Copy> ExclusiveOpt<V> {
    pub(crate) fn new(
        value: V,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            value,
            id: id.into(),
            label: label.into(),
            icon: None,
        }
    }

    pub(crate) fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }
}

type ExclusiveHandler<V> = Rc<dyn Fn(V, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub(crate) struct SettingsExclusiveRow<V: Copy + PartialEq + 'static> {
    bar_id: SharedString,
    label: SharedString,
    hint: Option<SharedString>,
    selected: V,
    options: Vec<ExclusiveOpt<V>>,
    disabled: bool,
    on_select: ExclusiveHandler<V>,
}

impl<V: Copy + PartialEq + 'static> SettingsExclusiveRow<V> {
    pub(crate) fn new<F>(
        bar_id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        selected: V,
        options: impl IntoIterator<Item = ExclusiveOpt<V>>,
        on_select: F,
    ) -> Self
    where
        F: Fn(V, &mut Window, &mut App) + 'static,
    {
        Self {
            bar_id: bar_id.into(),
            label: label.into(),
            hint: None,
            selected,
            options: options.into_iter().collect(),
            disabled: false,
            on_select: Rc::new(on_select),
        }
    }

    pub(crate) fn hint(mut self, text: impl Into<SharedString>) -> Self {
        self.hint = Some(text.into());
        self
    }

    pub(crate) fn hint_dynamic(mut self, text: Option<impl Into<SharedString>>) -> Self {
        self.hint = text.map(Into::into);
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl<V: Copy + PartialEq + 'static> RenderOnce for SettingsExclusiveRow<V> {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let selected_value = self.selected;
        let current = self
            .options
            .iter()
            .find(|opt| opt.value == selected_value);
        let current_label = current
            .map(|opt| opt.label.clone())
            .unwrap_or_else(|| "Choose".into());
        let current_icon = current.and_then(|opt| opt.icon.clone());
        let disabled = self.disabled;
        let options = self.options;
        let on_select = self.on_select;

        let trigger = Button::new(self.bar_id)
            .outline()
            .small()
            .min_w(px(132.))
            .label(current_label)
            .dropdown_caret(true)
            .disabled(disabled)
            .when_some(current_icon, |btn, icon| btn.icon(icon));

        let control = if disabled {
            trigger.into_any_element()
        } else {
            trigger
                .dropdown_menu_with_anchor(Corner::TopRight, {
                    let options = options.clone();
                    let on_select = on_select.clone();
                    move |menu, _, _| {
                        let mut menu = menu.min_w(px(168.)).check_side(Side::Right);
                        for opt in options.iter().cloned() {
                            let value = opt.value;
                            let on_select = on_select.clone();
                            let mut item =
                                PopupMenuItem::new(opt.label).checked(value == selected_value);
                            if let Some(icon) = opt.icon {
                                item = item.icon(icon);
                            }
                            menu = menu.item(item.on_click(move |_, window, cx| {
                                on_select(value, window, cx);
                            }));
                        }
                        menu
                    }
                })
                .into_any_element()
        };

        row_shell(self.label, self.hint, control, cx)
    }
}

#[derive(IntoElement)]
pub(crate) struct TypeFolderStrip {
    kind: FileTypeKind,
    folder_input: Entity<InputState>,
    current_folder: String,
    enabled: bool,
    organize_master: bool,
    app: Entity<DownloadApp>,
    on_enabled: ToggleHandler,
}

impl TypeFolderStrip {
    pub(crate) fn new<F>(
        kind: FileTypeKind,
        folder_input: &Entity<InputState>,
        current_folder: &str,
        enabled: bool,
        organize_master: bool,
        app: Entity<DownloadApp>,
        on_enabled: F,
    ) -> Self
    where
        F: Fn(bool, &mut Window, &mut App) + 'static,
    {
        Self {
            kind,
            folder_input: folder_input.clone(),
            current_folder: current_folder.to_string(),
            enabled,
            organize_master,
            app,
            on_enabled: Rc::new(on_enabled),
        }
    }
}

impl RenderOnce for TypeFolderStrip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let kind = self.kind;
        let label = kind.label();
        let default_name = kind.default_folder_name();
        let locked = !self.organize_master;
        let on_enabled = self.on_enabled.clone();
        let switch_id: SharedString = format!("category-enabled-{label}").into();

        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .opacity(if locked { 0.55 } else { 1.0 })
            .child(
                div()
                    .size(px(28.))
                    .flex_shrink_0()
                    .rounded(theme.radius)
                    .bg(theme.secondary.opacity(if theme.is_dark() { 0.7 } else { 0.9 }))
                    .border_1()
                    .border_color(theme.border.opacity(0.5))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::empty()
                            .path(kind.icon_path())
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(
                div()
                    .w(px(96.))
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(label),
            )
            .child(
                settings_input_with_reset(
                    format!("category-folder-reset-{label}"),
                    &self.folder_input,
                    &self.current_folder,
                    default_name,
                    default_name,
                    self.app,
                    locked,
                )
                .flex_1(),
            )
            .child(
                Switch::new(switch_id)
                    .small()
                    .checked(self.enabled)
                    .disabled(locked)
                    .on_click(move |next, window, cx| on_enabled(*next, window, cx)),
            )
    }
}

pub(crate) fn accent_preset_swatch(
    preset: AccentPreset,
    selected: bool,
    swatch: Hsla,
    theme: &Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let label = preset.label();
    let tip: SharedString = if preset == AccentPreset::Default {
        "Default, stock theme color".into()
    } else {
        label.to_string().into()
    };
    let light_fill = swatch.l > 0.72;
    let fill_border = if selected {
        if light_fill {
            theme.foreground.opacity(0.35)
        } else {
            theme.background.opacity(0.35)
        }
    } else if light_fill {
        theme.border.opacity(0.85)
    } else {
        theme.border.opacity(0.45)
    };
    div()
        .id(SharedString::from(format!("accent-{label}")))
        .size(px(32.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            if light_fill {
                theme.muted_foreground.opacity(0.95)
            } else {
                theme.foreground.opacity(0.92)
            }
        } else {
            theme.border.opacity(0.0)
        })
        .when(!selected, |el| {
            el.hover(|s| {
                s.border_color(theme.muted_foreground.opacity(0.55))
                    .bg(theme.secondary.opacity(0.4))
            })
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.set_accent_preset(preset, window, cx);
        }))
        .child(
            div()
                .size(px(20.))
                .rounded_full()
                .bg(swatch)
                .border_1()
                .border_color(fill_border),
        )
}

pub(crate) fn accent_custom_swatch(
    selected: bool,
    _custom_color: Hsla,
    theme: &Theme,
    cx: &mut Context<DownloadApp>,
) -> impl IntoElement {
    let tip: SharedString = "Custom, mix your own accent".into();
    let plate = hsla(0.0, 0.0, 0.98, 1.0);
    let brush = hsla(0.0, 0.0, 0.22, 1.0);

    div()
        .id("accent-Custom")
        .size(px(32.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            theme.foreground.opacity(0.92)
        } else {
            theme.border.opacity(0.0)
        })
        .when(!selected, |el| {
            el.hover(|s| {
                s.border_color(theme.muted_foreground.opacity(0.55))
                    .bg(theme.secondary.opacity(0.4))
            })
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(cx.listener(|this, _, window, cx| {
            this.set_accent_preset(AccentPreset::Custom, window, cx);
        }))
        .child(
            div()
                .size(px(20.))
                .rounded_full()
                .bg(plate)
                .border_1()
                .border_color(theme.border.opacity(0.5))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::empty()
                        .path("icons/paintbrush.svg")
                        .with_size(px(12.))
                        .text_color(brush),
                ),
        )
}

pub(crate) fn accent_hsl_slider_row(
    label: &'static str,
    value: String,
    slider: impl IntoElement,
    theme: &Theme,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .w_full()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(theme.muted_foreground.opacity(0.85))
                        .child(value),
                ),
        )
        .child(slider)
}
