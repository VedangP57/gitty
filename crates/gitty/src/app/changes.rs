//! Changes tab state: working-tree status, the selected file's HEAD → worktree diff with its
//! staged lines, and the write queue's progress.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gitty_core::diff::FileDiff;
use gitty_core::stage::Texts;
use gitty_core::status::{Status, StatusEntry};
use gitty_core::watch::{Changed, IndexMark};

use super::diffstate::DiffState;
use super::{App, Tab, Toast};
use crate::msg::{DiffKey, Msg, Request, WriteOp};

/// While focused, status is re-read at least this often (spec §5.4).
pub const BACKSTOP: Duration = Duration::from_secs(60);
const LOG_LINES: usize = 500;

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
    force_text: bool,
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
        self.tab = tab;
        // the diff pane belongs to the active tab; the other tab reloads from its cache
        self.diff = None;
        self.diff_wanted = None;
        self.diff_error = None;
        self.changes.current = None;
        self.file_gen = crate::msg::Gens::bump(&self.gens.file);
        match tab {
            Tab::Changes => {
                self.focus = super::Focus::Files;
                self.request_status();
                self.request_change_diff();
            }
            Tab::History => {
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
        self.outbox.push(Request::Status { generation: self.changes.status_gen });
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
                if let Some(mark) = &self.index_mark {
                    mark.note();
                }
                match result {
                    Ok(st) => self.install_status(st),
                    Err(e) => self.changes.status_error = Some(e),
                }
                if std::mem::take(&mut self.changes.status_again) {
                    self.request_status();
                }
            }
            Msg::ChangeDiff { generation, entry, key, diff, texts, staged, divergent } => {
                if generation != self.changes.diff_gen || self.tab != Tab::Changes {
                    return None;
                }
                self.changes.diff_error = None;
                self.install_change_diff(key, ChangeView { entry, texts, diff, staged, divergent });
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
            Msg::WriteDone { op, result } => {
                self.changes.busy = self.changes.busy.saturating_sub(1);
                match result {
                    Ok(_) => {}
                    Err(detail) => {
                        let what = format!("{} failed", op.label());
                        self.toast = Some(Toast { what, detail, error: true });
                    }
                }
                self.request_status();
                if op.moves_head() {
                    self.outbox.push(Request::Refs);
                }
            }
            Msg::Changed(c) => {
                if c.intersects(Changed::WORKTREE | Changed::INDEX | Changed::IGNORE_RULES | Changed::STATE) {
                    self.request_status();
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

    /// Lets status runs mark the index state they saw, so the watcher skips it.
    pub fn set_index_mark(&mut self, mark: IndexMark) {
        self.index_mark = Some(mark);
    }
}
