use std::time::Duration;

use gpui::{
    div, prelude::FluentBuilder, px, Context, ElementId, InteractiveElement, IntoElement,
    ParentElement, SharedString, Styled,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable,
};

use super::DownloadApp;

/// In-app toast (bottom-right). gpui-component's Notification layer is fixed top-right.
pub(crate) const TOAST_AUTO_HIDE: Duration = Duration::from_secs(5);
pub(crate) const TOAST_MAX_STACK: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Info,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastActionKind {
    ApplyUpdate,
}

#[derive(Debug, Clone)]
pub(crate) struct ToastAction {
    pub label: SharedString,
    pub kind: ToastActionKind,
}

#[derive(Debug, Clone)]
pub(crate) struct Toast {
    pub id: u64,
    pub message: SharedString,
    pub kind: ToastKind,
    pub action: Option<ToastAction>,
}

impl DownloadApp {
    pub(crate) fn flush_toast(&mut self, cx: &mut Context<Self>) {
        if let Some(message) = self.pending_toast.take() {
            self.push_toast(message, ToastKind::Info, None, cx);
        }
    }

    pub(crate) fn show_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.push_toast(message, ToastKind::Info, None, cx);
    }

    pub(crate) fn show_error_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.push_toast(message, ToastKind::Error, None, cx);
    }

    pub(crate) fn replace_update_toast(
        &mut self,
        message: impl Into<String>,
        kind: ToastKind,
        action: Option<(&str, ToastActionKind)>,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.update_toast_id.take() {
            self.toasts.retain(|t| t.id != id);
        }
        let id = self.push_toast(message, kind, action, cx);
        self.update_toast_id = Some(id);
    }

    pub(crate) fn clear_update_toast(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.update_toast_id.take() {
            self.dismiss_toast(id, cx);
        }
    }

    fn push_toast(
        &mut self,
        message: impl Into<String>,
        kind: ToastKind,
        action: Option<(&str, ToastActionKind)>,
        cx: &mut Context<Self>,
    ) -> u64 {
        let id = self.next_toast_id;
        self.next_toast_id = self.next_toast_id.wrapping_add(1);
        let action = action.map(|(label, action_kind)| ToastAction {
            label: SharedString::from(label.to_string()),
            kind: action_kind,
        });
        let has_action = action.is_some();
        self.toasts.push(Toast {
            id,
            message: SharedString::from(message.into()),
            kind,
            action,
        });
        if self.toasts.len() > TOAST_MAX_STACK {
            let overflow = self.toasts.len() - TOAST_MAX_STACK;
            let drained: Vec<u64> = self.toasts.drain(0..overflow).map(|t| t.id).collect();
            if let Some(uid) = self.update_toast_id {
                if drained.contains(&uid) {
                    self.update_toast_id = None;
                }
            }
        }

        if !has_action {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TOAST_AUTO_HIDE).await;
                let _ = this.update(cx, |app, cx| {
                    app.dismiss_toast(id, cx);
                });
            })
            .detach();
        }

        cx.notify();
        id
    }

    fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.id != id);
        if self.update_toast_id == Some(id) {
            self.update_toast_id = None;
        }
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    pub(crate) fn render_toast_layer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let toasts = self.toasts.clone();

        div().absolute().bottom(px(46.)).right_4().child(
            v_flex()
                .id("toast-list")
                .gap_3()
                .children(toasts.into_iter().map(|toast| {
                    let id = toast.id;
                    let action = toast.action.clone();
                    let (icon, icon_color) = match toast.kind {
                        ToastKind::Info => (IconName::Info, theme.info),
                        ToastKind::Error => (IconName::CircleX, theme.danger),
                    };
                    h_flex()
                        .id(ElementId::from(("toast", id)))
                        .occlude()
                        .items_center()
                        .gap_3()
                        .w_112()
                        .max_w(px(420.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .rounded(theme.radius_lg)
                        .shadow_md()
                        .py_3()
                        .px_4()
                        .child(div().pt_0p5().child(Icon::new(icon).text_color(icon_color)))
                        .child(div().flex_1().min_w_0().text_sm().child(toast.message))
                        .when_some(action, |this, action| {
                            let kind = action.kind;
                            this.child(
                                Button::new(ElementId::from(("toast-action", id)))
                                    .primary()
                                    .xsmall()
                                    .label(action.label.to_string())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.dismiss_toast(id, cx);
                                        this.on_update_toast_action(kind, cx);
                                    })),
                            )
                        })
                        .child(
                            Button::new(ElementId::from(("toast-close", id)))
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.dismiss_toast(id, cx);
                                })),
                        )
                })),
        )
    }
}
