# Changelog

All notable changes to gitty are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- A branch that was never pushed marks with ↑ the commits no remote branch has (what pushing it
  would publish), and the top bar says `↑N not published`, as GitHub Desktop does.

### Fixed

- An upstream set or removed outside gitty (`git push -u` in a terminal) shows up on the next
  refresh; it used to need a restart.
- When the branch has nothing to compare with, the last upstream's ↑/↓ marks no longer stay.

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

[Unreleased]: https://github.com/VedangP57/gitty/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/VedangP57/gitty/releases/tag/v0.1.0
