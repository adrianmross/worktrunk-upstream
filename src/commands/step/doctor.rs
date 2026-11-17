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
use crate::output::print_json;

/// Branch name expanded through the `worktree-path` template to find where
/// worktrees live and how their directories are named.
const PROBE_BRANCH: &str = "wt-doctor-probe";

/// One problem doctor found, as `--format=json` prints it.
#[derive(serde::Serialize)]
pub(crate) struct Finding {
    pub kind: FindingKind,
    pub path: PathBuf,
    /// Git's reason or error, for stale entries and broken worktrees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The command that addresses this finding, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FindingKind {
    StaleEntry,
    BrokenWorktree,
    StrayDirectory,
    AgentWorktree,
    AgentDirectory,
}

impl Finding {
    /// The finding's headline, styled with `cformat!` tags.
    fn line(&self) -> String {
        let path = format_path_for_display(&self.path);
        let detail = self.detail.as_deref().unwrap_or_default();
        match self.kind {
            FindingKind::StaleEntry => {
                cformat!("Stale worktree entry @ <bold>{path}</>: {detail}")
            }
            FindingKind::BrokenWorktree => cformat!("Broken worktree @ <bold>{path}</>"),
            FindingKind::StrayDirectory => {
                cformat!("Stray directory @ <bold>{path}</>: not a registered worktree")
            }
            FindingKind::AgentWorktree => cformat!(
                "Agent worktree @ <bold>{path}</>: outside the <bold>worktree-path</> layout"
            ),
            FindingKind::AgentDirectory => cformat!(
                "Stale agent worktree directory @ <bold>{path}</>: not a registered worktree"
            ),
        }
    }

    /// The headline as plain text, with the fix command when there is one.
    pub(crate) fn summary(&self) -> String {
        let line = anstream::adapter::strip_str(&self.line()).to_string();
        match self.fix {
            Some(fix) => format!("{line}; run {fix}"),
            None => line,
        }
    }

    fn render(&self) -> String {
        let warning = warning_message(self.line());
        match (self.kind, &self.detail) {
            (FindingKind::BrokenWorktree, Some(error)) => {
                format!("{warning}\n{}", format_with_gutter(error, None))
            }
            _ => warning.to_string(),
        }
    }
}

pub fn step_doctor(fix: bool, format: crate::cli::SwitchFormat) -> anyhow::Result<()> {
    let repo = Repository::current()?;
    let findings = diagnose(&repo)?;
    let stale = findings
        .iter()
        .filter(|f| f.kind == FindingKind::StaleEntry)
        .count();

    if format == crate::cli::SwitchFormat::Json {
        print_json(&findings)?;
    } else if findings.is_empty() {
        eprintln!("{}", info_message("No worktree problems found"));
    } else {
        for finding in &findings {
            println!("{}", finding.render());
        }
        if stale > 0 && !fix {
            println!(
                "{}",
                hint_message(cformat!(
                    "To clear stale entries, run <underline>wt step doctor --fix</>"
                ))
            );
        }
        if findings.iter().any(|f| {
            matches!(
                f.kind,
                FindingKind::AgentWorktree | FindingKind::AgentDirectory
            )
        }) {
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

/// Run every check against `repo`. Read-only.
pub(crate) fn diagnose(repo: &Repository) -> anyhow::Result<Vec<Finding>> {
    let config = repo.user_config();
    let worktrees = repo.list_worktrees()?;
    let registered: Vec<PathBuf> = worktrees
        .iter()
        .map(|wt| canonicalize_with_parents(&wt.path))
        .collect();
    let is_registered = |path: &Path| registered.contains(&canonicalize_with_parents(path));
    let finding = |kind, path: &Path, detail, fix| Finding {
        kind,
        path: path.to_path_buf(),
        detail,
        fix,
    };

    let mut findings = Vec::new();

    for wt in worktrees {
        if let Some(reason) = &wt.prunable {
            findings.push(finding(
                FindingKind::StaleEntry,
                &wt.path,
                Some(reason.clone()),
                Some("wt step doctor --fix"),
            ));
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
                findings.push(finding(
                    FindingKind::BrokenWorktree,
                    &wt.path,
                    Some(error),
                    None,
                ));
            }
        }
        if is_agent_path(&wt.path) {
            findings.push(finding(
                FindingKind::AgentWorktree,
                &wt.path,
                None,
                Some("wt step relocate"),
            ));
        }
    }

    // Stray directories: siblings named like this repo's worktrees but not
    // registered. A template whose directory name has no fixed prefix (e.g.
    // `{{ branch }}` alone) can't tell worktrees from unrelated directories, so
    // it is skipped. A sibling with its own `.git` directory is a separate
    // repository (`repo.github.io`), not a stray.
    if let Ok(probe) = compute_worktree_path(repo, PROBE_BRANCH, config)
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
                findings.push(finding(FindingKind::StrayDirectory, &dir, None, None));
            }
        }
    }

    // Leftovers in Claude Code's default worktree location.
    for wt in worktrees.iter().filter(|wt| !wt.is_prunable()) {
        for dir in subdirectories(&wt.path.join(".claude").join("worktrees")) {
            if !is_registered(&dir) {
                findings.push(finding(FindingKind::AgentDirectory, &dir, None, None));
            }
        }
    }

    Ok(findings)
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
