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
    assert_cmd_snapshot!(
        "doctor_clean_json",
        make_snapshot_cmd(&repo, "step", &["doctor", "--format=json"], None)
    );
}

/// A deleted worktree directory is a stale entry; `--fix` prunes it.
#[rstest]
fn test_doctor_stale_entry(mut repo: TestRepo) {
    let path = repo.add_worktree("gone");
    fs::remove_dir_all(&path).unwrap();

    assert_cmd_snapshot!(make_snapshot_cmd(&repo, "step", &["doctor"], None));
    assert_cmd_snapshot!(
        "doctor_stale_entry_json",
        make_snapshot_cmd(&repo, "step", &["doctor", "--format=json"], None)
    );
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

/// `wt` binaries on PATH that report different versions are one finding,
/// listing each; a symlink to one already seen is not counted twice.
#[cfg(unix)]
#[rstest]
fn test_doctor_wt_binaries(repo: TestRepo) {
    use std::os::unix::fs::PermissionsExt;

    let home = repo.root_path().join(".bins");
    let path = crate::common::setup_minimal_path_with_git(&home.join("git"));
    let fake = |dir: &str, version: &str| {
        let dir = home.join(dir);
        fs::create_dir_all(&dir).unwrap();
        let wt = dir.join("wt");
        fs::write(&wt, format!("#!/bin/sh\necho 'wt {version}'\n")).unwrap();
        fs::set_permissions(&wt, fs::Permissions::from_mode(0o755)).unwrap();
        dir
    };
    let old = fake("old", "0.1.0");
    let new = fake("new", "0.2.0");
    let alias = home.join("alias");
    fs::create_dir_all(&alias).unwrap();
    std::os::unix::fs::symlink(old.join("wt"), alias.join("wt")).unwrap();
    let joined = |dirs: &[&std::path::Path]| {
        let dirs: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        format!("{}:{path}", dirs.join(":"))
    };

    let mut cmd = make_snapshot_cmd(&repo, "step", &["doctor"], None);
    cmd.env("PATH", joined(&[&old, &alias, &new]));
    assert_cmd_snapshot!(cmd);

    let mut same = make_snapshot_cmd(&repo, "step", &["doctor"], None);
    same.env("PATH", joined(&[&old, &alias]));
    assert_cmd_snapshot!("doctor_wt_binaries_same", same);
}
