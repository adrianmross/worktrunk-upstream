//! `wt step restore` — recreate a removed branch and worktree from
//! `<git-common-dir>/wt/removed.log`, which `wt remove` appends to.

use std::collections::HashSet;

use anyhow::{Context, bail};
use color_print::cformat;
use serde::{Deserialize, Serialize};
use worktrunk::git::Repository;
use worktrunk::styling::{eprintln, info_message, println, success_message};

use crate::cli::SwitchFormat;
use crate::commands::worktree::switch_to_branch;
use crate::output::print_json;

#[derive(Deserialize, Serialize)]
struct Removal {
    time: String,
    event: String,
    branch: String,
    sha: String,
    path: Option<String>,
    agent: Option<String>,
}

/// Restorable records, newest first, one per branch and commit. A removal
/// writes both a `remove_worktree` and a `delete_branch` line for the same
/// tip, so the older duplicate is dropped. Lines without a branch or SHA
/// (detached worktrees) and unparseable lines are skipped.
fn load_removals(repo: &Repository) -> anyhow::Result<Vec<Removal>> {
    let path = repo.wt_dir().join("removed.log");
    let log = match std::fs::read_to_string(&path) {
        Ok(log) => log,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut seen = HashSet::new();
    Ok(log
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Removal>(line).ok())
        .filter(|r| seen.insert((r.branch.clone(), r.sha.clone())))
        .collect())
}

pub(crate) fn step_restore(
    branch: Option<&str>,
    yes: bool,
    format: SwitchFormat,
) -> anyhow::Result<()> {
    let repo = Repository::current()?;
    let removals = load_removals(&repo)?;

    let Some(branch) = branch else {
        if format == SwitchFormat::Json {
            return print_json(&removals);
        }
        if removals.is_empty() {
            eprintln!("{}", info_message("No removals logged"));
        }
        for r in &removals {
            let sha = r.sha.get(..7).unwrap_or(&r.sha);
            let agent = r.agent.as_deref().unwrap_or("-");
            println!("{}  {}  {sha}  {agent}", r.time, r.branch);
        }
        return Ok(());
    };

    let Some(removal) = removals.iter().find(|r| r.branch == branch) else {
        bail!(
            "No removal of {branch} in {}",
            repo.wt_dir().join("removed.log").display()
        );
    };
    let sha = &removal.sha;

    match repo.capture_refs()?.local_branch(branch) {
        Some(existing) if existing.commit_sha == *sha => {}
        Some(existing) => bail!(
            "Branch {branch} already exists at {}, not the removed {sha}; delete or rename it first",
            existing.commit_sha
        ),
        None => {
            if !repo.ref_exists(sha)? {
                bail!("Commit {sha} for {branch} no longer exists (garbage-collected)");
            }
            repo.run_command(&["branch", branch, sha])?;
            if format != SwitchFormat::Json {
                eprintln!(
                    "{}",
                    success_message(cformat!("Recreated branch <bold>{branch}</> at {sha}"))
                );
            }
        }
    }

    switch_to_branch(branch, yes, format)
}
