//! The conflict view (Changes tab, a conflicted file selected): the file's text with each
//! conflict block drawn as two sides, and the keys that settle a block (`o`, `t`, `b`) or, for a
//! conflict with no markers (binary, deleted by one side), the whole file.
//!
//! The file is read, parsed and counted on a worker. A resolution is computed here from the text
//! the view was built from and written by the writer thread, which refuses if the file on disk is
//! no longer that text. Nothing is staged: that stays the user's Space.

use std::collections::HashSet;
use std::sync::Arc;

use gitty_core::commit_files::BlobId;
use gitty_core::conflicts::{Choice, Conflict, Sides, resolve};
use gitty_core::diff::text::Text;
use gitty_core::status::StatusEntry;

use super::{App, Overlay, Tab, Toast};
use crate::external::External;
use crate::msg::{ConflictBody, Msg, Request, WriteOp};
use crate::text::{max_hscroll, widest};

/// Resolutions kept for `u` (each holds a copy of the file as it was).
const UNDO_DEPTH: usize = 8;

pub struct ConflictView {
    pub entry: StatusEntry,
    pub sides: Sides,
    pub body: ConflictBody,
    /// The block the cursor is in (an index into the parsed blocks).
    pub cur: usize,
    pub vscroll: usize,
    pub hscroll: u16,
    /// Display width of the longest line (capped at scroll time), set when the text loads.
    pub widest: u32,
    /// Columns of text shown last frame: what sideways scrolling is measured against.
    pub visible: u16,
}

/// What a line of the file is, for drawing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Plain,
    /// The `<<<<<<<` line of block `.0`: drawn as the first side's header.
    OursHead(usize),
    Ours(usize),
    /// The `|||||||` line.
    BaseHead(usize),
    Base(usize),
    /// The `=======` line: the second side's header.
    TheirsHead(usize),
    Theirs(usize),
    /// The `>>>>>>>` line: a rule that ends the block.
    End(usize),
}

impl ConflictView {
    pub fn text(&self) -> Option<&Arc<Text>> {
        match &self.body {
            ConflictBody::Text { text, .. } => Some(text),
            ConflictBody::Other(_) => None,
        }
    }

    pub fn conflicts(&self) -> &[Conflict] {
        match &self.body {
            ConflictBody::Text { conflicts, .. } => conflicts,
            ConflictBody::Other(_) => &[],
        }
    }

    /// Marker lines were found that no block could be made of.
    pub fn unknown(&self) -> bool {
        matches!(self.body, ConflictBody::Text { unknown: true, .. })
    }

    /// What the file list counts: the blocks, or 1 for markers that were not understood.
    pub fn marks(&self) -> usize {
        self.conflicts().len().max(usize::from(self.unknown()))
    }

    pub fn lines(&self) -> usize {
        self.text().map_or(0, |t| t.len() as usize)
    }

    pub fn role(&self, line: usize) -> Role {
        let cs = self.conflicts();
        let k = cs.partition_point(|c| c.end_line < line);
        let Some(c) = cs.get(k).filter(|c| c.start_line <= line) else { return Role::Plain };
        if line == c.start_line {
            Role::OursHead(k)
        } else if line == c.end_line {
            Role::End(k)
        } else if line == c.sep_line() {
            Role::TheirsHead(k)
        } else if c.base_line() == Some(line) {
            Role::BaseHead(k)
        } else if c.theirs.contains(&line) {
            Role::Theirs(k)
        } else if c.base.as_ref().is_some_and(|b| b.contains(&line)) {
            Role::Base(k)
        } else {
            Role::Ours(k)
        }
    }

    /// `Current (main)`, or `Current (<the marker's label>)` when no operation names the side.
    pub fn label(&self, k: usize, ours: bool) -> String {
        let (side, marker) = match self.conflicts().get(k) {
            Some(c) if ours => (&self.sides.ours, c.ours_label.as_str()),
            Some(c) => (&self.sides.theirs, c.theirs_label.as_str()),
            None if ours => (&self.sides.ours, ""),
            None => (&self.sides.theirs, ""),
        };
        let mut side = side.clone();
        if side.name.is_empty() {
            side.name = marker.to_string();
        }
        side.label()
    }

    /// The farthest the text can scroll sideways.
    pub fn max_hscroll(&self) -> u16 {
        max_hscroll(self.widest, u32::from(self.visible)) as u16
    }

    /// Puts the cursor's block near the top of the view.
    fn reveal(&mut self) {
        if let Some(c) = self.conflicts().get(self.cur) {
            self.vscroll = c.start_line.saturating_sub(2);
        }
    }
}

/// One resolution that can be taken back: the file as it was, and as the resolution left it.
pub struct Undo {
    path: String,
    before: Arc<Text>,
    after: BlobId,
    /// Blocks in `before`.
    left: usize,
}

/// Whether each side has the file, from the status code (`XY`, X ours, Y theirs; git's table
/// of unmerged states: DD both deleted, AU ours added, UD theirs deleted, UA theirs added, DU
/// ours deleted, AA both added, UU both modified).
pub fn has_file(x: char, y: char) -> (bool, bool) {
    (!(x == 'D' || (x, y) == ('U', 'A')), !(y == 'D' || (x, y) == ('A', 'U')))
}

/// What the status code says happened, in words, for a conflict the view has no blocks for.
pub fn describe(e: &StatusEntry, sides: &Sides) -> String {
    let (ours, theirs) = (sides.ours.label(), sides.theirs.label());
    match (e.x, e.y) {
        ('D', 'D') => "Both sides deleted this file".to_string(),
        ('A', 'A') => "Both sides added this file".to_string(),
        ('A', 'U') => format!("{ours} added this file; {theirs} does not have it"),
        ('U', 'A') => format!("{theirs} added this file; {ours} does not have it"),
        ('D', 'U') => format!("{ours} deleted this file; {theirs} changed it"),
        ('U', 'D') => format!("{ours} changed this file; {theirs} deleted it"),
        _ => "Both sides changed this file".to_string(),
    }
}

/// For a file with marker lines that no block could be made of.
pub const NOT_UNDERSTOOD: &str = "Conflict markers not understood: open in the editor (e)";

fn say(what: impl Into<String>) -> Option<Toast> {
    Some(Toast { what: what.into(), detail: String::new(), error: false })
}

impl App {
    /// A conflicted file is selected on the Changes tab: its conflict view replaces the diff.
    pub fn conflict_active(&self) -> bool {
        self.tab == Tab::Changes && self.changes.selected().is_some_and(StatusEntry::is_conflicted)
    }

    /// The view of the selected file, once it has loaded (an older file's is not it).
    pub fn conflict_view(&self) -> Option<&ConflictView> {
        let path = &self.changes.selected()?.path;
        self.changes.conflict.as_ref().filter(|v| &v.entry.path == path)
    }

    /// Asks for the block counts of the conflicted files (the worker skips the unchanged ones by
    /// their stamps), and forgets what is kept of files that are no longer conflicted: their
    /// counts and the resolutions that could be undone.
    pub(super) fn request_conflict_counts(&mut self) {
        let unmerged: HashSet<String> = self.changes.entries().iter().filter(|e| e.is_conflicted()).map(|e| e.path.clone()).collect();
        self.changes.conflict_counts.retain(|p, _| unmerged.contains(p));
        self.changes.counts_asked.retain(|p| unmerged.contains(p));
        self.changes.undo.retain(|u| unmerged.contains(&u.path));
        let mut want: Vec<(String, Option<crate::msg::FileStamp>)> = unmerged.into_iter().filter(|p| !self.changes.counts_asked.contains(p)).map(|p| {
            let stamp = self.changes.conflict_counts.get(&p).map(|(_, s)| *s);
            (p, stamp)
        }).collect();
        if want.is_empty() {
            return;
        }
        want.sort();
        self.changes.counts_asked.extend(want.iter().map(|(p, _)| p.clone()));
        self.outbox.push(Request::ConflictCounts { paths: want });
    }

    pub(super) fn handle_conflict_msg(&mut self, m: Msg) -> Option<Msg> {
        match m {
            Msg::ConflictFile { generation, entry, sides, result } => {
                if generation != self.changes.diff_gen || self.tab != Tab::Changes {
                    return None;
                }
                match result {
                    Ok(body) => self.install_conflict(entry, sides, body),
                    Err(detail) => {
                        // a file that is not there (a deletion) or cannot be read: the explanation stands in
                        let body = ConflictBody::Other(detail);
                        self.install_conflict(entry, sides, body);
                    }
                }
            }
            Msg::ConflictCounts { asked, counts } => {
                for p in &asked {
                    self.changes.counts_asked.remove(p);
                }
                // only files still in conflict: a late reply for one that was resolved is dropped
                let unmerged: HashSet<String> = self.changes.entries().iter().filter(|e| e.is_conflicted()).map(|e| e.path.clone()).collect();
                for (p, n, stamp) in counts {
                    if unmerged.contains(&p) {
                        self.changes.conflict_counts.insert(p, (n, stamp));
                    }
                }
            }
            m => return Some(m),
        }
        None
    }

    fn install_conflict(&mut self, entry: StatusEntry, sides: Sides, body: ConflictBody) {
        let old = self.changes.conflict.take().filter(|v| v.entry.path == entry.path);
        let mut view = ConflictView { entry, sides, body, cur: 0, vscroll: 0, hscroll: 0, widest: 0, visible: 0 };
        if let ConflictBody::Text { text, key, .. } = &view.body {
            view.widest = widest(text, self.config.tab_size);
            // the stamp is not known here: the next count request reads the file once more
            self.changes.conflict_counts.insert(view.entry.path.clone(), (Some(view.marks()), (0, 0)));
            let (key, text) = (key.clone(), text.clone());
            if !self.hl_cache.contains(&key) && self.hl_pending.insert(key.clone()) {
                self.outbox.push(Request::Highlight { generation: self.file_gen, key, text, files_view: false });
            }
        }
        let same_text = |a: &ConflictBody, b: &ConflictBody| matches!((a, b), (ConflictBody::Text { key: x, .. }, ConflictBody::Text { key: y, .. }) if x.blob == y.blob);
        match old {
            // a refresh that found the same file: the reader's place stays
            Some(o) if same_text(&o.body, &view.body) => {
                (view.cur, view.vscroll, view.hscroll) = (o.cur, o.vscroll, o.hscroll);
            }
            // the file changed (a block was resolved): the cursor stays on the same number, which
            // is the next block now
            Some(o) => {
                view.cur = o.cur.min(view.conflicts().len().saturating_sub(1));
                view.hscroll = o.hscroll;
                view.reveal();
            }
            None => view.reveal(),
        }
        self.changes.conflict = Some(view);
    }

    /// Syntax spans for the file in the view, once highlighted.
    pub fn conflict_highlights(&mut self) -> Option<Arc<gitty_highlight::Highlights>> {
        let key = match &self.changes.conflict.as_ref()?.body {
            ConflictBody::Text { key, .. } => key.clone(),
            ConflictBody::Other(_) => return None,
        };
        self.hl_cache.get(&key).cloned().flatten()
    }

    pub fn conflict_capacity(&self) -> usize {
        self.panes().diff.map_or(1, |r| r.height.saturating_sub(1) as usize).max(1)
    }

    /// `n` / `p`: the next or previous block, wrapping round.
    pub fn conflict_nav(&mut self, dir: i64) {
        if self.conflict_view().is_none() {
            return;
        }
        let Some(v) = self.changes.conflict.as_mut() else { return };
        let n = v.conflicts().len();
        if n == 0 {
            return;
        }
        v.cur = (v.cur as i64 + dir).rem_euclid(n as i64) as usize;
        v.reveal();
    }

    /// `h` and `l` in the conflict view: scrolls the text sideways.
    pub(super) fn conflict_scroll_sideways(&mut self, by: i32) {
        if let Some(v) = self.changes.conflict.as_mut() {
            v.hscroll = (i32::from(v.hscroll) + by).clamp(0, i32::from(v.max_hscroll())) as u16;
        }
    }

    /// `o`, `t`, `b`: settles the block the cursor is in, or the whole file when it has no blocks.
    pub fn conflict_resolve(&mut self, choice: Choice) {
        let Some(v) = self.conflict_view() else {
            self.toast = say("The conflict is still loading");
            return;
        };
        let (entry, sides) = (v.entry.clone(), v.sides.clone());
        // only a file both sides have can hold markers; the others (and files the view cannot
        // read) are settled whole
        let marked = matches!((entry.x, entry.y), ('U', 'U') | ('A', 'A'));
        let ConflictBody::Text { text, key, conflicts, unknown } = &v.body else { return self.conflict_whole_file(&entry, &sides, choice) };
        if !marked {
            return self.conflict_whole_file(&entry, &sides, choice);
        }
        // a file with no markers left is the user's own result
        if conflicts.is_empty() {
            self.toast = say(if *unknown { NOT_UNDERSTOOD } else { "No conflict markers left: press Space to stage the file" });
            return;
        }
        let Some(block) = conflicts.get(v.cur) else { return };
        if block.ambiguous {
            self.toast = say("Ambiguous markers in this block: press e to edit it");
            return;
        }
        let Ok(src) = std::str::from_utf8(text.bytes()) else { return };
        let Some(new) = resolve(src, block, choice) else {
            self.toast = say("The file changed; reloading");
            self.request_change_diff();
            return;
        };
        let left = conflicts.len() - 1;
        let undo = Undo { path: entry.path.clone(), before: text.clone(), after: BlobId::hash_of(new.as_bytes()), left: conflicts.len() };
        let op = WriteOp::ResolveConflict { path: entry.path.clone(), bytes: new.into_bytes(), expect: key.blob, left, undo: false };
        self.changes.undo.push(undo);
        if self.changes.undo.len() > UNDO_DEPTH {
            self.changes.undo.remove(0);
        }
        self.write(op);
    }

    /// `u`: puts the file back as it was before the last resolution made here, if it is still as
    /// the resolution left it (the writer checks).
    pub fn conflict_undo(&mut self) {
        let Some(path) = self.conflict_view().map(|v| v.entry.path.clone()) else {
            self.toast = say("The conflict is still loading");
            return;
        };
        let Some(i) = self.changes.undo.iter().rposition(|u| u.path == path) else {
            self.toast = say("Nothing to undo in this file");
            return;
        };
        // the entry stays until the writer says it was undone: a refusal leaves it for another try
        let u = &self.changes.undo[i];
        let op = WriteOp::ResolveConflict { path, bytes: u.before.bytes().to_vec(), expect: u.after, left: u.left, undo: true };
        self.write(op);
    }

    /// `e`: the file in `$EDITOR`, at the block the cursor is in.
    pub fn conflict_edit(&mut self) {
        let Some(v) = self.conflict_view() else { return };
        let line = v.conflicts().get(v.cur).map(|c| c.start_line as u32 + 1);
        let Some(root) = self.workdir.clone() else { return };
        let path = root.join(&v.entry.path);
        self.external = Some(External::Edit { path, line });
    }

    /// `o` / `t` on a conflict with no blocks: asks, then takes that side's file (or removes the
    /// path when that side deleted it).
    fn conflict_whole_file(&mut self, e: &StatusEntry, sides: &Sides, choice: Choice) {
        let theirs = match choice {
            Choice::Ours => false,
            Choice::Theirs => true,
            Choice::Both => {
                self.toast = say("Keeping both sides needs conflict markers: this file has none");
                return;
            }
        };
        let (ours_has, theirs_has) = has_file(e.x, e.y);
        let has = if theirs { theirs_has } else { ours_has };
        let side = if theirs { &sides.theirs } else { &sides.ours }.label();
        let (title, body) = if has {
            // a text file that could be merged line by line loses the other side's clean hunks too
            let more = if matches!((e.x, e.y), ('U', 'U') | ('A', 'A')) { " Non-conflicting changes from the other side in this file are discarded too." } else { "" };
            (format!("Keep the {side} version of {}?", e.path), format!("The file becomes that side's version and is staged.{more} A copy of the file as it is now goes to the Trash."))
        } else {
            (format!("Delete {}?", e.path), format!("{side} has no such file, so it is removed and the removal is staged. A copy of the file as it is now goes to the Trash."))
        };
        self.overlay = Some(Overlay::Confirm { title, body, op: WriteOp::TakeSide { path: e.path.clone(), theirs, delete: !has } });
    }

    /// A write of the conflict view came back: keeps the undo history true. A resolution that did
    /// not happen cannot be undone; an undo that did happen is used up (one that did not stays).
    pub(super) fn conflict_write_settled(&mut self, op: &WriteOp, ok: bool) {
        let WriteOp::ResolveConflict { path, bytes, expect, undo, .. } = op else { return };
        if *undo && ok {
            self.changes.undo.retain(|u| !(u.path == *path && u.after == *expect));
        } else if !*undo && !ok {
            let after = BlobId::hash_of(bytes);
            self.changes.undo.retain(|u| !(u.path == *path && u.after == after && BlobId::hash_of(u.before.bytes()) == *expect));
        }
    }

    /// A write of the conflict view failed: the reason is the message (the status that follows
    /// reloads the file).
    pub(super) fn conflict_write_failed(&mut self, op: &WriteOp, detail: &str) -> bool {
        if !matches!(op, WriteOp::ResolveConflict { .. } | WriteOp::TakeSide { .. }) {
            return false;
        }
        let what = detail.lines().next().unwrap_or("").to_string();
        self.toast = Some(Toast { what, detail: detail.to_string(), error: !detail.contains("changed on disk") });
        true
    }
}
