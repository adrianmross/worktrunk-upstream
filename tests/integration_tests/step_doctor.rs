//! Integration tests for `wt step doctor`

use crate::common::{TestRepo, hide_host_wt, make_snapshot_cmd, repo};
use insta_cmd::assert_cmd_snapshot;
use rstest::rstest;
use std::fs;
use std::process::Command;

/// `wt step <args>`, blind to the host's `wt` binaries.
fn doctor(repo: &TestRepo, args: &[&str]) -> Command {
    let mut cmd = make_snapshot_cmd(repo, "step", args, None);
    hide_host_wt(&mut cmd, repo);
    cmd
}

/// A repository with only well-placed worktrees reports nothing.
#[rstest]
fn test_doctor_clean(mut repo: TestRepo) {
    repo.add_worktree("feature");

    assert_cmd_snapshot!(doctor(&repo, &["doctor"]));
    assert_cmd_snapshot!(
        "doctor_clean_json",
        doctor(&repo, &["doctor", "--format=json"])
    );
}

/// A deleted worktree directory is a stale entry; `--fix` prunes it.
#[rstest]
fn test_doctor_stale_entry(mut repo: TestRepo) {
    let path = repo.add_worktree("gone");
    fs::remove_dir_all(&path).unwrap();

    assert_cmd_snapshot!(doctor(&repo, &["doctor"]));
    assert_cmd_snapshot!(
        "doctor_stale_entry_json",
        doctor(&repo, &["doctor", "--format=json"])
    );
    assert_cmd_snapshot!(
        "doctor_stale_entry_fix",
        doctor(&repo, &["doctor", "--fix"])
    );
    assert_cmd_snapshot!("doctor_stale_entry_after_fix", doctor(&repo, &["doctor"]));
}

/// A worktree whose `.git` file points nowhere is still registered (git
/// doesn't call it prunable) but git can't operate in it.
#[rstest]
fn test_doctor_broken_worktree(mut repo: TestRepo) {
    let path = repo.add_worktree("broken");
    fs::write(path.join(".git"), "gitdir: /nonexistent/gitdir\n").unwrap();

    assert_cmd_snapshot!(doctor(&repo, &["doctor"]));
}

/// A directory named like a worktree but not registered is stray; a sibling
/// repository with its own `.git` directory and an unrelated name are not.
#[rstest]
fn test_doctor_stray_directory(repo: TestRepo) {
    let parent = repo.root_path().parent().unwrap();
    fs::create_dir(parent.join("repo.leftover")).unwrap();
    fs::create_dir_all(parent.join("repo.github.io").join(".git")).unwrap();
    fs::create_dir(parent.join("unrelated")).unwrap();

    assert_cmd_snapshot!(doctor(&repo, &["doctor"]));
    assert!(parent.join("repo.leftover").exists());
}

/// A worktree under `.claude/worktrees/` and a leftover directory there are
/// both reported, with the relocate and plugin hints.
#[rstest]
fn test_doctor_agent_worktree(mut repo: TestRepo) {
    let agents = repo.root_path().join(".claude").join("worktrees");
    repo.add_worktree_at_path("agent", &agents.join("agent"));
    fs::create_dir_all(agents.join("abandoned")).unwrap();

    assert_cmd_snapshot!(doctor(&repo, &["doctor"]));
    assert_cmd_snapshot!(
        "doctor_agent_worktree_json",
        doctor(&repo, &["doctor", "--format=json"])
    );
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

    let mut cmd = doctor(&repo, &["doctor"]);
    cmd.env("PATH", joined(&[&old, &alias, &new]));
    assert_cmd_snapshot!(cmd);

    let mut same = doctor(&repo, &["doctor"]);
    same.env("PATH", joined(&[&old, &alias]));
    assert_cmd_snapshot!("doctor_wt_binaries_same", same);

    let mut bin = doctor(&repo, &["doctor", "--format=json"]);
    bin.env("PATH", joined(&[&old]))
        .env("WORKTRUNK_BIN", new.join("wt"));
    assert_cmd_snapshot!("doctor_wt_binaries_worktrunk_bin", bin);
}

/// `agent-<hex id>` branches with no worktree are reported, merged with a
/// `wt remove` hint and unmerged with a `git log` hint. A checked-out agent
/// branch and a non-hex name are not.
#[rstest]
fn test_doctor_agent_branches(mut repo: TestRepo) {
    repo.run_git(&["branch", "agent-0123abcd"]);
    repo.run_git(&["branch", "agent-notahexid"]);
    let unmerged = repo.add_worktree("agent-89abcdef01");
    repo.commit_in_worktree(&unmerged, "work.txt", "work", "Agent work");
    repo.run_git(&["worktree", "remove", "--force", unmerged.to_str().unwrap()]);
    repo.add_worktree("agent-fedcba98");

    assert_cmd_snapshot!(doctor(&repo, &["doctor"]));
    assert_cmd_snapshot!(
        "doctor_agent_branches_json",
        doctor(&repo, &["doctor", "--format=json"])
    );
}

/// A primary worktree off the default branch is reported, with the command
/// that switches it back; a detached one too.
#[rstest]
fn test_doctor_root_branch(repo: TestRepo) {
    repo.run_git(&["switch", "-c", "wip"]);

    assert_cmd_snapshot!(doctor(&repo, &["doctor"]));
    assert_cmd_snapshot!(
        "doctor_root_branch_json",
        doctor(&repo, &["doctor", "--format=json"])
    );

    repo.run_git(&["switch", "--detach"]);
    assert_cmd_snapshot!("doctor_root_branch_detached", doctor(&repo, &["doctor"]));
}

/// A worktree inside the primary worktree is reported; one under
/// `.claude/worktrees/` is left to the agent-worktree check.
#[rstest]
fn test_doctor_nested_worktree(mut repo: TestRepo) {
    let nested = repo.root_path().join("nested");
    repo.add_worktree_at_path("inner", &nested);
    let agents = repo.root_path().join(".claude").join("worktrees");
    repo.add_worktree_at_path("agent", &agents.join("agent"));

    assert_cmd_snapshot!(doctor(&repo, &["doctor"]));
    assert_cmd_snapshot!(
        "doctor_nested_worktree_json",
        doctor(&repo, &["doctor", "--format=json"])
    );
}

/// A `worktree-path` template inside the repository makes nesting intended.
#[rstest]
fn test_doctor_nested_worktree_by_template(mut repo: TestRepo) {
    repo.write_test_config(
        r#"worktree-path = "{{ repo_path }}/.worktrees/{{ branch | sanitize }}"
"#,
    );
    let nested = repo.root_path().join("nested");
    repo.add_worktree_at_path("inner", &nested);

    assert_cmd_snapshot!(doctor(&repo, &["doctor"]));
}

/// `--repo` checks each repository under its own header, skipping a plain
/// directory, a file, and a repeat; with JSON, one array tags each finding
/// with its repository.
#[rstest]
fn test_doctor_repos(repo: TestRepo) {
    let parent = fs::canonicalize(repo.root_path().parent().unwrap()).unwrap();
    let alpha = parent.join("alpha");
    fs::create_dir(&alpha).unwrap();
    repo.run_git_in(&alpha, &["init", "-q", "-b", "main"]);
    repo.run_git_in(&alpha, &["commit", "-q", "--allow-empty", "-m", "init"]);
    repo.run_git_in(&alpha, &["switch", "-q", "-c", "wip"]);
    let plain = parent.join("plain");
    fs::create_dir(&plain).unwrap();
    let file = parent.join("notes.md");
    fs::write(&file, "").unwrap();
    // Spelled through the repo root so the snapshot's `args:` redact to `_REPO_/..`.
    let root = repo.root_path();
    let paths: Vec<String> = [
        root.join("../alpha"),
        root.join("../plain"),
        root.join("../notes.md"),
        root.to_path_buf(),
        root.join("../alpha"),
    ]
    .iter()
    .map(|p| p.display().to_string())
    .collect();
    let mut args = vec!["doctor", "--repo"];
    args.extend(paths.iter().map(String::as_str));
    assert_cmd_snapshot!(doctor(&repo, &args));

    let paths: Vec<String> = [&alpha, &plain, &file, repo.root_path(), &alpha]
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let mut args = vec!["doctor", "--repo"];
    args.extend(paths.iter().map(String::as_str));
    args.push("--format=json");
    let output = doctor(&repo, &args).output().unwrap();
    assert!(output.status.success());
    let findings: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        findings,
        serde_json::json!([{
            "repo": alpha,
            "kind": "root_branch",
            "path": alpha,
            "branch": "wip",
            "detail": "main",
            "fix": format!("git -C {} switch main", alpha.display()),
        }])
    );
}

/// With `wt-bin` set, only the first `wt` on PATH and `WORKTRUNK_BIN` count:
/// another binary later on PATH is fine, a different first one or
/// `WORKTRUNK_BIN` is reported.
#[cfg(unix)]
#[rstest]
fn test_doctor_wt_bin(repo: TestRepo) {
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
    repo.write_test_config(&format!("wt-bin = \"{}\"\n", new.join("wt").display()));
    let joined = |dirs: &[&std::path::Path]| {
        let dirs: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        format!("{}:{path}", dirs.join(":"))
    };

    let mut first = doctor(&repo, &["doctor"]);
    first.env("PATH", joined(&[&new, &old]));
    assert_cmd_snapshot!("doctor_wt_bin_first", first);

    let mut shadowed = doctor(&repo, &["doctor"]);
    shadowed
        .env("PATH", joined(&[&old, &new]))
        .env("WORKTRUNK_BIN", old.join("wt"));
    assert_cmd_snapshot!("doctor_wt_bin_shadowed", shadowed);
}
