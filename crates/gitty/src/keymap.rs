//! Key bindings (spec §11.4): every non-text action has a name and default keys, and
//! `[keys]` in config.toml rebinds them. Text inputs (commit box, prompts, search bar, picker
//! query) and Ctrl-C / Ctrl-Z never go through here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Focus, Tab};

/// Where an action applies. Earlier contexts win when two active ones share a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    Global,
    /// History tab, compare mode, history pane focused.
    Compare,
    /// Changes tab, any pane.
    Changes,
    /// Changes tab, file list (not the diff).
    ChangesList,
    /// Changes tab, diff pane.
    ChangesDiff,
    /// History tab, any pane.
    History,
    /// The diff, from any pane of either tab.
    Diff,
    /// Moving around, in every pane.
    Nav,
}

/// What is on screen, for picking the active contexts.
#[derive(Debug, Clone, Copy)]
pub struct State {
    pub tab: Tab,
    pub focus: Focus,
    pub compare: bool,
}

impl Ctx {
    fn active(self, s: State) -> bool {
        match self {
            Ctx::Global | Ctx::Diff | Ctx::Nav => true,
            Ctx::Compare => s.tab == Tab::History && s.compare && s.focus == Focus::History,
            Ctx::Changes => s.tab == Tab::Changes,
            Ctx::ChangesList => s.tab == Tab::Changes && s.focus != Focus::Diff,
            Ctx::ChangesDiff => s.tab == Tab::Changes && s.focus == Focus::Diff,
            Ctx::History => s.tab == Tab::History,
        }
    }
    /// Whether both can be active at once (a shared key would then hide one of them).
    fn overlaps(self, other: Ctx) -> bool {
        use Ctx::*;
        let tab = |c: Ctx| match c {
            Changes | ChangesList | ChangesDiff => Some(Tab::Changes),
            History | Compare => Some(Tab::History),
            Global | Diff | Nav => None,
        };
        match (tab(self), tab(other)) {
            (Some(a), Some(b)) if a != b => false,
            _ => !matches!((self, other), (ChangesList, ChangesDiff) | (ChangesDiff, ChangesList)),
        }
    }
}

macro_rules! actions {
    ($($v:ident $name:literal $ctx:ident [$($key:literal),*] $help:literal,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $($v,)* }
        /// (action, config name, context, default keys, help), in precedence and help order.
        pub const ACTIONS: &[(Action, &str, Ctx, &[&str], &str)] = &[$((Action::$v, $name, Ctx::$ctx, &[$($key),*], $help),)*];
    };
}

actions! {
    Quit "quit" Global ["q"] "quit",
    ChangesTab "changes_tab" Global ["1"] "Changes tab",
    HistoryTab "history_tab" Global ["2"] "History tab",
    Fetch "fetch" Global ["f"] "fetch",
    Pull "pull" Global ["p"] "pull",
    Push "push" Global ["P"] "push",
    Cancel "cancel" Global ["x"] "cancel the running fetch, pull or push",
    Difftool "difftool" Global ["O"] "open the diff in the difftool",
    OpenPr "open_pr" Global ["R"] "open the branch's pull request in the browser",
    Theme "theme" Global ["T"] "theme picker",
    Branches "branches" Global ["B"] "branches: switch, create, rename, delete",
    Stashes "stashes" Global ["S"] "stashes: apply, pop, drop",
    Help "help" Global ["?"] "this help",
    ErrorDetails "error_details" Global ["!"] "details of the last error",
    CompareBehind "compare_prev_tab" Compare ["h", "left"] "compare: previous tab",
    CompareAhead "compare_next_tab" Compare ["l", "right"] "compare: next tab",
    CommitBox "commit_box" Changes ["c"] "Changes: write the commit message",
    Amend "amend" Changes ["A"] "Changes: amend the last commit",
    UndoCommit "undo_commit" Changes ["u"] "Changes: undo the last commit",
    StashPush "stash_push" Changes ["Z"] "Changes: stash all changes",
    Stage "stage" Changes ["space"] "Changes: stage file / line (again: unstage)",
    StageAll "stage_all" Changes ["a"] "Changes: stage everything / the whole file",
    Discard "discard" Changes ["d"] "Changes: discard file / lines (asks first)",
    Filter "filter" ChangesList ["F"] "Changes: filter the file list",
    LineRange "line_range" ChangesDiff ["v"] "Changes: select a range of lines",
    StageHunk "stage_hunk" ChangesDiff ["H"] "Changes: stage the hunk",
    Search "search" History ["/"] "search history (text, path:dir)",
    NextMatch "next_match" History ["n"] "next search match",
    PrevMatch "prev_match" History ["N"] "previous search match",
    Range "range" History ["V"] "select a range of commits",
    Compare "compare" History ["b"] "compare with a branch",
    Tree "tree" History ["t"] "file list as a tree",
    Scope "scope" History ["r"] "branch + upstream ↔ all refs",
    CopySha "copy_sha" History ["y"] "copy the short SHA",
    CopyFullSha "copy_full_sha" History ["Y"] "copy the full SHA",
    Header "header" History ["o"] "expand the commit header",
    Dates "dates" History ["D"] "date format",
    Density "density" History ["z"] "row density",
    ScrollLeft "scroll_left" Diff ["h", "left"] "scroll the diff left",
    ScrollRight "scroll_right" Diff ["l", "right"] "scroll the diff right",
    PrevHunk "prev_hunk" Diff ["["] "previous hunk",
    NextHunk "next_hunk" Diff ["]"] "next hunk",
    PrevFile "prev_file" Diff ["{"] "previous file",
    NextFile "next_file" Diff ["}"] "next file",
    Expand "expand" Diff ["e"] "more context near the cursor",
    ExpandFile "expand_file" Diff ["E"] "whole file",
    Split "split" Diff ["s"] "split ↔ unified",
    Whitespace "whitespace" Diff ["w"] "whitespace mode",
    Wrap "wrap" Diff ["W"] "wrap long lines",
    Fullscreen "fullscreen" Diff ["F"] "full-screen diff",
    Narrower "narrower" Diff ["<"] "shrink the focused pane",
    Wider "wider" Diff [">"] "grow the focused pane",
    Down "down" Nav ["j", "down"] "move down",
    Up "up" Nav ["k", "up"] "move up",
    HalfDown "half_page_down" Nav ["ctrl-d"] "half a page down",
    HalfUp "half_page_up" Nav ["ctrl-u"] "half a page up",
    PageDown "page_down" Nav ["ctrl-f", "pagedown"] "a page down",
    PageUp "page_up" Nav ["ctrl-b", "pageup"] "a page up",
    Top "top" Nav ["g", "home"] "first row",
    Bottom "bottom" Nav ["G", "end"] "last row",
    NextPane "next_pane" Nav ["tab"] "next pane",
    PrevPane "prev_pane" Nav ["shift-tab"] "previous pane",
    Open "open" Nav ["enter"] "open / drill in (a directory: fold)",
    Back "back" Nav ["esc"] "back (ends a range, search or compare)",
}

/// A key as bindings see it: Shift is part of the character (`G`), not a modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
}

impl Key {
    pub fn of(k: &KeyEvent) -> Key {
        let code = match k.code {
            // terminals report Shift-Tab either way
            KeyCode::Tab if k.modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
            c => c,
        };
        Key { code, ctrl: k.modifiers.contains(KeyModifiers::CONTROL), alt: k.modifiers.contains(KeyModifiers::ALT) }
    }

    /// `g`, `G`, `ctrl-d`, `alt-x`, `shift-tab`, `F5`, `enter`, `space`, `pagedown`…
    pub fn parse(s: &str) -> Option<Key> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut rest = s;
        loop {
            let lower = rest.to_ascii_lowercase();
            if let Some(r) = lower.strip_prefix("ctrl-").or_else(|| lower.strip_prefix("c-")) {
                ctrl = true;
                rest = &rest[rest.len() - r.len()..];
            } else if let Some(r) = lower.strip_prefix("alt-").or_else(|| lower.strip_prefix("m-")) {
                alt = true;
                rest = &rest[rest.len() - r.len()..];
            } else if let Some(r) = lower.strip_prefix("shift-") {
                shift = true;
                rest = &rest[rest.len() - r.len()..];
            } else {
                break;
            }
        }
        let mut chars = rest.chars();
        let code = match (chars.next(), chars.next()) {
            (Some(c), None) => KeyCode::Char(if shift { c.to_ascii_uppercase() } else if ctrl { c.to_ascii_lowercase() } else { c }),
            _ => match rest.to_ascii_lowercase().as_str() {
                "enter" | "return" => KeyCode::Enter,
                "esc" | "escape" => KeyCode::Esc,
                "space" => KeyCode::Char(' '),
                "tab" if shift => KeyCode::BackTab,
                "tab" => KeyCode::Tab,
                "backtab" => KeyCode::BackTab,
                "backspace" => KeyCode::Backspace,
                "delete" | "del" => KeyCode::Delete,
                "up" => KeyCode::Up,
                "down" => KeyCode::Down,
                "left" => KeyCode::Left,
                "right" => KeyCode::Right,
                "home" => KeyCode::Home,
                "end" => KeyCode::End,
                "pageup" | "pgup" => KeyCode::PageUp,
                "pagedown" | "pgdn" => KeyCode::PageDown,
                f if f.starts_with('f') => KeyCode::F(f[1..].parse().ok().filter(|n| (1..=24).contains(n))?),
                _ => return None,
            },
        };
        Some(Key { code, ctrl, alt })
    }

    /// The way help and the README write it.
    pub fn label(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "Shift-Tab".into(),
            KeyCode::Backspace => "Backspace".into(),
            KeyCode::Delete => "Delete".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::F(n) => format!("F{n}"),
            c => format!("{c:?}"),
        };
        match (self.ctrl, self.alt) {
            (true, true) => format!("Ctrl-Alt-{base}"),
            (true, false) => format!("Ctrl-{base}"),
            (false, true) => format!("Alt-{base}"),
            _ => base,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Keymap {
    /// Keys per entry of [`ACTIONS`], same order.
    keys: Vec<Vec<Key>>,
}

impl Default for Keymap {
    fn default() -> Keymap {
        Keymap { keys: ACTIONS.iter().map(|a| a.3.iter().map(|k| Key::parse(k).expect("default keys parse")).collect()).collect() }
    }
}

impl Keymap {
    /// `[keys]`: `action = "key"` or `action = ["key", …]`. A rebound action loses its default
    /// keys; a key also bound to another action in an overlapping context is taken from it
    /// (the first binding in the file wins between two of the user's).
    pub fn from_config(t: &toml::Table) -> (Keymap, Vec<String>) {
        let mut map = Keymap::default();
        let mut warnings = Vec::new();
        let mut user: Vec<(usize, Key)> = Vec::new();
        for (name, v) in t {
            let Some(i) = ACTIONS.iter().position(|a| a.1 == name) else {
                warnings.push(format!("keys: unknown action `{name}`"));
                continue;
            };
            let strs: Vec<&str> = match v {
                toml::Value::String(s) => vec![s.as_str()],
                toml::Value::Array(a) => a.iter().filter_map(toml::Value::as_str).collect(),
                _ => {
                    warnings.push(format!("keys: `{name}` wants a key or a list of keys"));
                    continue;
                }
            };
            let mut keys = Vec::new();
            for s in strs {
                match Key::parse(s) {
                    // handled before the keymap is asked, so a binding there would never fire
                    Some(k) if k.ctrl && !k.alt && matches!(k.code, KeyCode::Char('c' | 'z')) => {
                        warnings.push(format!("keys: `{name}`: {} is fixed (quit / suspend) and cannot be bound", k.label()))
                    }
                    Some(k) => keys.push(k),
                    None => warnings.push(format!("keys: `{name}`: unknown key `{s}`")),
                }
            }
            if keys.is_empty() {
                continue;
            }
            for &k in &keys {
                if let Some(&(j, _)) = user.iter().find(|&&(j, uk)| uk == k && j != i && ACTIONS[j].2.overlaps(ACTIONS[i].2)) {
                    warnings.push(format!("keys: {} is bound to both `{}` and `{name}`; `{}` keeps it", k.label(), ACTIONS[j].1, ACTIONS[j].1));
                }
            }
            let keys: Vec<Key> = keys.into_iter().filter(|k| !user.iter().any(|&(j, uk)| uk == *k && j != i && ACTIONS[j].2.overlaps(ACTIONS[i].2))).collect();
            user.extend(keys.iter().map(|&k| (i, k)));
            map.keys[i] = keys;
        }
        // the user's keys leave the defaults that shared them
        for &(i, k) in &user {
            for (j, keys) in map.keys.iter_mut().enumerate() {
                if j != i && ACTIONS[j].2.overlaps(ACTIONS[i].2) && !user.iter().any(|&(uj, _)| uj == j) && keys.contains(&k) {
                    keys.retain(|x| *x != k);
                    if keys.is_empty() {
                        warnings.push(format!("keys: `{}` has no key left (its {} now does `{}`)", ACTIONS[j].1, k.label(), ACTIONS[i].1));
                    }
                }
            }
        }
        (map, warnings)
    }

    /// The action `k` means on the current screen.
    pub fn resolve(&self, k: &KeyEvent, s: State) -> Option<Action> {
        let key = Key::of(k);
        ACTIONS.iter().zip(&self.keys).find(|((_, _, ctx, _, _), keys)| ctx.active(s) && keys.contains(&key)).map(|(a, _)| a.0)
    }

    /// (action, config name, keys, help) in help order.
    pub fn bindings(&self) -> Vec<(Action, &'static str, Vec<Key>, &'static str)> {
        ACTIONS.iter().zip(&self.keys).map(|(a, keys)| (a.0, a.1, keys.clone(), a.4)).collect()
    }

    pub fn keys_of(&self, a: Action) -> &[Key] {
        ACTIONS.iter().position(|x| x.0 == a).map_or(&[], |i| &self.keys[i])
    }

    /// The README's key table: `| Keys | Action | Name |`.
    pub fn markdown(&self) -> String {
        let mut out = String::from("| Keys | Action | Config name |\n|---|---|---|\n");
        for (_, name, keys, help) in self.bindings() {
            let ks: Vec<String> = keys.iter().map(|k| format!("`{}`", k.label().replace('|', "\\|"))).collect();
            out.push_str(&format!("| {} | {help} | `{name}` |\n", ks.join(" ")));
        }
        out
    }
}
