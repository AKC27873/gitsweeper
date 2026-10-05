# gitsweep

Status, pull, and branch cleanup across every Git repo in a folder.

## Install

    cargo install --path .

## Usage

    gitsweep status ~/code            # branch, uncommitted changes, ahead/behind
    gitsweep status ~/code --fetch    # fetch first so ahead/behind is current
    gitsweep pull ~/code              # ff-only pull of every clean repo, in parallel
    gitsweep prune ~/code             # dry run: merged branches that could go
    gitsweep prune ~/code --yes       # delete them (uses `git branch -d`)

`--depth N` controls how deep to search (default 3). Hidden folders,
`node_modules`, and `target` are skipped.
