# Changelog

All notable changes to gitty are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- A commit graph in History, laid out and drawn like lazygit's (its layout ported, MIT): one row
  per commit, `○` for commits and `◎` for merges, each branch in a theme colour it keeps all the
  way down. With the graph on, History lists commits in topological order
  (`git rev-list --topo-order`) and lays out the graph as the walk streams in. `L` shows or hides
  it, and `history_graph = false` starts with it hidden. It steps aside during a search, a
  range, compare mode and in a History pane under 34 columns, and cuts lines wider than the pane
  with `›`. On a large repository without a commit-graph file the first rows wait for git's sort
  (about 0.3 s per 100,000 commits).
- Resolving conflicts in gitty. Conflicted files head the Changes list with their number of
  blocks; selecting one draws its blocks as two tinted sides under header lines that name them by
  the operation (merge: Current / Incoming; rebase: Base branch / Your commit; cherry-pick:
  Current branch / Picked commit; revert: Current branch / Without change, since git swaps ours and
  theirs there and a revert's second side is the code without the commit). `o` keeps the first side, `t` takes
  the second, `b` keeps both, `n` and `p` move between blocks, `u` takes the last resolution back
  and `e` opens the editor at the block. A resolution is written atomically and only over the text
  that was shown; a block with a line that looks like a marker is flagged and left to the editor,
  and `conflict-marker-size` is honoured. Conflicts without markers (binary, deleted by one side) are explained and settled
  whole after asking. Nothing is staged for you. `p` and `u` mean previous conflict and undo while
  a conflicted file is selected. The marker parser is adapted from druk (MIT).
- A merge, rebase, cherry-pick or revert that stopped (started in gitty or in another terminal) is
  shown in the top bar with its conflict count (`MERGING topic · 2 conflicts`, `REBASING feat ·
  step 2/5 · 1 conflict`). `m` opens a dialog to continue it once nothing conflicts, with no
  editor and the prepared message, or to abort it after asking. If the next step of a rebase
  conflicts too, it says so instead of failing.
- Twelve more built-in themes: `nord`, `one-dark`, `one-light`, `gruvbox-light`, `catppuccin-frappe`,
  `catppuccin-macchiato`, `tokyo-night-storm`, `tokyo-night-day`, `kanagawa`, `everforest-dark`,
  `ayu-mirage` and `nightfox`. Each is credited, with its licence, in `THIRD-PARTY-LICENSES.md`.
- A Files tab (`3`): the working tree as a lazily loaded tree (ignored files dimmed, symlinks and
  submodules as leaves) with a read-only, syntax-coloured viewer. `Enter`/`l` open a directory,
  `h` closes it, `e` or a double-click opens the file in `$EDITOR`. Files named like secrets
  (`.env`, keys, tokens) are not read until you press `v`.
- Git marks in the Files tab: changed files show their Changes letter and colour, and directories
  with changes below them show a `●` (also when folded). `i` hides or shows ignored files; the
  `files_show_ignored` option sets the starting state.
- `B` opens a branch picker: Enter switches (a remote-only branch gets a local tracking branch),
  `Ctrl-N` creates a branch from the typed name, `Ctrl-R` renames, `Ctrl-D` deletes. Deleting a
  branch with unmerged commits asks twice. With uncommitted changes, switching asks first.
- `Ctrl-G` in the branch picker merges the highlighted branch (a remote-only one too) into the
  checked-out branch after asking. A merge that hits conflicts is left open and gitty asks
  "Resolve now?": Enter goes to the first conflicted file in Changes, `a` aborts the merge, Esc
  decides later (the top bar shows it and `m` opens the dialog). With uncommitted changes it
  offers "stash and merge": the changes are put back after the merge, and stay in the stash (with
  a notice) if they cannot be. When the merge has conflicts they stay in the stash while it is
  open; aborting from the question puts them back.
- A pull (merge or rebase) that hits conflicts asks the same "Resolve now?" question instead of
  failing, decided by the state git leaves on disk. A pull that fails with nothing in progress, and
  a diverged fast-forward, are reported as before.
- `S` opens the stash list: `a` applies, `p` pops, `d` drops (asks), `n` stashes the current
  changes under a message. Each stash shows its age. `Z` in the Changes tab stashes them with an optional message. A stash that hits conflicts
  is kept.
- Switching branches with uncommitted changes offers "stash and switch": the changes are stashed
  (untracked files included) under `gitty: auto-stash from <branch>`, and put back if the switch
  fails. gitty never re-applies a stash by itself.
- `R` opens the current branch's pull-request page (GitHub only) in the browser (the branch must be
  pushed first). It opens the open pull request if there is one (needs the `gh` CLI), otherwise the
  new-pull-request page.
- The top bar shows `PR #<n>` after the branch when it has a pull request (GitHub only, needs the
  `gh` CLI and a GitHub remote; a merged pull request still shows after its branch is deleted on the remote). The colour is the state, as on GitHub: green open, grey draft,
  purple merged, red closed; set `pr_open`, `pr_draft`, `pr_merged` and `pr_closed` under `[ui]` in
  a theme to change them. Clicking the badge opens the pull request in the browser. Pull requests
  from forks that share the branch name are ignored (also by `R`). The badge is looked up in the
  background whenever the refs refresh (focus regained, commits, branch changes, fetch, push), at
  most every 30 seconds per branch, and every 5 minutes while the terminal has focus. Without
  `gh`, a login or a network the badge is not added, and when a lookup fails (offline, timeout) the
  badge already shown for that branch stays; it goes when the branch changes or `gh` reports that
  there is no pull request.

- A push rejected because the remote moved on (typically after amending a pushed commit or
  rebasing) asks whether to force push with a lease. The question lists the remote commits you
  don't have that the push would remove (and says so when git cannot tell). Enter runs
  `git push --force-with-lease` with the commit you last fetched as the expected value, so the
  remote branch is replaced only if nobody pushed to it since; a fetch in between (auto-fetch
  included) makes git refuse instead of overwriting, and the refusal says to fetch first. A
  rejection that says "fetch first" asks you to fetch instead. `main` and `master` (any case) and
  the branch `origin/HEAD` points to are never force pushed, nor is a branch with no
  remote-tracking branch to check against.

### Changed

- The stash message prompt and the "stash and switch/merge" prompt say that untracked files are included, and how many when there are more than 500 (it can take a while).

### Fixed

- Sideways scrolling (`h`/`l`, the sideways wheel, Shift+wheel) in the diff and the Files viewer only happens when a line is wider than the pane, and stops at the end of the widest line. `‹` and `›` mark lines with text hidden on the left or right.
- In compare mode the footer labels `b` "other branch" (it picks another branch to compare with) instead of "branch".
- The footer no longer shows the hints of the pane underneath while a picker, prompt or confirmation is open.
- Deleting an unmerged branch asks the force question under non-English git locales too, and judges "merged" against the branch's upstream (HEAD without one), as git does.
- Outside a repository gitty prints `gitty: <dir> is not a git repository`, without the internal source path.

## [0.1.2] - 2026-10-05

### Changed

- Published on crates.io as `gitty-cli` (`cargo install --locked gitty-cli`); the command is still
  `gitty`. Release archives and the shell installer are now named `gitty-cli-…`
  (`gitty-cli-installer.sh`); the Homebrew formula is still `gitty`.

## [0.1.1] - 2026-10-05

### Added

- A branch that was never pushed marks with ↑ the commits no remote branch has (what pushing it
  would publish), and the top bar says `↑N not published`, as GitHub Desktop does.

### Fixed

- An upstream set or removed outside gitty (`git push -u` in a terminal) shows up on the next
  refresh; it used to need a restart.
- When the branch has nothing to compare with, the last upstream's ↑/↓ marks no longer stay.
- After committing everything, the Changes diff pane no longer shows the History tab's file as its
  title.
- Switching from Changes back to History returns to the pane History had focused, not the
  Changes file list (or the commit editor, after a click on the tab).
- In split view, a long hunk header label no longer runs across the divider.

## [0.1.0] - 2026-10-05

The first public release.

### Added

- History: every commit of HEAD and its upstream (or all refs, `r`), with refs, ahead/behind
  marks, a commit detail header, and a file list (flat or a directory tree, `t`).
- Diffs: syntax highlighting (tree-sitter, with a syntect fallback), changed-word emphasis,
  unified or split view (`s`, automatic on wide terminals), line wrap (`W`), expandable context,
  and whitespace modes (`w`). Added and deleted files always use the full width.
- Changes: live status from the file system, staging by file, hunk or line (Space, gutter clicks
  and drags), discard with a copy in the Trash, and a commit box with amend, co-authors and undo.
- Network: fetch, pull (fast-forward, merge or rebase) and push with progress and cancel,
  credential and passphrase prompts inside the app, and a background fetch every few minutes.
- Search (`/`: text, author, `path:`) across the whole history, `V` ranges with a combined diff,
  and `b` to compare HEAD with any branch.
- Opening files in `$EDITOR` at a line and diffs in a configurable difftool.
- A remappable keymap, 11 built-in themes plus user themes, and a TOML config.
- Automatic tuning of large repositories (commit-graph, untracked cache, fsmonitor where git
  supports it), reverted exactly by `gitty untune`.

[Unreleased]: https://github.com/VedangP57/gitty/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/VedangP57/gitty/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/VedangP57/gitty/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/VedangP57/gitty/releases/tag/v0.1.0
