use gpui::prelude::*;
use gpui::{div, px, App, ClickEvent, Decorations, SharedString, Window};
use rgitui_i18n::t;
use rgitui_theme::{ActiveTheme, Color, StyledExt};
use rgitui_ui::{
    Icon, IconName, IconSize, Label, LabelSize, Tooltip, WindowControl, WindowControlType,
};

use std::cell::Cell;
use std::rc::Rc;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Height of the unified title row: the window's title bar and the
/// repository toolbar share it, as in a native macOS unified toolbar.
pub const TITLE_ROW_HEIGHT: f32 = 40.;

/// Diameter of a macOS traffic light. Fixed in points by AppKit; it does not
/// follow the interface's rem size.
const TRAFFIC_LIGHT_SIZE: f32 = 14.;

/// Left inset of the first traffic light, matching the gap AppKit leaves above
/// it so the three buttons sit in a square corner.
const TRAFFIC_LIGHT_INSET: f32 = 13.;

/// Room the title row leaves for the traffic lights: the inset and the three
/// buttons. The title's own left padding adds the gap before its first label.
const TRAFFIC_LIGHT_PADDING: f32 = 72.;

/// Where the traffic lights sit so they are centred in the title row.
pub fn traffic_light_position() -> gpui::Point<gpui::Pixels> {
    gpui::point(
        px(TRAFFIC_LIGHT_INSET),
        px((TITLE_ROW_HEIGHT - TRAFFIC_LIGHT_SIZE) / 2.),
    )
}

/// Keeps a press on a control inside the window-chrome row from reaching the
/// row, which would otherwise arm a window drag on it and count a double-click
/// on a button as a double-click on the title bar.
pub fn stop_chrome_drag<E: gpui::InteractiveElement>(element: E) -> E {
    element.on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// The window-chrome row that hosts the title and toolbar.
///
/// The window uses a transparent title bar, so this row is what the user grabs
/// to move the window. On macOS it leaves room for the traffic lights (none in
/// full screen, where AppKit hides them), drags the window, and answers a
/// double-click with the system's title-bar action (zoom or minimize, per the
/// user's Dock setting). Elsewhere it is a plain row.
pub fn window_chrome_row(window: &mut Window, cx: &mut App) -> gpui::Stateful<gpui::Div> {
    let colors = cx.colors();
    let row = div()
        .id("window-chrome-row")
        .h_flex()
        .flex_shrink_0()
        .w_full()
        .h(px(TITLE_ROW_HEIGHT))
        .items_center()
        .bg(colors.toolbar_background)
        .border_b_1()
        .border_color(colors.border_variant);

    if !cfg!(target_os = "macos") {
        return row;
    }

    // Armed on a press on the row's own background and spent on the first
    // move. Controls inside the row stop their presses from reaching it (see
    // [`stop_chrome_drag`]), so a click on a button stays a click. A plain cell
    // rather than entity state, so arming it redraws nothing.
    let drag_armed = window
        .use_keyed_state("window-chrome-drag", cx, |_, _| Rc::new(Cell::new(false)))
        .read(cx)
        .clone();
    let arm = drag_armed.clone();
    let disarm_up = drag_armed.clone();
    let disarm_out = drag_armed.clone();

    row.window_control_area(gpui::WindowControlArea::Drag)
        .when(!window.is_fullscreen(), |row| {
            row.pl(px(TRAFFIC_LIGHT_PADDING))
        })
        .on_mouse_down(gpui::MouseButton::Left, move |_, _, _| arm.set(true))
        .on_mouse_up(gpui::MouseButton::Left, move |_, _, _| disarm_up.set(false))
        .on_mouse_down_out(move |_, _, _| disarm_out.set(false))
        .on_mouse_move(move |_, window, _| {
            if drag_armed.replace(false) {
                window.start_window_move();
            }
        })
        .on_click(|event, window, _| {
            if event.click_count() == 2 {
                window.titlebar_double_click();
            }
        })
}

/// The application title bar.
#[derive(IntoElement)]
pub struct TitleBar {
    repo_name: SharedString,
    branch_name: SharedString,
    has_changes: bool,
    head_detached: bool,
    repo_state_label: Option<SharedString>,
    on_branch_click: Option<ClickHandler>,
}

impl TitleBar {
    pub fn new(repo_name: impl Into<SharedString>, branch_name: impl Into<SharedString>) -> Self {
        Self {
            repo_name: repo_name.into(),
            branch_name: branch_name.into(),
            has_changes: false,
            head_detached: false,
            repo_state_label: None,
            on_branch_click: None,
        }
    }

    pub fn has_changes(mut self, has_changes: bool) -> Self {
        self.has_changes = has_changes;
        self
    }

    pub fn head_detached(mut self, detached: bool) -> Self {
        self.head_detached = detached;
        self
    }

    pub fn repo_state(mut self, label: impl Into<SharedString>) -> Self {
        self.repo_state_label = Some(label.into());
        self
    }

    pub fn on_branch_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_branch_click = Some(Box::new(handler));
        self
    }

    fn render_separator(colors: &rgitui_theme::ThemeColors) -> gpui::Div {
        div()
            .w(px(1.))
            .h(px(14.))
            .rounded(px(0.5))
            .bg(colors.border_variant)
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.colors().clone();
        // The title bar can paint before settings finish loading, so fall
        // back to English rather than requiring the global.

        let branch_color = if self.head_detached {
            Color::Warning
        } else {
            Color::Accent
        };

        let hover_bg = colors.ghost_element_active;
        let branch_bg = colors.ghost_element_hover;
        let tooltip = if self.head_detached {
            t!("workspace.detached.title", commit = self.branch_name)
        } else {
            self.branch_name.clone()
        };

        let mut branch_pill = stop_chrome_drag(div().id("title-branch-pill"))
            .h_flex()
            .min_w_0()
            .max_w(px(240.))
            .h(px(22.))
            .px(px(8.))
            .gap(px(4.))
            .items_center()
            .rounded(px(4.))
            .bg(branch_bg)
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .tooltip(Tooltip::text(tooltip))
            .child(
                Icon::new(IconName::GitBranch)
                    .size(IconSize::Small)
                    .color(branch_color),
            )
            .child(
                Label::new(self.branch_name.clone())
                    .size(LabelSize::Small)
                    .color(branch_color)
                    .weight(gpui::FontWeight::SEMIBOLD)
                    .truncate(),
            )
            .when(self.head_detached, |el| {
                el.child(
                    Label::new(t!("title_bar.detached"))
                        .size(LabelSize::XSmall)
                        .color(Color::Warning),
                )
            })
            .when(self.has_changes && !self.head_detached, |el| {
                el.child(
                    div()
                        .w(px(6.))
                        .h(px(6.))
                        .rounded_full()
                        .bg(colors.vc_modified),
                )
            });

        if let Some(handler) = self.on_branch_click {
            branch_pill = branch_pill.on_click(handler);
        }

        let mut bar = div()
            .h_flex()
            .h_full()
            .flex_shrink_0()
            .pl(px(12.))
            .pr(px(8.))
            .gap(px(8.))
            .items_center();

        // On Linux with server decorations, the WM owns the window controls so we
        // don't render them here. On client decorations we render them.
        let use_window_controls = matches!(window.window_decorations(), Decorations::Client { .. });

        if use_window_controls {
            // Window controls on the left (Linux CSD convention)
            bar = bar.child(
                div()
                    .h_flex()
                    .gap(px(4.))
                    .items_center()
                    .child(WindowControl::new(
                        "minimize",
                        WindowControlType::Minimize,
                        cx,
                    ))
                    .child(WindowControl::new(
                        "maximize",
                        if window.is_maximized() {
                            WindowControlType::Restore
                        } else {
                            WindowControlType::Maximize
                        },
                        cx,
                    ))
                    .child(WindowControl::new("close", WindowControlType::Close, cx)),
            );
        }

        bar = bar
            .child(
                div()
                    .h_flex()
                    .gap(px(5.))
                    .min_w_0()
                    .max_w(px(200.))
                    .items_center()
                    .child(
                        Icon::new(IconName::Folder)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(self.repo_name)
                            .size(LabelSize::Small)
                            .weight(gpui::FontWeight::SEMIBOLD)
                            .truncate(),
                    ),
            )
            .child(Self::render_separator(&colors))
            .child(branch_pill);

        if let Some(state_label) = self.repo_state_label {
            bar = bar.child(
                div()
                    .h_flex()
                    .h(px(20.))
                    .px(px(6.))
                    .gap(px(4.))
                    .items_center()
                    .rounded(px(3.))
                    .bg(colors.ghost_element_selected)
                    .child(
                        Icon::new(IconName::FileConflict)
                            .size(IconSize::XSmall)
                            .color(Color::Warning),
                    )
                    .child(
                        Label::new(state_label)
                            .size(LabelSize::XSmall)
                            .color(Color::Warning)
                            .weight(gpui::FontWeight::SEMIBOLD),
                    ),
            );
        }

        bar
    }
}
