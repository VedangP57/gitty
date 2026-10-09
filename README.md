# gitty

[![Release](https://img.shields.io/github/v/release/VedangP57/gitty)](https://github.com/VedangP57/gitty/releases/latest)
[![crates.io](https://img.shields.io/crates/v/gitty-cli)](https://crates.io/crates/gitty-cli)
[![CI](https://github.com/VedangP57/gitty/actions/workflows/ci.yml/badge.svg)](https://github.com/VedangP57/gitty/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

**Website:** [gitty.runs-on.dev](https://gitty.runs-on.dev)

A fast terminal git client with the GitHub Desktop experience: History, Changes, line staging,
fetch / pull / push, search and compare, in Rust with Ratatui.

![gitty: browsing History with a split diff, then staging and committing in Changes](assets/demo.gif)

- **History:** local and remote commits with their refs, and marks for what is ahead of or
  behind the upstream (on a branch never pushed, ↑ marks what pushing it would publish). Select a commit (or a `V` range of them) for its files and a syntax-highlighted diff, unified
  or split (added and deleted files always use the full width).
- **Changes:** status that follows the file system, staging by file, hunk or line (Space, or a
  click in the gutter), discard with a copy kept in the Trash, and a commit box with amend and
  undo.
- **Network:** fetch, pull (fast-forward, merge or rebase) and push with progress, cancel, and
  password or passphrase prompts inside the app. A background fetch runs every few minutes. A push
  rejected because the remote moved on (after an amend or rebase) offers a force push with lease,
  which only replaces what you last fetched and lists the remote commits it would remove; `main`,
  `master` and the branch `origin/HEAD` points to are never force pushed.
- **Branches:** switch, create, rename, delete and merge branches from a picker (`B`), and stash and
  restore work (`S`, `Z`).
- **Pull requests:** the top bar shows `PR #<n>` when the branch has one on GitHub, coloured by
  state (green open, grey draft, purple merged, red closed); click it to open the page. It needs the
  [`gh` CLI](https://cli.github.com/) logged in and is left out otherwise.
- **Search and compare:** `/` searches the whole history (text, author, `path:`) while you keep
  working; `b` compares HEAD with any branch (behind, ahead, and the files that differ).

gitty runs `git` for everything that writes and reads with [gitoxide](https://github.com/GitoxideLabs/gitoxide),
so hooks, signing, credential helpers and your git config work as they do on the command line.

## Install

**Homebrew** (macOS and Linux):

```sh
brew install vedangp57/tap/gitty
```

Use the full name: it tells Homebrew to trust this one formula from the tap.

**Shell installer** (a prebuilt binary into `~/.cargo/bin`):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/VedangP57/gitty/releases/latest/download/gitty-cli-installer.sh | sh
```

**Prebuilt archives** for each release are on the
[Releases page](https://github.com/VedangP57/gitty/releases). On macOS, a binary downloaded with
a browser is quarantined: run `xattr -d com.apple.quarantine gitty` once.

**From crates.io** (Rust 1.90 or newer, and a C compiler for the bundled grammars and Oniguruma).
The package is `gitty-cli`, because `gitty` on crates.io is another project; the command is `gitty`:

```sh
cargo install --locked gitty-cli
```

From a clone: `cargo install --locked --path crates/gitty`. Building without the bundled
tree-sitter grammars (`--no-default-features`) makes a smaller binary that highlights through the
syntect fallback.

## Requirements

- macOS (Apple Silicon or Intel) or Linux (x86_64 or arm64). The prebuilt Linux binaries need
  glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36 and later); build from source elsewhere.
- `git` 2.30 or newer on your `PATH`.
- On Linux, live refresh watches every directory of the worktree with inotify. A repository with
  more directories than `fs.inotify.max_user_watches` allows (often 8,192 on older kernels) opens
  with a notice and refreshes when the terminal regains focus instead.
- A terminal with mouse reporting; truecolor is used where the terminal has it, the nearest
  256-colour palette entry elsewhere.

## Usage

```sh
gitty              # the repository containing the current directory
gitty path/to/repo
gitty --theme dracula
gitty untune [PATH]
```

The window has a History tab (`2`), a Changes tab (`1`) and a Files tab (`3`). `Tab` moves between
panes and `?` shows every key. The mouse works throughout: click to select, scroll any pane, drag the diff
gutter to pick lines, Shift-click for a commit range, and double-click a file or diff line to
open it in `$EDITOR` at that line.

### History

The History tab lists the commits of the checked-out branch and its upstream (`r` switches to all
branches and tags). A graph column draws them the way `git log --graph` does: a dot per commit and
lines where branches split and merge, each lane in a theme colour. git draws the graph and gitty
loads it a page at a time as you scroll. With the graph on, the list is in git's topological order
(a branch's commits stay together); `L` hides it and lists commits by date (`history_graph = false`
starts that way). The graph steps aside during a search, a range (`V`) and compare mode, and in a
History pane narrower than 48 columns. On a very large repository without a commit-graph file the
graph can take a second or more on first open; gitty writes one automatically when auto-tune is on,
or turn the graph off with `L` or `history_graph = false`.

### Files

The Files tab (`3`) browses the working tree, ignored files included, and shows the selected file
read-only with line numbers and syntax colours. Directories load when opened (`Enter` or `l`; `h`
closes one, or goes to the parent), so a large tree costs nothing until you go into it. Symlinks are
shown as `name -> target` and never followed; submodules and nested repositories are leaves. Files
over 2 MiB and binary files are not loaded (`e` opens them in `$EDITOR`).

Changed files carry the same letter and colour as in the Changes tab in the last column (`M`, `A`
for added or untracked, `D`, `R`, `U` for conflicted), and a directory with anything changed below
it shows a `●` even when it is folded, so you can see which folders hold changes without opening
them. The dot takes the colour of the strongest change inside: conflicted, then deleted, then
modified (added and renamed count as modified), then untracked. A file deleted from the work tree
or renamed away is not listed under its old name, but its old folders are marked as deleted. `i` hides the ignored files and directories (they are
dimmed otherwise); expanded folders stay expanded. The `files_show_ignored` option sets whether
they start out shown.

Files that look like secrets are marked `(secret)` and are not read from disk until you press `v`
on them: `.env` and its variants (`.env.*`, `.env-*`, `*.env`, `.envrc`; only the exact names
`.env.example`, `.env.sample`, `.env.template` and `.env.dist` are shown), private keys and
certificates (`*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks`, `*.keystore`, `*.ppk`, `*.gpg`, the SSH
`id_*` keys), password stores and tokens (`*.kdbx`, `.netrc`, `.npmrc`, `.pypirc`, `.pgpass`,
`.htpasswd`, `.git-credentials`, `.vault-token`, `credentials*`, `secrets.*`, `*.token`,
`*.secret`, `service-account*.json`), Terraform `*.tfvars` and `*.tfstate`, and anything inside
`.ssh`, `.aws`, `.gnupg`, `.kube`, `secrets` or `.env` directories (and `.docker/config.json`).
Case, trailing dots and spaces, and backup or editor suffixes (`~`, `.bak`, `.orig`, `.swp`, …)
do not hide a match. The reveal ends when you select another file or leave the tab, and is never
saved.

Limits: the match is by name, so a secret under an innocent name (or a hard link to one, or a name
that only looks like one through Unicode lookalikes) is shown as any file. Masking applies to the
Files tab only: an untracked `.env` still appears in the Changes tab's diff.

### Merge in progress

When a merge, rebase, cherry-pick or revert has stopped (started in gitty's terminal or any
other), the top bar shows `MERGING topic · 2 conflicts` (or `REBASING feat · step 2/5 · 1 conflict`,
`CHERRY-PICKING`, `REVERTING`) in the warning colour; on a narrow terminal it drops the branch
and the step before it drops the word. Press `m` for the dialog: `c` continues once no file
conflicts any more (resolve each file and stage it in the Changes tab first; the dialog says why
Continue is not available yet), `a` aborts after asking, `Esc` closes. Continue never opens an
editor: a merge uses its prepared message, and a rebase or pick keeps the commit's own. If the
next step of a rebase or pick conflicts too, you are told and the bar shows the new count.
Hooks run for the commit that finishes the stopped step, and a hook that fails leaves the
operation open and its output is in the error details (`!`). If a file you staged still has
conflict markers in it (`<<<<<<<`), Continue asks "Continue anyway?" before committing it.
Because no editor opens, a cherry-pick, revert or reworded commit keeps its message as it was.
Abort restores the branch as it was before the operation; what the operation did is discarded.
`m` with nothing in progress says so.

A merge (`Ctrl-G` in the branch picker, "merge anyway" and "stash and merge" included) or a pull
(merge or rebase) that hits conflicts is not aborted and not reported as a failure: it is left in
progress, the bar shows it, and gitty asks "Resolve now?". `Enter` opens the Changes tab on the
first conflicted file (the file list keeps the focus; `o`, `t`, `b`, `n`, `p`, `u` and Space work
from there), `a` aborts the merge or rebase (the branch, index and files go back to how they were),
and `Esc` decides later: the operation stays open and `m` opens the dialog above.
With "stash and merge" your uncommitted changes stay in the stash (named by its message, `gitty:
auto-stash from <branch>`) while the merge is open, because applying them onto conflicted files
would mix the two: pop them after you finish the merge. The prompt, the `m` dialog, the top bar
(`changes in stash`) and the notices all remind you. Aborting from the question or from `m` aborts
the merge and then puts the stashed changes back (unless the stash list changed meanwhile, or the
merge was already gone: then they stay in the stash and gitty says so); finishing the merge with
Continue leaves them in the stash and says so. If the operation ended meanwhile gitty says "No
longer in progress" and aborts nothing. An abort acts only on the operation with the id the
question was about: a different merge or rebase that has started since is left alone, but a new
merge of the same branch at the same commit has the same id and would be aborted. While any merge
or rebase is open, pulling is refused ("finish or abort the merge first: m"). When another dialog
is open at the moment of the conflict the question is not shown: a note says "Conflicts: press m".

### Resolving conflicts

Conflicted files form the top of the Changes list, with their number of conflict blocks
(`a.txt (3)`) once read. Selecting one shows the file in the right pane instead of a diff: each
block is drawn as two tinted sides under header lines that name them, the block the cursor is in
is highlighted (`n` and `p`, or `[` and `]`, move to the next and previous, wrapping round), and
a diff3 base is shown dim. What the sides are called depends on the operation, because git's
"ours" and "theirs" swap roles when commits are replayed: in a merge they are *Current (your
branch)* and *Incoming (the merged branch)*; in a rebase *Base branch (what you rebase onto)* and
*Your commit (its subject)*; in a cherry-pick *Current branch* and *Picked commit*; in a revert
*Current branch* and *Without change (the reverted commit)*: git merges a revert with the commit
as the base, so its second side is the code as it was without that commit, and `t` undoes it. `o` keeps the first side, `t` takes the second, `b` keeps
both (first, then second) and `u` takes the last resolution of the file back (while the file is
as it left it); `e` opens the file in `$EDITOR` at the block. A resolution rewrites the file
atomically and only if it is still the text on screen (otherwise: "The file changed on disk;
reloaded"). Per-block resolution never touches symlinks, secret-looking files or files over
2 MiB (open them with `e`); the whole-file choices below do replace such a file, after asking. A
block where the sides cannot be told apart (a line of `=======` inside it, say) is counted but
refused with "Ambiguous markers in this block: press e to edit it"; markers longer than seven
characters (`conflict-marker-size` in `.gitattributes`) are understood.
Nothing is staged for you: after the last block gitty says "No conflicts left in a.txt; press
Space to stage it". Staging a file that still has markers works, with a notice.

Conflicts with no markers (binary files, a file one side deleted, both sides adding it) show
what happened in words; `o` and `t` then ask before taking that side's whole file (or deleting
the path when that side deleted it), stage the result and keep a copy of the file in the Trash.
For a file too large or not text to show line by line, that also drops the other side's
non-conflicting changes in it, which the question says. A submodule conflict takes the chosen
side's commit.
`b` needs markers. While a conflicted file is selected `p` and `u` mean previous conflict and
undo, not pull and undo-commit.

### In herdr

gitty ships a [herdr](https://herdr.dev) plugin that opens it in a popup on the repository of the
pane you are in:

```sh
herdr plugin install VedangP57/gitty/herdr-plugin
```

Bind it to a key in your herdr config:

```toml
[[keys.command]]
key = "prefix+g"
type = "plugin_action"
command = "vedangp57.gitty.open"
description = "gitty"
```

The plugin uses the gitty on your `PATH` (or in Homebrew's or cargo's usual folders). Without one, it
downloads the latest release for your machine into the plugin's own state folder, checks it against
the published SHA-256, and uses that copy; nothing else on the system changes. `q` closes the popup.

## Keys

Every key below can be rebound in `~/.config/gitty/config.toml` under `[keys]`, using the
config name: `fetch = "F5"` or `down = ["j", "ctrl-n"]`. A rebound action loses its default
keys. Text inputs (the commit box, prompts, the search bar) are never remapped. These are
reported at startup:
- one key bound to two of your actions that share a screen (the first in the file keeps it);
- a default action left with no key because you gave its key to another action;
- an unknown action name or key;
- Ctrl-C or Ctrl-Z, which always quit and suspend.

<!-- keys:start -->
| Keys | Action | Config name |
|---|---|---|
| `o` | Conflict: keep the first side (Current; Base branch in a rebase) | `conflict_ours` |
| `t` | Conflict: take the second side (Incoming; Your commit in a rebase) | `conflict_theirs` |
| `b` | Conflict: keep both, first side then second | `conflict_both` |
| `n` | Conflict: next conflict in the file | `conflict_next` |
| `p` | Conflict: previous conflict in the file | `conflict_prev` |
| `u` | Conflict: undo the last resolution of this file | `conflict_undo` |
| `e` | Conflict: open the file in $EDITOR at the conflict | `conflict_edit` |
| `q` | quit | `quit` |
| `1` | Changes tab | `changes_tab` |
| `2` | History tab | `history_tab` |
| `3` | Files tab | `files_tab` |
| `f` | fetch | `fetch` |
| `p` | pull | `pull` |
| `P` | push | `push` |
| `x` | cancel the running fetch, pull or push | `cancel` |
| `O` | open the diff in the difftool | `difftool` |
| `R` | open the branch's open pull request in the browser (needs the `gh` CLI; otherwise the new-PR page) | `open_pr` |
| `T` | theme picker | `theme` |
| `B` | branches: switch, create, rename, delete, merge | `branches` |
| `S` | stashes: apply, pop, drop | `stashes` |
| `m` | the merge, rebase, cherry-pick or revert in progress: continue or abort it | `operation` |
| `?` | this help | `help` |
| `!` | details of the last error | `error_details` |
| `h` `←` | compare: previous tab | `compare_prev_tab` |
| `l` `→` | compare: next tab | `compare_next_tab` |
| `c` | Changes: write the commit message | `commit_box` |
| `A` | Changes: amend the last commit | `amend` |
| `u` | Changes: undo the last commit | `undo_commit` |
| `Z` | Changes: stash all changes | `stash_push` |
| `Space` | Changes: stage file / line (again: unstage) | `stage` |
| `a` | Changes: stage everything / the whole file | `stage_all` |
| `d` | Changes: discard file / lines (asks first) | `discard` |
| `F` | Changes: filter the file list | `filter` |
| `v` | Changes: select a range of lines | `line_range` |
| `H` | Changes: stage the hunk | `stage_hunk` |
| `v` | Files: show or hide a secret file (.env, keys…) | `reveal_secret` |
| `e` | Files: open the file in $EDITOR | `open_editor` |
| `i` | Files: show or hide ignored files | `toggle_ignored` |
| `h` `←` | Files: close the directory (or go to its parent) | `files_collapse` |
| `l` `→` | Files: open the directory | `files_expand` |
| `/` | search history (text, path:dir) | `search` |
| `n` | next search match | `next_match` |
| `N` | previous search match | `prev_match` |
| `V` | select a range of commits | `range` |
| `b` | compare with a branch | `compare` |
| `t` | file list as a tree | `tree` |
| `r` | branch + upstream ↔ all refs | `scope` |
| `y` | copy the short SHA | `copy_sha` |
| `Y` | copy the full SHA | `copy_full_sha` |
| `o` | expand the commit header | `header` |
| `D` | date format | `dates` |
| `z` | row density | `density` |
| `L` | show or hide the commit graph | `graph` |
| `h` `←` | scroll the diff left | `scroll_left` |
| `l` `→` | scroll the diff right | `scroll_right` |
| `[` | previous hunk | `prev_hunk` |
| `]` | next hunk | `next_hunk` |
| `{` | previous file | `prev_file` |
| `}` | next file | `next_file` |
| `e` | more context near the cursor | `expand` |
| `E` | whole file | `expand_file` |
| `s` | split ↔ unified | `split` |
| `w` | whitespace mode | `whitespace` |
| `W` | wrap long lines | `wrap` |
| `F` | full-screen diff | `fullscreen` |
| `<` | shrink the focused pane | `narrower` |
| `>` | grow the focused pane | `wider` |
| `j` `↓` | move down | `down` |
| `k` `↑` | move up | `up` |
| `Ctrl-d` | half a page down | `half_page_down` |
| `Ctrl-u` | half a page up | `half_page_up` |
| `Ctrl-f` `PgDn` | a page down | `page_down` |
| `Ctrl-b` `PgUp` | a page up | `page_up` |
| `g` `Home` | first row | `top` |
| `G` `End` | last row | `bottom` |
| `Tab` | next pane | `next_pane` |
| `Shift-Tab` | previous pane | `prev_pane` |
| `Enter` | open / drill in (a directory: fold) | `open` |
| `Esc` | back (ends a range, search or compare) | `back` |
<!-- keys:end -->

## Configuration

`~/.config/gitty/config.toml` (or `$XDG_CONFIG_HOME/gitty/config.toml`). Every key is optional;
unknown keys and bad values are reported at startup and fall back to the default.

| Key | Default | Values |
|---|---|---|
| `theme` | `"auto"` | a theme name, or `"auto"` (light or dark by the terminal background) |
| `tab_size` | `4` | 1–16 |
| `diff_algorithm` | `"myers"` | `"myers"`, `"histogram"` |
| `whitespace` | `"show"` | `"show"`, `"ignore-all"` (`-w`), `"ignore-amount"` (`-b`) |
| `split_threshold` | `200` | terminal width at which split view turns on by itself |
| `date_mode` | `"relative"` | `"relative"`, `"absolute"`, `"both"` |
| `density` | `"compact"` | `"compact"`, `"comfortable"` |
| `emph_alpha` | theme's | 0.0–1.0, strength of the changed-word highlight |
| `auto_fetch_minutes` | `5` | 0 turns the background fetch off |
| `auto_tune` | `true` | tune large repositories (see below) |
| `files_show_ignored` | `true` | list ignored files in the Files tab when it starts (`i` toggles them there) |
| `history_graph` | `true` | start History with the commit graph and topological order (`L` toggles) |
| `difftool` | none | command for `O`, run as `<difftool> <old> <new>`, e.g. `"delta"`; a GUI tool needs its wait flag (`"code --wait --diff"`), because the two temp files are removed when the command returns |
| `[keys]` | | action → key or keys, see above |

```toml
theme = "rose-pine"
difftool = "delta --side-by-side"
auto_fetch_minutes = 10

[keys]
fetch = "F5"
down = ["j", "ctrl-n"]
```

`$EDITOR` (or `$VISUAL`) and `difftool` are split like shell words but never run by a shell,
and file paths are always passed as separate arguments. Per-repository UI state (pane sizes,
history scope, tree view) is kept under `~/.local/state/gitty/`.

## Themes

Built in: `github-dark`, `github-light`, `rose-pine`, `rose-pine-dawn`, `catppuccin-mocha`,
`catppuccin-latte`, `tokyo-night`, `dracula`, `gruvbox-dark`, `solarized-dark`,
`solarized-light`, `nord`, `one-dark`, `one-light`, `gruvbox-light`, `catppuccin-frappe`,
`catppuccin-macchiato`, `tokyo-night-storm`, `tokyo-night-day`, `kanagawa`, `everforest-dark`,
`ayu-mirage` and `nightfox`. `T` opens a picker with a live preview.

Your own themes go in `~/.config/gitty/themes/<name>.toml`. A theme names a `[palette]` (`bg`,
`fg`, `muted`, `accent`, `border`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, and
optionally `panel` and `orange`) and overrides only what differs. `inherit = "<theme>"` layers
it over another theme:

```toml
inherit = "github-dark"

[palette]
accent = "#ff79c6"
```

Colours are truecolor where the terminal supports it and the nearest xterm-256 colour elsewhere.

## Large repositories and `gitty untune`

On a repository with at least 10,000 commits or 20,000 index entries, gitty tunes git once, in
the background, unless `auto_tune = false`:

- writes a commit-graph (`git commit-graph write --reachable --changed-paths --split`), when
  `core.commitGraph` is not off and the clone is not shallow;
- sets `core.untrackedCache`, and `core.fsmonitor` where your git has the builtin fsmonitor
  daemon (macOS and Windows builds; most Linux packages do not), for faster status, but never a
  key you have already set.

The keys gitty sets are recorded in `gitty.tuned`. `gitty untune [PATH]` unsets exactly those
(skipping any you have since changed) and leaves everything else alone.

## Performance

Measured on Apple Silicon (macOS), release build, warm cache, against the git/git repository
(85,887 commits, 1,010 refs) and a blobless Linux kernel clone (1,484,291 commits). `bench/run.sh`
checks these budgets and exits non-zero when one is missed; `bench/README.md` has the details
and the history.

| Operation | Measured | Budget |
|---|---|---|
| first frame (layout drawn) | 14 ms | < 16 ms |
| first 500 history rows, kernel | 23 ms | < 50 ms |
| full history walk, kernel | 225–258 ms | < 400 ms |
| a 220×60 frame (history and a highlighted diff) | 0.5 ms | < 16 ms |
| commit file list, p50 | 0.12 ms | < 5 ms |
| file diff, p50 | 0.19 ms | < 10 ms |
| ahead/behind, 360,606 commits | 88 ms | < 150 ms |
| status, git/git | 16–17 ms | < 70 ms |
| search, one 20,000-row chunk, kernel | 337 ms on one search thread (2 of them); the UI never waits | — |

## License

MIT: see [LICENSE](LICENSE). The binary includes third-party code under MIT, Apache-2.0, BSD,
ISC, Zlib, Unicode, CC0 and MPL-2.0 licenses, listed with their texts in
[THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md) (regenerate it with
`scripts/third-party-licenses.sh`).

gitty is built on [gitoxide](https://github.com/GitoxideLabs/gitoxide),
[Ratatui](https://github.com/ratatui/ratatui), [tree-sitter](https://github.com/tree-sitter/tree-sitter)
and [syntect](https://github.com/trishume/syntect) with [bat](https://github.com/sharkdp/bat)'s
syntax definitions. The built-in themes reproduce the palettes of Ayu (MIT), Catppuccin (MIT),
Dracula (MIT), Everforest (MIT), GitHub (MIT), Gruvbox (MIT/X11), Kanagawa (MIT), Nightfox (MIT),
Nord (MIT), Atom One Dark and One Light (MIT), Rosé Pine (MIT), Solarized (MIT) and Tokyo Night
(MIT and Apache-2.0); the copyright holders and licence text are in
[THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md). The conflict-marker parsing and resolving
logic is adapted from the editor [druk](https://github.com/letstri/druk) (MIT, Copyright (c) 2026
Valerii Strilets).
