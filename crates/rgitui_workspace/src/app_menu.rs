//! The native application menu bar.
//!
//! Every item dispatches an existing `rgitui::` command, so a menu item, its
//! shortcut and the command palette can never disagree about what runs. AppKit
//! reads each item's key equivalent from the keymap when the menus are set, and
//! greys out an item whose action no element on the focused window's dispatch
//! path handles, so availability comes from the same place the keyboard uses.
//!
//! Labels are translated when the menus are built, so they are rebuilt whenever
//! the language or the keymap changes.

use gpui::{App, Menu, MenuItem, PromptLevel, SystemMenuType};
use rgitui_i18n::{t, ActiveLanguage};

use crate::keymap::actions as a;
use crate::keymap::KeymapState;

/// Installs the menu bar and the app-level handlers for the window commands,
/// and keeps the menus in step with the language and the keymap.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &a::About, cx| show_about(cx));
    cx.on_action(|_: &a::Hide, cx| cx.hide());
    cx.on_action(|_: &a::HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &a::ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &a::Minimize, cx| {
        with_active_window(cx, |window| window.minimize_window());
    });
    cx.on_action(|_: &a::Zoom, cx| {
        with_active_window(cx, |window| window.zoom_window());
    });

    set_menus(cx);
    cx.observe_global::<ActiveLanguage>(set_menus).detach();
    cx.observe_global::<KeymapState>(set_menus).detach();
}

fn set_menus(cx: &mut App) {
    cx.set_menus(app_menus());
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window)) {
    if let Some(handle) = cx.active_window() {
        handle.update(cx, |_, window, _| f(window)).ok();
    }
}

fn show_about(cx: &mut App) {
    let Some(handle) = cx.active_window() else {
        return;
    };
    let detail = t!("menu.app.about_detail", version = env!("CARGO_PKG_VERSION"));
    let ok = t!("menu.app.about_ok");
    handle
        .update(cx, |_, window, cx| {
            let answer = window.prompt(
                PromptLevel::Info,
                "rgitui",
                Some(&detail),
                &[ok.as_ref()],
                cx,
            );
            // The answer carries nothing, there being one button, but it is
            // awaited rather than dropped so the prompt is not cancelled.
            cx.background_executor()
                .spawn(async move {
                    answer.await.ok();
                })
                .detach();
        })
        .ok();
}

/// The menu bar, top to bottom. Pure apart from translation.
pub fn app_menus() -> Vec<Menu> {
    vec![
        Menu::new("rgitui").items([
            MenuItem::action(t!("menu.app.about"), a::About),
            MenuItem::separator(),
            MenuItem::action(t!("menu.app.settings"), a::Settings),
            MenuItem::separator(),
            MenuItem::os_submenu(t!("menu.app.services"), SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(t!("menu.app.hide"), a::Hide),
            MenuItem::action(t!("menu.app.hide_others"), a::HideOthers),
            MenuItem::action(t!("menu.app.show_all"), a::ShowAll),
            MenuItem::separator(),
            MenuItem::action(t!("menu.app.quit"), a::Quit),
        ]),
        Menu::new(t!("menu.file")).items([
            MenuItem::action(t!("menu.file.open_repo"), a::OpenRepo),
            MenuItem::action(t!("menu.file.clone_repo"), a::CloneRepo),
            MenuItem::separator(),
            MenuItem::action(t!("menu.file.restore_workspace"), a::RestoreLastWorkspace),
            MenuItem::action(t!("menu.file.home"), a::WorkspaceHome),
            MenuItem::separator(),
            MenuItem::action(t!("menu.file.close_tab"), a::CloseTab),
        ]),
        Menu::new(t!("menu.view")).items([
            MenuItem::action(t!("menu.view.command_palette"), a::CommandPalette),
            MenuItem::separator(),
            // Toggle Diff Mode is left out: its only binding is a bare
            // Shift+D, and a menu key equivalent would fire it while typing.
            MenuItem::action(t!("menu.view.graph"), a::ToggleGraph),
            MenuItem::separator(),
            MenuItem::action(t!("menu.view.search"), a::Search),
            MenuItem::action(t!("menu.view.global_search"), a::GlobalSearch),
            MenuItem::separator(),
            MenuItem::action(t!("menu.view.issues"), a::ToggleIssues),
            MenuItem::action(t!("menu.view.pull_requests"), a::TogglePullRequests),
            MenuItem::action(t!("menu.view.branch_health"), a::ToggleBranchHealth),
            MenuItem::action(t!("menu.view.stashes"), a::ToggleStashes),
            MenuItem::separator(),
            MenuItem::action(t!("menu.view.theme_editor"), a::OpenThemeEditor),
            MenuItem::action(t!("menu.view.refresh"), a::Refresh),
        ]),
        Menu::new(t!("menu.repository")).items([
            MenuItem::action(t!("menu.repository.fetch"), a::Fetch),
            MenuItem::action(t!("menu.repository.pull"), a::Pull),
            MenuItem::action(t!("menu.repository.pull_autostash"), a::PullAutostash),
            MenuItem::action(t!("menu.repository.push"), a::Push),
            MenuItem::separator(),
            MenuItem::action(t!("menu.repository.commit"), a::Commit),
            MenuItem::action(t!("menu.repository.stage_all"), a::StageAll),
            MenuItem::action(t!("menu.repository.unstage_all"), a::UnstageAll),
            MenuItem::separator(),
            MenuItem::action(t!("menu.repository.stash"), a::StashSave),
            MenuItem::action(t!("menu.repository.stash_pop"), a::StashPop),
            MenuItem::separator(),
            MenuItem::action(t!("menu.repository.new_branch"), a::CreateBranch),
            MenuItem::action(t!("menu.repository.switch_branch"), a::SwitchBranch),
            MenuItem::action(t!("menu.repository.merge_branch"), a::MergeBranch),
            MenuItem::action(t!("menu.repository.new_tag"), a::CreateTag),
            MenuItem::action(t!("menu.repository.new_worktree"), a::CreateWorktree),
            MenuItem::action(t!("menu.repository.create_pr"), a::CreatePr),
            MenuItem::separator(),
            MenuItem::action(t!("menu.repository.undo"), a::Undo),
            MenuItem::action(t!("menu.repository.reflog"), a::Reflog),
        ]),
        Menu::new(t!("menu.window")).items([
            MenuItem::action(t!("menu.window.minimize"), a::Minimize),
            MenuItem::action(t!("menu.window.zoom"), a::Zoom),
            MenuItem::separator(),
            MenuItem::action(t!("menu.window.next_tab"), a::NextTab),
            MenuItem::action(t!("menu.window.previous_tab"), a::PrevTab),
        ]),
        Menu::new(t!("menu.help")).items([
            MenuItem::action(t!("menu.help.shortcuts"), a::Shortcuts),
            MenuItem::action(t!("menu.help.edit_keymap"), a::OpenKeymap),
        ]),
    ]
}
