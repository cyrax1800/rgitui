use gpui::prelude::*;
use gpui::{div, px, Context, ElementId, Entity, EventEmitter, Render, SharedString, Window};
use rgitui_theme::{ActiveTheme, Color, StyledExt};
use rgitui_ui::{
    Button, ButtonSize, ButtonStyle, CheckState, Checkbox, IconName, Label, LabelSize, TextInput,
    TextInputEvent,
};

use crate::keymap;
use crate::CommandId;
use rgitui_i18n::{key, t};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteDialogEvent {
    Add {
        name: String,
        url: String,
        is_default: bool,
    },
    Edit {
        old_name: String,
        name: String,
        url: String,
        is_default: bool,
    },
    Dismissed,
}

#[derive(Clone)]
enum RemoteDialogMode {
    Add,
    Edit { old_name: String },
}

pub struct RemoteDialog {
    name_editor: Entity<TextInput>,
    url_editor: Entity<TextInput>,
    is_default: bool,
    mode: RemoteDialogMode,
    visible: bool,
    pending_focus: bool,
    error: Option<String>,
    submitting: bool,
}

impl EventEmitter<RemoteDialogEvent> for RemoteDialog {}

impl RemoteDialog {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let name_editor = cx.new(|cx| {
            let mut editor = TextInput::new(cx);
            editor.set_placeholder("origin");
            editor
        });
        let url_editor = cx.new(|cx| {
            let mut editor = TextInput::new(cx);
            editor.set_placeholder_message(key!("remote_dialog.url_placeholder"));
            editor
        });
        for editor in [&name_editor, &url_editor] {
            cx.subscribe(
                editor,
                |this: &mut Self, _, event: &TextInputEvent, cx| match event {
                    TextInputEvent::Changed(_) => {
                        this.error = None;
                        cx.notify();
                    }
                    TextInputEvent::Submit => this.submit(cx),
                    _ => {}
                },
            )
            .detach();
        }
        Self {
            name_editor,
            url_editor,
            is_default: false,
            mode: RemoteDialogMode::Add,
            visible: false,
            pending_focus: false,
            error: None,
            submitting: false,
        }
    }

    pub fn show_add(&mut self, cx: &mut Context<Self>) {
        self.visible = true;
        self.pending_focus = true;
        self.mode = RemoteDialogMode::Add;
        self.is_default = false;
        self.error = None;
        self.submitting = false;
        self.name_editor.update(cx, |editor, cx| editor.clear(cx));
        self.url_editor.update(cx, |editor, cx| editor.clear(cx));
        cx.notify();
    }

    pub fn show_edit(
        &mut self,
        name: String,
        url: String,
        is_default: bool,
        cx: &mut Context<Self>,
    ) {
        self.visible = true;
        self.pending_focus = true;
        self.mode = RemoteDialogMode::Edit {
            old_name: name.clone(),
        };
        self.is_default = is_default;
        self.name_editor
            .update(cx, |editor, cx| editor.set_text(name, cx));
        self.error = None;
        self.submitting = false;
        self.url_editor
            .update(cx, |editor, cx| editor.set_text(url, cx));
        cx.notify();
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if self.visible {
            self.visible = false;
            self.submitting = false;
            self.error = None;
            cx.emit(RemoteDialogEvent::Dismissed);
            cx.notify();
        }
    }

    pub fn on_finished(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        if !self.submitting {
            return;
        }
        self.submitting = false;
        match result {
            Ok(()) => {
                self.visible = false;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        let url = self.url_editor.read(cx).text().trim().to_string();
        if url.is_empty() {
            self.error = Some(t!("remote_dialog.error.url_empty").to_string());
            cx.notify();
            return;
        }
        let event = match &self.mode {
            RemoteDialogMode::Add => {
                let name = self.name_editor.read(cx).text().trim().to_string();
                if name.is_empty() {
                    self.error = Some(t!("remote_dialog.error.name_empty").to_string());
                    cx.notify();
                    return;
                }
                RemoteDialogEvent::Add {
                    name,
                    url,
                    is_default: self.is_default,
                }
            }
            RemoteDialogMode::Edit { old_name } => {
                let name = self.name_editor.read(cx).text().trim().to_string();
                if name.is_empty() {
                    self.error = Some(t!("remote_dialog.error.name_empty").to_string());
                    cx.notify();
                    return;
                }
                RemoteDialogEvent::Edit {
                    old_name: old_name.clone(),
                    name,
                    url,
                    is_default: self.is_default,
                }
            }
        };
        self.submitting = true;
        self.error = None;
        cx.emit(event);
        cx.notify();
    }

    fn dispatch_command(
        &mut self,
        command: CommandId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            CommandId::Cancel => self.hide(cx),
            _ => cx.propagate(),
        }
    }
}

impl Render for RemoteDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().id("remote-dialog").into_any_element();
        }
        if self.pending_focus {
            self.pending_focus = false;
            self.url_editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        }
        let colors = cx.colors().clone();
        let is_add = matches!(self.mode, RemoteDialogMode::Add);
        let title = if is_add {
            t!("remote_dialog.title.add")
        } else {
            t!("remote_dialog.title.edit")
        };
        let submit_label = if is_add {
            t!("remote_dialog.submit.add")
        } else {
            t!("remote_dialog.submit.edit")
        };
        let submit_icon = if is_add {
            IconName::Plus
        } else {
            IconName::Edit
        };

        let mut modal = div()
            .id("remote-dialog-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0., 0., 0., 0.35))
            .occlude()
            .child(
                div()
                    .id("remote-dialog-content")
                    .v_flex()
                    .w(px(560.))
                    .gap(px(12.))
                    .p(px(16.))
                    .bg(colors.panel_background)
                    .border_1()
                    .border_color(colors.border_variant)
                    .rounded(px(6.))
                    .child(
                        Label::new(title)
                            .size(LabelSize::Large)
                            .color(Color::Default),
                    )
                    .child(self.field(t!("remote_dialog.name"), self.name_editor.clone()))
                    .child(self.field(t!("remote_dialog.url"), self.url_editor.clone()))
                    .child(
                        div()
                            .h_flex()
                            .gap(px(8.))
                            .items_center()
                            .child(
                                Checkbox::new(
                                    ElementId::Name("remote-default-checkbox".into()),
                                    if self.is_default {
                                        CheckState::Checked
                                    } else {
                                        CheckState::Unchecked
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.is_default = !this.is_default;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                Label::new(t!("remote_dialog.set_default"))
                                    .size(LabelSize::Small)
                                    .color(Color::Default),
                            ),
                    )
                    .when_some(self.error.clone(), |el, error| {
                        el.child(
                            Label::new(SharedString::from(error))
                                .size(LabelSize::Small)
                                .color(Color::Error),
                        )
                    })
                    .child(
                        div()
                            .h_flex()
                            .justify_end()
                            .gap(px(8.))
                            .pt(px(8.))
                            .border_t_1()
                            .border_color(colors.border_variant)
                            .child(
                                Button::new("remote-dialog-cancel", t!("common.cancel"))
                                    .size(ButtonSize::Default)
                                    .style(ButtonStyle::Subtle)
                                    .on_click(cx.listener(|this, _, _, cx| this.hide(cx))),
                            )
                            .child(
                                Button::new("remote-dialog-submit", submit_label)
                                    .icon(submit_icon)
                                    .size(ButtonSize::Default)
                                    .style(ButtonStyle::Filled)
                                    .disabled(self.submitting)
                                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                            ),
                    ),
            );
        modal = modal.map(|element| {
            keymap::bind_actions(
                element,
                "RemoteDialog",
                &["Workspace", "TextInput"],
                cx,
                Self::dispatch_command,
            )
        });
        modal.into_any_element()
    }
}

impl RemoteDialog {
    fn field<T: IntoElement>(&self, label: SharedString, editor: T) -> gpui::Div {
        div()
            .h_flex()
            .gap(px(8.))
            .items_center()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(div().flex_1().child(editor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgitui_test_support::ViewTest;

    #[test]
    fn editing_each_remote_preserves_its_name_and_url() {
        let mut dialog = ViewTest::open(|_, cx| {
            rgitui_settings::init_test(cx);
            RemoteDialog::new(cx)
        });
        for name in ["origin", "cy"] {
            dialog.update(|dialog, _, cx| {
                let url = format!("ssh://git@example.com/{name}/client.git");
                dialog.show_edit(name.into(), url.clone(), false, cx);
                assert_eq!(dialog.name_editor.read(cx).text(), name);
                assert_eq!(dialog.url_editor.read(cx).text(), url);
            });
        }
    }
}
