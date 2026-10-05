use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use walkdir::WalkDir;

/// Keep a folder full of Git repos tidy.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Folder to scan for repositories
    #[arg(global = true, default_value = ".")]
    root: PathBuf,

    /// How many directory levels deep to search
    #[arg(long, global = true, default_value_t = 3)]
    depth: usize,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show branch, uncommitted changes, and ahead/behind for each repo
    Status {
        /// Fetch from remotes first so ahead/behind is current
        #[arg(long)]
        fetch: bool,
    },
    /// Fast-forward pull every clean repo that has an upstream (in parallel)
    Pull,
    /// List local branches already merged into the default branch
    Prune {
        /// Actually delete the branches (default is a dry run)
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repos = find_repos(&cli.root, cli.depth);
    if repos.is_empty() {
        println!("No Git repositories found under {}", cli.root.display());
        return Ok(());
    }

    match cli.cmd {
        Cmd::Status { fetch } => status(&repos, fetch),
        Cmd::Pull => pull(&repos),
        Cmd::Prune { yes } => prune(&repos, yes),
    }
    Ok(())
}

fn find_repos(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut repos = Vec::new();
    let mut walker = WalkDir::new(root).max_depth(max_depth).into_iter();

    while let Some(entry) = walker.next() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if entry.depth() > 0
            && (name.starts_with('.') || name == "node_modules" || name == "target")
        {
            walker.skip_current_dir();
            continue;
        }
        if entry.path().join(".git").exists() {
            repos.push(entry.path().to_path_buf());
            walker.skip_current_dir(); // don't descend into a repo looking for more
        }
    }
    repos.sort();
    repos
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("failed to run git (is it installed and on PATH?)")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn current_branch(repo: &Path) -> String {
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|_| "?".into())
}

fn dirty_count(repo: &Path) -> usize {
    git(repo, &["status", "--porcelain"])
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn ahead_behind(repo: &Path) -> Option<(u32, u32)> {
    let out = git(
        repo,
        &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
    )
    .ok()?;
    let mut parts = out.split_whitespace().map(|n| n.parse().unwrap_or(0));
    Some((parts.next()?, parts.next()?))
}

fn default_branch(repo: &Path) -> Option<String> {
    if let Ok(r) = git(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        return r.strip_prefix("origin/").map(str::to_string);
    }
    ["main", "master"]
        .into_iter()
        .map(str::to_string)
        .find(|b| {
            git(
                repo,
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{b}"),
                ],
            )
            .is_ok()
        })
}

fn display_name(repo: &Path) -> String {
    repo.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.display().to_string())
}

fn status(repos: &[PathBuf], fetch: bool) {
    let rows: Vec<_> = repos
        .par_iter()
        .map(|r| {
            if fetch {
                let _ = git(r, &["fetch", "--quiet"]);
            }
            (
                display_name(r),
                current_branch(r),
                dirty_count(r),
                ahead_behind(r),
            )
        })
        .collect();

    let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(4).max(4);
    println!(
        "{:<w$}  {:<20}  {:>7}  {}",
        "REPO", "BRANCH", "CHANGES", "SYNC"
    );
    for (name, branch, dirty, sync) in rows {
        let sync = match sync {
            None => "no upstream".to_string(),
            Some((0, 0)) => "up to date".to_string(),
            Some((a, b)) => format!("↑{a} ↓{b}"),
        };
        let dirty = if dirty == 0 {
            "-".to_string()
        } else {
            dirty.to_string()
        };
        println!("{name:<w$}  {branch:<20}  {dirty:>7}  {sync}");
    }
}

fn pull(repos: &[PathBuf]) {
    let mut results: Vec<_> = repos
        .par_iter()
        .map(|r| {
            let name = display_name(r);
            let msg = if dirty_count(r) > 0 {
                "skipped: uncommitted changes".to_string()
            } else if ahead_behind(r).is_none() {
                "skipped: no upstream".to_string()
            } else {
                match git(r, &["pull", "--ff-only", "--quiet"]) {
                    Ok(_) => "pulled".to_string(),
                    Err(e) => format!("failed: {}", e.to_string().lines().next().unwrap_or("")),
                }
            };
            (name, msg)
        })
        .collect();

    results.sort();
    for (name, msg) in results {
        println!("{name}: {msg}");
    }
}

fn prune(repos: &[PathBuf], delete: bool) {
    let mut total = 0;
    for repo in repos {
        let Some(base) = default_branch(repo) else {
            continue;
        };
        let current = current_branch(repo);
        let Ok(merged) = git(
            repo,
            &["branch", "--merged", &base, "--format=%(refname:short)"],
        ) else {
            continue;
        };
        let stale: Vec<&str> = merged
            .lines()
            .filter(|b| *b != base && *b != current)
            .collect();
        if stale.is_empty() {
            continue;
        }

        println!("{} (merged into {base}):", display_name(repo));
        for branch in stale {
            total += 1;
            if delete {
                // -d (not -D) refuses to delete anything unmerged, as a safety net
                match git(repo, &["branch", "-d", branch]) {
                    Ok(_) => println!("  deleted  {branch}"),
                    Err(e) => println!("  kept     {branch} ({e})"),
                }
            } else {
                println!("  would delete  {branch}");
            }
        }
    }

    match (total, delete) {
        (0, _) => println!("No merged branches to clean up."),
        (_, false) => println!("\nDry run: {total} branch(es). Re-run with --yes to delete."),
        (_, true) => {}
    }
}
