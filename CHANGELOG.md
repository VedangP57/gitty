# Changelog

All notable changes to gitty are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-10-19

### Added

- A commit graph in History, shown by default. Its lanes are laid out as lazygit lays them out
  (the layout is ported from lazygit, MIT): `●` for a commit, `◉` for a merge and `◎` for the
  checked-out commit. Each branch keeps one theme colour from top to bottom, its labels are pills
  in that colour, and the current branch's line is drawn heavy. Each commit keeps its one line from
  the list, with a spacer line under it that carries only the branch lines down to the next commit.
  `history_graph_style = "compact"` drops the spacers (one line per commit); any other value than
  `"spaced"` or `"compact"` warns and keeps `"spaced"`. `L` shows or hides the graph, and
  `history_graph = false` starts with it hidden. It steps aside during a search, a `V` range,
  compare mode and in a History pane under 34 columns, and cuts lines wider than the pane with `›`.
- Conflict help. Conflicted files head the Changes list with their number of blocks; selecting one
  shows its blocks as two tinted sides under headers named for the operation (merge: Current /
  Incoming; rebase: Base branch / Your commit; cherry-pick: Current branch / Picked commit; revert:
  Current branch / Without change). `o` keeps the first side, `t` takes the second, `b` keeps both,
  `n` and `p` move between blocks, `u` takes the last resolution back and `e` opens the editor at
  the block. A resolution is written atomically and only over the text that was shown; a block
  with ambiguous markers is left to the editor, and `conflict-marker-size` is honoured. Conflicts
  without markers (binary, deleted by one side) are explained and settled whole after asking.
  Nothing is staged for you. The marker parser is adapted from druk (MIT).
- A merge, rebase, cherry-pick or revert that stopped (in gitty or in another terminal) shows in the
  top bar with its conflict count (`MERGING topic · 2 conflicts`, `REBASING feat · step 2/5 · 1
  conflict`). `m` continues it once nothing conflicts (no editor; the prepared message is used) or
  aborts it after asking.
- A merge or pull (merge or rebase) that hits conflicts is left open and asks "Resolve now?":
  Enter goes to the first conflicted file, `a` aborts, Esc decides later (`m` opens the dialog).
- A branch picker (`B`): Enter switches (a remote-only branch gets a local tracking branch),
  `Ctrl-N` creates, `Ctrl-R` renames, `Ctrl-D` deletes (asking twice when the branch has commits
  its upstream lacks, or HEAD without an upstream, as git judges it) and `Ctrl-G` merges the
  highlighted branch into the checked-out one after asking. With uncommitted changes, switching
  offers "stash and switch" and merging "stash and merge". After a switch the changes are put back;
  after a conflicted merge they stay in the stash until you finish it (aborting from gitty puts
  them back). gitty never re-applies a stash by itself otherwise.
- A stash list (`S`): `a` applies, `p` pops, `d` drops (asks), `n` stashes the current changes
  under a message; each stash shows its age. `Z` in the Changes tab stashes with an optional
  message. Stashes include untracked files, and the prompts say so (with the count when there are
  more than 500).
- A Files tab (`3`): the working tree as a lazily loaded tree with a read-only, syntax-coloured
  viewer. Changed files show their Changes letter and colour, and folders with changes below them
  a `●`. `i` hides or shows ignored files (`files_show_ignored` sets the start). `e` or a
  double-click opens the file in `$EDITOR`. Files named like secrets (`.env`, keys, tokens) are not
  read until you press `v`.
- The top bar shows `PR #<n>` when the branch has a GitHub pull request, coloured by its state
  (open, draft, merged, closed; themes can set `pr_open`, `pr_draft`, `pr_merged`, `pr_closed`
  under `[ui]`). Clicking it opens the pull request. It needs the `gh` CLI logged in, is looked up
  in the background (at most every 30 seconds per branch, every 5 minutes while focused), ignores
  pull requests from forks, and keeps the last known badge through a failed lookup.
- `R` opens the branch's pull request on GitHub in the browser, or the new-pull-request page when
  there is none (or no `gh`). The branch must be pushed first.
- A push rejected because the remote moved on (after an amend or a rebase) offers a force push with
  lease. The question lists the remote commits it would remove; the lease is the commit you last
  fetched, so git refuses if anyone pushed since. `main`, `master` and the branch `origin/HEAD`
  points to are never force pushed.
- Twelve more built-in themes: `nord`, `one-dark`, `one-light`, `gruvbox-light`,
  `catppuccin-frappe`, `catppuccin-macchiato`, `tokyo-night-storm`, `tokyo-night-day`, `kanagawa`,
  `everforest-dark`, `ayu-mirage` and `nightfox`, credited in `THIRD-PARTY-LICENSES.md`.
- A [herdr](https://herdr.dev) plugin (`herdr-plugin/`) that opens gitty in a popup on the focused
  pane's repository, downloading a checksum-verified release binary when none is installed.

### Changed

- With the graph shown (the default), History lists commits in topological order (`git rev-list
  --topo-order`) instead of by date; `L` or `history_graph = false` brings the date order back. On
  a large repository without a commit-graph file the first rows wait for git's sort (about 0.3 s
  per 100,000 commits); auto-tune writes that file on repositories with 10,000 commits or more.
- While a conflicted file is selected in Changes, `p` and `u` mean previous conflict and undo the
  resolution, not pull and undo the last commit.

### Fixed

- Sideways scrolling (`h`/`l`, the sideways wheel, Shift+wheel) in the diff and the Files viewer
  only happens when a line is wider than the pane, and stops at the end of the widest line. `‹` and
  `›` mark lines with text hidden on the left or right.
- In compare mode the footer labels `b` "other branch" (it picks another branch to compare with)
  instead of "branch".
- The footer no longer shows the hints of the pane underneath while a picker, prompt or
  confirmation is open.
- Outside a repository gitty prints `gitty: <dir> is not a git repository`, without the internal
  source path.

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

[Unreleased]: https://github.com/VedangP57/gitty/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/VedangP57/gitty/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/VedangP57/gitty/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/VedangP57/gitty/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/VedangP57/gitty/releases/tag/v0.1.0
