# gitty

A fast terminal git client with the GitHub Desktop experience: History, Changes, line staging,
fetch / pull / push, search and compare, in Rust with Ratatui.

## Keys

Every key below can be rebound in `~/.config/gitty/config.toml` under `[keys]`, using the
config name: `fetch = "F5"` or `down = ["j", "ctrl-n"]`. A rebound action loses its default
keys. Text inputs (the commit box, prompts, the search bar) are never remapped.

<!-- keys:start -->
| Keys | Action | Config name |
|---|---|---|
| `q` | quit | `quit` |
| `1` | Changes tab | `changes_tab` |
| `2` | History tab | `history_tab` |
| `f` | fetch | `fetch` |
| `p` | pull | `pull` |
| `P` | push | `push` |
| `x` | cancel the running fetch, pull or push | `cancel` |
| `O` | open the diff in the difftool | `difftool` |
| `T` | theme picker | `theme` |
| `?` | this help | `help` |
| `!` | details of the last error | `error_details` |
| `h` `←` | compare: previous tab | `compare_prev_tab` |
| `l` `→` | compare: next tab | `compare_next_tab` |
| `c` | Changes: write the commit message | `commit_box` |
| `A` | Changes: amend the last commit | `amend` |
| `u` | Changes: undo the last commit | `undo_commit` |
| `Space` | Changes: stage file / line (again: unstage) | `stage` |
| `a` | Changes: stage everything / the whole file | `stage_all` |
| `d` | Changes: discard file / lines (asks first) | `discard` |
| `F` | Changes: filter the file list | `filter` |
| `v` | Changes: select a range of lines | `line_range` |
| `H` | Changes: stage the hunk | `stage_hunk` |
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
