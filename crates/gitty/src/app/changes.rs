//! Changes tab state: working-tree status, the selected file's HEAD → worktree diff with its
//! staged lines, and the write queue's progress.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gitty_core::diff::FileDiff;
use gitty_core::stage::Texts;
use gitty_core::status::{Status, StatusEntry};
use gitty_core::watch::{Changed, IndexMark};

use gitty_core::diff::view::{Row, SplitRow};
use gitty_core::commit_files::BlobId;
use gitty_core::diff::text::Text;
use gitty_core::stage::{Plan, build, change_lines, plan};
use gitty_core::status::EntryKind;

use super::diffstate::{DiffState, VRow};
use super::{App, Overlay, Tab, Toast};
use crate::msg::{DiffKey, Msg, Request, WriteOp};

/// While focused, status is re-read at least this often (spec §5.4).
pub const BACKSTOP: Duration = Duration::from_secs(60);
const LOG_LINES: usize = 500;
/// Slow statuses trigger an index refresh at most this often.
const REFRESH_EVERY: Duration = Duration::from_secs(60);

/// File-list filter (`F`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Included,
    Excluded,
    New,
    Modified,
    Deleted,
}

impl Filter {
    pub fn next(self) -> Filter {
        match self {
            Filter::All => Filter::Included,
            Filter::Included => Filter::Excluded,
            Filter::Excluded => Filter::New,
            Filter::New => Filter::Modified,
            Filter::Modified => Filter::Deleted,
            Filter::Deleted => Filter::All,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Included => "included",
            Filter::Excluded => "excluded",
            Filter::New => "new",
            Filter::Modified => "modified",
            Filter::Deleted => "deleted",
        }
    }
    pub fn keeps(self, e: &StatusEntry) -> bool {
        use gitty_core::status::Check;
        match self {
            Filter::All => true,
            Filter::Included => e.check() != Check::Unstaged,
            Filter::Excluded => e.check() != Check::Staged,
            Filter::New => e.letter() == 'A',
            Filter::Modified => matches!(e.letter(), 'M' | 'R' | 'C' | 'T' | 'U'),
            Filter::Deleted => e.letter() == 'D',
        }
    }
}

/// The diff installed for the selected file, with what staging needs.
pub struct ChangeView {
    pub entry: StatusEntry,
    pub texts: Texts,
    pub diff: Arc<FileDiff>,
    /// Staged flag per changed line; None when only whole-file staging applies.
    pub staged: Option<Vec<bool>>,
    pub divergent: bool,
    /// Flag index of each old (HEAD) and new (worktree) line that changed.
    old_flag: Vec<Option<u32>>,
    new_flag: Vec<Option<u32>>,
}

impl ChangeView {
    pub fn new(entry: StatusEntry, texts: Texts, diff: Arc<FileDiff>, staged: Option<Vec<bool>>, divergent: bool) -> ChangeView {
        let mut old_flag = vec![None; diff.old.len() as usize];
        let mut new_flag = vec![None; diff.new.len() as usize];
        for (k, c) in change_lines(&diff.ops).iter().enumerate() {
            match (c.old, c.new) {
                (Some(o), _) => old_flag[o as usize] = Some(k as u32),
                (_, Some(n)) => new_flag[n as usize] = Some(k as u32),
                _ => {}
            }
        }
        ChangeView { entry, texts, diff, staged, divergent, old_flag, new_flag }
    }

    /// Flag indices of the changed lines a diff row shows; in split view `side` keeps one half.
    pub fn flags_of(&self, row: &VRow, side: Option<Side>) -> Vec<usize> {
        let (o, n) = match row {
            VRow::Row(Row::Del { old, .. }) => (Some(*old), None),
            VRow::Row(Row::Add { new, .. }) => (None, Some(*new)),
            VRow::Split(SplitRow::Change { old, new, .. }) => match side {
                Some(Side::Old) => (*old, None),
                Some(Side::New) => (None, *new),
                None => (*old, *new),
            },
            _ => (None, None),
        };
        let o = o.and_then(|o| self.old_flag.get(o as usize).copied().flatten());
        let n = n.and_then(|n| self.new_flag.get(n as usize).copied().flatten());
        o.into_iter().chain(n).map(|k| k as usize).collect()
    }

    /// Whether the old/new line is staged (`old` picks the side).
    pub fn is_staged(&self, line: u32, old: bool) -> bool {
        let map = if old { &self.old_flag } else { &self.new_flag };
        let k = map.get(line as usize).copied().flatten();
        k.zip(self.staged.as_ref()).is_some_and(|(k, s)| s.get(k as usize).copied().unwrap_or(false))
    }
}

/// A half of the split view: deletions on the left, additions on the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Old,
    New,
}

#[derive(Default)]
pub struct Changes {
    pub status: Option<Status>,
    pub status_error: Option<String>,
    /// Selection and scroll, as positions in [`Changes::visible`].
    pub sel: usize,
    pub scroll: usize,
    pub filter: Filter,
    pub current: Option<ChangeView>,
    pub diff_error: Option<(String, String)>,
    /// Writes queued or running.
    pub busy: usize,
    /// Output of the latest write (hooks).
    pub log: Vec<String>,
    status_gen: u64,
    status_in_flight: bool,
    status_again: bool,
    last_status: Option<Instant>,
    diff_gen: u64,
    pub(super) force_text: bool,
    /// `v`: the other end of the line range.
    pub visual: Option<usize>,
    /// A gutter drag (mouse) is selecting lines.
    pub(super) gutter_drag: bool,
    /// Split view: the half the last click landed on; Space and gutter clicks stage only it.
    pub side: Option<Side>,
    pub commit: super::commit::CommitBox,
    last_refresh: Option<Instant>,
}

impl Changes {
    pub fn entries(&self) -> &[StatusEntry] {
        self.status.as_ref().map_or(&[], |s| &s.entries)
    }
    /// Indices into [`Changes::entries`] that pass the filter.
    pub fn visible(&self) -> Vec<usize> {
        self.entries().iter().enumerate().filter(|(_, e)| self.filter.keeps(e)).map(|(i, _)| i).collect()
    }
    pub fn selected(&self) -> Option<&StatusEntry> {
        self.visible().get(self.sel).map(|&i| &self.entries()[i])
    }
}

impl App {
    pub fn set_tab(&mut self, tab: Tab) {
        if tab == self.tab {
            return;
        }
        if tab == Tab::Files && self.workdir.is_none() {
            self.toast = Some(Toast { what: "Files needs a working tree (this is a bare repository)".into(), detail: String::new(), error: false });
            return;
        }
        // a half-typed query belongs to the History tab: the bar must not swallow keys here
        self.search.bar = None;
        let from = self.tab;
        self.tab = tab;
        if from == Tab::Files {
            self.files_leave();
        }
        // the diff pane belongs to the active tab; the other tab reloads from its cache
        self.diff = None;
        self.diff_wanted = None;
        self.diff_error = None;
        self.changes.current = None;
        self.file_gen = crate::msg::Gens::bump(&self.gens.file);
        // History's pane focus comes back when leaving the other two tabs for it
        if from == Tab::History {
            self.history_focus = self.focus;
        }
        match tab {
            Tab::Changes => {
                self.focus = super::Focus::Files;
                self.request_status();
                self.request_change_diff();
            }
            Tab::Files => {
                self.focus = super::Focus::Files;
                self.files_enter();
            }
            Tab::History => {
                self.focus = self.history_focus;
                self.last_diff_request = None;
                if self.current_file().is_some() {
                    self.schedule_diff();
                }
            }
        }
    }

    /// Asks for a status run, or for one more after the running one.
    pub fn request_status(&mut self) {
        if self.changes.status_in_flight {
            self.changes.status_again = true;
            return;
        }
        self.changes.status_in_flight = true;
        self.changes.status_gen += 1;
        self.outbox.push(Request::Status { generation: self.changes.status_gen, mark: self.index_mark.clone() });
    }

    /// Loads the selected file's diff (Changes tab only).
    pub fn request_change_diff(&mut self) {
        if self.tab != Tab::Changes {
            return;
        }
        self.changes.diff_gen += 1;
        let Some(entry) = self.changes.selected().cloned() else {
            self.diff = None;
            self.changes.current = None;
            return;
        };
        self.outbox.push(Request::ChangeDiff { generation: self.changes.diff_gen, entry, opts: self.diff_opts(), force_text: self.changes.force_text });
    }

    pub fn select_change(&mut self, i: usize) {
        let n = self.changes.visible().len();
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        if i != self.changes.sel {
            self.changes.force_text = false;
            self.changes.side = None;
        }
        self.changes.sel = i;
        let cap = self.files_capacity();
        let c = &mut self.changes;
        if c.sel < c.scroll {
            c.scroll = c.sel;
        } else if c.sel >= c.scroll + cap {
            c.scroll = c.sel + 1 - cap;
        }
        self.request_change_diff();
    }

    pub fn write(&mut self, op: WriteOp) {
        self.changes.busy += 1;
        self.changes.log.clear();
        self.outbox.push(Request::Write(op));
    }

    pub fn handle_focus(&mut self, gained: bool) {
        self.focused = gained;
        if gained {
            self.request_status();
            self.outbox.push(Request::Refs);
        }
    }

    pub(super) fn status_deadline(&self) -> Option<Instant> {
        if !self.focused || self.changes.status_in_flight {
            return None;
        }
        Some(self.changes.last_status.map_or(self.clock, |t| t + BACKSTOP))
    }

    pub(super) fn tick_changes(&mut self, at: Instant) {
        if self.status_deadline().is_some_and(|d| d <= at) {
            self.request_status();
        }
    }

    /// Messages for the Changes tab; returns the message back when it is not one.
    pub(super) fn handle_changes_msg(&mut self, m: Msg) -> Option<Msg> {
        match m {
            Msg::Status { generation, result } => {
                if generation != self.changes.status_gen {
                    return None;
                }
                self.changes.status_in_flight = false;
                self.changes.last_status = Some(self.clock);
                match result {
                    Ok(st) => self.install_status(st),
                    Err(e) => self.changes.status_error = Some(e),
                }
                if std::mem::take(&mut self.changes.status_again) {
                    self.request_status();
                }
            }
            Msg::OpState { generation, state } => {
                // one that a newer status run has just superseded still beats the state shown
                if generation + 1 >= self.changes.status_gen {
                    self.op = state;
                    // the state ended (finished or aborted anywhere): its dialog has nothing to act on
                    if self.op.is_none() && matches!(self.overlay, Some(Overlay::InProgress)) {
                        self.overlay = None;
                    }
                }
            }
            Msg::ChangeDiff { generation, entry, key, diff, texts, staged, divergent } => {
                if generation != self.changes.diff_gen || self.tab != Tab::Changes {
                    return None;
                }
                self.changes.diff_error = None;
                self.install_change_diff(key, ChangeView::new(entry, texts, diff, staged, divergent));
            }
            Msg::ChangeDiffError { generation, path, detail } => {
                if generation == self.changes.diff_gen {
                    self.diff = None;
                    self.changes.current = None;
                    self.changes.diff_error = Some((path, detail));
                }
            }
            Msg::WriteLog { line } => {
                if self.changes.log.len() < LOG_LINES {
                    self.changes.log.push(line);
                }
            }
            Msg::WriteDone { op: WriteOp::RefreshIndex, .. } => {}
            Msg::WriteDone { op, result } => {
                self.changes.busy = self.changes.busy.saturating_sub(1);
                match result {
                    _ if self.commit_done(&op, &result) => {}
                    // a discard whose copies did not go to the Trash says where they are
                    Ok(Some(note)) => self.toast = Some(Toast { what: note, detail: String::new(), error: false }),
                    Ok(None) => {}
                    Err(detail) if self.offer_force_delete(&op, &detail) => {}
                    Err(detail) => {
                        let what = format!("{} failed", op.label());
                        self.toast = Some(Toast { what, detail, error: true });
                    }
                }
                self.request_status();
                if op.moves_head() {
                    self.outbox.push(Request::Refs);
                }
                if op.touches_stash() {
                    self.outbox.push(Request::StashList);
                }
            }
            Msg::StagedMarkers { op, id, files } => self.offer_continue_anyway(op, id, files),
            Msg::HeadMessage { result } => self.install_head_message(result),
            Msg::StaleIndexLock { seen } => {
                // queued like the other dialogs: it never replaces an open overlay
                self.pending_stale_lock = Some(seen);
                self.next_ask();
            }
            Msg::StatusSlow => {
                if self.changes.last_refresh.is_none_or(|t| self.clock.saturating_duration_since(t) >= REFRESH_EVERY) {
                    self.changes.last_refresh = Some(self.clock);
                    // not counted in `busy`: it is housekeeping, not the user's work
                    self.outbox.push(Request::Write(WriteOp::RefreshIndex));
                }
            }
            Msg::Changed(c) => {
                if c.intersects(Changed::WORKTREE | Changed::INDEX | Changed::IGNORE_RULES | Changed::STATE) {
                    self.request_status();
                }
                if self.tab == Tab::Files && c.intersects(Changed::WORKTREE | Changed::INDEX | Changed::IGNORE_RULES) {
                    self.refresh_files();
                }
                if c.intersects(Changed::REFS | Changed::REMOTE | Changed::CONFIG | Changed::STASH) {
                    self.outbox.push(Request::Refs);
                }
            }
            m => return Some(m),
        }
        None
    }

    fn install_status(&mut self, st: Status) {
        self.track_committed_head(st.head.as_deref());
        let keep = self.changes.selected().map(|e| e.path.clone());
        self.changes.status = Some(st);
        self.changes.status_error = None;
        let visible = self.changes.visible();
        let entries = self.changes.entries();
        let found = keep.and_then(|p| visible.iter().position(|&i| entries[i].path == p));
        self.changes.sel = found.unwrap_or(self.changes.sel).min(visible.len().saturating_sub(1));
        self.changes.scroll = self.changes.scroll.min(self.changes.sel);
        // the selected file may have changed on disk or in the index
        self.request_change_diff();
    }

    fn install_change_diff(&mut self, key: DiffKey, view: ChangeView) {
        let same_file = self.diff.as_ref().is_some_and(|d| d.key.path == key.path);
        let (cursor, scroll, hscroll) = self.diff.as_ref().map_or((0, 0, 0), |d| (d.cursor, d.scroll, d.hscroll));
        let complete = (0..view.diff.changes.len()).all(|c| view.diff.intraline_ready(c).is_some());
        if !complete {
            self.outbox.push(Request::Intraline { generation: self.file_gen, key: key.clone(), diff: view.diff.clone() });
        }
        let mut d = DiffState::new(key, view.diff.clone());
        if same_file {
            let n = d.rows(self.split_active());
            d.cursor = cursor.min(n.saturating_sub(1));
            d.scroll = scroll.min(d.cursor);
            d.hscroll = hscroll;
        }
        self.diff = Some(d);
        self.changes.current = Some(view);
        self.request_highlights();
    }

    /// Why lines of the current file cannot be staged one by one (shown above the diff).
    pub fn changes_notice(&self) -> Option<String> {
        let v = self.changes.current.as_ref().filter(|_| self.tab == Tab::Changes)?;
        if v.staged.is_some() || !v.diff.is_text() {
            return None;
        }
        Some(if v.entry.is_conflicted() {
            "Conflicted: resolve it, then stage the whole file".into()
        } else if !v.entry.line_stageable() || v.texts.wt_mode & 0o170000 == 0o120000 {
            "Symlinks, submodules and file-type changes are staged as whole files".into()
        } else if v.divergent {
            "The index holds a version of its own (staged elsewhere): stage or unstage the whole file".into()
        } else {
            "Whitespace is hidden (w): only whole files can be staged".into()
        })
    }

    fn whole_file_paths(e: &StatusEntry) -> Vec<String> {
        let mut v = vec![e.path.clone()];
        v.extend(e.orig_path.clone());
        v
    }

    /// Space / checkbox: stage a whole file, or unstage it when fully staged.
    pub fn toggle_file(&mut self, visible_pos: usize) {
        let Some(e) = self.changes.visible().get(visible_pos).map(|&i| self.changes.entries()[i].clone()) else { return };
        let paths = Self::whole_file_paths(&e);
        if e.check() == gitty_core::status::Check::Staged {
            self.write(WriteOp::Unstage(paths));
        } else {
            self.write(WriteOp::Stage(paths));
        }
    }

    /// `a` in the file list / header checkbox: everything staged, or nothing.
    pub fn toggle_all_files(&mut self) {
        let all = !self.changes.entries().is_empty() && self.changes.entries().iter().all(|e| e.check() == gitty_core::status::Check::Staged);
        self.write(if all { WriteOp::UnstageAll } else { WriteOp::StageAll });
    }

    /// Diff rows the next line action applies to: the `v` range, or the cursor row.
    fn target_rows(&self) -> std::ops::RangeInclusive<usize> {
        let c = self.diff.as_ref().map_or(0, |d| d.cursor);
        match self.changes.visual {
            Some(a) => a.min(c)..=a.max(c),
            None => c..=c,
        }
    }

    /// The hunk around the cursor: rows between the surrounding gap/header rows.
    fn hunk_rows(&self) -> std::ops::RangeInclusive<usize> {
        let Some(d) = &self.diff else { return 0..=0 };
        let split = self.split_active();
        let is_edge = |i: usize| matches!(d.vrow(i, split), None | Some(VRow::Header(_)) | Some(VRow::Row(Row::Gap { .. })) | Some(VRow::Split(SplitRow::Gap { .. })));
        let mut lo = d.cursor;
        while lo > 0 && !is_edge(lo - 1) {
            lo -= 1;
        }
        let mut hi = d.cursor;
        while !is_edge(hi + 1) {
            hi += 1;
        }
        lo..=hi
    }

    fn flags_in(&self, rows: std::ops::RangeInclusive<usize>, side: Option<Side>) -> Vec<usize> {
        let (Some(d), Some(v)) = (&self.diff, &self.changes.current) else { return Vec::new() };
        let split = self.split_active();
        let side = side.filter(|_| split);
        rows.filter_map(|i| d.vrow(i, split)).flat_map(|r| v.flags_of(&r, side)).collect()
    }

    /// Stages the lines, or unstages them when all are staged already.
    fn toggle_flags(&mut self, ks: Vec<usize>) {
        let Some(v) = self.changes.current.as_mut() else { return };
        let Some(staged) = v.staged.as_mut() else {
            let what = self.changes_notice().unwrap_or_else(|| "Only whole files can be staged here".into());
            self.toast = Some(Toast { what, detail: String::new(), error: false });
            return;
        };
        if ks.is_empty() {
            return;
        }
        let on = !ks.iter().all(|&k| staged.get(k).copied().unwrap_or(false));
        for &k in &ks {
            if let Some(f) = staged.get_mut(k) {
                *f = on;
            }
        }
        let op = WriteOp::SetStaged { entry: v.entry.clone(), texts: v.texts.clone(), diff: v.diff.clone(), flags: staged.clone() };
        // the writer runs ops in order: the next toggle builds on this one's index, not on the
        // status/diff round trip still in flight
        let in_index = match plan(&v.entry, &v.texts, &v.diff.ops, staged) {
            Plan::Nothing => v.entry.index_blob.is_some(),
            Plan::StageFile(_) => v.texts.wt_mode != 0,
            Plan::UnstageFile(_) => v.entry.head_blob.is_some(),
            Plan::Patch { .. } => true,
        };
        let target = build(&v.texts.head, &v.texts.wt, &v.diff.ops, staged);
        v.entry.index_blob = in_index.then(|| BlobId::hash_of(&target));
        v.texts.index = Arc::new(Text::new(target));
        self.changes.visual = None;
        self.write(op);
    }

    /// Space in the diff.
    pub fn toggle_lines(&mut self) {
        let ks = self.flags_in(self.target_rows(), self.changes.side);
        self.toggle_flags(ks);
    }

    /// `H`.
    pub fn toggle_hunk(&mut self) {
        let ks = self.flags_in(self.hunk_rows(), None);
        self.toggle_flags(ks);
    }

    /// `a` in the diff: the whole current file.
    pub fn toggle_current_file(&mut self) {
        self.toggle_file(self.changes.sel);
    }

    /// `d` in the file list: discard all changes to the selected file, after confirmation.
    pub fn confirm_discard_file(&mut self) {
        let Some(e) = self.changes.selected().cloned() else { return };
        let in_head = e.head_blob.is_some() || e.head_mode != 0;
        let path = e.path.clone();
        // a rename's HEAD content lives at the original path; a copy leaves the original alone
        let (restore, remove, title) = match (&e.orig_path, e.kind) {
            (Some(orig), EntryKind::Renamed) => (vec![orig.clone()], vec![path.clone()], format!("Undo the rename and changes: {path} back to {orig}?")),
            (Some(_), _) => (Vec::new(), vec![path.clone()], format!("Delete the copy {path}?")),
            (None, _) if in_head => (vec![path.clone()], Vec::new(), format!("Discard all changes to {path}?")),
            (None, _) => (Vec::new(), vec![path.clone()], format!("Delete the new file {path}?")),
        };
        self.overlay = Some(Overlay::Confirm {
            title,
            body: "A copy goes to the Trash.".into(),
            op: WriteOp::DiscardFiles { restore, remove },
        });
    }

    /// `d` in the diff: discard the target lines from the working tree, after confirmation.
    pub fn confirm_discard_lines(&mut self) {
        let ks = self.flags_in(self.target_rows(), self.changes.side);
        let Some(v) = &self.changes.current else { return };
        if ks.is_empty() {
            return;
        }
        if v.staged.is_none() || !v.texts.wt_is_raw {
            let what = if !v.texts.wt_is_raw {
                "This file is converted on checkout (line endings or filters): discard the whole file from the file list"
            } else {
                "Only whole files can be discarded here: use d in the file list"
            };
            self.toast = Some(Toast { what: what.into(), detail: String::new(), error: false });
            return;
        }
        // keep every change except the discarded ones
        let n = change_lines(&v.diff.ops).len();
        let keep: Vec<bool> = (0..n).map(|k| !ks.contains(&k)).collect();
        let bytes = build(&v.diff.old, &v.diff.new, &v.diff.ops, &keep);
        let lines = if ks.len() == 1 { "line".to_string() } else { format!("{} lines", ks.len()) };
        let mut op = WriteOp::WriteFile { path: v.entry.path.clone(), bytes, expect: v.texts.wt_blob, head_path: v.entry.orig_path.clone().unwrap_or_else(|| v.entry.path.clone()), head: v.entry.head_blob };
        // a staged line would still be committed after leaving the worktree: unstage it first
        if let Some(staged) = v.staged.as_ref().filter(|s| ks.iter().any(|&k| s.get(k).copied().unwrap_or(false))) {
            let mut flags = staged.clone();
            for &k in &ks {
                if let Some(f) = flags.get_mut(k) {
                    *f = false;
                }
            }
            let unstage = WriteOp::SetStaged { entry: v.entry.clone(), texts: v.texts.clone(), diff: v.diff.clone(), flags };
            op = WriteOp::Seq(vec![unstage, op]);
        }
        self.overlay = Some(Overlay::Confirm {
            title: format!("Discard the selected {lines} in {}?", v.entry.path),
            body: "A copy of the file goes to the Trash.".into(),
            op,
        });
        self.changes.visual = None;
    }

    /// Lets status runs mark the index state they saw, so the watcher skips it.
    pub fn set_index_mark(&mut self, mark: IndexMark) {
        self.index_mark = Some(mark);
    }
}
