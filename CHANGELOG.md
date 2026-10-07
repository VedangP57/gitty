# Changelog

All notable changes to gitty are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `B` opens a branch picker: Enter switches (a remote-only branch gets a local tracking branch),
  `Ctrl-N` creates a branch from the typed name, `Ctrl-R` renames, `Ctrl-D` deletes. Deleting a
  branch with unmerged commits asks twice. With uncommitted changes, switching asks first.
- `Ctrl-G` in the branch picker merges the highlighted branch (a remote-only one too) into the
  checked-out branch after asking. A merge that hits conflicts is aborted and the files are listed,
  so the repository is never left mid-merge. With uncommitted changes it offers "stash and merge":
  the changes are put back after the merge, and stay in the stash (with a notice) if they cannot be.
- `S` opens the stash list: `a` applies, `p` pops, `d` drops (asks), `n` stashes the current
  changes under a message. Each stash shows its age. `Z` in the Changes tab stashes them with an optional message. A stash that hits conflicts
  is kept.
- Switching branches with uncommitted changes offers "stash and switch": the changes are stashed
  (untracked files included) under `gitty: auto-stash from <branch>`, and put back if the switch
  fails. gitty never re-applies a stash by itself.
- `R` opens the current branch's pull-request page (GitHub only) in the browser (the branch must be
  pushed first). It opens the open pull request if there is one (needs the `gh` CLI), otherwise the
  new-pull-request page.

### Changed

- The stash message prompt and the "stash and switch/merge" prompt say that untracked files are included, and how many when there are more than 500 (it can take a while).

### Fixed

- In compare mode the footer labels `b` "other branch" (it picks another branch to compare with) instead of "branch".
- The footer no longer shows the hints of the pane underneath while a picker, prompt or confirmation is open.
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
