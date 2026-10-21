# gitsweeper

Keep a folder full of Git repos tidy. `gitsweeper` finds every repository under a
directory and lets you check, sync, clean up and search them all at once, in
parallel, with colored output that makes problems easy to spot.

```
$ gitsweeper status
REPO      BRANCH  CHANGES   SYNC            STASH  LAST COMMIT
api       main    clean     ↓2                     3h ago
cli       main    clean     up to date             2d ago
docs      main    clean     ↑1 ↓1 diverged         just now
web       main    +1 ~1 ?1  ↑1              ≡1     20m ago
work/cli  main    clean     no upstream            5mo ago

5 repos · 1 dirty · 2 ahead · 2 behind · 1 no upstream · 1 with stashes
+staged  ~modified  ?untracked  !conflicts  ≡stashes
```

## Installation Process with Cargo

Requires Rust 1.88 or newer and `git` on your `PATH`.

```sh
cargo install --path .
```

## Usage

```sh
gitsweeper [OPTIONS] <COMMAND>
```

By default `gitsweeper` scans the current directory, three levels deep. Point it
somewhere else with `-C`, or set `gitsweeper_ROOT` once in your shell profile:

```sh
gitsweeper -C ~/example status
export gitsweeper_ROOT=~/example   # then just: gitsweeper status
```

Hidden directories, `node_modules` and `target` are skipped, and it doesn't
look for repos nested inside other repos. Repos are named by their path relative
to the root, so `work/cli` and `cli` stay distinct.

### Commands

| Command | What it does |
|---|---|
| `status` (`st`) | Branch, changes, ahead/behind, stashes and last commit for each repo |
| `fetch` | Fetch all remotes for every repo |
| `pull` | Fast-forward pull every repo that's safe to pull |
| `push` | Push repos that are ahead of their upstream *(dry run unless `--yes`)* |
| `switch [BRANCH]` | Switch every clean repo to a branch, or back to its default branch |
| `prune` | Delete local branches already merged into the default branch *(dry run unless `--yes`)* |
| `branches` (`br`) | List every local branch with its tracking state |
| `recent` | Most recent commit per repo, newest first |
| `grep PATTERN` | Search tracked files in every repo with `git grep` |
| `run GIT_ARGS...` | Run any git command in every repo |
| `exec -- CMD...` | Run any program in every repo |
| `list` (`ls`) | Print the full path of every repo found |

Run `gitsweeper <command> --help` for each command's options.

### Global options

These work with every command:

| Option | Description |
|---|---|
| `-C, --root <DIR>` | Folder to scan (default `.`, or `$gitsweeper_ROOT`) |
| `--depth <N>` | How many directory levels deep to search (default `3`) |
| `-f, --filter <TEXT>` | Only include repos whose path contains `TEXT` (repeatable) |
| `-x, --exclude <TEXT>` | Skip repos whose path contains `TEXT` (repeatable) |
| `-j, --jobs <N>` | How many repos to work on at once (default: number of CPUs) |
| `--no-color` | Turn off colors |

Filters are case-insensitive. Several `-f` values match any of them; `-x` always wins.

## Commands in detail

### `status`

```sh
gitsweeper status            # every repo
gitsweeper status --fetch    # fetch first so ahead/behind is current
gitsweeper status -d         # only repos that need attention
```

The **CHANGES** column breaks changes down: `+` staged, `~` modified,
`?` untracked, `!` conflicts. **SYNC** shows `↑` commits to push, `↓` commits
to pull, `diverged` when both, `upstream gone` when the remote branch was
deleted, and `no upstream` for local-only branches. `-d` shows only repos that are
dirty, out of sync (including no upstream), have stashes, or hit an error.

### `fetch`

```sh
gitsweeper fetch           # git fetch --all in every repo
gitsweeper fetch --prune   # also drop remote-tracking branches deleted on the remote
```

Reports how many commits are waiting to be pulled in each repo.

### `pull`

Fast-forward only (`git pull --ff-only`), so it never creates merge commits.
A repo is skipped when it has uncommitted changes to tracked files, a detached
HEAD, no upstream, or an upstream that was deleted. Untracked files don't block
a pull. Repos that have diverged are reported as needing a merge or rebase.

### `push`

```sh
gitsweeper push         # show what would be pushed
gitsweeper push --yes   # actually push
```

Only repos that are ahead get pushed. Repos that are also behind are skipped
with a reminder to pull first.

### `switch`

```sh
gitsweeper switch release/2.0   # switch every repo that has this branch
gitsweeper switch               # put every repo back on its default branch
```

Repos with uncommitted changes are skipped, and repos without the branch
(locally or on a remote) are left alone. The default branch comes from
`origin/HEAD`, falling back to `main`, then `master`.

### `prune`

```sh
gitsweeper prune                       # dry run: list merged branches
gitsweeper prune --yes                 # delete them
gitsweeper prune --gone                # also include branches whose remote was deleted
gitsweeper prune --gone --yes --force  # delete those too, even if git calls them unmerged
```

The current branch and the default branch are never touched. Deletion uses
`git branch -d`, which refuses anything Git doesn't consider merged. Squash- or
rebase-merged branches often fall into that group. Once you've checked the list,
`--force` (with `--yes`) switches to `git branch -D`.

### `branches`

Lists every local branch per repo, most recently committed first, marking the
current branch and showing ahead/behind, upstream and age.

### `recent`

```sh
gitsweeper recent        # 20 most recently active repos
gitsweeper recent -n 5
```

Shows when, which branch, who and the commit subject. Handy for "what was I
working on?"

### `grep`

```sh
gitsweeper grep TODO
gitsweeper grep -i 'fixme|hack'   # case-insensitive
gitsweeper grep -l unwrap         # only file names
```

Searches tracked files only, skipping binaries. Results are prefixed with the
repo name.

### `run` and `exec`

```sh
gitsweeper run log -1 --oneline
gitsweeper run remote -v
gitsweeper exec -- cargo check
gitsweeper exec -- sh -c 'ls | wc -l'
```

`run` passes its arguments to `git` in each repo; `exec` runs any program with
each repo as the working directory. Commands run in parallel and output is
grouped per repo. Repos that print nothing are counted rather than listed.
Put `--` before the command so its flags aren't read as `gitsweeper` options.

### `list`

```sh
gitsweeper list
cd "$(gitsweeper list | fzf)"
```

Prints one absolute path per line, with no color or headers.

## Colors and output

| Color | Meaning |
|---|---|
| Green | Clean, up to date, done |
| Cyan | Ahead, or would do (dry run) |
| Yellow | Behind, modified, skipped |
| Red | Diverged, upstream gone, failed |
| Dim | Nothing to do |

Colors switch off automatically when output isn't a terminal, and with
`--no-color` or the `NO_COLOR` environment variable. Set `CLICOLOR_FORCE=1` to
keep them when piping. Long-running commands show a progress counter on stderr.

Git never stops to ask for a password: `GIT_TERMINAL_PROMPT=0` is set, so a repo
needing credentials fails and the rest carry on.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Everything succeeded |
| `1` | At least one repo failed (a pull, push, command, etc.) |
| `2` | `gitsweeper` itself couldn't run (bad root folder, program not found) |

