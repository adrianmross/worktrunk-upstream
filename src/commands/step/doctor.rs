//! `wt step doctor` — report worktree health problems for the current repository.
//!
//! Seven read-only checks, printed to stdout as one report:
//!
//! - root branch: the primary worktree is not on the default branch
//! - stale entries: registrations git lists as prunable
//! - broken worktrees: live registrations where `git rev-parse` can't read `HEAD`
//! - stray directories: `<prefix><name><suffix>` directories beside the
//!   template's worktree location that no registration names
//! - agent worktrees: registrations under `.claude/worktrees/` or `.codex/`, and
//!   unregistered directories left in a worktree's `.claude/worktrees/`
//! - wt binaries: the `wt` / `git-wt` executables on PATH and `WORKTRUNK_BIN`
//!   report different `--version`s
//! - agent branches: `agent-<hex id>` branches, from Claude Code's `Agent`
//!   isolation, that no worktree has checked out, merged or not
//!
//! `--fix` runs only `git worktree prune`, which clears the stale entries and
//! touches no directory or branch.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use color_print::cformat;
use worktrunk::git::Repository;
use worktrunk::path::{canonicalize_with_parents, format_path_for_display, paths_match};
use worktrunk::shell_exec::Cmd;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The branch, for agent branches left without a worktree and the
    /// primary worktree's branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Git's reason or error, for stale entries and broken worktrees; the
    /// default branch, for a primary worktree off it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The command that addresses this finding, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FindingKind {
    RootBranch,
    StaleEntry,
    BrokenWorktree,
    StrayDirectory,
    AgentWorktree,
    AgentDirectory,
    WtBinaries,
    MergedAgentBranch,
    UnmergedAgentBranch,
}

impl Finding {
    /// The finding's headline, styled with `cformat!` tags.
    fn line(&self) -> String {
        let path = self
            .path
            .as_deref()
            .map(format_path_for_display)
            .unwrap_or_default();
        let branch = self.branch.as_deref().unwrap_or_default();
        let detail = self.detail.as_deref().unwrap_or_default();
        match self.kind {
            FindingKind::RootBranch => {
                let on = if branch.is_empty() {
                    "a detached HEAD".to_string()
                } else {
                    cformat!("<bold>{branch}</>")
                };
                cformat!(
                    "Primary worktree @ <bold>{path}</> is on {on}, not the default branch <bold>{detail}</>"
                )
            }
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
            FindingKind::WtBinaries => cformat!(
                "wt binaries report different versions; agents and hooks without <bold>WORKTRUNK_BIN</> run the first on PATH, <bold>{path}</>"
            ),
            FindingKind::MergedAgentBranch => {
                cformat!("Agent branch <bold>{branch}</> has no worktree; merged ({detail})")
            }
            FindingKind::UnmergedAgentBranch => {
                cformat!("Agent branch <bold>{branch}</> has no worktree; {detail}")
            }
        }
    }

    /// The headline as plain text, with the fix command when there is one.
    pub(crate) fn summary(&self) -> String {
        let line = anstream::adapter::strip_str(&self.line()).to_string();
        match &self.fix {
            Some(fix) => format!("{line}; run {fix}"),
            None => line,
        }
    }

    fn render(&self) -> String {
        let warning = warning_message(self.line());
        match (self.kind, &self.detail) {
            (FindingKind::BrokenWorktree | FindingKind::WtBinaries, Some(error)) => {
                format!("{warning}\n{}", format_with_gutter(error, None))
            }
            (FindingKind::RootBranch, _) => format!(
                "{warning}\n{}",
                hint_message(cformat!(
                    "To put it back on the default branch, run <underline>{}</>",
                    self.fix.as_deref().unwrap_or_default()
                ))
            ),
            (FindingKind::MergedAgentBranch, _) => format!(
                "{warning}\n{}",
                hint_message(cformat!(
                    "To delete the merged branch, run <underline>{}</>",
                    self.fix.as_deref().unwrap_or_default()
                ))
            ),
            (FindingKind::UnmergedAgentBranch, _) => format!(
                "{warning}\n{}",
                hint_message(cformat!(
                    "To review the unmerged commits, run <underline>{}</>",
                    self.fix.as_deref().unwrap_or_default()
                ))
            ),
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
    let finding = |kind, path: &Path, detail, fix: Option<&str>| Finding {
        kind,
        path: Some(path.to_path_buf()),
        branch: None,
        detail,
        fix: fix.map(str::to_string),
    };

    let mut findings = Vec::new();
    let default_branch = repo.default_branch();

    // The primary checkout belongs on the default branch. A bare repository's
    // primary is the default branch's worktree, so it never fires there.
    if let Some(primary) = repo.primary_worktree()?
        && let Some(target) = &default_branch
        && let Some(root) = worktrees
            .iter()
            .find(|wt| !wt.is_prunable() && paths_match(&wt.path, &primary))
        && root.branch.as_deref() != Some(target.as_str())
    {
        let dir = shell_escape::unix::escape(root.path.to_string_lossy());
        findings.push(Finding {
            kind: FindingKind::RootBranch,
            path: Some(root.path.clone()),
            branch: root.branch.clone(),
            detail: Some(target.clone()),
            fix: Some(format!("git -C {dir} switch {target}")),
        });
    }

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

    // Branches Claude Code's `Agent` isolation leaves behind (`agent-<hex id>`)
    // once their worktree is gone. Any registration, stale or not, counts as
    // checked out.
    let snapshot = repo.capture_refs()?;
    let leftovers: Vec<&str> = snapshot
        .local_branches()
        .iter()
        .map(|local| local.name.as_str())
        .filter(|name| {
            name.strip_prefix("agent-").is_some_and(|id| {
                id.len() >= 8 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            }) && !worktrees
                .iter()
                .any(|wt| wt.branch.as_deref() == Some(*name))
        })
        .collect();
    if !leftovers.is_empty()
        && let Some(target) = &default_branch
    {
        for name in leftovers {
            let (_, reason) = repo.integration_reason(&snapshot, name, target)?;
            findings.push(match reason {
                Some(reason) => Finding {
                    kind: FindingKind::MergedAgentBranch,
                    path: None,
                    branch: Some(name.to_string()),
                    detail: Some(format!("{} {target}", reason.description())),
                    fix: Some(format!("wt remove {name}")),
                },
                None => Finding {
                    kind: FindingKind::UnmergedAgentBranch,
                    path: None,
                    branch: Some(name.to_string()),
                    detail: Some(format!("not merged into {target}")),
                    fix: Some(format!("git log {target}..{name}")),
                },
            });
        }
    }

    findings.extend(wt_binaries_finding());
    Ok(findings)
}

/// Every distinct `wt` / `git-wt` on PATH, plus `WORKTRUNK_BIN`, with its
/// `--version`; a finding only when they disagree.
fn wt_binaries_finding() -> Option<Finding> {
    let mut binaries: Vec<(PathBuf, PathBuf)> = Vec::new();
    // On Windows `wt` is usually Windows Terminal, which `--version` would
    // open; Worktrunk installs there as `git-wt`.
    let names: &[&str] = if cfg!(windows) {
        &["git-wt"]
    } else {
        &["wt", "git-wt"]
    };
    let candidates = names
        .iter()
        .filter_map(|name| which::which_all(name).ok())
        .flatten()
        .chain(std::env::var_os("WORKTRUNK_BIN").map(PathBuf::from));
    for path in candidates {
        let resolved = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !binaries.iter().any(|(r, _)| *r == resolved) {
            binaries.push((resolved, path));
        }
    }
    let versions: Vec<(PathBuf, String)> = binaries
        .into_iter()
        .map(|(_, path)| {
            let version = Cmd::new(path.to_string_lossy())
                .arg("--version")
                .timeout(Duration::from_secs(2))
                .run()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .unwrap_or_else(|| "failed to run --version".to_string());
            (path, version)
        })
        .collect();
    if versions.iter().all(|(_, v)| *v == versions[0].1) {
        return None;
    }
    let detail = versions
        .iter()
        .map(|(path, version)| format!("{} → {version}", format_path_for_display(path)))
        .collect::<Vec<_>>()
        .join("\n");
    Some(Finding {
        kind: FindingKind::WtBinaries,
        path: Some(versions[0].0.clone()),
        branch: None,
        detail: Some(detail),
        fix: None,
    })
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
