//! Compare to a branch (spec §11.2): `b` picks a branch, then the history pane lists the commits
//! only it has (Behind), the commits only HEAD has (Ahead), and the files it changed since the
//! merge base (Files).

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gitty_core::CommitId;
use gitty_core::compare::Compare;
use gitty_core::history::CommitRow;
use gitty_core::refs::RefKind;

use super::{App, Overlay, Toast};
use crate::editor::Editor;
use crate::msg::{FilesOf, Msg, Request};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareTab {
    Behind,
    Ahead,
    Files,
}

pub struct CompareMode {
    pub other: String,
    pub other_id: CommitId,
    pub tab: CompareTab,
    /// None while git works it out.
    pub result: Option<Compare>,
    /// Selection and scroll of the Behind and Ahead lists.
    pub sel: [usize; 2],
    pub scroll: [usize; 2],
    pub rows: HashMap<CommitId, CommitRow>,
    requested: HashSet<CommitId>,
    generation: u64,
    saved: Saved,
}

/// What the history pane showed before compare. The history row itself stays in
/// `App::selected`, which a refresh keeps on that commit.
#[derive(Clone)]
struct Saved {
    scroll: usize,
    commit: Option<CommitId>,
    /// The selected file, restored by path when its list is installed.
    file: Option<String>,
    /// The range's anchor commit (rows move when a refresh walks the history again).
    range_anchor: Option<CommitId>,
    /// The active search's query as typed.
    search: Option<String>,
}

impl CompareMode {
    /// The commits of the Behind or Ahead tab (empty for Files and while loading).
    pub fn list(&self) -> &[CommitId] {
        match (&self.result, self.tab) {
            (Some(r), CompareTab::Behind) => &r.behind,
            (Some(r), CompareTab::Ahead) => &r.ahead,
            _ => &[],
        }
    }
    fn slot(&self) -> Option<usize> {
        match self.tab {
            CompareTab::Behind => Some(0),
            CompareTab::Ahead => Some(1),
            CompareTab::Files => None,
        }
    }
    pub fn selected(&self) -> Option<usize> {
        self.slot().map(|s| self.sel[s])
    }
    pub fn first_visible(&self) -> usize {
        self.slot().map_or(0, |s| self.scroll[s])
    }
}

/// Subsequence score: each matched letter counts, more at a word start (after `/ - _ .`) and
/// when it follows the previous match. None when `q` is not a subsequence of `name`.
fn score(q: &[char], name: &str) -> Option<i32> {
    let mut qi = 0;
    let mut total = 0;
    let mut prev_match: Option<usize> = None;
    let mut prev_char = None;
    for (i, c) in name.chars().enumerate() {
        if qi == q.len() {
            break;
        }
        if c.to_lowercase().eq(q[qi].to_lowercase()) {
            let mut s = 1;
            if prev_char.is_none_or(|p: char| matches!(p, '/' | '-' | '_' | '.')) {
                s += 10;
            }
            if prev_match == Some(i.wrapping_sub(1)) {
                s += 5;
            }
            total += s;
            prev_match = Some(i);
            qi += 1;
        }
        prev_char = Some(c);
    }
    (qi == q.len()).then_some(total)
}

/// Indices of `names` matching `query`, best first, ties by name.
pub fn fuzzy_rank(query: &str, names: &[String]) -> Vec<usize> {
    let q: Vec<char> = query.chars().collect();
    let mut scored: Vec<(i32, usize)> = names.iter().enumerate().filter_map(|(i, n)| score(&q, n).map(|s| (s, i))).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| names[a.1].cmp(&names[b.1])));
    scored.into_iter().map(|(_, i)| i).collect()
}

impl App {
    /// Local and remote branches other than the checked-out one, by name.
    pub fn branch_candidates(&self) -> Vec<(String, CommitId)> {
        let Some(refs) = &self.refs else { return Vec::new() };
        let mut v: Vec<(String, CommitId)> = refs
            .labels
            .iter()
            .flat_map(|(id, ls)| ls.iter().map(move |l| (l, *id)))
            .filter(|(l, _)| matches!(l.kind, RefKind::LocalBranch | RefKind::RemoteBranch) && !l.is_head && !l.name.ends_with("/HEAD"))
            .map(|(l, id)| (l.name.clone(), id))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The picker's matches for its query, best first.
    pub fn picker_matches(&self, query: &str) -> Vec<(String, CommitId)> {
        let all = self.branch_candidates();
        let names: Vec<String> = all.iter().map(|b| b.0.clone()).collect();
        fuzzy_rank(query, &names).into_iter().map(|i| all[i].clone()).collect()
    }

    pub fn open_branch_picker(&mut self) {
        if self.refs.as_ref().and_then(|r| r.head_id()).is_none() {
            self.toast = Some(Toast { what: "Nothing to compare yet".into(), detail: String::new(), error: false });
            return;
        }
        self.overlay = Some(Overlay::BranchPicker { query: Editor::single(), sel: 0 });
    }

    /// Keys while the picker is open: letters filter, arrows (and Ctrl-n/p) move.
    pub(super) fn picker_key(&mut self, mut query: Editor, mut sel: usize, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.picker_matches(query.text()).len();
        match k.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                if let Some((name, id)) = self.picker_matches(query.text()).into_iter().nth(sel) {
                    self.start_compare(name, id);
                    return;
                }
            }
            KeyCode::Down | KeyCode::Tab => sel = (sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('n') if ctrl => sel = (sel + 1).min(n.saturating_sub(1)),
            KeyCode::Up | KeyCode::BackTab => sel = sel.saturating_sub(1),
            KeyCode::Char('p') if ctrl => sel = sel.saturating_sub(1),
            KeyCode::Backspace => {
                query.backspace();
                sel = 0;
            }
            KeyCode::Char('u') if ctrl => {
                query.set("");
                sel = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                let mut b = [0; 4];
                query.insert(c.encode_utf8(&mut b));
                sel = 0;
            }
            _ => {}
        }
        self.overlay = Some(Overlay::BranchPicker { query, sel });
    }

    fn start_compare(&mut self, other: String, other_id: CommitId) {
        let Some(head) = self.refs.as_ref().and_then(|r| r.head_id()) else { return };
        let saved = match &self.compare {
            Some(c) => c.saved.clone(),
            None => Saved {
                scroll: self.list_scroll,
                commit: self.selected_id,
                file: self.current_file().map(|f| f.path.clone()),
                range_anchor: self.range_anchor.and_then(|i| self.history_id_at(i)),
                search: self.search_active().then(|| self.search.input.clone()),
            },
        };
        self.compare_gen += 1;
        self.files_restore = None;
        self.anchor_restore = None;
        self.end_range();
        self.clear_search();
        self.compare = Some(CompareMode {
            other,
            other_id,
            tab: CompareTab::Behind,
            result: None,
            sel: [0; 2],
            scroll: [0; 2],
            rows: HashMap::new(),
            requested: HashSet::new(),
            generation: self.compare_gen,
            saved,
        });
        self.outbox.push(Request::Compare { generation: self.compare_gen, head, other: other_id });
    }

    /// Back to the history as it was before `b`.
    pub fn leave_compare(&mut self) {
        let Some(c) = self.compare.take() else { return };
        let s = c.saved;
        self.list_scroll = s.scroll;
        self.files_restore = s.file;
        self.anchor_restore = s.range_anchor;
        // while a new walk is still looking for the saved commit, it restores the rest on arrival
        if self.reselect.is_none() {
            self.restore_anchor();
            self.select_at(self.selected);
            self.apply_files_restore();
        }
        if let Some(input) = s.search {
            self.resume_search(input);
        }
    }

    pub(super) fn handle_compare_msg(&mut self, m: Msg) -> Option<Msg> {
        match m {
            Msg::Compare { generation, result } => {
                let c = self.compare.as_mut().filter(|c| c.generation == generation)?;
                match result {
                    Ok(r) => {
                        // nothing behind: start on Ahead, so the pane is not empty
                        if r.behind.is_empty() && !r.ahead.is_empty() {
                            c.tab = CompareTab::Ahead;
                        }
                        c.result = Some(r);
                        self.compare_show();
                    }
                    Err(detail) => {
                        self.compare = None;
                        self.toast = Some(Toast { what: "comparing branches".into(), detail, error: true });
                    }
                }
                None
            }
            Msg::CommitRows { rows } => {
                if let Some(c) = self.compare.as_mut() {
                    c.rows.extend(rows.into_iter().map(|r| (r.id, r)));
                }
                None
            }
            m => Some(m),
        }
    }

    /// Shows the selected compare commit (or the Files tab's list) and decodes visible rows.
    fn compare_show(&mut self) {
        let Some(c) = &self.compare else { return };
        match c.selected().and_then(|i| c.list().get(i).copied()) {
            Some(id) => self.show_commit(id),
            None => self.load_files(),
        }
        self.request_compare_rows();
    }

    fn request_compare_rows(&mut self) {
        let cap = self.list_capacity().saturating_sub(1);
        let Some(c) = self.compare.as_mut() else { return };
        let first = c.first_visible();
        let ids: Vec<CommitId> = c.list().iter().skip(first.saturating_sub(cap)).take(3 * cap.max(1)).copied().filter(|id| !c.rows.contains_key(id) && !c.requested.contains(id)).collect();
        if ids.is_empty() {
            return;
        }
        c.requested.extend(ids.iter().copied());
        self.outbox.push(Request::CommitRows { ids });
    }

    /// The history commit a refresh keeps selected: compare's own selection is not it.
    pub(super) fn history_selection(&self) -> Option<CommitId> {
        match &self.compare {
            Some(c) => c.saved.commit,
            None => self.selected_id,
        }
    }

    /// The list Files shows in compare mode's Files tab.
    pub(super) fn compare_files(&self) -> Option<FilesOf> {
        let c = self.compare.as_ref()?;
        let r = c.result.as_ref()?;
        (c.tab == CompareTab::Files).then_some(FilesOf::Between { from: r.merge_base, to: c.other_id })
    }

    /// `h`/`l`: previous / next tab.
    pub fn compare_tab(&mut self, dir: i32) {
        let Some(c) = self.compare.as_mut() else { return };
        let tabs = [CompareTab::Behind, CompareTab::Ahead, CompareTab::Files];
        let i = tabs.iter().position(|t| *t == c.tab).unwrap_or(0) as i32;
        c.tab = tabs[(i + dir).clamp(0, 2) as usize];
        self.compare_show();
    }

    /// Selects row `i` of the current compare list.
    pub fn compare_select(&mut self, i: usize) {
        let cap = self.list_capacity().saturating_sub(1).max(1);
        let Some(c) = self.compare.as_mut() else { return };
        let (Some(slot), n) = (c.slot(), c.list().len()) else { return };
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        c.sel[slot] = i;
        if i < c.scroll[slot] {
            c.scroll[slot] = i;
        } else if i >= c.scroll[slot] + cap {
            c.scroll[slot] = i + 1 - cap;
        }
        self.compare_show();
    }

    pub fn compare_len(&self) -> usize {
        self.compare.as_ref().map_or(0, |c| c.list().len())
    }
}
