use super::messages::error;
use anyhow::{Context as _, Result};
use git2::{Oid, Repository};
use rgitui_i18n::msg;

/// 切换分支时如何处理当前工作树的未提交修改。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchSwitchMode {
    RequireClean,
    LeaveChanges,
    BringChanges,
}

/// 在后台读取磁盘状态，避免侧栏快照尚未刷新时漏掉本地修改。
pub fn branch_switch_state(path: &std::path::Path) -> Result<(String, Oid, bool)> {
    let repo = Repository::open(path)?;
    let head = repo.head()?;
    let name = head.shorthand().unwrap_or("HEAD").to_string();
    let oid = head.peel_to_commit()?.id();
    Ok((name, oid, has_changes(&repo)?))
}

pub(super) fn switch_branch(
    repo: &mut Repository,
    target: &str,
    mode: BranchSwitchMode,
    expected: Option<(&str, Oid)>,
) -> Result<(String, bool)> {
    let worktree = repo
        .workdir()
        .context("Repository has no working tree")?
        .to_path_buf();
    // 与拉取和推送共用仓库锁，避免并发操作改写 HEAD、索引或共享储藏列表。
    super::network::with_network_lock(&worktree, || {
        switch_branch_locked(repo, target, mode, expected)
    })
}

fn switch_branch_locked(
    repo: &mut Repository,
    target: &str,
    mode: BranchSwitchMode,
    expected: Option<(&str, Oid)>,
) -> Result<(String, bool)> {
    let head = repo.head()?;
    let source = head.shorthand().unwrap_or("HEAD").to_string();
    let source_ref = head
        .name()
        .context("HEAD has no reference name")?
        .to_string();
    let head_oid = head.peel_to_commit()?.id();
    drop(head);
    if let Some((name, oid)) = expected {
        anyhow::ensure!(
            source == name && head_oid == oid,
            error(msg!("git.checkout_branch.changed"))
        );
    }
    anyhow::ensure!(
        repo.state() == git2::RepositoryState::Clean && !repo.index()?.has_conflicts(),
        error(msg!("git.checkout_branch.unresolved"))
    );
    let (local_name, tracking) = if repo.find_branch(target, git2::BranchType::Local).is_ok() {
        (target.to_string(), false)
    } else {
        repo.find_reference(&format!("refs/remotes/{target}"))?;
        let (_, branch) = super::remote_tracking_branch_parts(repo, target)?;
        anyhow::ensure!(
            repo.find_branch(&branch, git2::BranchType::Local).is_err(),
            error(msg!("git.error.local_branch_exists", branch = &branch))
        );
        (branch, true)
    };
    anyhow::ensure!(
        source != local_name,
        error(msg!("git.error.already_on_branch", branch = &local_name))
    );
    let source_saved = saved_ref(repo, &source_ref)?;
    let target_saved = saved_ref(repo, &format!("refs/heads/{local_name}"))?;
    let dirty = has_changes(repo)?;
    if mode == BranchSwitchMode::RequireClean {
        super::ensure_clean_worktree(repo, crate::GitOperationKind::Checkout.name())?;
    }
    if dirty && mode == BranchSwitchMode::BringChanges && saved_oid(repo, &target_saved)?.is_some()
    {
        anyhow::bail!(error(msg!(
            "git.checkout_branch.saved_exists",
            branch = &local_name
        )));
    }
    let stashed = dirty && mode == BranchSwitchMode::LeaveChanges;
    if stashed {
        anyhow::ensure!(
            saved_oid(repo, &source_saved)?.is_none(),
            error(msg!("git.checkout_branch.saved_exists", branch = &source))
        );
        // 自动储藏包含未跟踪文件；保留独立引用，重启后仍能按分支找回。
        let signature = git2::Signature::now("rgitui", "rgitui@local")?;
        let oid = repo.stash_save(
            &signature,
            &format!("rgitui: changes on {source}"),
            Some(git2::StashFlags::INCLUDE_UNTRACKED),
        )?;
        if let Err(save_error) = repo.reference(&source_saved, oid, false, "Save branch changes") {
            run(repo, &["stash", "apply", "--index", &oid.to_string()])
                .with_context(|| format!("{save_error}; changes remain in stash {oid}"))?;
            return Err(save_error.into());
        }
    }
    let switched = if tracking {
        run(
            repo,
            &[
                "switch",
                "--no-overwrite-ignore",
                "--track",
                "-c",
                &local_name,
                "--",
                target,
            ],
        )
    } else {
        run(
            repo,
            &[
                "switch",
                "--no-overwrite-ignore",
                "--no-guess",
                "--",
                &local_name,
            ],
        )
    };
    if let Err(switch_error) = switched {
        if stashed {
            // 切换失败只能在原分支恢复原修改，绝不强制检出或丢弃文件。
            let current = repo.head()?;
            anyhow::ensure!(
                current.name() == Some(source_ref.as_str()),
                error(msg!("git.checkout_branch.changed"))
            );
            drop(current);
            restore_saved(repo, &source_saved).with_context(|| switch_error.to_string())?;
        }
        return Err(switch_error);
    }
    if saved_oid(repo, &target_saved)?.is_some() {
        restore_saved(repo, &target_saved).map_err(|restore_error| {
            error(msg!(
                "git.checkout_branch.restore_failed",
                branch = &local_name,
                details = restore_error.to_string()
            ))
        })?;
    }
    Ok((local_name, tracking))
}

pub(super) fn has_changes(repo: &Repository) -> Result<bool> {
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    Ok(!repo.statuses(Some(&mut options))?.is_empty())
}

fn saved_ref(repo: &Repository, branch_ref: &str) -> Result<String> {
    let worktree = Oid::hash_object(
        git2::ObjectType::Blob,
        repo.path().as_os_str().as_encoded_bytes(),
    )?;
    let branch = Oid::hash_object(git2::ObjectType::Blob, branch_ref.as_bytes())?;
    Ok(format!("refs/rgitui/branch-stashes/{worktree}/{branch}"))
}

fn saved_oid(repo: &Repository, name: &str) -> Result<Option<Oid>> {
    match repo.find_reference(name) {
        Ok(reference) => Ok(Some(reference.peel_to_commit()?.id())),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn restore_saved(repo: &mut Repository, name: &str) -> Result<()> {
    let oid = saved_oid(repo, name)?.context("Saved branch changes are missing")?;
    // 先恢复，再删除对应储藏和所有权引用；冲突时两份恢复入口均保留。
    run(repo, &["stash", "apply", "--index", &oid.to_string()])?;
    let mut index = None;
    repo.stash_foreach(|i, _, id| {
        if *id == oid {
            index = Some(i);
            false
        } else {
            true
        }
    })?;
    if let Some(index) = index {
        repo.stash_drop(index)?;
    }
    repo.find_reference(name)?.delete()?;
    Ok(())
}

fn run(repo: &Repository, args: &[&str]) -> Result<()> {
    let output = super::git_command()
        .current_dir(repo.workdir().context("Repository has no working tree")?)
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Git command failed ({}):\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Git CLI 会重写索引，必须同步 libgit2 已打开的索引缓存。
    repo.index()?.read(true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgitui_test_support::TempRepo;

    #[test]
    fn restoring_branch_changes_does_not_remove_an_unrelated_manual_stash() {
        let fixture = TempRepo::init();
        fixture.commit_file("tracked.txt", "base\n", "base");
        fixture.branch("feature");
        fixture.write_file("tracked.txt", "main work\n");
        let mut repo = Repository::open(fixture.path()).unwrap();
        switch_branch(&mut repo, "feature", BranchSwitchMode::LeaveChanges, None).unwrap();
        fixture.write_file("tracked.txt", "manual work\n");
        let manual = repo
            .stash_save(&fixture.signature(), "manual stash", None)
            .unwrap();
        switch_branch(&mut repo, "main", BranchSwitchMode::RequireClean, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("tracked.txt")).unwrap(),
            "main work\n"
        );
        let mut remaining = Vec::new();
        repo.stash_foreach(|_, _, oid| {
            remaining.push(*oid);
            true
        })
        .unwrap();
        assert_eq!(remaining, [manual]);
    }

    #[test]
    fn remote_branch_creates_tracking_branch_while_bringing_local_changes() {
        let fixture = TempRepo::init();
        let commit = fixture.commit_file("tracked.txt", "base\n", "base");
        fixture
            .repo()
            .remote("origin", "https://example.invalid/repo.git")
            .unwrap();
        fixture
            .repo()
            .reference(
                "refs/remotes/origin/topic",
                commit,
                true,
                "test tracking ref",
            )
            .unwrap();
        fixture.write_file("tracked.txt", "local work\n");
        let mut repo = Repository::open(fixture.path()).unwrap();
        let (_, tracking) = switch_branch(
            &mut repo,
            "origin/topic",
            BranchSwitchMode::BringChanges,
            None,
        )
        .unwrap();
        assert!(tracking);
        assert_eq!(repo.head().unwrap().shorthand(), Some("topic"));
        assert_eq!(
            repo.find_branch("topic", git2::BranchType::Local)
                .unwrap()
                .upstream()
                .unwrap()
                .name()
                .unwrap(),
            Some("origin/topic")
        );
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("tracked.txt")).unwrap(),
            "local work\n"
        );
    }

    #[test]
    fn conflicting_bring_does_not_change_head_or_local_files() {
        let fixture = TempRepo::init();
        fixture.commit_file("tracked.txt", "base\n", "base");
        fixture.branch("source");
        fixture.commit_file("tracked.txt", "target\n", "target");
        let mut repo = Repository::open(fixture.path()).unwrap();
        switch_branch(&mut repo, "source", BranchSwitchMode::RequireClean, None).unwrap();
        fixture.write_file("tracked.txt", "unfinished\n");
        assert!(switch_branch(&mut repo, "main", BranchSwitchMode::BringChanges, None).is_err());
        assert_eq!(repo.head().unwrap().shorthand(), Some("source"));
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("tracked.txt")).unwrap(),
            "unfinished\n"
        );
    }

    #[test]
    fn failed_leave_restores_source_modifications_and_index() {
        let fixture = TempRepo::init();
        fixture.commit_file("tracked.txt", "base\n", "base");
        fixture.branch("feature");
        let other = tempfile::tempdir().unwrap();
        let branch = fixture.repo().find_reference("refs/heads/feature").unwrap();
        let mut options = git2::WorktreeAddOptions::new();
        options.reference(Some(&branch));
        fixture
            .repo()
            .worktree("occupied", &other.path().join("worktree"), Some(&options))
            .unwrap();
        fixture.write_file("tracked.txt", "staged\n");
        fixture.stage("tracked.txt");
        fixture.write_file("tracked.txt", "unfinished\n");
        let mut repo = Repository::open(fixture.path()).unwrap();
        assert!(switch_branch(&mut repo, "feature", BranchSwitchMode::LeaveChanges, None).is_err());
        assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("tracked.txt")).unwrap(),
            "unfinished\n"
        );
        let entry = repo
            .index()
            .unwrap()
            .get_path(std::path::Path::new("tracked.txt"), 0)
            .unwrap();
        assert_eq!(repo.find_blob(entry.id).unwrap().content(), b"staged\n");
    }

    #[test]
    fn changed_source_is_rejected_before_stashing() {
        let fixture = TempRepo::init();
        let old = fixture.commit_file("tracked.txt", "base\n", "base");
        fixture.branch("feature");
        fixture.commit_file("tracked.txt", "new base\n", "commit while dialog open");
        fixture.write_file("tracked.txt", "unfinished\n");
        let mut repo = Repository::open(fixture.path()).unwrap();
        assert!(switch_branch(
            &mut repo,
            "feature",
            BranchSwitchMode::LeaveChanges,
            Some(("main", old))
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("tracked.txt")).unwrap(),
            "unfinished\n"
        );
        assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
        assert!(
            saved_oid(&repo, &saved_ref(&repo, "refs/heads/main").unwrap())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn failed_saved_restore_keeps_the_recovery_ref_and_stash() {
        let fixture = TempRepo::init();
        fixture.commit_file("tracked.txt", "base\n", "base");
        fixture.branch("feature");
        fixture.write_file("tracked.txt", "unfinished\n");
        let mut repo = Repository::open(fixture.path()).unwrap();
        switch_branch(&mut repo, "feature", BranchSwitchMode::LeaveChanges, None).unwrap();
        let saved = saved_ref(&repo, "refs/heads/main").unwrap();
        let stash = saved_oid(&repo, &saved).unwrap().unwrap();
        let changed = fixture.commit_file("tracked.txt", "different base\n", "new base");
        repo.reference("refs/heads/main", changed, true, "simulate other checkout")
            .unwrap();
        assert!(switch_branch(&mut repo, "main", BranchSwitchMode::RequireClean, None).is_err());
        assert_eq!(repo.head().unwrap().shorthand(), Some("main"));
        assert_eq!(saved_oid(&repo, &saved).unwrap(), Some(stash));
        let mut found = false;
        repo.stash_foreach(|_, _, oid| {
            found |= *oid == stash;
            true
        })
        .unwrap();
        assert!(found);
    }

    #[test]
    fn preflight_detects_untracked_files_and_ignored_files_are_not_overwritten() {
        let fixture = TempRepo::init();
        fixture.commit_file(".gitignore", "ignored.txt\n", "ignore");
        fixture.branch("source");
        fixture.commit_file("ignored.txt", "target\n", "tracked on target");
        let mut repo = Repository::open(fixture.path()).unwrap();
        switch_branch(&mut repo, "source", BranchSwitchMode::RequireClean, None).unwrap();
        fixture.write_file("new.txt", "untracked\n");
        assert!(branch_switch_state(fixture.path()).unwrap().2);
        fixture.write_file("ignored.txt", "local ignored work\n");
        assert!(switch_branch(&mut repo, "main", BranchSwitchMode::BringChanges, None).is_err());
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("ignored.txt")).unwrap(),
            "local ignored work\n"
        );
    }

    #[test]
    fn bring_changes_preserves_index_worktree_and_untracked_files() {
        let fixture = TempRepo::init();
        fixture.commit_file("tracked.txt", "base\n", "base");
        fixture.branch("feature");
        fixture.write_file("tracked.txt", "staged\n");
        fixture.stage("tracked.txt");
        fixture.write_file("tracked.txt", "staged and unstaged\n");
        fixture.write_file("new.txt", "untracked\n");
        let mut repo = Repository::open(fixture.path()).unwrap();
        switch_branch(&mut repo, "feature", BranchSwitchMode::BringChanges, None).unwrap();
        assert_eq!(repo.head().unwrap().shorthand(), Some("feature"));
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("tracked.txt")).unwrap(),
            "staged and unstaged\n"
        );
        let index = repo.index().unwrap();
        let blob = repo
            .find_blob(
                index
                    .get_path(std::path::Path::new("tracked.txt"), 0)
                    .unwrap()
                    .id,
            )
            .unwrap();
        assert_eq!(blob.content(), b"staged\n");
        assert!(fixture.path().join("new.txt").exists());
    }

    #[test]
    fn leave_changes_restores_them_on_return_even_after_reopening() {
        let fixture = TempRepo::init();
        fixture.commit_file("tracked.txt", "base\n", "base");
        fixture.branch("feature");
        fixture.write_file("tracked.txt", "staged\n");
        fixture.stage("tracked.txt");
        fixture.write_file("tracked.txt", "unfinished\n");
        fixture.write_file("new.txt", "untracked\n");
        let mut repo = Repository::open(fixture.path()).unwrap();
        switch_branch(&mut repo, "feature", BranchSwitchMode::LeaveChanges, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("tracked.txt")).unwrap(),
            "base\n"
        );
        assert!(!fixture.path().join("new.txt").exists());
        drop(repo);
        let mut repo = Repository::open(fixture.path()).unwrap();
        switch_branch(&mut repo, "main", BranchSwitchMode::RequireClean, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("tracked.txt")).unwrap(),
            "unfinished\n"
        );
        assert!(fixture.path().join("new.txt").exists());
        let index = repo.index().unwrap();
        let blob = repo
            .find_blob(
                index
                    .get_path(std::path::Path::new("tracked.txt"), 0)
                    .unwrap()
                    .id,
            )
            .unwrap();
        assert_eq!(blob.content(), b"staged\n");
    }
}
