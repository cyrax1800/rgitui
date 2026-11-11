use super::messages::{error, error_message};
use anyhow::{Context as _, Result};
use git2::{Repository, RepositoryState, StatusOptions};
use gpui::{AsyncApp, Context, Task, WeakEntity};
use rgitui_i18n::{msg, msgp, Message};
use rgitui_settings::SettingsState;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::types::*;

use super::argsafe::{validate_branch_name, validate_remote_name};
use super::auth::run_git_network_command;
use super::autostash::{pending_autostash, AutostashRestore};
use super::local_ops::format_path_list;
use super::refresh::gather_refresh_data;
use super::{
    head_branch_name, pull_target, push_target, repo_has_worktree_changes, GitProject,
    GitProjectEvent, RefreshData,
};

/// Non-fast-forward and fetch-first rejections are recoverable; permissions,
/// network errors, protected branches, and hook rejections are not.
fn is_remote_ahead_rejection(error: &str) -> bool {
    let message = error.to_ascii_lowercase();
    let normal_non_fast_forward = message.contains("[rejected]")
        && (message.contains("(fetch first)")
            || message.contains("(non-fast-forward)")
            || message.contains("remote contains work that you do not have locally")
            || message.contains("tip of your current branch is behind"));
    // A concurrent push can race the remote receive-pack after negotiation.
    // Git reports that race as `remote rejected` instead of `rejected`, with
    // the old and new ref values in the error. It is recoverable by fetching
    // the newer ref, merging it, and retrying once.
    let concurrent_remote_update = message.contains("cannot lock ref")
        && message.contains("failed to update ref")
        && message.contains(" but expected ");
    normal_non_fast_forward || concurrent_remote_update
}

fn network_lock(worktree: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<std::path::PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let path = Repository::open(worktree)
        .map(|repo| repo.commondir().to_path_buf())
        .unwrap_or_else(|_| worktree.to_path_buf())
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf());
    let mut locks = locks.lock().unwrap_or_else(|poison| poison.into_inner());
    locks.entry(path).or_default().clone()
}

pub(super) fn with_network_lock<T>(worktree: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock = network_lock(worktree);
    let _guard = lock.lock().unwrap_or_else(|poison| poison.into_inner());
    operation()
}

fn pull_preview_from_repo(
    repo: &Repository,
    remote: &str,
    branch: &str,
    repo_path: &Path,
    worktree_path: &Path,
) -> Result<PullPreview> {
    let head = repo.head()?.peel_to_commit()?.id();
    let upstream = repo
        .find_reference(&format!("refs/remotes/{remote}/{branch}"))?
        .peel_to_commit()?
        .id();
    let (ahead, behind) = repo.graph_ahead_behind(head, upstream)?;
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .include_ignored(false)
        .include_unmodified(false);
    let statuses = repo.statuses(Some(&mut options))?;
    let local_paths = statuses
        .iter()
        .filter_map(|entry| entry.path().ok().map(std::path::PathBuf::from))
        .collect::<std::collections::HashSet<_>>();
    let local_changes = !local_paths.is_empty();
    let remote_ref = format!("refs/remotes/{remote}/{branch}");
    let changed_files = if behind == 0 {
        Vec::new()
    } else {
        let output = std::process::Command::new("git")
            .current_dir(worktree_path)
            .args(["diff", "--name-only", "HEAD", &remote_ref, "--"])
            .output()
            .with_context(|| "Failed to list files changed by the pull")?;
        if !output.status.success() {
            anyhow::bail!("git diff could not preview the pull");
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(std::path::PathBuf::from)
            .collect()
    };
    let mut conflict_files = if behind == 0 {
        Vec::new()
    } else {
        let output = std::process::Command::new("git")
            .current_dir(worktree_path)
            .args([
                "merge-tree",
                "--write-tree",
                "--name-only",
                "-z",
                "HEAD",
                &remote_ref,
            ])
            .output()
            .with_context(|| "Failed to preview the pull merge")?;
        if output.status.success() {
            Vec::new()
        } else if output.status.code() == Some(1) {
            parse_merge_tree_conflict_paths(&output.stdout)
        } else {
            anyhow::bail!(
                "git merge-tree preview failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    };
    conflict_files.extend(
        changed_files
            .iter()
            .filter(|path| local_paths.contains(*path))
            .cloned(),
    );
    conflict_files.sort();
    conflict_files.dedup();
    Ok(PullPreview {
        remote: remote.to_owned(),
        branch: branch.to_owned(),
        repo_path: repo_path.to_path_buf(),
        worktree_path: worktree_path.to_path_buf(),
        incoming_commits: behind,
        outgoing_commits: ahead,
        changed_files,
        conflict_files,
        has_local_changes: local_changes,
        local_changes: PullLocalChanges::default(),
    })
}

fn parse_merge_tree_conflict_paths(output: &[u8]) -> Vec<std::path::PathBuf> {
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let first = fields.next().unwrap_or_default();
    let first_is_tree_id =
        (first.len() == 40 || first.len() == 64) && first.iter().all(u8::is_ascii_hexdigit);
    let mut paths = Vec::new();
    if !first_is_tree_id && !first.is_empty() {
        paths.push(std::path::PathBuf::from(
            String::from_utf8_lossy(first).into_owned(),
        ));
    }
    for field in fields {
        let path = String::from_utf8_lossy(field);
        if path.starts_with("CONFLICT ") || path.starts_with("Auto-merging ") {
            continue;
        }
        paths.push(std::path::PathBuf::from(path.into_owned()));
    }
    paths.sort();
    paths.dedup();
    paths
}

fn pending_merge_autostash(repo: &mut Repository) -> Result<Option<git2::Oid>> {
    match repo.find_reference("MERGE_AUTOSTASH") {
        Ok(reference) => {
            if let Some(oid) = reference.target() {
                return Ok(Some(oid));
            }
        }
        Err(error) if error.code() == git2::ErrorCode::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut autostash = None;
    repo.stash_foreach(|_, message, oid| {
        if message.to_ascii_lowercase().contains("autostash") {
            autostash = Some(*oid);
            false
        } else {
            true
        }
    })?;
    Ok(autostash)
}

#[cfg(test)]
fn head_contains_fetched_commit(repo: &Repository) -> Result<(git2::Oid, git2::Oid, bool)> {
    let head = repo.head()?.peel_to_commit()?.id();
    let fetched = repo.revparse_single("FETCH_HEAD")?.id();
    let integrated = head == fetched || repo.graph_descendant_of(head, fetched)?;
    Ok((head, fetched, integrated))
}

fn head_contains_remote_tracking_commit(
    repo: &Repository,
    remote: &str,
    branch: &str,
) -> Result<(git2::Oid, git2::Oid, bool)> {
    let head = repo.head()?.peel_to_commit()?.id();
    let reference = format!("refs/remotes/{remote}/{branch}");
    let remote_tip = repo.find_reference(&reference)?.peel_to_commit()?.id();
    let integrated = head == remote_tip || repo.graph_descendant_of(head, remote_tip)?;
    Ok((head, remote_tip, integrated))
}

/// Attempt to recover a normal rejected push by incorporating the exact
/// destination ref. Local edits are temporarily stashed by Git while pulling,
/// then restored before the retry. Never force push or silently overwrite user
/// changes; Git reports a real merge or stash-apply conflict to the caller.
///
/// A merge (rather than rebase) preserves local commit IDs and matches the
/// GitHub Desktop-style "pull then push" behavior. Git can fail if a conflict
/// needs user attention; that failure is deliberately returned to the UI.
fn push_with_auto_pull(
    worktree: &Path,
    args: &[&str],
    remote: &str,
    remote_branch: &str,
    force: bool,
    auto_pull_on_rejection: bool,
    pulled: &mut bool,
) -> Result<Option<String>> {
    let operation_started = std::time::Instant::now();
    let first_started = std::time::Instant::now();
    let initial_push = run_git_network_command(worktree, args);
    log::info!(
        "push timing: initial push completed in {:?} result={} remote={} branch={}",
        first_started.elapsed(),
        if initial_push.is_ok() {
            "success"
        } else {
            "failed"
        },
        remote,
        remote_branch,
    );
    match initial_push {
        Ok(details) => Ok(details),
        Err(push_error)
            if !force
                && is_remote_ahead_rejection(&push_error.to_string())
                && !auto_pull_on_rejection =>
        {
            anyhow::bail!(
                "Remote {remote}/{remote_branch} has new commits. Automatic integration after a rejected push is disabled in Settings; fetch and pull the branch before retrying."
            )
        }
        Err(push_error) if !force && is_remote_ahead_rejection(&push_error.to_string()) => {
            log::info!(
                "Push rejected because remote {}/{} is ahead; trying automatic pull",
                remote,
                remote_branch
            );
            let check_started = std::time::Instant::now();
            let mut repo = open_worktree_repo(worktree)?;
            // A remote may use a different push URL from its fetch URL.
            // Pulling from that other repository would merge the wrong history.
            let push_url_differs = {
                let configured_remote = repo.find_remote(remote)?;
                configured_remote
                    .pushurl()
                    .ok()
                    .flatten()
                    .is_some_and(|url| Some(url) != configured_remote.url().ok())
            };
            if push_url_differs {
                anyhow::bail!(
                    "Remote {remote} has a different push URL and fetch URL. Automatic pull \
                     was skipped to avoid merging an unrelated branch."
                );
            }
            if let Some(autostash_oid) = pending_merge_autostash(&mut repo)? {
                anyhow::bail!(
                    "Remote {remote}/{remote_branch} has new commits, but automatic pull was skipped because Git has a pending MERGE_AUTOSTASH ({autostash_oid}). The saved local changes were preserved; inspect the worktree and stash before retrying."
                );
            }
            if repo.state() != RepositoryState::Clean {
                anyhow::bail!(
                    "Remote {remote}/{remote_branch} has new commits. Automatic pull was skipped \
                     because another Git operation is in progress; finish or abort it before pushing."
                );
            }
            let mut options = StatusOptions::new();
            options
                .include_untracked(false)
                .include_ignored(false)
                .include_unmodified(false);
            let has_local_changes = !repo.statuses(Some(&mut options))?.is_empty();
            log::info!(
                "push timing: automatic pull preflight completed in {:?} worktree={} autostash={}",
                check_started.elapsed(),
                worktree.display(),
                has_local_changes,
            );
            drop(repo);
            // Refresh the exact destination ref before previewing. No merge or
            // stash is started until the preview has been inspected.
            run_git_network_command(worktree, &["fetch", remote, remote_branch])?;
            *pulled = true;
            let preview_repo = open_worktree_repo(worktree)?;
            let preview =
                pull_preview_from_repo(&preview_repo, remote, remote_branch, worktree, worktree)?;
            if !preview.conflict_files.is_empty() {
                let paths = preview
                    .conflict_files
                    .iter()
                    .take(8)
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!(
                    "Automatic pull preview found potential conflicts in {} file(s): {}. No merge was started. Review the files and use Pull to continue manually.",
                    preview.conflict_files.len(),
                    paths
                );
            }
            // Set before pulling: a merge conflict may modify HEAD, the index
            // or the worktree even when the Git command returns an error.
            let pull_started = std::time::Instant::now();
            let pull_args = if has_local_changes {
                vec![
                    "pull",
                    "--autostash",
                    "--no-rebase",
                    "--no-edit",
                    remote,
                    remote_branch,
                ]
            } else {
                vec!["pull", "--no-rebase", "--no-edit", remote, remote_branch]
            };
            let pull_result = run_git_network_command(worktree, &pull_args);
            log::info!(
                "push timing: automatic pull completed in {:?} result={} remote={} branch={}",
                pull_started.elapsed(),
                if pull_result.is_ok() {
                    "success"
                } else {
                    "failed"
                },
                remote,
                remote_branch,
            );
            pull_result.map_err(|error| {
                anyhow::anyhow!(
                    "Automatic pull of {remote}/{remote_branch} failed: {error}. \
                     Resolve any conflicts or other pull errors, then push again."
                )
            })?;

            // Verify against the remote-tracking ref that the pull just
            // updated. FETCH_HEAD can contain multiple entries and can be
            // replaced by another fetch, so it is not a stable destination
            // identity for deciding whether the merge is complete.
            let repo = open_worktree_repo(worktree)?;
            let (head, fetched, integrated) =
                head_contains_remote_tracking_commit(&repo, remote, remote_branch).map_err(|error| {
                anyhow::anyhow!(
                    "Automatic pull of {remote}/{remote_branch} reported success, but rgitui could not verify that the fetched commit was merged into the checked-out branch: {error}. Push was not retried."
                )
            })?;
            log::info!(
                "push auto-pull ancestry: head={} fetch_head={} integrated={} remote={} branch={}",
                head,
                fetched,
                integrated,
                remote,
                remote_branch
            );
            if !integrated {
                anyhow::bail!(
                    "Automatic pull of {remote}/{remote_branch} reported success, but the checked-out branch does not contain fetched commit {fetched}. Push was not retried. Inspect the branch and resolve the merge before pushing."
                );
            }

            // Retry exactly once. A second rejection may mean a concurrent
            // remote update; never loop indefinitely or silently force.
            let retry_started = std::time::Instant::now();
            let retry_result = run_git_network_command(worktree, args);
            log::info!(
                "push timing: retry push completed in {:?} result={} remote={} branch={}",
                retry_started.elapsed(),
                if retry_result.is_ok() {
                    "success"
                } else {
                    "failed"
                },
                remote,
                remote_branch
            );
            let details = match retry_result {
                Ok(details) => details,
                Err(error) if is_remote_ahead_rejection(&error.to_string()) => {
                    // The remote advanced during the pull/retry window. Fetch
                    // once more and retry only when that new remote tip is
                    // already contained in our merge commit; otherwise a
                    // second automatic merge would be an unbounded mutation.
                    run_git_network_command(worktree, &["fetch", remote, remote_branch])?;
                    let repo = open_worktree_repo(worktree)?;
                    let (_, latest_remote, contained) =
                        head_contains_remote_tracking_commit(&repo, remote, remote_branch)?;
                    if !contained {
                        return Err(anyhow::anyhow!(
                            "Automatic pull of {remote}/{remote_branch} succeeded, but the remote advanced again to {latest_remote}. Resolve the new divergence and retry pushing."
                        ));
                    }
                    run_git_network_command(worktree, args).map_err(|retry_error| {
                        anyhow::anyhow!(
                            "Automatic pull of {remote}/{remote_branch} succeeded, but the final push retry failed: {retry_error}"
                        )
                    })?
                }
                Err(error) => {
                    let remote_advanced = is_remote_ahead_rejection(&error.to_string());
                    log::warn!(
                    "push retry failed after successful automatic pull: remote_ahead={} total_elapsed={:?} worktree={}",
                    remote_advanced,
                    operation_started.elapsed(),
                    worktree.display()
                );
                    if remote_advanced {
                        anyhow::bail!(
                            "Automatic pull of {remote}/{remote_branch} succeeded, but the retry push was rejected as non-fast-forward: {error}"
                        )
                    } else {
                        anyhow::bail!(
                            "Automatic pull of {remote}/{remote_branch} succeeded, but the retry push failed: {error}. Inspect the remote error and retry."
                        )
                    }
                }
            };
            log::info!(
                "push timing: auto-pull/retry operation completed in {:?} remote={} branch={}",
                operation_started.elapsed(),
                remote,
                remote_branch
            );
            let auto_pull_detail = if has_local_changes {
                format!(
                    "Automatically pulled {remote}/{remote_branch} and restored local changes before pushing."
                )
            } else {
                format!("Automatically pulled {remote}/{remote_branch} before pushing.")
            };
            Ok(Some(match details {
                Some(details) => format!("{auto_pull_detail}\n{details}"),
                None => auto_pull_detail,
            }))
        }
        Err(error) => Err(error),
    }
}

/// A successful "Already up to date" pull cannot change the checked-out
/// index/worktree when AutoStash is disabled. Conflicts and AutoStash always
/// require a fresh status scan.
fn pull_can_reuse_status_cache(
    auto_stash: bool,
    is_conflict: bool,
    already_up_to_date: bool,
) -> bool {
    !auto_stash && !is_conflict && already_up_to_date
}

/// Open the repository handle for a specific working tree. Network operations
/// resolve their branch and upstream from *this* checkout, which is what makes
/// pushing from an inspected worktree push that worktree's branch rather than
/// the branch the main checkout happens to be on.
fn open_worktree_repo(worktree_path: &Path) -> Result<Repository> {
    Repository::open(worktree_path)
        .with_context(|| format!("Failed to open worktree at {}", worktree_path.display()))
}

/// How a pull ended, when git got far enough to leave something to report.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PullOutcome {
    /// There was nothing to integrate.
    UpToDate,
    /// The remote branch was integrated.
    Updated,
    /// Git stopped part-way with a merge or rebase to resolve.
    Conflicts {
        count: usize,
        /// Git stashed uncommitted changes first and is holding them until the
        /// operation is continued or aborted.
        autostash_pending: bool,
    },
}

#[derive(Debug)]
struct PullReport {
    outcome: PullOutcome,
    /// What git printed.
    output: Option<String>,
    /// The pull itself went through, but reapplying the changes git had
    /// stashed for it conflicted. Git has put them in the stash list.
    autostash_conflicted: bool,
}

/// A pull git refused or could not carry out.
#[derive(Debug)]
struct PullFailure {
    error: anyhow::Error,
    /// See [`PullAttempt::blocked_by_local_changes`].
    blocked_by_local_changes: bool,
}

impl From<anyhow::Error> for PullFailure {
    fn from(error: anyhow::Error) -> Self {
        Self {
            error,
            blocked_by_local_changes: false,
        }
    }
}

impl From<git2::Error> for PullFailure {
    fn from(error: git2::Error) -> Self {
        anyhow::Error::from(error).into()
    }
}

/// What stood in the way when git refused to pull over uncommitted changes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalChangesBlock {
    /// An incoming commit touches these files, which have uncommitted changes.
    Files(Vec<String>),
    /// The pull rebases, and a rebase will not start with any tracked file
    /// changed, whichever files the incoming commits touch.
    Rebase,
}

/// Recognise git refusing a pull because of uncommitted changes to tracked
/// files, from its output (which [`super::git_command`] keeps in English).
///
/// Only that refusal counts. Git words an untracked file standing in the way
/// almost identically, but a stash does not take untracked files, so offering
/// autostash for it would promise a fix that does not work.
fn local_changes_block(output: &str) -> Option<LocalChangesBlock> {
    if output.contains("cannot pull with rebase") {
        return Some(LocalChangesBlock::Rebase);
    }

    let mut lines = output.lines().skip_while(|line| {
        !line.contains("Your local changes to the following files would be overwritten")
    });
    lines.next()?;
    // The files follow, indented, up to git's advice on what to do about them.
    let files = lines
        .take_while(|line| line.starts_with([' ', '\t']))
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    Some(LocalChangesBlock::Files(files))
}

impl LocalChangesBlock {
    fn message(&self) -> Message {
        match self {
            LocalChangesBlock::Files(files) if !files.is_empty() => msg!(
                "git.pull.blocked_files_detail",
                paths = format_path_list(files)
            ),
            LocalChangesBlock::Files(_) => msg!("git.pull.blocked_detail"),
            LocalChangesBlock::Rebase => msg!("git.pull.blocked_rebase_detail"),
        }
    }
}

/// Run `git pull` in `worktree_path` and work out what it left behind.
///
/// Whether uncommitted changes stop the pull is `local_changes`' call. Under
/// [`PullLocalChanges::GitDecides`] nothing is checked up front: git refuses on
/// its own, touching nothing, when an incoming commit would overwrite one, and
/// that refusal comes back as a [`PullFailure`] marked as blocked.
fn run_pull(
    worktree_path: &Path,
    remote_name: &str,
    branch_name: &str,
    local_changes: PullLocalChanges,
) -> Result<PullReport, PullFailure> {
    validate_remote_name(remote_name)?;
    validate_branch_name(branch_name)?;

    let state_before = {
        let repo = Repository::open(worktree_path)?;
        if local_changes == PullLocalChanges::RequireClean && repo_has_worktree_changes(&repo)? {
            return Err(PullFailure {
                error: error(msg!(
                    "git.error.clean_worktree_required",
                    operation = GitOperationKind::Pull.name()
                )),
                blocked_by_local_changes: true,
            });
        }
        repo.state()
    };

    let mut args = vec!["pull"];
    if local_changes == PullLocalChanges::Autostash {
        args.push("--autostash");
    }
    args.push(remote_name);
    args.push(branch_name);

    match run_git_network_command(worktree_path, &args) {
        Ok(output) => {
            let up_to_date = output
                .as_deref()
                .is_some_and(|text| text.contains("Already up to date."));
            // Git exits successfully with unmerged entries in exactly one case:
            // the pull went through and reapplying its autostash conflicted.
            let autostash_conflicted = Repository::open(worktree_path)?.index()?.has_conflicts();
            Ok(PullReport {
                outcome: if up_to_date {
                    PullOutcome::UpToDate
                } else {
                    PullOutcome::Updated
                },
                output,
                autostash_conflicted,
            })
        }
        Err(e) => {
            let text = e.to_string();
            let repo = Repository::open(worktree_path)?;
            // A pull that leaves a merge or rebase behind stopped part-way,
            // whatever it printed; one that was already in progress did not
            // come from this pull.
            let stopped = state_before == git2::RepositoryState::Clean
                && repo.state() != git2::RepositoryState::Clean;
            if stopped || text.contains("CONFLICT") || text.contains("Automatic merge failed") {
                Ok(PullReport {
                    outcome: PullOutcome::Conflicts {
                        count: repo.index()?.conflicts()?.count(),
                        autostash_pending: pending_autostash(&repo).is_some(),
                    },
                    output: Some(text),
                    autostash_conflicted: false,
                })
            } else if let Some(block) = local_changes_block(&text) {
                Err(PullFailure {
                    error: error(block.message()),
                    blocked_by_local_changes: true,
                })
            } else {
                Err(e.into())
            }
        }
    }
}

impl GitProject {
    /// Apply the post-network snapshot without keeping the user-facing Git
    /// operation in its running state. Keep the watcher gated until refresh
    /// completes so it cannot start a competing scan of the same checkout.
    fn refresh_after_network_operation(&mut self, cx: &mut Context<Self>) {
        let caches = self.refresh_caches.clone();
        caches.begin_operation();
        let refresh = self.refresh(cx);
        cx.spawn(async move |_this: WeakEntity<Self>, _cx: &mut AsyncApp| {
            let result = refresh.await;
            caches.finish_operation();
            if let Err(error) = result {
                log::error!("Post-network repository refresh failed: {}", error);
            }
        })
        .detach();
    }

    pub fn fetch_default_at(
        &mut self,
        worktree_path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        log::info!("fetch_default: worktree={}", worktree_path.display());
        let remote_name = match self.preferred_remote_name_at(worktree_path) {
            Ok(remote_name) => remote_name,
            Err(error) => {
                return self.fail_to_start_task(
                    GitOperationKind::Fetch,
                    msg!("git.fetch.not_started"),
                    error,
                    true,
                    worktree_path,
                    cx,
                )
            }
        };
        self.fetch_at(&remote_name, worktree_path, cx)
    }

    /// Pull the branch `worktree_path` has checked out from its upstream.
    /// `local_changes` says what to do about uncommitted changes.
    pub fn pull_default_at(
        &mut self,
        local_changes: PullLocalChanges,
        worktree_path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        log::info!(
            "pull_default: local_changes={:?} worktree={}",
            local_changes,
            worktree_path.display()
        );
        let (remote_name, branch_name) =
            match open_worktree_repo(worktree_path).and_then(|repo| pull_target(&repo, None)) {
                Ok(target) => target,
                Err(error) => {
                    return self.fail_to_start_task(
                        GitOperationKind::Pull,
                        msg!("git.pull.not_started"),
                        error,
                        true,
                        worktree_path,
                        cx,
                    )
                }
            };
        self.pull_from_at(&remote_name, &branch_name, local_changes, worktree_path, cx)
    }

    pub fn push_default_at(
        &mut self,
        force: bool,
        worktree_path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        log::info!(
            "push_default: force={} worktree={}",
            force,
            worktree_path.display()
        );
        let (remote_name, branch_name, set_upstream) =
            match open_worktree_repo(worktree_path).and_then(|repo| push_target(&repo, None)) {
                Ok(target) => target,
                Err(error) => {
                    return self.fail_to_start_task(
                        GitOperationKind::push(force),
                        msg!("git.push.not_started"),
                        error,
                        true,
                        worktree_path,
                        cx,
                    )
                }
            };
        self.push_to_at(
            &remote_name,
            &branch_name,
            force,
            set_upstream,
            worktree_path,
            cx,
        )
    }

    /// Fetch from a remote, running git in the given worktree.
    pub fn fetch_at(
        &mut self,
        remote_name: &str,
        worktree_path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        log::info!(
            "fetch: remote={} worktree={}",
            remote_name,
            worktree_path.display()
        );
        let remote_name = remote_name.to_string();
        let task_remote_name = remote_name.clone();
        let task_worktree_path = worktree_path.to_path_buf();
        let repo_path = self.repo_path.clone();
        let commit_limit = super::refresh::effective_refresh_commit_limit(
            self.commit_limit,
            self.loaded_commit_count(),
        );
        let operation_id = self.begin_operation(
            GitOperationKind::Fetch,
            msg!("git.fetch.running", remote = &remote_name),
            Some(remote_name.clone()),
            self.head_branch_at(worktree_path),
            cx,
        );
        self.note_operation_worktree(operation_id, worktree_path);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result: anyhow::Result<(Option<String>, RefreshData)> = cx
                .background_executor()
                .spawn(async move {
                    validate_remote_name(&task_remote_name)?;
                    let details = with_network_lock(&task_worktree_path, || {
                        run_git_network_command(
                            &task_worktree_path,
                            &["fetch", "--prune", &task_remote_name],
                        )
                    })?;
                    let data = gather_refresh_data(&repo_path, commit_limit)?;
                    anyhow::Ok((details, data))
                })
                .await;

            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    match result {
                        Ok((details, data)) => {
                            this.apply_refresh_data(data);
                            this.complete_op(
                                operation_id,
                                GitOperationKind::Fetch,
                                msg!("git.fetch.done", remote = &remote_name),
                                (
                                    details
                                        .map(Message::verbatim)
                                        .or(Some(msg!("git.fetch.detail"))),
                                    Some(remote_name.clone()),
                                    this.head_branch.clone(),
                                ),
                                cx,
                            );
                            cx.emit(GitProjectEvent::RefsChanged);
                        }
                        Err(e) => {
                            log::error!("Fetch failed: {}", e);
                            this.fail_op(
                                operation_id,
                                GitOperationKind::Fetch,
                                msg!("git.fetch.failed", remote = &remote_name),
                                error_message(&e),
                                (Some(remote_name.clone()), this.head_branch.clone(), true),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                    Ok(())
                })
            })?
        })
    }

    /// Pull into the given worktree, merging into the branch *that* worktree
    /// has checked out.
    pub fn pull_at(
        &mut self,
        remote_name: &str,
        local_changes: PullLocalChanges,
        worktree_path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        log::info!(
            "pull: remote={} local_changes={:?} worktree={}",
            remote_name,
            local_changes,
            worktree_path.display()
        );
        let branch_name = match open_worktree_repo(worktree_path)
            .and_then(|repo| pull_target(&repo, Some(remote_name)).map(|(_, branch)| branch))
        {
            Ok(branch_name) => branch_name,
            Err(error) => {
                return self.fail_to_start_task(
                    GitOperationKind::Pull,
                    msg!("git.pull.not_started"),
                    error,
                    true,
                    worktree_path,
                    cx,
                )
            }
        };
        self.pull_from_at(remote_name, &branch_name, local_changes, worktree_path, cx)
    }

    /// Fetch the pull target and calculate its effect without changing HEAD,
    /// the index, or working-tree files. The fetched remote-tracking ref is
    /// intentionally updated so a subsequent Pull uses the exact previewed
    /// commit rather than a stale advertisement.
    pub fn preview_pull_at(
        &mut self,
        worktree_path: &Path,
        local_changes: PullLocalChanges,
        cx: &mut Context<Self>,
    ) -> Task<Result<PullPreview>> {
        let (remote_name, branch_name) =
            match open_worktree_repo(worktree_path).and_then(|repo| pull_target(&repo, None)) {
                Ok(target) => target,
                Err(error) => return Task::ready(Err(error)),
            };
        let remote_name = remote_name.to_string();
        let branch_name = branch_name.to_string();
        let worktree_path = worktree_path.to_path_buf();
        let repo_path = self.repo_path.clone();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            cx.background_executor()
                .spawn(async move {
                    validate_remote_name(&remote_name)?;
                    validate_branch_name(&branch_name)?;
                    with_network_lock(&worktree_path, || {
                        run_git_network_command(
                            &worktree_path,
                            &["fetch", "--prune", &remote_name, &branch_name],
                        )
                    })?;
                    let repo = open_worktree_repo(&worktree_path)?;
                    let mut preview = pull_preview_from_repo(
                        &repo,
                        &remote_name,
                        &branch_name,
                        &repo_path,
                        &worktree_path,
                    )?;
                    preview.local_changes = local_changes;
                    Ok(preview)
                })
                .await
        })
    }

    pub fn pull_from_at(
        &mut self,
        remote_name: &str,
        branch_name: &str,
        local_changes: PullLocalChanges,
        worktree_path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        log::info!(
            "pull_from: remote={} branch={} local_changes={:?} worktree={}",
            remote_name,
            branch_name,
            local_changes,
            worktree_path.display()
        );
        let remote_name = remote_name.to_string();
        let branch_name = branch_name.to_string();
        let task_remote_name = remote_name.clone();
        let task_branch_name = branch_name.clone();
        let task_worktree_path = worktree_path.to_path_buf();
        let refresh_caches = self.refresh_caches.clone();
        let operation_id = self.begin_operation(
            GitOperationKind::Pull,
            if local_changes == PullLocalChanges::Autostash {
                msg!(
                    "git.pull.autostash_running",
                    branch = &branch_name,
                    remote = &remote_name
                )
            } else {
                msg!(
                    "git.pull.running",
                    branch = &branch_name,
                    remote = &remote_name
                )
            },
            Some(remote_name.clone()),
            Some(branch_name.clone()),
            cx,
        );
        self.note_operation_worktree(operation_id, worktree_path);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result: Result<PullReport, PullFailure> = cx
                .background_executor()
                .spawn(async move {
                    let git_started = std::time::Instant::now();
                    let report = with_network_lock(&task_worktree_path, || {
                        Ok(run_pull(
                            &task_worktree_path,
                            &task_remote_name,
                            &task_branch_name,
                            local_changes,
                        ))
                    })??;
                    log::info!(
                        "pull timing: git command completed in {:?} (remote={} branch={})",
                        git_started.elapsed(),
                        task_remote_name,
                        task_branch_name
                    );
                    // A successful no-op pull without AutoStash cannot alter the
                    // checked-out files or index. Preserve the last validated
                    // status snapshot in this narrow case; anything else may have
                    // rewritten them without the watcher delivering every event.
                    let is_conflict = matches!(report.outcome, PullOutcome::Conflicts { .. })
                        || report.autostash_conflicted;
                    if pull_can_reuse_status_cache(
                        local_changes == PullLocalChanges::Autostash,
                        is_conflict,
                        report.outcome == PullOutcome::UpToDate,
                    ) {
                        log::info!(
                            "pull status cache preserved: already up to date and autostash disabled"
                        );
                    } else {
                        refresh_caches.mark_worktree_status_dirty(&task_worktree_path);
                    }
                    Ok(report)
                })
                .await;

            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    let finish = |this: &Self,
                                  state: GitOperationState,
                                  summary: Message,
                                  details: Message,
                                  (retryable, blocked_by_local_changes): (bool, bool),
                                  failure_kind: Option<GitOperationFailureKind>,
                                  cx: &mut Context<Self>| {
                        this.refresh_caches.finish_operation();
                        cx.emit(GitProjectEvent::OperationUpdated(GitOperationUpdate {
                            id: operation_id,
                            kind: GitOperationKind::Pull,
                            state,
                            summary,
                            details: Some(details),
                            remote_name: Some(remote_name.clone()),
                            branch_name: Some(branch_name.clone()),
                            worktree_path: this.operation_worktree(operation_id),
                            retryable,
                            failure_kind,
                            pull: Some(PullAttempt {
                                local_changes,
                                blocked_by_local_changes,
                            }),
                        }));
                    };

                    // Refs may have moved even when git refused the merge, so
                    // reconcile in the background with the watcher kept gated.
                    this.refresh_after_network_operation(cx);
                    match result {
                        Ok(report) => {
                            let output = report.output.map(Message::verbatim);
                            match report.outcome {
                                PullOutcome::Conflicts {
                                    count,
                                    autostash_pending,
                                } => {
                                    let details = if autostash_pending {
                                        msgp!(
                                            "git.pull.conflicts_autostash_detail",
                                            count,
                                            remote = &remote_name,
                                            branch = &branch_name
                                        )
                                    } else {
                                        msgp!(
                                            "git.pull.conflicts_detail",
                                            count,
                                            remote = &remote_name,
                                            branch = &branch_name
                                        )
                                    };
                                    finish(
                                        this,
                                        GitOperationState::Failed,
                                        msg!("git.pull.conflict", remote = &remote_name),
                                        details,
                                        (false, false),
                                        Some(GitOperationFailureKind::MergeConflict),
                                        cx,
                                    );
                                }
                                PullOutcome::UpToDate => finish(
                                    this,
                                    GitOperationState::Succeeded,
                                    msg!(
                                        "git.pull.up_to_date",
                                        branch = &branch_name,
                                        remote = &remote_name
                                    ),
                                    output.unwrap_or(msg!("git.pull.detail")),
                                    (false, false),
                                    None,
                                    cx,
                                ),
                                PullOutcome::Updated => finish(
                                    this,
                                    GitOperationState::Succeeded,
                                    msg!(
                                        "git.pull.done",
                                        branch = &branch_name,
                                        remote = &remote_name
                                    ),
                                    output.unwrap_or(msg!("git.pull.detail")),
                                    (false, false),
                                    None,
                                    cx,
                                ),
                            }
                            if report.autostash_conflicted {
                                this.report_autostash_restore(
                                    AutostashRestore::Conflicted,
                                    Some(branch_name.clone()),
                                    cx,
                                );
                            }
                            cx.emit(GitProjectEvent::RepositoryChanged);
                        }
                        Err(failure) => {
                            log::error!("Pull failed: {}", failure.error);
                            finish(
                                this,
                                GitOperationState::Failed,
                                if failure.blocked_by_local_changes {
                                    msg!("git.pull.blocked", remote = &remote_name)
                                } else {
                                    msg!("git.pull.failed", remote = &remote_name)
                                },
                                error_message(&failure.error),
                                (
                                    !failure.blocked_by_local_changes,
                                    failure.blocked_by_local_changes,
                                ),
                                Some(if failure.blocked_by_local_changes {
                                    GitOperationFailureKind::LocalChangesBlocked
                                } else {
                                    GitOperationFailureKind::OperationInProgress
                                }),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                    Ok(())
                })
            })?
        })
    }

    /// Push the branch checked out in the given worktree.
    pub fn push_at(
        &mut self,
        remote_name: &str,
        force: bool,
        worktree_path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        log::info!(
            "push: remote={} force={} worktree={}",
            remote_name,
            force,
            worktree_path.display()
        );
        let (branch_name, set_upstream) = match open_worktree_repo(worktree_path).and_then(|repo| {
            push_target(&repo, Some(remote_name)).map(|(_, branch, set)| (branch, set))
        }) {
            Ok(target) => target,
            Err(error) => {
                return self.fail_to_start_task(
                    GitOperationKind::push(force),
                    msg!("git.push.not_started"),
                    error,
                    true,
                    worktree_path,
                    cx,
                )
            }
        };
        self.push_to_at(
            remote_name,
            &branch_name,
            force,
            set_upstream,
            worktree_path,
            cx,
        )
    }

    pub fn push_to_at(
        &mut self,
        remote_name: &str,
        remote_branch_name: &str,
        force: bool,
        set_upstream: bool,
        worktree_path: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        log::info!(
            "push_to: remote={} branch={} force={} worktree={}",
            remote_name,
            remote_branch_name,
            force,
            worktree_path.display()
        );
        let remote_name = remote_name.to_string();
        let remote_branch_name = remote_branch_name.to_string();
        let task_remote_name = remote_name.clone();
        let task_remote_branch_name = remote_branch_name.clone();
        let task_worktree_path = worktree_path.to_path_buf();
        let refresh_caches = self.refresh_caches.clone();
        let auto_pull_on_rejection = cx
            .try_global::<SettingsState>()
            .map(|settings| settings.settings().auto_pull_on_push_rejection)
            .unwrap_or(true);
        let kind = GitOperationKind::push(force);
        let operation_id = self.begin_operation(
            kind,
            if force {
                msg!(
                    "git.force_push.running",
                    remote = &remote_name,
                    branch = &remote_branch_name
                )
            } else {
                msg!(
                    "git.push.running",
                    remote = &remote_name,
                    branch = &remote_branch_name
                )
            },
            Some(remote_name.clone()),
            Some(remote_branch_name.clone()),
            cx,
        );
        self.note_operation_worktree(operation_id, worktree_path);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let (result, pulled) = cx
                .background_executor()
                .spawn(async move {
                    let mut pulled = false;
                    let result: anyhow::Result<(Message, Option<String>)> = (|| {
                        let repo = Repository::open(&task_worktree_path)?;
                        let branch_name = head_branch_name(&repo)?;
                        drop(repo);

                        validate_remote_name(&task_remote_name)?;
                        validate_branch_name(&task_remote_branch_name)?;

                        let mut args = vec!["push"];
                        if force {
                            args.push("--force");
                        }
                        if set_upstream {
                            args.push("--set-upstream");
                        }
                        args.push(task_remote_name.as_str());
                        let refspec = format!("HEAD:{}", task_remote_branch_name);
                        args.push(refspec.as_str());
                        let details = with_network_lock(&task_worktree_path, || {
                            push_with_auto_pull(
                                &task_worktree_path,
                                &args,
                                &task_remote_name,
                                &task_remote_branch_name,
                                force,
                                auto_pull_on_rejection,
                                &mut pulled,
                            )
                        })?;
                        let msg = if force {
                            msg!(
                                "git.force_push.done",
                                branch = &branch_name,
                                remote = &task_remote_name,
                                remote_branch = &task_remote_branch_name
                            )
                        } else if set_upstream {
                            msg!(
                                "git.push.done_set_upstream",
                                branch = &branch_name,
                                remote = &task_remote_name,
                                remote_branch = &task_remote_branch_name
                            )
                        } else {
                            msg!(
                                "git.push.done",
                                branch = &branch_name,
                                remote = &task_remote_name,
                                remote_branch = &task_remote_branch_name
                            )
                        };
                        if pulled {
                            refresh_caches.mark_worktree_status_dirty(&task_worktree_path);
                        }
                        log::info!(
                            "push timing: network operation completed; refresh deferred auto_pulled={}",
                            pulled
                        );
                        Ok((msg, details))
                    })();

                    // A failed automatic merge may have changed the working
                    // tree and index. Refresh after reporting the failure so
                    // the conflict state remains visible without delaying it.
                    (result, pulled)
                })
                .await;

            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    match result {
                        Ok((msg, details)) => {
                            this.refresh_after_network_operation(cx);
                            this.complete_op(
                                operation_id,
                                kind,
                                msg,
                                (
                                    details
                                        .map(Message::verbatim)
                                        .or(Some(msg!("git.push.detail"))),
                                    Some(remote_name.clone()),
                                    Some(remote_branch_name.clone()),
                                ),
                                cx,
                            );
                            cx.emit(GitProjectEvent::RefsChanged);
                        }
                        Err(e) => {
                            if pulled {
                                this.refresh_after_network_operation(cx);
                            }
                            log::error!("Push failed: {}", e);
                            let error_text = e.to_string();
                            let failure_kind = if error_text.contains("pending MERGE_AUTOSTASH") {
                                Some(GitOperationFailureKind::PendingAutostash)
                            } else if error_text.contains("Automatic pull preview found potential conflicts")
                                || error_text.contains("CONFLICT")
                                || error_text.contains("Automatic merge failed")
                            {
                                Some(GitOperationFailureKind::MergeConflict)
                            } else if error_text.contains("Automatic pull of")
                                && error_text.contains("autostash")
                            {
                                Some(GitOperationFailureKind::AutostashConflict)
                            } else if is_remote_ahead_rejection(&error_text)
                                || error_text.contains("Automatic integration after a rejected push is disabled")
                            {
                                Some(GitOperationFailureKind::ConcurrentRemoteUpdate)
                            } else {
                                None
                            };
                            this.fail_op_with_kind(
                                operation_id,
                                kind,
                                if force {
                                    msg!("git.force_push.failed", remote = &remote_name)
                                } else {
                                    msg!("git.push.failed", remote = &remote_name)
                                },
                                error_message(&e),
                                (
                                    Some(remote_name.clone()),
                                    Some(remote_branch_name.clone()),
                                    true,
                                ),
                                failure_kind,
                                cx,
                            );
                        }
                    }
                    cx.notify();
                    Ok(())
                })
            })?
        })
    }
}

#[cfg(test)]
mod push_auto_pull_tests {
    use super::{
        head_contains_fetched_commit, is_remote_ahead_rejection, pending_merge_autostash,
        pull_preview_from_repo, push_with_auto_pull,
    };
    use git2::{Repository, RepositoryState};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn git(path: &Path, args: &[&str]) -> String {
        let output = super::super::git_command()
            .current_dir(path)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_SSH_COMMAND")
            .output()
            .expect("git executable is required by rgitui");
        assert!(
            output.status.success(),
            "git {} failed:\n{}\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn commit(path: &Path, filename: &str, content: &str, message: &str) {
        fs::write(path.join(filename), content).unwrap();
        git(path, &["add", "--", filename]);
        git(path, &["commit", "-m", message]);
    }

    struct TestRepos {
        _dir: TempDir,
        remote: PathBuf,
        local: PathBuf,
        peer: PathBuf,
    }

    impl TestRepos {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let remote = dir.path().join("origin.git");
            let local = dir.path().join("local");
            let peer = dir.path().join("peer");
            git(
                dir.path(),
                &[
                    "init",
                    "--bare",
                    "--initial-branch=dev",
                    remote.to_str().unwrap(),
                ],
            );
            git(
                dir.path(),
                &["init", "--initial-branch=dev", local.to_str().unwrap()],
            );
            git(&local, &["config", "user.name", "Local Tester"]);
            git(&local, &["config", "user.email", "local@example.test"]);
            commit(&local, "base.txt", "base\n", "base");
            let remote_url = format!("file:///{}", remote.to_string_lossy().replace('\\', "/"));
            git(&local, &["remote", "add", "origin", &remote_url]);
            git(&local, &["push", "-u", "origin", "dev"]);
            git(
                dir.path(),
                &[
                    "clone",
                    "--branch",
                    "dev",
                    remote.to_str().unwrap(),
                    peer.to_str().unwrap(),
                ],
            );
            git(&peer, &["config", "user.name", "Peer Tester"]);
            git(&peer, &["config", "user.email", "peer@example.test"]);
            Self {
                _dir: dir,
                remote,
                local,
                peer,
            }
        }

        fn push_peer(&self, filename: &str, content: &str) {
            commit(&self.peer, filename, content, "peer change");
            git(&self.peer, &["push", "origin", "dev"]);
        }

        fn push_with_recovery(&self, pulled: &mut bool) -> anyhow::Result<Option<String>> {
            push_with_auto_pull(
                &self.local,
                &["push", "origin", "HEAD:dev"],
                "origin",
                "dev",
                false,
                true,
                pulled,
            )
        }

        fn remote_head(&self) -> String {
            git(&self.remote, &["rev-parse", "refs/heads/dev"])
        }
    }

    #[test]
    fn only_remote_ahead_rejections_are_auto_recoverable() {
        assert!(is_remote_ahead_rejection(
            " ! [rejected] HEAD -> dev (fetch first)\nerror: failed to push some refs"
        ));
        assert!(is_remote_ahead_rejection(
            " ! [rejected] HEAD -> dev (non-fast-forward)"
        ));
        assert!(is_remote_ahead_rejection(
            " ! [remote rejected] HEAD -> dev (failed to update ref)\n\
             remote: error: cannot lock ref 'refs/heads/dev': is at \
             831c266225783dba1a679861c18e06e936ee1013 but expected \
             9f8b0d5602a5396e68665353d090b7678d8edfcf"
        ));
        assert!(!is_remote_ahead_rejection(
            " ! [remote rejected] HEAD -> dev (pre-receive hook declined)"
        ));
        assert!(!is_remote_ahead_rejection("Permission denied (publickey)"));
        assert!(!is_remote_ahead_rejection(
            " ! [rejected] HEAD -> dev (stale info)"
        ));
    }

    #[test]
    fn successful_pull_is_not_retried_unless_head_contains_fetch_head() {
        let repos = TestRepos::new();
        repos.push_peer("peer.txt", "remote update\n");
        git(&repos.local, &["fetch", "origin", "dev"]);

        let repo = Repository::open(&repos.local).unwrap();
        let (head, fetched, integrated) = head_contains_fetched_commit(&repo).unwrap();
        assert_ne!(head, fetched);
        assert!(!integrated);
    }

    #[test]
    fn pending_merge_autostash_blocks_another_automatic_pull_without_mutation() {
        let repos = TestRepos::new();
        repos.push_peer("peer.txt", "remote update\n");
        fs::write(repos.local.join("base.txt"), "local edit\n").unwrap();
        let local_head = git(&repos.local, &["rev-parse", "HEAD"]);
        let mut pulled = false;
        let mut repo = Repository::open(&repos.local).unwrap();
        let oid = repo.head().unwrap().target().unwrap();
        repo.reference("MERGE_AUTOSTASH", oid, true, "simulate stranded autostash")
            .unwrap();
        assert_eq!(pending_merge_autostash(&mut repo).unwrap(), Some(oid));
        drop(repo);

        let error = repos.push_with_recovery(&mut pulled).unwrap_err();

        assert!(error.to_string().contains("pending MERGE_AUTOSTASH"));
        assert!(!pulled);
        assert_eq!(git(&repos.local, &["rev-parse", "HEAD"]), local_head);
        assert_eq!(
            fs::read_to_string(repos.local.join("base.txt")).unwrap(),
            "local edit\n"
        );
        assert_eq!(
            repos.remote_head(),
            git(&repos.peer, &["rev-parse", "HEAD"])
        );
        assert_eq!(
            Repository::open(&repos.local).unwrap().state(),
            RepositoryState::Clean
        );
    }

    #[test]
    fn stranded_autostash_in_stash_list_blocks_automatic_pull() {
        let repos = TestRepos::new();
        repos.push_peer("peer.txt", "remote update\n");
        fs::write(repos.local.join("base.txt"), "local edit\n").unwrap();
        git(&repos.local, &["stash", "push", "--message=autostash"]);
        let mut repo = Repository::open(&repos.local).unwrap();
        assert!(pending_merge_autostash(&mut repo).unwrap().is_some());
        drop(repo);

        let mut pulled = false;
        let error = repos.push_with_recovery(&mut pulled).unwrap_err();

        assert!(error.to_string().contains("pending MERGE_AUTOSTASH"));
        assert!(!pulled);
        assert!(git(&repos.local, &["stash", "list"]).contains("autostash"));
    }

    #[test]
    fn rejected_divergent_push_pulls_and_retries_without_rewriting_commits() {
        let repos = TestRepos::new();
        repos.push_peer("peer.txt", "new remote file\n");
        commit(&repos.local, "mine.txt", "new local file\n", "local change");
        let original_local_head = git(&repos.local, &["rev-parse", "HEAD"]);
        let mut pulled = false;
        let result = repos.push_with_recovery(&mut pulled).unwrap();
        assert!(pulled);
        assert!(result.unwrap().contains("Automatically pulled origin/dev"));
        assert_eq!(
            git(&repos.local, &["rev-parse", "HEAD"]),
            repos.remote_head()
        );
        let parents = git(&repos.local, &["show", "-s", "--format=%P", "HEAD"]);
        assert_eq!(parents.split_whitespace().count(), 2);
        assert!(parents
            .split_whitespace()
            .any(|sha| sha == original_local_head));
        assert!(repos.local.join("peer.txt").exists());
        assert!(repos.local.join("mine.txt").exists());
    }

    #[test]
    fn rejected_push_fast_forwards_when_local_has_no_new_commits() {
        let repos = TestRepos::new();
        repos.push_peer("peer.txt", "remote update\n");
        let mut pulled = false;
        repos.push_with_recovery(&mut pulled).unwrap();
        assert!(pulled);
        assert_eq!(
            git(&repos.local, &["rev-parse", "HEAD"]),
            repos.remote_head()
        );
        let parents = git(&repos.local, &["show", "-s", "--format=%P", "HEAD"]);
        assert_eq!(parents.split_whitespace().count(), 1);
    }

    #[test]
    fn uncommitted_worktree_is_stashed_while_automatic_pull_merges() {
        let repos = TestRepos::new();
        repos.push_peer("peer.txt", "remote update\n");
        fs::write(repos.local.join("base.txt"), "unsaved local work\n").unwrap();
        let previous_head = git(&repos.local, &["rev-parse", "HEAD"]);
        let mut pulled = false;
        let result = repos.push_with_recovery(&mut pulled).unwrap();
        assert!(pulled);
        assert!(result.unwrap().contains("restored local changes"));
        assert_eq!(
            git(&repos.local, &["rev-parse", "HEAD"]),
            repos.remote_head()
        );
        assert_ne!(git(&repos.local, &["rev-parse", "HEAD"]), previous_head);
        assert!(repos.local.join("peer.txt").exists());
        assert_eq!(
            fs::read_to_string(repos.local.join("base.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "unsaved local work\n"
        );
    }

    #[test]
    fn merge_conflict_is_previewed_without_mutating_the_worktree() {
        let repos = TestRepos::new();
        repos.push_peer("base.txt", "remote text\n");
        commit(&repos.local, "base.txt", "local text\n", "local edit");
        let local_head = git(&repos.local, &["rev-parse", "HEAD"]);
        let remote_head = repos.remote_head();
        let mut pulled = false;
        let error = repos.push_with_recovery(&mut pulled).unwrap_err();
        assert!(error
            .to_string()
            .contains("preview found potential conflicts"));
        assert!(error.to_string().contains("base.txt"));
        assert!(pulled);
        assert_eq!(repos.remote_head(), remote_head);
        assert_eq!(
            Repository::open(&repos.local).unwrap().state(),
            RepositoryState::Clean
        );
        assert_eq!(git(&repos.local, &["rev-parse", "HEAD"]), local_head);
        assert_eq!(
            fs::read_to_string(repos.local.join("base.txt")).unwrap(),
            "local text\n"
        );
        assert_eq!(
            Repository::open(&repos.local)
                .unwrap()
                .index()
                .unwrap()
                .conflicts()
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn pull_preview_counts_commits_and_detects_file_conflicts_without_checkout_changes() {
        let repos = TestRepos::new();
        repos.push_peer("base.txt", "remote text\n");
        commit(&repos.local, "base.txt", "local text\n", "local edit");
        git(&repos.local, &["fetch", "origin", "dev"]);
        let head = git(&repos.local, &["rev-parse", "HEAD"]);
        let contents = fs::read_to_string(repos.local.join("base.txt")).unwrap();
        let repo = Repository::open(&repos.local).unwrap();

        let preview =
            pull_preview_from_repo(&repo, "origin", "dev", &repos.local, &repos.local).unwrap();

        assert_eq!(preview.incoming_commits, 1);
        assert_eq!(preview.outgoing_commits, 1);
        assert_eq!(preview.conflict_files, vec![PathBuf::from("base.txt")]);
        assert_eq!(git(&repos.local, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            fs::read_to_string(repos.local.join("base.txt")).unwrap(),
            contents
        );
        assert_eq!(
            Repository::open(&repos.local).unwrap().state(),
            RepositoryState::Clean
        );
    }

    #[test]
    fn automatic_pull_can_be_disabled_without_fetching_or_mutating() {
        let repos = TestRepos::new();
        repos.push_peer("peer.txt", "remote update\n");
        let local_head = git(&repos.local, &["rev-parse", "HEAD"]);
        let mut pulled = false;
        let error = push_with_auto_pull(
            &repos.local,
            &["push", "origin", "HEAD:dev"],
            "origin",
            "dev",
            false,
            false,
            &mut pulled,
        )
        .unwrap_err();

        assert!(error.to_string().contains("disabled in Settings"));
        assert!(!pulled);
        assert_eq!(git(&repos.local, &["rev-parse", "HEAD"]), local_head);
        assert_eq!(
            Repository::open(&repos.local).unwrap().state(),
            RepositoryState::Clean
        );
    }
}

#[cfg(test)]
mod pull_status_cache_tests {
    use super::pull_can_reuse_status_cache;

    #[test]
    fn only_noop_pull_without_autostash_or_conflict_reuses_status_cache() {
        assert!(pull_can_reuse_status_cache(false, false, true));
        assert!(!pull_can_reuse_status_cache(true, false, true));
        assert!(!pull_can_reuse_status_cache(false, true, true));
        assert!(!pull_can_reuse_status_cache(false, false, false));
    }
}

#[cfg(test)]
mod pull_preview_tests {
    use super::parse_merge_tree_conflict_paths;
    use std::path::PathBuf;

    #[test]
    fn parses_nul_delimited_conflict_paths_after_tree_id() {
        let paths = parse_merge_tree_conflict_paths(
            b"0123456789012345678901234567890123456789\0src/a.rs\0nested/b.txt\0",
        );
        assert_eq!(
            paths,
            vec![PathBuf::from("nested/b.txt"), PathBuf::from("src/a.rs")]
        );
    }

    #[test]
    fn drops_merge_tree_diagnostic_fields() {
        let paths = parse_merge_tree_conflict_paths(
            b"0123456789012345678901234567890123456789\0CONFLICT (content): file.txt\0file.txt\0",
        );
        assert_eq!(paths, vec![PathBuf::from("file.txt")]);
    }
}

#[cfg(test)]
mod tests {
    use super::super::local_ops::{abort_in_progress_operation, continue_in_progress_operation};
    use super::*;
    use rgitui_test_support::TempRepo;

    const BASE: &str = "one\ntwo\nthree\nfour\nfive\n";

    fn git(repo: &TempRepo, args: &[&str]) -> String {
        let output = super::super::git_command()
            .current_dir(repo.path())
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn read(repo: &TempRepo, file: &str) -> String {
        std::fs::read_to_string(repo.path().join(file)).expect("file should exist")
    }

    fn state(repo: &TempRepo) -> git2::RepositoryState {
        Repository::open(repo.path()).unwrap().state()
    }

    /// A repository to pull from, and a checkout of it that is one pull behind
    /// nothing yet: both hold `shared.txt` and `mine.txt` at the same commit.
    ///
    /// The checkout pins every setting a pull consults, so the machine's own
    /// git config — a global `pull.rebase` or `merge.autoStash` — cannot change
    /// what these tests exercise.
    fn remote_and_checkout(rebase: bool) -> (TempRepo, TempRepo) {
        let remote = TempRepo::init();
        remote.commit_file("shared.txt", BASE, "add shared");
        remote.commit_file("mine.txt", "mine\n", "add mine");

        let checkout = TempRepo::init();
        let url = remote.path().to_string_lossy().to_string();
        git(&checkout, &["remote", "add", "origin", &url]);
        for (key, value) in [
            ("pull.rebase", if rebase { "true" } else { "false" }),
            ("pull.ff", "true"),
            ("merge.autoStash", "false"),
            ("rebase.autoStash", "false"),
            ("commit.gpgSign", "false"),
        ] {
            git(&checkout, &["config", key, value]);
        }
        git(&checkout, &["pull", "origin", "main"]);
        (remote, checkout)
    }

    fn pull(
        checkout: &TempRepo,
        local_changes: PullLocalChanges,
    ) -> Result<PullReport, PullFailure> {
        run_pull(checkout.path(), "origin", "main", local_changes)
    }

    /// The remote and the checkout each commit a different first line of
    /// `shared.txt`, so pulling stops on a conflict; `mine.txt` is edited and
    /// left uncommitted.
    fn diverged_with_uncommitted_change(rebase: bool) -> (TempRepo, TempRepo) {
        let (remote, checkout) = remote_and_checkout(rebase);
        remote.commit_file("shared.txt", &BASE.replace("one", "theirs"), "their change");
        checkout.commit_file("shared.txt", &BASE.replace("one", "ours"), "our change");
        checkout.write_file("mine.txt", "mine\nuncommitted\n");
        (remote, checkout)
    }

    #[test]
    fn nothing_to_pull_is_reported_as_up_to_date() {
        let (_remote, checkout) = remote_and_checkout(false);
        let report = pull(&checkout, PullLocalChanges::GitDecides).unwrap();
        assert_eq!(report.outcome, PullOutcome::UpToDate);
        assert!(!report.autostash_conflicted);
    }

    /// The case from the issue: an uncommitted edit to one file must not stop
    /// a pull whose commits only touch another.
    #[test]
    fn git_pulls_past_uncommitted_changes_the_incoming_commits_do_not_touch() {
        let (remote, checkout) = remote_and_checkout(false);
        remote.commit_file("shared.txt", &BASE.replace("one", "theirs"), "their change");
        checkout.write_file("mine.txt", "mine\nuncommitted\n");

        let report = pull(&checkout, PullLocalChanges::GitDecides).unwrap();

        assert_eq!(report.outcome, PullOutcome::Updated);
        assert_eq!(read(&checkout, "shared.txt"), BASE.replace("one", "theirs"));
        assert_eq!(read(&checkout, "mine.txt"), "mine\nuncommitted\n");
    }

    #[test]
    fn git_refusing_to_overwrite_a_change_is_reported_as_blocked_and_names_the_file() {
        let (remote, checkout) = remote_and_checkout(false);
        remote.commit_file("shared.txt", &BASE.replace("one", "theirs"), "their change");
        checkout.write_file("shared.txt", &BASE.replace("five", "uncommitted"));
        let head = checkout.head_oid();

        let failure = pull(&checkout, PullLocalChanges::GitDecides).unwrap_err();

        assert!(failure.blocked_by_local_changes);
        let message = error_message(&failure.error).english();
        assert!(message.contains("shared.txt"), "got {message:?}");
        assert!(message.contains("AutoStash"), "got {message:?}");
        // Git refused before touching anything.
        assert_eq!(checkout.head_oid(), head);
        assert_eq!(
            read(&checkout, "shared.txt"),
            BASE.replace("five", "uncommitted")
        );
    }

    #[test]
    fn requiring_a_clean_tree_refuses_before_git_runs() {
        let (remote, checkout) = remote_and_checkout(false);
        remote.commit_file("shared.txt", &BASE.replace("one", "theirs"), "their change");
        checkout.write_file("mine.txt", "mine\nuncommitted\n");
        let head = checkout.head_oid();

        let failure = pull(&checkout, PullLocalChanges::RequireClean).unwrap_err();

        assert!(failure.blocked_by_local_changes);
        assert_eq!(
            checkout.head_oid(),
            head,
            "a pull git itself would have accepted must still be refused"
        );
    }

    #[test]
    fn requiring_a_clean_tree_ignores_untracked_files() {
        let (remote, checkout) = remote_and_checkout(false);
        remote.commit_file("shared.txt", &BASE.replace("one", "theirs"), "their change");
        checkout.write_file("scratch.txt", "untracked\n");

        let report = pull(&checkout, PullLocalChanges::RequireClean).unwrap();
        assert_eq!(report.outcome, PullOutcome::Updated);
    }

    #[test]
    fn autostash_pulls_over_a_changed_file_and_puts_the_change_back() {
        let (remote, checkout) = remote_and_checkout(false);
        remote.commit_file("shared.txt", &BASE.replace("one", "theirs"), "their change");
        checkout.write_file("shared.txt", &BASE.replace("five", "uncommitted"));

        let report = pull(&checkout, PullLocalChanges::Autostash).unwrap();

        assert_eq!(report.outcome, PullOutcome::Updated);
        assert!(!report.autostash_conflicted);
        assert_eq!(
            read(&checkout, "shared.txt"),
            BASE.replace("one", "theirs").replace("five", "uncommitted")
        );
        assert_eq!(git(&checkout, &["stash", "list"]), "");
    }

    #[test]
    fn an_autostash_that_conflicts_on_the_way_back_is_kept_in_the_stash_list() {
        let (remote, checkout) = remote_and_checkout(false);
        remote.commit_file("shared.txt", &BASE.replace("one", "theirs"), "their change");
        checkout.write_file("shared.txt", &BASE.replace("one", "uncommitted"));

        let report = pull(&checkout, PullLocalChanges::Autostash).unwrap();

        // The pull itself went through; it is the reapplied change that
        // conflicts, and the caller reports that separately.
        assert_eq!(report.outcome, PullOutcome::Updated);
        assert!(report.autostash_conflicted);
        assert_eq!(checkout.head_oid(), remote.head_oid());
        assert_eq!(state(&checkout), git2::RepositoryState::Clean);
        assert!(
            git(&checkout, &["stash", "list"]).contains("autostash"),
            "the uncommitted change must be recoverable from the stash list"
        );
    }

    #[test]
    fn a_pull_stopped_by_a_merge_conflict_holds_the_autostash_until_it_is_aborted() {
        let (_remote, checkout) = diverged_with_uncommitted_change(false);
        let head = checkout.head_oid();

        let report = pull(&checkout, PullLocalChanges::Autostash).unwrap();
        assert_eq!(
            report.outcome,
            PullOutcome::Conflicts {
                count: 1,
                autostash_pending: true
            }
        );
        // Git took the uncommitted change out of the tree for the merge.
        assert_eq!(read(&checkout, "mine.txt"), "mine\n");
        assert_eq!(git(&checkout, &["stash", "list"]), "");

        let restore = abort_in_progress_operation(checkout.path()).unwrap();

        assert_eq!(restore, Some(AutostashRestore::Applied));
        assert_eq!(state(&checkout), git2::RepositoryState::Clean);
        assert_eq!(checkout.head_oid(), head);
        assert_eq!(read(&checkout, "shared.txt"), BASE.replace("one", "ours"));
        assert_eq!(read(&checkout, "mine.txt"), "mine\nuncommitted\n");
        assert!(pending_autostash(checkout.repo()).is_none());
    }

    #[test]
    fn concluding_a_conflicted_merge_pull_restores_the_autostash() {
        let (remote, checkout) = diverged_with_uncommitted_change(false);
        let ours = checkout.head_oid();
        pull(&checkout, PullLocalChanges::Autostash).unwrap();

        checkout.write_file("shared.txt", &BASE.replace("one", "resolved"));
        git(&checkout, &["add", "shared.txt"]);
        let (_summary, restore) = continue_in_progress_operation(checkout.path()).unwrap();

        assert_eq!(restore, Some(AutostashRestore::Applied));
        assert_eq!(state(&checkout), git2::RepositoryState::Clean);
        let merge = checkout.repo().find_commit(checkout.head_oid()).unwrap();
        assert_eq!(
            merge.parent_ids().collect::<Vec<_>>(),
            vec![ours, remote.head_oid()]
        );
        assert_eq!(read(&checkout, "mine.txt"), "mine\nuncommitted\n");
        assert!(pending_autostash(checkout.repo()).is_none());
        assert_eq!(git(&checkout, &["stash", "list"]), "");
    }

    #[test]
    fn aborting_a_conflicted_rebase_pull_restores_the_autostash() {
        let (_remote, checkout) = diverged_with_uncommitted_change(true);
        let head = checkout.head_oid();

        let report = pull(&checkout, PullLocalChanges::Autostash).unwrap();
        assert_eq!(
            report.outcome,
            PullOutcome::Conflicts {
                count: 1,
                autostash_pending: true
            }
        );

        let restore = abort_in_progress_operation(checkout.path()).unwrap();

        assert_eq!(restore, Some(AutostashRestore::Applied));
        assert_eq!(state(&checkout), git2::RepositoryState::Clean);
        // Back on the branch, at the commit it had — not left detached on the
        // commit the rebase had reached.
        assert_eq!(git(&checkout, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(checkout.head_oid(), head);
        assert_eq!(read(&checkout, "shared.txt"), BASE.replace("one", "ours"));
        assert_eq!(read(&checkout, "mine.txt"), "mine\nuncommitted\n");
        assert_eq!(git(&checkout, &["stash", "list"]), "");
    }

    #[test]
    fn aborting_a_rebase_without_an_autostash_returns_to_the_branch() {
        let (_remote, checkout) = diverged_with_uncommitted_change(true);
        git(&checkout, &["checkout", "--", "mine.txt"]);
        let head = checkout.head_oid();
        let report = pull(&checkout, PullLocalChanges::GitDecides).unwrap();
        assert_eq!(
            report.outcome,
            PullOutcome::Conflicts {
                count: 1,
                autostash_pending: false
            }
        );

        let restore = abort_in_progress_operation(checkout.path()).unwrap();

        assert_eq!(restore, None);
        assert_eq!(state(&checkout), git2::RepositoryState::Clean);
        assert_eq!(git(&checkout, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(checkout.head_oid(), head);
    }

    #[test]
    fn continuing_a_conflicted_rebase_pull_leaves_the_autostash_to_git() {
        let (_remote, checkout) = diverged_with_uncommitted_change(true);
        pull(&checkout, PullLocalChanges::Autostash).unwrap();

        checkout.write_file("shared.txt", &BASE.replace("one", "resolved"));
        git(&checkout, &["add", "shared.txt"]);
        let (_summary, restore) = continue_in_progress_operation(checkout.path()).unwrap();

        // Git reapplied it cleanly, so there is nothing further to report.
        assert_eq!(restore, None);
        assert_eq!(state(&checkout), git2::RepositoryState::Clean);
        assert_eq!(read(&checkout, "mine.txt"), "mine\nuncommitted\n");
        assert_eq!(git(&checkout, &["stash", "list"]), "");
    }

    #[test]
    fn a_rebase_pull_refused_over_uncommitted_changes_is_reported_as_blocked() {
        let (remote, checkout) = remote_and_checkout(true);
        remote.commit_file("shared.txt", &BASE.replace("one", "theirs"), "their change");
        checkout.write_file("mine.txt", "mine\nuncommitted\n");

        let failure = pull(&checkout, PullLocalChanges::GitDecides).unwrap_err();
        assert!(failure.blocked_by_local_changes);

        // The same pull with autostash is what gets past it.
        let report = pull(&checkout, PullLocalChanges::Autostash).unwrap();
        assert_eq!(report.outcome, PullOutcome::Updated);
        assert_eq!(read(&checkout, "mine.txt"), "mine\nuncommitted\n");
    }

    /// A stash does not take untracked files, so autostash cannot help here and
    /// must not be offered.
    #[test]
    fn an_untracked_file_in_the_way_is_not_reported_as_blocked_by_local_changes() {
        let (remote, checkout) = remote_and_checkout(false);
        remote.commit_file("new.txt", "theirs\n", "add new");
        checkout.write_file("new.txt", "untracked\n");

        let failure = pull(&checkout, PullLocalChanges::GitDecides).unwrap_err();

        assert!(!failure.blocked_by_local_changes);
        assert_eq!(read(&checkout, "new.txt"), "untracked\n");
    }

    #[test]
    fn a_pull_without_conflicts_is_not_mistaken_for_one_when_a_merge_is_already_in_progress() {
        let (_remote, checkout) = diverged_with_uncommitted_change(false);
        git(&checkout, &["checkout", "--", "mine.txt"]);
        pull(&checkout, PullLocalChanges::GitDecides).unwrap();
        assert_eq!(state(&checkout), git2::RepositoryState::Merge);

        // Git refuses a second pull outright; that is a failure, not a new
        // conflict to resolve.
        let failure = pull(&checkout, PullLocalChanges::GitDecides).unwrap_err();
        assert!(!failure.blocked_by_local_changes);
    }

    #[test]
    fn the_blocking_files_are_read_from_gits_refusal() {
        let output = "From /tmp/remote\n * branch            main       -> FETCH_HEAD\n\
                      error: Your local changes to the following files would be overwritten by merge:\n\
                      \tsrc/a.rs\n\tdocs/b.md\n\
                      Please commit your changes or stash them before you merge.\nAborting\n\
                      Updating b7b4571..8f9bbdd";
        assert_eq!(
            local_changes_block(output),
            Some(LocalChangesBlock::Files(vec![
                "src/a.rs".to_string(),
                "docs/b.md".to_string()
            ]))
        );
    }

    #[test]
    fn a_refusal_from_the_merge_strategy_is_recognised_too() {
        // A true merge with a staged change words the same refusal differently.
        let output =
            "error: Your local changes to the following files would be overwritten by merge:\n  \
                      shared.txt\nMerge with strategy ort failed.";
        assert_eq!(
            local_changes_block(output),
            Some(LocalChangesBlock::Files(vec!["shared.txt".to_string()]))
        );
    }

    #[test]
    fn a_rebase_refusal_names_no_files() {
        let output = "error: cannot pull with rebase: You have unstaged changes.\n\
                      error: Please commit or stash them.";
        assert_eq!(local_changes_block(output), Some(LocalChangesBlock::Rebase));
    }

    #[test]
    fn other_failures_are_not_blamed_on_local_changes() {
        for output in [
            "error: The following untracked working tree files would be overwritten by merge:\n\
             \tnew.txt\nPlease move or remove them before you merge.\nAborting",
            "fatal: unable to access 'https://example.com/repo.git/': Could not resolve host",
            "fatal: Need to specify how to reconcile divergent branches.",
        ] {
            assert_eq!(local_changes_block(output), None, "for {output:?}");
        }
    }
}
