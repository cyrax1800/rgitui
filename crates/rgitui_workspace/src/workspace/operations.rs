use std::time::Instant;

use gpui::Context;
use rgitui_git::{GitOperationKind, GitOperationUpdate, PullAttempt, PullLocalChanges};

use crate::ToastKind;

use super::{Workspace, OPERATION_OUTPUT_AUTO_HIDE_SECS};

impl Workspace {
    pub(super) fn schedule_operation_output_auto_hide(
        &mut self,
        timestamp: Instant,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(
                    OPERATION_OUTPUT_AUTO_HIDE_SECS,
                ))
                .await;
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    if let Some(output) = &this.operations.last_operation_output {
                        if output.timestamp == timestamp && !output.expanded && !output.is_error {
                            this.operations.last_operation_output = None;
                            cx.notify();
                        }
                    }
                })
                .ok();
            })
        })
        .detach();
    }

    /// What Pull does about uncommitted changes, as chosen in Settings.
    pub(super) fn pull_local_changes_setting(cx: &gpui::App) -> PullLocalChanges {
        cx.try_global::<rgitui_settings::SettingsState>()
            .map(|state| state.settings().pull_local_changes)
            .unwrap_or_default()
    }

    /// Run a failed operation again. `with_autostash` repeats a pull with
    /// autostash whatever it was first run with — the status bar offers that when
    /// uncommitted changes are what stopped it.
    pub(super) fn retry_git_operation(
        &mut self,
        update: &GitOperationUpdate,
        with_autostash: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.active_project().cloned() else {
            self.show_toast("No active repository to retry.", ToastKind::Warning, cx);
            return;
        };

        if update.kind == GitOperationKind::Checkout {
            if let Some(branch) = update.branch_name.as_ref() {
                self.request_branch_checkout(project, branch.clone(), cx);
            }
            return;
        }

        // The checkout the operation actually ran in, carried on the update
        // itself. Resolving it here from what is being inspected *now* is
        // wrong whenever the failure arrives after the user has moved on —
        // and for a network operation, whose failure is a timeout or a
        // rejected push, arriving late is the normal case. Falling back to the
        // effective path covers operations that record no worktree.
        let worktree_path = update
            .worktree_path
            .clone()
            .unwrap_or_else(|| self.effective_worktree_path(cx));

        let local_changes = retry_pull_local_changes(
            update.pull,
            Self::pull_local_changes_setting(cx),
            with_autostash,
        );

        let handled = project.update(cx, |proj, cx| match update.kind {
            GitOperationKind::Fetch => {
                if let Some(remote_name) = update.remote_name.as_deref() {
                    proj.fetch_at(remote_name, &worktree_path, cx).detach();
                    true
                } else {
                    proj.fetch_default_at(&worktree_path, cx).detach();
                    true
                }
            }
            GitOperationKind::Pull => {
                if let Some(remote_name) = update.remote_name.as_deref() {
                    proj.pull_at(remote_name, local_changes, &worktree_path, cx)
                        .detach();
                    true
                } else {
                    proj.pull_default_at(local_changes, &worktree_path, cx)
                        .detach();
                    true
                }
            }
            GitOperationKind::Push | GitOperationKind::ForcePush => {
                let force = update.kind == GitOperationKind::ForcePush;
                if let Some(remote_name) = update.remote_name.as_deref() {
                    proj.push_at(remote_name, force, &worktree_path, cx)
                        .detach();
                    true
                } else {
                    proj.push_default_at(force, &worktree_path, cx).detach();
                    true
                }
            }
            GitOperationKind::Merge => {
                if let Some(branch_name) = update.branch_name.as_deref() {
                    proj.merge_branch(branch_name, cx).detach();
                    true
                } else {
                    false
                }
            }
            GitOperationKind::RemoveRemote => {
                if let Some(remote_name) = update.remote_name.as_deref() {
                    proj.remove_remote(remote_name, cx).detach();
                    true
                } else {
                    false
                }
            }
            _ => false,
        });

        if !handled {
            self.show_toast(
                "This operation cannot be retried automatically.",
                ToastKind::Info,
                cx,
            );
        } else {
            self.operations.last_failed_git_operation = None;
        }
    }
}

/// How a retried pull should treat uncommitted changes.
///
/// A pull that ran with autostash is retried with it: the user may have asked
/// for it explicitly, against the setting, and a pull lost to a timeout should
/// not come back as a plain one that their changes then block. Anything else
/// follows the setting as it stands now, so changing it and pressing Retry
/// does what was just chosen.
fn retry_pull_local_changes(
    attempt: Option<PullAttempt>,
    setting: PullLocalChanges,
    with_autostash: bool,
) -> PullLocalChanges {
    let ran_with_autostash =
        attempt.is_some_and(|attempt| attempt.local_changes == PullLocalChanges::Autostash);
    if with_autostash || ran_with_autostash {
        PullLocalChanges::Autostash
    } else {
        setting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(local_changes: PullLocalChanges) -> Option<PullAttempt> {
        Some(PullAttempt {
            local_changes,
            blocked_by_local_changes: false,
        })
    }

    #[test]
    fn a_pull_that_ran_with_autostash_is_retried_with_it() {
        assert_eq!(
            retry_pull_local_changes(
                attempt(PullLocalChanges::Autostash),
                PullLocalChanges::RequireClean,
                false
            ),
            PullLocalChanges::Autostash,
            "an explicit autostash pull must not be retried as the setting's plain one"
        );
    }

    #[test]
    fn any_other_pull_is_retried_with_the_setting_as_it_stands() {
        for ran_with in [
            None,
            attempt(PullLocalChanges::GitDecides),
            attempt(PullLocalChanges::RequireClean),
        ] {
            for setting in [
                PullLocalChanges::GitDecides,
                PullLocalChanges::Autostash,
                PullLocalChanges::RequireClean,
            ] {
                assert_eq!(retry_pull_local_changes(ran_with, setting, false), setting);
            }
        }
    }

    #[test]
    fn retrying_with_autostash_overrides_the_setting() {
        assert_eq!(
            retry_pull_local_changes(
                attempt(PullLocalChanges::RequireClean),
                PullLocalChanges::RequireClean,
                true
            ),
            PullLocalChanges::Autostash
        );
    }
}
