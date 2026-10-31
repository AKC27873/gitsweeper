use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use colored::{ColoredString, Colorize};
use rayon::prelude::*;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(version, about, propagate_version = true)]
struct Cli {
    #[arg(
        short = 'C',
        long = "root",
        global = true,
        default_value = ".",
        env = "GITSWEEP_ROOT",
        value_name = "DIR"
    )]
    root: PathBuf,

    #[arg(long, global = true, default_value_t = 3)]
    depth: usize,

    #[arg(short, long, global = true, value_name = "TEXT")]
    filter: Vec<String>,

    #[arg(short = 'x', long, global = true, value_name = "TEXT")]
    exclude: Vec<String>,

    #[arg(short, long, global = true, value_name = "N")]
    jobs: Option<usize>,

    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(visible_alias = "st")]
    Status {
        #[arg(long)]
        fetch: bool,
        #[arg(short, long)]
        dirty: bool,
    },
    Fetch {
        #[arg(long)]
        prune: bool,
    },
    Pull,
    Push {
        #[arg(long)]
        yes: bool,
    },
    Switch {
        branch: Option<String>,
    },
    Prune {
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        gone: bool,
        #[arg(long, requires = "yes")]
        force: bool,
    },
    #[command(visible_alias = "br")]
    Branches,
    Recent {
        #[arg(short = 'n', long = "count", default_value_t = 20)]
        count: usize,
    },
    Grep {
        pattern: String,
        #[arg(short, long)]
        ignore_case: bool,
        #[arg(short = 'l', long)]
        files: bool,
    },
    Run {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Exec {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    #[command(visible_alias = "ls")]
    List,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.no_color {
        colored::control::set_override(false);
    }
    if let Some(n) = cli.jobs {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(n.max(1))
            .build_global();
    }

    match run(cli) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{} {e:#}", "error:".red().bold());
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<usize> {
    let root = cli
        .root
        .canonicalize()
        .with_context(|| format!("can't open {}", cli.root.display()))?;
    let repos: Vec<Repo> = find_repos(&root, cli.depth)
        .into_iter()
        .filter(|r| r.matches(&cli.filter, &cli.exclude))
        .collect();

    if repos.is_empty() {
        println!(
            "{} {}",
            "No matching Git repositories found under".yellow(),
            root.display().to_string().bold()
        );
        return Ok(0);
    }

    Ok(match cli.cmd {
        Cmd::Status { fetch, dirty } => cmd_status(&repos, fetch, dirty),
        Cmd::Fetch { prune } => cmd_fetch(&repos, prune),
        Cmd::Pull => cmd_pull(&repos),
        Cmd::Push { yes } => cmd_push(&repos, yes),
        Cmd::Switch { branch } => cmd_switch(&repos, branch.as_deref()),
        Cmd::Prune { yes, gone, force } => cmd_prune(&repos, yes, gone, force),
        Cmd::Branches => cmd_branches(&repos),
        Cmd::Recent { count } => cmd_recent(&repos, count),
        Cmd::Grep {
            pattern,
            ignore_case,
            files,
        } => cmd_grep(&repos, &pattern, ignore_case, files),
        Cmd::Run { args } => cmd_each(&repos, &args, true)?,
        Cmd::Exec { args } => cmd_each(&repos, &args, false)?,
        Cmd::List => {
            for r in &repos {
                println!("{}", r.path.display());
            }
            0
        }
    })
}

struct Repo {
    path: PathBuf,
    name: String,
}

impl Repo {
    fn matches(&self, include: &[String], exclude: &[String]) -> bool {
        let name = self.name.to_lowercase();
        let has = |t: &String| name.contains(&t.to_lowercase());
        (include.is_empty() || include.iter().any(has)) && !exclude.iter().any(has)
    }
}

fn find_repos(root: &Path, max_depth: usize) -> Vec<Repo> {
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
            let path = entry.path().to_path_buf();
            let name = match path.strip_prefix(root) {
                Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
                _ => path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            };
            repos.push(Repo { path, name });
            walker.skip_current_dir();
        }
    }
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    repos
}

fn git_output(repo: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("failed to run git (is it installed and on PATH?)")
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = git_output(repo, args)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if stderr.is_empty() {
            bail!(
                "git {} exited with {}",
                args.first().unwrap_or(&""),
                out.status
            );
        }
        bail!("{stderr}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn short_err(e: &anyhow::Error) -> String {
    let s = e.to_string();
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("hint:"))
        .unwrap_or("unknown error");
    line.trim_start_matches("fatal: ")
        .trim_start_matches("error: ")
        .to_string()
}

fn head(repo: &Path) -> Option<String> {
    git(repo, &["rev-parse", "HEAD"]).ok()
}

fn current_branch(repo: &Path) -> String {
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|_| "?".into())
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
        .find(|b| local_branch_exists(repo, b))
}

fn local_branch_exists(repo: &Path, branch: &str) -> bool {
    git(
        repo,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

fn remote_branch_exists(repo: &Path, branch: &str) -> bool {
    git(
        repo,
        &[
            "for-each-ref",
            "--count=1",
            "--format=x",
            &format!("refs/remotes/*/{branch}"),
        ],
    )
    .is_ok_and(|s| !s.is_empty())
}

#[derive(Default, Clone, Copy, PartialEq)]
enum SyncState {
    #[default]
    NoUpstream,
    Gone,
    Tracking {
        ahead: u32,
        behind: u32,
    },
}

const IN_SYNC: SyncState = SyncState::Tracking {
    ahead: 0,
    behind: 0,
};

#[derive(Default)]
struct RepoStatus {
    branch: String,
    detached: bool,
    sync: SyncState,
    staged: usize,
    modified: usize,
    untracked: usize,
    conflicts: usize,
    stashes: usize,
    last_commit: Option<i64>,
    error: Option<String>,
}

impl RepoStatus {
    fn has_tracked_changes(&self) -> bool {
        self.staged + self.modified + self.conflicts > 0
    }
    fn is_dirty(&self) -> bool {
        self.has_tracked_changes() || self.untracked > 0
    }
    fn needs_attention(&self) -> bool {
        self.is_dirty() || self.stashes > 0 || self.error.is_some() || self.sync != IN_SYNC
    }
}

fn read_status(repo: &Path) -> Result<RepoStatus> {
    let out = git(repo, &["status", "--porcelain=v2", "--branch"])?;
    let mut st = RepoStatus::default();
    let mut oid = "";
    let mut has_upstream = false;
    let mut ab = None;

    for line in out.lines() {
        if let Some(v) = line.strip_prefix("# branch.oid ") {
            oid = v;
        } else if let Some(v) = line.strip_prefix("# branch.head ") {
            st.branch = v.to_string();
        } else if line.starts_with("# branch.upstream ") {
            has_upstream = true;
        } else if let Some(v) = line.strip_prefix("# branch.ab ") {
            ab = parse_ab(v);
        } else {
            match line.as_bytes().first() {
                Some(b'1' | b'2') => {
                    let xy = line.as_bytes().get(2..4).unwrap_or(b"..");
                    if xy[0] != b'.' {
                        st.staged += 1;
                    }
                    if xy[1] != b'.' {
                        st.modified += 1;
                    }
                }
                Some(b'u') => st.conflicts += 1,
                Some(b'?') => st.untracked += 1,
                _ => {}
            }
        }
    }

    if st.branch == "(detached)" {
        st.detached = true;
        st.branch = format!("@{}", oid.get(..7).unwrap_or(oid));
    }
    st.sync = match (has_upstream, ab) {
        (false, _) => SyncState::NoUpstream,
        (true, None) => SyncState::Gone,
        (true, Some((ahead, behind))) => SyncState::Tracking { ahead, behind },
    };
    st.stashes = git(repo, &["stash", "list"])
        .map(|s| s.lines().count())
        .unwrap_or(0);
    st.last_commit = git(repo, &["log", "-1", "--format=%ct"])
        .ok()
        .and_then(|s| s.parse().ok());
    Ok(st)
}

fn parse_ab(v: &str) -> Option<(u32, u32)> {
    let mut it = v.split_whitespace();
    let ahead = it.next()?.trim_start_matches('+').parse().ok()?;
    let behind = it.next()?.trim_start_matches('-').parse().ok()?;
    Some((ahead, behind))
}

fn cmd_status(repos: &[Repo], fetch: bool, only_attention: bool) -> usize {
    let progress = fetch.then(|| Progress::new("Fetching", repos.len()));
    let rows: Vec<(&Repo, RepoStatus)> = repos
        .par_iter()
        .map(|r| {
            let fetch_err = if fetch {
                git(&r.path, &["fetch", "--quiet"]).err()
            } else {
                None
            };
            let mut st = read_status(&r.path).unwrap_or_else(|e| RepoStatus {
                branch: "?".into(),
                error: Some(short_err(&e)),
                ..Default::default()
            });
            if st.error.is_none() {
                st.error = fetch_err.map(|e| format!("fetch failed: {}", short_err(&e)));
            }
            if let Some(p) = &progress {
                p.tick();
            }
            (r, st)
        })
        .collect();
    if let Some(p) = &progress {
        p.finish();
    }

    let shown: Vec<_> = rows
        .iter()
        .filter(|(_, s)| !only_attention || s.needs_attention())
        .collect();

    if shown.is_empty() {
        println!(
            "{} {}",
            "✓".green().bold(),
            format!("All {} repos are clean and in sync.", rows.len()).green()
        );
        return 0;
    }

    let mut table =
        Table::with_header(&["REPO", "BRANCH", "CHANGES", "SYNC", "STASH", "LAST COMMIT"]);
    for (r, st) in &shown {
        let name = if st.error.is_some() {
            r.name.red().bold()
        } else {
            r.name.bold()
        };
        let branch = truncate(&st.branch, 30);
        let branch = if st.detached {
            branch.yellow()
        } else {
            branch.cyan()
        };
        let stash = if st.stashes > 0 {
            format!("≡{}", st.stashes).blue()
        } else {
            "".normal()
        };
        let error = st
            .error
            .as_deref()
            .map(|e| format!("⚠ {}", truncate(e, 60)).red())
            .unwrap_or_else(|| "".normal());
        table.add(vec![
            name.into(),
            branch.into(),
            changes_cell(st),
            sync_cell(st.sync),
            stash.into(),
            age_cell(st.last_commit),
            error.into(),
        ]);
    }
    table.print();

    let count = |f: &dyn Fn(&RepoStatus) -> bool| rows.iter().filter(|(_, s)| f(s)).count();
    let dirty = count(&|s| s.is_dirty());
    let ahead = count(&|s| matches!(s.sync, SyncState::Tracking { ahead, .. } if ahead > 0));
    let behind = count(&|s| matches!(s.sync, SyncState::Tracking { behind, .. } if behind > 0));
    let no_up = count(&|s| s.sync == SyncState::NoUpstream);
    let gone = count(&|s| s.sync == SyncState::Gone);
    let stashed = count(&|s| s.stashes > 0);
    let errors = count(&|s| s.error.is_some());

    let mut parts = vec![format!("{} repos", rows.len()).bold()];
    let mut add = |n: usize, label: &str, color: fn(String) -> ColoredString| {
        if n > 0 {
            parts.push(color(format!("{n} {label}")));
        }
    };
    add(dirty, "dirty", |s| s.yellow());
    add(ahead, "ahead", |s| s.cyan());
    add(behind, "behind", |s| s.yellow());
    add(gone, "upstream gone", |s| s.red());
    add(no_up, "no upstream", |s| s.dimmed());
    add(stashed, "with stashes", |s| s.blue());
    add(errors, "errors", |s| s.red().bold());
    let sep = " · ".dimmed().to_string();
    println!(
        "\n{}",
        parts
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(&sep)
    );
    if dirty > 0 {
        println!(
            "{}",
            "+staged  ~modified  ?untracked  !conflicts  ≡stashes".dimmed()
        );
    }
    errors
}

fn changes_cell(st: &RepoStatus) -> Cell {
    let mut c = Cell::default();
    if st.conflicts > 0 {
        c.push(format!("!{}", st.conflicts).magenta().bold());
    }
    if st.staged > 0 {
        c.push(format!("+{}", st.staged).green());
    }
    if st.modified > 0 {
        c.push(format!("~{}", st.modified).yellow());
    }
    if st.untracked > 0 {
        c.push(format!("?{}", st.untracked).red());
    }
    if c.width == 0 {
        c.push("clean".green());
    }
    c
}

fn sync_cell(sync: SyncState) -> Cell {
    match sync {
        SyncState::NoUpstream => "no upstream".dimmed(),
        SyncState::Gone => "upstream gone".red(),
        SyncState::Tracking {
            ahead: 0,
            behind: 0,
        } => "up to date".green(),
        SyncState::Tracking { ahead, behind: 0 } => format!("↑{ahead}").cyan().bold(),
        SyncState::Tracking { ahead: 0, behind } => format!("↓{behind}").yellow().bold(),
        SyncState::Tracking { ahead, behind } => {
            format!("↑{ahead} ↓{behind} diverged").red().bold()
        }
    }
    .into()
}

fn age_cell(ts: Option<i64>) -> Cell {
    let Some(ts) = ts else {
        return "no commits".dimmed().into();
    };
    let days = (now() - ts) / 86_400;
    let s = ago(ts);
    if days < 1 {
        s.green()
    } else if days > 180 {
        s.dimmed()
    } else {
        s.normal()
    }
    .into()
}

fn cmd_fetch(repos: &[Repo], prune: bool) -> usize {
    let results = in_parallel(repos, "Fetching", |r| {
        let has_remote = git(&r.path, &["remote"]).is_ok_and(|s| !s.is_empty());
        if !has_remote {
            return Outcome::noop("no remotes");
        }
        let mut args = vec!["fetch", "--all", "--quiet"];
        if prune {
            args.push("--prune");
        }
        if let Err(e) = git(&r.path, &args) {
            return Outcome::failed(short_err(&e));
        }
        match read_status(&r.path).map(|s| s.sync) {
            Ok(SyncState::Tracking { behind, .. }) if behind > 0 => {
                Outcome::done(format!("fetched · ↓{behind} to pull"))
            }
            _ => Outcome::done("fetched"),
        }
    });
    print_outcomes(
        &results,
        ["fetched", "without remotes", "", "skipped", "failed"],
    )
}

fn cmd_pull(repos: &[Repo]) -> usize {
    let results = in_parallel(repos, "Pulling", |r| {
        let path = &r.path;
        let st = match read_status(path) {
            Ok(s) => s,
            Err(e) => return Outcome::failed(short_err(&e)),
        };
        if st.detached {
            return Outcome::skipped("detached HEAD");
        }
        // Untracked files are fine — git refuses on its own if one would be overwritten.
        if st.has_tracked_changes() {
            return Outcome::skipped("uncommitted changes");
        }
        match st.sync {
            SyncState::NoUpstream => return Outcome::skipped("no upstream"),
            SyncState::Gone => return Outcome::skipped("upstream branch was deleted"),
            SyncState::Tracking { .. } => {}
        }

        let before = head(path);
        if let Err(e) = git(path, &["pull", "--ff-only", "--quiet"]) {
            let msg = short_err(&e);
            return Outcome::failed(if msg.contains("fast-forward") {
                "diverged from upstream — needs a merge or rebase".to_string()
            } else {
                msg
            });
        }
        match (before, head(path)) {
            (Some(b), Some(a)) if a != b => {
                let n: usize = git(path, &["rev-list", "--count", &format!("{b}..{a}")])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                Outcome::done(format!("pulled {}", plural(n, "commit")))
            }
            _ => Outcome::noop("already up to date"),
        }
    });
    print_outcomes(&results, ["pulled", "up to date", "", "skipped", "failed"])
}

fn cmd_push(repos: &[Repo], yes: bool) -> usize {
    let label = if yes { "Pushing" } else { "Checking" };
    let results = in_parallel(repos, label, |r| {
        let st = match read_status(&r.path) {
            Ok(s) => s,
            Err(e) => return Outcome::failed(short_err(&e)),
        };
        if st.detached {
            return Outcome::skipped("detached HEAD");
        }
        match st.sync {
            SyncState::NoUpstream => Outcome::noop("no upstream"),
            SyncState::Gone => Outcome::skipped("upstream branch was deleted"),
            SyncState::Tracking { ahead: 0, .. } => Outcome::noop("nothing to push"),
            SyncState::Tracking { behind, .. } if behind > 0 => {
                Outcome::skipped(format!("behind by {behind} — pull first"))
            }
            SyncState::Tracking { ahead, .. } if !yes => {
                Outcome::planned(format!("would push {}", plural(ahead as usize, "commit")))
            }
            SyncState::Tracking { ahead, .. } => match git(&r.path, &["push", "--quiet"]) {
                Ok(_) => Outcome::done(format!("pushed {}", plural(ahead as usize, "commit"))),
                Err(e) => Outcome::failed(short_err(&e)),
            },
        }
    });
    let failures = print_outcomes(
        &results,
        ["pushed", "nothing to push", "to push", "skipped", "failed"],
    );
    if !yes && results.iter().any(|(_, o)| o.kind == Kind::Planned) {
        println!("{}", "Dry run — re-run with --yes to push.".dimmed());
    }
    failures
}

fn cmd_switch(repos: &[Repo], target: Option<&str>) -> usize {
    let results = in_parallel(repos, "Switching", |r| {
        let path = &r.path;
        let target = match target {
            Some(b) => b.to_string(),
            None => match default_branch(path) {
                Some(b) => b,
                None => return Outcome::skipped("can't tell which branch is the default"),
            },
        };
        let st = match read_status(path) {
            Ok(s) => s,
            Err(e) => return Outcome::failed(short_err(&e)),
        };
        if !st.detached && st.branch == target {
            return Outcome::noop(format!("already on {target}"));
        }
        if !local_branch_exists(path, &target) && !remote_branch_exists(path, &target) {
            return Outcome::noop(format!("no {target} branch"));
        }
        if st.has_tracked_changes() {
            return Outcome::skipped("uncommitted changes");
        }
        match git(path, &["switch", "--quiet", &target]) {
            Ok(_) => Outcome::done(format!("{} → {target}", st.branch)),
            Err(e) => Outcome::failed(short_err(&e)),
        }
    });
    print_outcomes(&results, ["switched", "unchanged", "", "skipped", "failed"])
}

#[derive(PartialEq)]
enum Reason {
    Merged,
    Gone,
}

struct PrunePlan<'a> {
    repo: &'a Repo,
    base: String,
    branches: Vec<(String, Reason)>,
}

fn plan_prune(repo: &Repo, include_gone: bool) -> Option<PrunePlan<'_>> {
    let path = &repo.path;
    let base = default_branch(path)?;
    let current = current_branch(path);
    let keep = |b: &str| b == base || b == current;

    let merged = git(
        path,
        &["branch", "--merged", &base, "--format=%(refname:short)"],
    )
    .ok()?;
    let mut branches: Vec<(String, Reason)> = merged
        .lines()
        .filter(|b| !keep(b))
        .map(|b| (b.to_string(), Reason::Merged))
        .collect();

    if include_gone {
        let refs = git(
            path,
            &[
                "for-each-ref",
                "--format=%(refname:short)\t%(upstream:track)",
                "refs/heads",
            ],
        )
        .unwrap_or_default();
        for line in refs.lines() {
            let Some((name, track)) = line.split_once('\t') else {
                continue;
            };
            if track == "[gone]" && !keep(name) && !branches.iter().any(|(b, _)| b == name) {
                branches.push((name.to_string(), Reason::Gone));
            }
        }
    }

    (!branches.is_empty()).then_some(PrunePlan {
        repo,
        base,
        branches,
    })
}

fn cmd_prune(repos: &[Repo], delete: bool, gone: bool, force: bool) -> usize {
    let plans: Vec<PrunePlan> = repos
        .par_iter()
        .filter_map(|r| plan_prune(r, gone))
        .collect();

    if plans.is_empty() {
        println!(
            "{} {}",
            "✓".green().bold(),
            "No branches to clean up.".green()
        );
        return 0;
    }

    let w = plans
        .iter()
        .flat_map(|p| p.branches.iter().map(|(b, _)| b.chars().count()))
        .max()
        .unwrap_or(0);
    let (mut total, mut deleted, mut kept) = (0, 0, 0);

    for plan in &plans {
        println!(
            "{} {}",
            plan.repo.name.bold(),
            format!("(base: {})", plan.base).dimmed()
        );
        for (branch, reason) in &plan.branches {
            total += 1;
            let tag = match reason {
                Reason::Merged => "merged".dimmed(),
                Reason::Gone => "upstream gone".yellow(),
            };
            if !delete {
                println!("  {} {}  {tag}", "→".cyan(), pad(branch, w).cyan());
                continue;
            }
            // -d (not -D) refuses to delete anything unmerged, as a safety net
            let flag = if force { "-D" } else { "-d" };
            match git(&plan.repo.path, &["branch", flag, branch]) {
                Ok(_) => {
                    deleted += 1;
                    println!("  {} {}  {tag}", "✓".green(), pad(branch, w).green());
                }
                Err(e) => {
                    kept += 1;
                    let msg = short_err(&e);
                    let why = if msg.contains("not fully merged") {
                        "not fully merged — add --force to delete anyway".to_string()
                    } else {
                        msg
                    };
                    println!(
                        "  {} {}  {}",
                        "–".yellow(),
                        pad(branch, w).yellow(),
                        format!("kept: {why}").yellow()
                    );
                }
            }
        }
    }

    println!();
    if delete {
        let mut line = format!("Deleted {}", plural(deleted, "branch"))
            .green()
            .to_string();
        if kept > 0 {
            line += &format!(" · kept {kept}").yellow().to_string();
        }
        println!("{line}");
    } else {
        println!(
            "{} {}",
            format!(
                "Dry run: {} in {}.",
                plural(total, "branch"),
                plural(plans.len(), "repo")
            )
            .bold(),
            "Re-run with --yes to delete.".dimmed()
        );
    }
    0
}

fn cmd_branches(repos: &[Repo]) -> usize {
    let format = "--format=%(HEAD)\t%(refname:short)\t%(upstream:short)\t%(upstream:track,nobracket)\t%(committerdate:unix)";
    let data: Vec<(&Repo, Result<String>)> = repos
        .par_iter()
        .map(|r| {
            (
                r,
                git(
                    &r.path,
                    &[
                        "for-each-ref",
                        "--sort=-committerdate",
                        format,
                        "refs/heads",
                    ],
                ),
            )
        })
        .collect();

    let mut table = Table::default();
    let mut failures = 0;
    for (i, (r, out)) in data.iter().enumerate() {
        let gap = if i == 0 { "" } else { "\n" };
        table.heading(format!("{gap}{} {}", "▸".blue().bold(), r.name.bold()));
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                failures += 1;
                table.heading(format!("  {}", short_err(e).red()));
                continue;
            }
        };
        if out.is_empty() {
            table.heading(format!("  {}", "no branches yet".dimmed()));
        }
        for line in out.lines() {
            let f: Vec<&str> = line.splitn(5, '\t').collect();
            let [mark, name, upstream, track, ts] = f[..] else {
                continue;
            };
            let current = mark == "*";
            let sync = match (upstream, track) {
                ("", _) => SyncState::NoUpstream,
                (_, "gone") => SyncState::Gone,
                _ => {
                    let num = |key: &str| {
                        track
                            .split(", ")
                            .find_map(|p| p.strip_prefix(key))
                            .and_then(|n| n.trim().parse().ok())
                            .unwrap_or(0)
                    };
                    SyncState::Tracking {
                        ahead: num("ahead"),
                        behind: num("behind"),
                    }
                }
            };
            let sync_c = if sync == SyncState::NoUpstream {
                "local only".dimmed().into()
            } else {
                sync_cell(sync)
            };
            table.add(vec![
                if current {
                    "  *".green().bold()
                } else {
                    "   ".normal()
                }
                .into(),
                if current {
                    name.green().bold()
                } else {
                    name.normal()
                }
                .into(),
                sync_c,
                upstream.dimmed().into(),
                age_cell(ts.parse().ok()),
            ]);
        }
    }
    table.print();
    failures
}

fn cmd_recent(repos: &[Repo], count: usize) -> usize {
    let mut rows: Vec<(&Repo, i64, String, String, String)> = repos
        .par_iter()
        .filter_map(|r| {
            let out = git(
                &r.path,
                &[
                    "for-each-ref",
                    "--sort=-committerdate",
                    "--count=1",
                    "--format=%(committerdate:unix)\t%(refname:short)\t%(authorname)\t%(subject)",
                    "refs/heads",
                ],
            )
            .ok()?;
            let mut f = out.splitn(4, '\t');
            let ts = f.next()?.parse().ok()?;
            Some((
                r,
                ts,
                f.next()?.to_string(),
                f.next()?.to_string(),
                f.next().unwrap_or("").to_string(),
            ))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    rows.truncate(count);

    let mut table = Table::with_header(&["WHEN", "REPO", "BRANCH", "AUTHOR", "SUBJECT"]);
    for (r, ts, branch, author, subject) in rows {
        table.add(vec![
            age_cell(Some(ts)),
            r.name.bold().into(),
            truncate(&branch, 25).cyan().into(),
            truncate(&author, 20).blue().into(),
            truncate(&subject, 60).normal().into(),
        ]);
    }
    table.print();
    0
}

fn cmd_grep(repos: &[Repo], pattern: &str, ignore_case: bool, files: bool) -> usize {
    let color = colored::control::SHOULD_COLORIZE.should_colorize();
    let results: Vec<(&Repo, Result<String>)> = repos
        .par_iter()
        .map(|r| {
            let mut args = vec!["grep", "-I"];
            args.push(if color {
                "--color=always"
            } else {
                "--color=never"
            });
            args.push(if files { "-l" } else { "-n" });
            if ignore_case {
                args.push("-i");
            }
            args.extend(["-e", pattern]);
            let res = git_output(&r.path, &args).and_then(|o| match o.status.code() {
                Some(0) => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
                Some(1) => Ok(String::new()), // no matches
                _ => Err(anyhow!("{}", String::from_utf8_lossy(&o.stderr))),
            });
            (r, res)
        })
        .collect();

    let (mut hits, mut hit_repos, mut failures) = (0, 0, 0);
    for (r, res) in &results {
        match res {
            Ok(out) if out.is_empty() => {}
            Ok(out) => {
                hit_repos += 1;
                let prefix = format!("{}/", r.name).blue().bold();
                for line in out.lines() {
                    hits += 1;
                    println!("{prefix}{line}");
                }
            }
            Err(e) => {
                failures += 1;
                eprintln!(
                    "{} {}: {}",
                    "✗".red().bold(),
                    r.name.bold(),
                    short_err(e).red()
                );
            }
        }
    }
    let what = if files { "file" } else { "match" };
    let summary = format!("{} in {}", plural(hits, what), plural(hit_repos, "repo"));
    eprintln!(
        "\n{}",
        if hits == 0 {
            summary.yellow()
        } else {
            summary.green()
        }
    );
    failures
}

fn cmd_each(repos: &[Repo], argv: &[String], via_git: bool) -> Result<usize> {
    let color = colored::control::SHOULD_COLORIZE.should_colorize();
    let progress = Progress::new("Running", repos.len());
    let results: Vec<(&Repo, std::io::Result<Output>)> = repos
        .par_iter()
        .map(|r| {
            let mut cmd = if via_git {
                let mut c = Command::new("git");
                c.arg("-C").arg(&r.path);
                if color {
                    c.args(["-c", "color.ui=always"]);
                }
                c.args(argv);
                c
            } else {
                let mut c = Command::new(&argv[0]);
                c.args(&argv[1..]).current_dir(&r.path);
                c
            };
            cmd.stdin(Stdio::null()).env("GIT_TERMINAL_PROMPT", "0");
            let out = cmd.output();
            progress.tick();
            (r, out)
        })
        .collect();
    progress.finish();

    if let Some(Err(e)) = results.first().map(|(_, o)| o)
        && results.iter().all(|(_, o)| o.is_err())
    {
        let prog = if via_git { "git" } else { &argv[0] };
        bail!("couldn't run `{prog}`: {e}");
    }

    let (mut failed, mut quiet) = (0, 0);
    let mut first = true;
    for (r, out) in &results {
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                failed += 1;
                println!(
                    "{} {}  {}",
                    "✗".red().bold(),
                    r.name.bold(),
                    e.to_string().red()
                );
                continue;
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let ok = out.status.success();
        if ok && stdout.trim().is_empty() && stderr.trim().is_empty() {
            quiet += 1;
            continue;
        }
        if !first {
            println!();
        }
        first = false;
        let mark = if ok {
            "▸".blue().bold()
        } else {
            "✗".red().bold()
        };
        println!("{mark} {}", r.name.bold());
        print!("{}", stdout.trim_end_matches('\n'));
        if !stdout.trim().is_empty() {
            println!();
        }
        for line in stderr.lines() {
            println!("{}", if ok { line.dimmed() } else { line.red() });
        }
        if !ok {
            failed += 1;
            let code = out
                .status
                .code()
                .map_or("killed by a signal".to_string(), |c| {
                    format!("exit code {c}")
                });
            println!("{}", code.red());
        }
    }

    let mut parts = vec![format!("Ran in {}", plural(results.len(), "repo")).bold()];
    if quiet > 0 {
        parts.push(format!("{quiet} with no output").dimmed());
    }
    if failed > 0 {
        parts.push(format!("{failed} failed").red().bold());
    }
    let sep = " · ".dimmed().to_string();
    println!(
        "\n{}",
        parts
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(&sep)
    );
    Ok(failed)
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Done,
    Noop,
    Planned,
    Skipped,
    Failed,
}

struct Outcome {
    kind: Kind,
    msg: String,
}

impl Outcome {
    fn new(kind: Kind, msg: impl Into<String>) -> Self {
        Self {
            kind,
            msg: msg.into(),
        }
    }
    fn done(m: impl Into<String>) -> Self {
        Self::new(Kind::Done, m)
    }
    fn noop(m: impl Into<String>) -> Self {
        Self::new(Kind::Noop, m)
    }
    fn planned(m: impl Into<String>) -> Self {
        Self::new(Kind::Planned, m)
    }
    fn skipped(m: impl Into<String>) -> Self {
        Self::new(Kind::Skipped, m)
    }
    fn failed(m: impl Into<String>) -> Self {
        Self::new(Kind::Failed, m)
    }
}

fn in_parallel<'a, F>(repos: &'a [Repo], label: &'static str, f: F) -> Vec<(&'a Repo, Outcome)>
where
    F: Fn(&Repo) -> Outcome + Sync + Send,
{
    let progress = Progress::new(label, repos.len());
    let out = repos
        .par_iter()
        .map(|r| {
            let o = f(r);
            progress.tick();
            (r, o)
        })
        .collect();
    progress.finish();
    out
}

fn print_outcomes(results: &[(&Repo, Outcome)], labels: [&str; 5]) -> usize {
    let w = results
        .iter()
        .map(|(r, _)| r.name.chars().count())
        .max()
        .unwrap_or(0);
    for (r, o) in results {
        let (sym, msg) = match o.kind {
            Kind::Done => ("✓".green().bold(), o.msg.green()),
            Kind::Noop => ("·".dimmed(), o.msg.dimmed()),
            Kind::Planned => ("→".cyan().bold(), o.msg.cyan()),
            Kind::Skipped => ("–".yellow().bold(), o.msg.yellow()),
            Kind::Failed => ("✗".red().bold(), o.msg.red()),
        };
        let name = pad(&r.name, w);
        let name = if o.kind == Kind::Noop {
            name.dimmed()
        } else {
            name.bold()
        };
        println!("{sym} {name}  {msg}");
    }

    let kinds = [
        Kind::Done,
        Kind::Noop,
        Kind::Planned,
        Kind::Skipped,
        Kind::Failed,
    ];
    let parts: Vec<String> = kinds
        .iter()
        .zip(labels)
        .filter_map(|(k, label)| {
            let n = results.iter().filter(|(_, o)| o.kind == *k).count();
            (n > 0 && !label.is_empty()).then(|| {
                let s = format!("{n} {label}");
                match k {
                    Kind::Done => s.green().bold(),
                    Kind::Noop => s.dimmed(),
                    Kind::Planned => s.cyan().bold(),
                    Kind::Skipped => s.yellow(),
                    Kind::Failed => s.red().bold(),
                }
                .to_string()
            })
        })
        .collect();
    if !parts.is_empty() {
        println!("\n{}", parts.join(&" · ".dimmed().to_string()));
    }
    results
        .iter()
        .filter(|(_, o)| o.kind == Kind::Failed)
        .count()
}

#[derive(Default)]
struct Cell {
    text: String,
    width: usize,
}

impl Cell {
    fn push(&mut self, s: ColoredString) {
        if s.is_empty() {
            return;
        }
        if self.width > 0 {
            self.text.push(' ');
            self.width += 1;
        }
        self.width += s.chars().count();
        self.text.push_str(&s.to_string());
    }
}

impl From<ColoredString> for Cell {
    fn from(s: ColoredString) -> Self {
        let mut c = Cell::default();
        c.push(s);
        c
    }
}

enum Row {
    Cells(Vec<Cell>),
    Heading(String),
}

#[derive(Default)]
struct Table {
    rows: Vec<Row>,
}

impl Table {
    fn with_header(names: &[&str]) -> Self {
        let mut t = Table::default();
        t.add(names.iter().map(|n| n.bold().dimmed().into()).collect());
        t
    }
    fn add(&mut self, cells: Vec<Cell>) {
        self.rows.push(Row::Cells(cells));
    }
    fn heading(&mut self, text: String) {
        self.rows.push(Row::Heading(text));
    }
    fn print(&self) {
        let mut widths: Vec<usize> = Vec::new();
        for row in &self.rows {
            if let Row::Cells(cells) = row {
                for (i, c) in cells.iter().enumerate() {
                    if widths.len() <= i {
                        widths.push(0);
                    }
                    widths[i] = widths[i].max(c.width);
                }
            }
        }
        for row in &self.rows {
            match row {
                Row::Heading(h) => println!("{h}"),
                Row::Cells(cells) => {
                    let mut line = String::new();
                    for (i, c) in cells.iter().enumerate() {
                        line.push_str(&c.text);
                        if i + 1 < cells.len() {
                            line.push_str(&" ".repeat(widths[i] - c.width + 2));
                        }
                    }
                    println!("{}", line.trim_end());
                }
            }
        }
    }
}

struct Progress {
    label: &'static str,
    total: usize,
    done: AtomicUsize,
    on: bool,
}

impl Progress {
    fn new(label: &'static str, total: usize) -> Self {
        let p = Progress {
            label,
            total,
            done: AtomicUsize::new(0),
            on: std::io::stderr().is_terminal() && total > 1,
        };
        p.draw(0);
        p
    }
    fn tick(&self) {
        let n = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        self.draw(n);
    }
    fn draw(&self, n: usize) {
        if self.on {
            let mut e = std::io::stderr().lock();
            let _ = write!(e, "\r\x1b[2K{} {n}/{}", self.label.dimmed(), self.total);
            let _ = e.flush();
        }
    }
    fn finish(&self) {
        if self.on {
            eprint!("\r\x1b[2K");
        }
    }
}

fn pad(s: &str, w: usize) -> String {
    format!("{s:<w$}")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn plural(n: usize, word: &str) -> String {
    match (n, word.ends_with("ch") || word.ends_with('s')) {
        (1, _) => format!("1 {word}"),
        (_, true) => format!("{n} {word}es"),
        _ => format!("{n} {word}s"),
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn ago(ts: i64) -> String {
    let s = (now() - ts).max(0);
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    match s {
        s if s < MIN => "just now".into(),
        s if s < HOUR => format!("{}m ago", s / MIN),
        s if s < DAY => format!("{}h ago", s / HOUR),
        s if s < 14 * DAY => format!("{}d ago", s / DAY),
        s if s < 60 * DAY => format!("{}w ago", s / (7 * DAY)),
        s if s < 365 * DAY => format!("{}mo ago", s / (30 * DAY)),
        s => format!("{}y ago", s / (365 * DAY)),
    }
}
