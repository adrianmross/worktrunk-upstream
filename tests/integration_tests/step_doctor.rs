//! Integration tests for `wt step doctor`

use crate::common::{TestRepo, make_snapshot_cmd, repo};
use insta_cmd::assert_cmd_snapshot;
use rstest::rstest;
use std::fs;

/// A repository with only well-placed worktrees reports nothing.
#[rstest]
fn test_doctor_clean(mut repo: TestRepo) {
    repo.add_worktree("feature");

    assert_cmd_snapshot!(make_snapshot_cmd(&repo, "step", &["doctor"], None));
}

/// A deleted worktree directory is a stale entry; `--fix` prunes it.
#[rstest]
fn test_doctor_stale_entry(mut repo: TestRepo) {
    let path = repo.add_worktree("gone");
    fs::remove_dir_all(&path).unwrap();

    assert_cmd_snapshot!(make_snapshot_cmd(&repo, "step", &["doctor"], None));
    assert_cmd_snapshot!(
        "doctor_stale_entry_fix",
        make_snapshot_cmd(&repo, "step", &["doctor", "--fix"], None)
    );
    assert_cmd_snapshot!(
        "doctor_stale_entry_after_fix",
        make_snapshot_cmd(&repo, "step", &["doctor"], None)
    );
}

/// A worktree whose `.git` file points nowhere is still registered (git
/// doesn't call it prunable) but git can't operate in it.
#[rstest]
fn test_doctor_broken_worktree(mut repo: TestRepo) {
    let path = repo.add_worktree("broken");
    fs::write(path.join(".git"), "gitdir: /nonexistent/gitdir\n").unwrap();

    assert_cmd_snapshot!(make_snapshot_cmd(&repo, "step", &["doctor"], None));
}

/// A directory named like a worktree but not registered is stray; a sibling
/// repository with its own `.git` directory and an unrelated name are not.
#[rstest]
fn test_doctor_stray_directory(repo: TestRepo) {
    let parent = repo.root_path().parent().unwrap();
    fs::create_dir(parent.join("repo.leftover")).unwrap();
    fs::create_dir_all(parent.join("repo.github.io").join(".git")).unwrap();
    fs::create_dir(parent.join("unrelated")).unwrap();

    assert_cmd_snapshot!(make_snapshot_cmd(&repo, "step", &["doctor"], None));
    assert!(parent.join("repo.leftover").exists());
}

/// A worktree under `.claude/worktrees/` and a leftover directory there are
/// both reported, with the relocate and plugin hints.
#[rstest]
fn test_doctor_agent_worktree(mut repo: TestRepo) {
    let agents = repo.root_path().join(".claude").join("worktrees");
    repo.add_worktree_at_path("agent", &agents.join("agent"));
    fs::create_dir_all(agents.join("abandoned")).unwrap();

    assert_cmd_snapshot!(make_snapshot_cmd(&repo, "step", &["doctor"], None));
}
