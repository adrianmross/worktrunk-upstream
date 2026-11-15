//! `wt step doctor` — report worktree health problems for the current repository.
//!
//! Four read-only checks, printed to stdout as one report:
//!
//! - stale entries: registrations git lists as prunable
//! - broken worktrees: live registrations where `git rev-parse` can't read `HEAD`
//! - stray directories: `<prefix><name><suffix>` directories beside the
//!   template's worktree location that no registration names
//! - agent worktrees: registrations under `.claude/worktrees/` or `.codex/`, and
//!   unregistered directories left in a worktree's `.claude/worktrees/`
//!
//! `--fix` runs only `git worktree prune`, which clears the stale entries and
//! touches no directory or branch.

use std::path::{Component, Path, PathBuf};

use color_print::cformat;
use worktrunk::git::Repository;
use worktrunk::path::{canonicalize_with_parents, format_path_for_display};
use worktrunk::styling::{
    eprintln, format_with_gutter, hint_message, info_message, println, success_message,
    warning_message,
};

use super::super::worktree::compute_worktree_path;

/// Branch name expanded through the `worktree-path` template to find where
/// worktrees live and how their directories are named.
const PROBE_BRANCH: &str = "wt-doctor-probe";

pub fn step_doctor(fix: bool) -> anyhow::Result<()> {
    let repo = Repository::current()?;
    let config = repo.user_config();
    let worktrees = repo.list_worktrees()?;
    let registered: Vec<PathBuf> = worktrees
        .iter()
        .map(|wt| canonicalize_with_parents(&wt.path))
        .collect();
    let is_registered = |path: &Path| registered.contains(&canonicalize_with_parents(path));

    let mut findings: Vec<String> = Vec::new();
    let mut stale = 0;
    let mut agent = false;

    for wt in worktrees {
        let path = format_path_for_display(&wt.path);
        if let Some(reason) = &wt.prunable {
            stale += 1;
            findings.push(
                warning_message(cformat!("Stale worktree entry @ <bold>{path}</>: {reason}"))
                    .to_string(),
            );
            continue;
        }
        if wt.has_commits() {
            let probe = repo.worktree_at(&wt.path).run_command_output(&[
                "rev-parse",
                "--verify",
                "HEAD^{commit}",
            ]);
            let error = match probe {
                Ok(output) if output.status.success() => None,
                Ok(output) => Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
                Err(e) => Some(format!("{e:#}")),
            };
            if let Some(error) = error {
                findings.push(
                    warning_message(cformat!("Broken worktree @ <bold>{path}</>")).to_string(),
                );
                findings.push(format_with_gutter(&error, None));
            }
        }
        if is_agent_path(&wt.path) {
            agent = true;
            findings.push(
                warning_message(cformat!(
                    "Agent worktree @ <bold>{path}</>: outside the <bold>worktree-path</> layout"
                ))
                .to_string(),
            );
        }
    }

    // Stray directories: siblings named like this repo's worktrees but not
    // registered. A template whose directory name has no fixed prefix (e.g.
    // `{{ branch }}` alone) can't tell worktrees from unrelated directories, so
    // it is skipped. A sibling with its own `.git` directory is a separate
    // repository (`repo.github.io`), not a stray.
    if let Ok(probe) = compute_worktree_path(&repo, PROBE_BRANCH, config)
        && let (Some(parent), Some(name)) =
            (probe.parent(), probe.file_name().and_then(|n| n.to_str()))
        && let Some((prefix, suffix)) = name.split_once(PROBE_BRANCH)
        && !prefix.is_empty()
    {
        for dir in subdirectories(parent) {
            let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.len() > prefix.len() + suffix.len()
                && name.starts_with(prefix)
                && name.ends_with(suffix)
                && !dir.join(".git").is_dir()
                && !is_registered(&dir)
            {
                findings.push(
                    warning_message(cformat!(
                        "Stray directory @ <bold>{}</>: not a registered worktree",
                        format_path_for_display(&dir)
                    ))
                    .to_string(),
                );
            }
        }
    }

    // Leftovers in Claude Code's default worktree location.
    for wt in worktrees.iter().filter(|wt| !wt.is_prunable()) {
        for dir in subdirectories(&wt.path.join(".claude").join("worktrees")) {
            if !is_registered(&dir) {
                agent = true;
                findings.push(
                    warning_message(cformat!(
                        "Stale agent worktree directory @ <bold>{}</>: not a registered worktree",
                        format_path_for_display(&dir)
                    ))
                    .to_string(),
                );
            }
        }
    }

    if findings.is_empty() {
        eprintln!("{}", info_message("No worktree problems found"));
        return Ok(());
    }
    for finding in &findings {
        println!("{finding}");
    }
    if stale > 0 && !fix {
        println!(
            "{}",
            hint_message(cformat!(
                "To clear stale entries, run <underline>wt step doctor --fix</>"
            ))
        );
    }
    if agent {
        println!(
            "{}",
            hint_message(cformat!(
                "To move registered agent worktrees to their <bold>worktree-path</> location, run <underline>wt step relocate</>"
            ))
        );
        println!(
            "{}",
            hint_message(cformat!(
                "To have Claude Code create worktrees in this layout, run <underline>wt config plugins claude install</>"
            ))
        );
    }

    if fix && stale > 0 {
        repo.run_command(&["worktree", "prune"])?;
        eprintln!(
            "{}",
            success_message(format!(
                "Pruned {stale} stale worktree entr{}",
                if stale == 1 { "y" } else { "ies" }
            ))
        );
    }
    Ok(())
}

/// A path inside Claude Code's (`.claude/worktrees/`) or Codex's (`.codex/`)
/// default worktree location.
fn is_agent_path(path: &Path) -> bool {
    let names: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    names.contains(&".codex") || names.windows(2).any(|w| w == [".claude", "worktrees"])
}

/// Directories directly inside `dir`; none when it is missing or unreadable.
fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}
