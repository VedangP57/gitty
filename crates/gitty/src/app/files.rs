//! Files tab state: the working tree as a lazily loaded tree (only expanded directories are
//! listed, on a worker) and a read-only viewer for the selected file. The UI thread never
//! touches the filesystem; `rows` is the flat list the pane draws a slice of.
//!
//! A file whose name looks like a secret (`gitty_core::files::is_secret`) is never requested
//! for display until `v` reveals it, and the reveal ends with the selection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gitty_core::files::{DirEntry, EntryKind, is_secret};

use super::{App, Focus, Tab, Toast};
use crate::external::External;
use crate::msg::{FileView, Gens, HlKey, Msg, Request};
use crate::text::{max_hscroll, widest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Dir { open: bool, loading: bool },
    File,
    /// Shown as `name -> target`; never followed.
    Symlink { target: String },
    Submodule,
    /// An inline message under a directory: its read error, or that it is empty.
    Note { error: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Relative to the work tree (the real path, not the lossy display name).
    pub path: PathBuf,
    pub name: String,
    pub depth: u16,
    pub kind: RowKind,
    pub ignored: bool,
    pub secret: bool,
}

impl Row {
    fn is_dir(&self) -> bool {
        matches!(self.kind, RowKind::Dir { .. })
    }
    /// Rows the viewer can show: files and symlinks.
    fn viewable(&self) -> bool {
        matches!(self.kind, RowKind::File | RowKind::Symlink { .. })
    }
}

/// What the viewer has for the selected file.
pub enum Viewing {
    Nothing,
    Loading,
    Ready(FileView),
    Failed(String),
}

pub struct FilesState {
    pub rows: Vec<Row>,
    pub sel: usize,
    pub scroll: usize,
    expanded: HashSet<PathBuf>,
    dirs: HashMap<PathBuf, Result<Vec<DirEntry>, String>>,
    loading: HashSet<PathBuf>,
    /// Replies to ReadDir requests of an older generation are dropped (`Gens::files_dirs`).
    dir_gen: u64,
    /// The viewer's own generation (`Gens::files_view`): only Files-tab actions move it.
    view_gen: u64,
    /// Listings changed since `rows` was built; rebuilt once per batch of messages.
    dirty: bool,
    /// The file the viewer is for, and what it has of it.
    pub shown: Option<PathBuf>,
    pub viewing: Viewing,
    /// `v`: the shown secret file is revealed. Never persisted; ends with the selection.
    pub reveal: bool,
    pub vscroll: usize,
    pub hscroll: u16,
    /// Display width of the longest line of the loaded text (capped), set once when it loads.
    pub widest: u32,
    /// Columns of text the viewer showed last frame: what sideways scrolling is measured against.
    pub visible: u16,
}

impl Default for FilesState {
    fn default() -> Self {
        FilesState {
            rows: Vec::new(),
            sel: 0,
            scroll: 0,
            expanded: HashSet::new(),
            dirs: HashMap::new(),
            loading: HashSet::new(),
            dir_gen: 0,
            view_gen: 0,
            dirty: false,
            shown: None,
            viewing: Viewing::Nothing,
            reveal: false,
            vscroll: 0,
            hscroll: 0,
            widest: 0,
            visible: 0,
        }
    }
}

impl FilesState {
    /// The farthest the viewer can scroll sideways: to the end of the widest line.
    pub fn max_hscroll(&self) -> u16 {
        max_hscroll(self.widest, u32::from(self.visible)) as u16
    }

    /// Scrolls the viewer sideways by `by` columns, between the left edge and the widest line.
    pub fn scroll_sideways(&mut self, by: i32) {
        self.hscroll = (i32::from(self.hscroll) + by).clamp(0, i32::from(self.max_hscroll())) as u16;
    }

    /// The root has not been listed yet.
    pub fn loading_root(&self) -> bool {
        !self.dirs.contains_key(Path::new(""))
    }

    pub fn selected(&self) -> Option<&Row> {
        self.rows.get(self.sel)
    }

    /// The shown file is a secret and has not been revealed: its content must not be drawn.
    pub fn masked(&self) -> bool {
        self.shown.as_deref().is_some_and(|p| is_secret(p) && !self.reveal)
    }

    fn row_at(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|r| r.path == path)
    }

    /// Rebuilds `rows` from the loaded directories, keeping the selection on the same path.
    fn rebuild(&mut self) {
        let keep = self.selected().map(|r| r.path.clone());
        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();
        push_dir(&mut rows, self, Path::new(""), 0);
        self.rows = rows;
        self.dirty = false;
        self.sel = keep.and_then(|p| self.row_at(&p)).unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
    }

    /// Drops what is remembered of the directories below `dir` that `entries` no longer has.
    fn prune(&mut self, dir: &Path, entries: &[DirEntry]) {
        let gone: Vec<PathBuf> = self
            .expanded
            .iter()
            .chain(self.dirs.keys())
            .filter(|p| p.parent() == Some(dir) && !entries.iter().any(|e| e.kind == EntryKind::Dir && p.file_name() == Some(e.name.as_os_str())))
            .cloned()
            .collect();
        for g in gone {
            self.expanded.retain(|p| !p.starts_with(&g));
            self.dirs.retain(|p, _| !p.starts_with(&g));
            self.loading.retain(|p| !p.starts_with(&g));
        }
    }
}

/// Appends the rows of `rel` and of its open subdirectories, by reference to the listings.
fn push_dir(rows: &mut Vec<Row>, f: &FilesState, rel: &Path, depth: u16) {
    match f.dirs.get(rel) {
        Some(Ok(entries)) => {
            if entries.is_empty() && depth > 0 {
                rows.push(Row { path: rel.join("(empty)"), name: "(empty)".into(), depth, kind: RowKind::Note { error: false }, ignored: false, secret: false });
            }
            for e in entries {
                let path = rel.join(&e.name);
                let name = e.name.to_string_lossy().into_owned();
                let secret = matches!(e.kind, EntryKind::File | EntryKind::Symlink { .. }) && is_secret(&path);
                let kind = match &e.kind {
                    EntryKind::File => RowKind::File,
                    EntryKind::Dir => RowKind::Dir { open: f.expanded.contains(&path), loading: f.loading.contains(&path) },
                    EntryKind::Symlink { target } => RowKind::Symlink { target: target.to_string_lossy().into_owned() },
                    EntryKind::Submodule => RowKind::Submodule,
                };
                let open = matches!(kind, RowKind::Dir { open: true, .. });
                rows.push(Row { path: path.clone(), name, depth, kind, ignored: e.ignored, secret });
                if open {
                    push_dir(rows, f, &path, depth + 1);
                }
            }
        }
        Some(Err(e)) => {
            let text = format!("cannot read: {}", e.lines().next().unwrap_or(""));
            rows.push(Row { path: rel.join("(error)"), name: text, depth, kind: RowKind::Note { error: true }, ignored: false, secret: false });
        }
        None => {}
    }
}

impl App {
    /// Applies listings that arrived since the rows were built (once per batch of messages: the
    /// main loop asks for requests after each one, and the draw calls this first).
    pub fn settle_files(&mut self) {
        if self.tab != Tab::Files || !self.files_tab.dirty {
            return;
        }
        self.files_tab.rebuild();
        // a scrolled tree stays where the user left it; only a shorter list pulls it back
        let cap = self.files_capacity();
        let f = &mut self.files_tab;
        f.scroll = f.scroll.min(f.rows.len().saturating_sub(cap));
        self.sync_viewer();
    }

    /// Entering the tab: list the root (and what was expanded) afresh.
    pub(super) fn files_enter(&mut self) {
        self.files_tab.reveal = false;
        self.refresh_files();
        self.sync_viewer();
    }

    /// Leaving the tab: a revealed file is hidden again and its content dropped.
    pub(super) fn files_leave(&mut self) {
        self.files_tab.reveal = false;
        self.files_tab.shown = None;
        self.files_tab.viewing = Viewing::Nothing;
        // in-flight reads and highlights of the file are cancelled
        self.files_tab.view_gen = Gens::bump(&self.gens.files_view);
        self.hl_pending.clear();
    }

    /// Lists the root and every expanded directory again (the old listing stays on screen until
    /// the new one arrives), and re-reads the open file.
    pub fn refresh_files(&mut self) {
        // one burst, one request per directory: queued ones are superseded, and so are the ones
        // already running (the worker checks the generation)
        self.outbox.retain(|r| !matches!(r, Request::ReadDir { .. } | Request::ReadFile { .. }));
        self.files_tab.dir_gen = Gens::bump(&self.gens.files_dirs);
        self.files_tab.loading.clear();
        let mut dirs: Vec<PathBuf> = self.files_tab.expanded.iter().cloned().collect();
        dirs.sort();
        for d in std::iter::once(PathBuf::new()).chain(dirs) {
            self.request_dir(d);
        }
        if self.files_tab.shown.is_some() && !self.files_tab.masked() {
            self.request_file();
        }
    }

    fn request_dir(&mut self, dir: PathBuf) {
        // a directory already listed keeps its rows while the new listing is on its way
        if !self.files_tab.dirs.contains_key(&dir) {
            self.files_tab.loading.insert(dir.clone());
        }
        self.outbox.push(Request::ReadDir { generation: self.files_tab.dir_gen, dir });
    }

    fn request_file(&mut self) {
        let Some(path) = self.files_tab.shown.clone() else { return };
        self.files_tab.view_gen = Gens::bump(&self.gens.files_view);
        self.outbox.push(Request::ReadFile { generation: self.files_tab.view_gen, path, reveal: self.files_tab.reveal });
    }

    pub(super) fn handle_files_msg(&mut self, m: Msg) -> Option<Msg> {
        match m {
            Msg::Dir { generation, dir, result } => {
                // a late reply for a directory that was pruned or closed for good is not kept
                let wanted = dir.as_os_str().is_empty() || self.files_tab.expanded.contains(&dir);
                if generation == self.files_tab.dir_gen && self.tab == Tab::Files && wanted {
                    self.files_tab.loading.remove(&dir);
                    if let Ok(entries) = &result {
                        self.files_tab.prune(&dir, entries);
                    }
                    self.files_tab.dirs.insert(dir, result);
                    self.files_tab.dirty = true;
                }
            }
            Msg::File { generation, path, result } => {
                // a secret that is not revealed never takes content, whatever arrives
                if generation == self.files_tab.view_gen && self.tab == Tab::Files && self.files_tab.shown.as_ref() == Some(&path) && !self.files_tab.masked() {
                    self.files_tab.viewing = match result {
                        Ok(view) => {
                            if let FileView::Text { text, key } = &view {
                                self.files_tab.widest = widest(text, self.config.tab_size);
                                self.request_file_highlight(key.clone(), text.clone());
                            } else {
                                self.files_tab.widest = 0;
                                self.files_tab.hscroll = 0;
                            }
                            Viewing::Ready(view)
                        }
                        Err(e) => {
                            self.files_tab.widest = 0;
                            self.files_tab.hscroll = 0;
                            Viewing::Failed(e)
                        }
                    };
                }
            }
            m => return Some(m),
        }
        None
    }

    fn request_file_highlight(&mut self, key: HlKey, text: std::sync::Arc<gitty_core::diff::text::Text>) {
        if !self.hl_cache.contains(&key) && self.hl_pending.insert(key.clone()) {
            self.outbox.push(Request::Highlight { generation: self.files_tab.view_gen, key, text, files_view: true });
        }
    }

    /// Makes the viewer follow the selected row: a new file resets the reveal and the scroll, and
    /// is requested unless it is a masked secret.
    fn sync_viewer(&mut self) {
        let want = self.files_tab.selected().filter(|r| r.viewable()).map(|r| r.path.clone());
        if want == self.files_tab.shown {
            return;
        }
        self.files_tab.reveal = false;
        self.files_tab.vscroll = 0;
        self.files_tab.hscroll = 0;
        self.files_tab.widest = 0;
        self.files_tab.viewing = Viewing::Nothing;
        self.files_tab.shown = want;
        // the bump also cancels the highlight of the file left behind
        self.files_tab.view_gen = Gens::bump(&self.gens.files_view);
        self.hl_pending.clear();
        if self.files_tab.shown.is_some() && !self.files_tab.masked() {
            self.files_tab.viewing = Viewing::Loading;
            self.request_file();
        }
    }

    pub fn select_files_row(&mut self, i: usize) {
        if self.files_tab.rows.is_empty() {
            return;
        }
        self.files_tab.sel = i.min(self.files_tab.rows.len() - 1);
        self.ensure_files_visible_sel();
        self.sync_viewer();
    }

    fn ensure_files_visible_sel(&mut self) {
        let cap = self.files_capacity();
        let f = &mut self.files_tab;
        if f.sel < f.scroll {
            f.scroll = f.sel;
        } else if f.sel >= f.scroll + cap {
            f.scroll = f.sel + 1 - cap;
        }
        f.scroll = f.scroll.min(f.rows.len().saturating_sub(cap));
    }

    /// Enter or `l` on a directory: open it (loaded on a worker), or close it when open.
    pub fn toggle_files_dir(&mut self) -> bool {
        let Some(row) = self.files_tab.selected().filter(|r| r.is_dir()).cloned() else { return false };
        if self.files_tab.expanded.remove(&row.path) {
            self.files_tab.rebuild();
        } else {
            self.files_tab.expanded.insert(row.path.clone());
            self.request_dir(row.path);
            self.files_tab.rebuild();
        }
        self.ensure_files_visible_sel();
        true
    }

    /// `l`: open a directory, or step into an open one.
    pub fn files_expand(&mut self) {
        let Some(row) = self.files_tab.selected().cloned() else { return };
        match row.kind {
            RowKind::Dir { open: false, .. } => {
                self.toggle_files_dir();
            }
            RowKind::Dir { open: true, .. } if self.files_tab.rows.get(self.files_tab.sel + 1).is_some_and(|r| r.depth > row.depth) => {
                self.select_files_row(self.files_tab.sel + 1);
            }
            _ => {}
        }
    }

    /// `h`: close an open directory; on anything else go to the directory that holds it.
    pub fn files_collapse(&mut self) {
        let Some(row) = self.files_tab.selected().cloned() else { return };
        if matches!(row.kind, RowKind::Dir { open: true, .. }) {
            self.toggle_files_dir();
            return;
        }
        let parent = row.path.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf);
        if let Some(i) = parent.and_then(|p| self.files_tab.row_at(&p)) {
            self.select_files_row(i);
        }
    }

    /// Enter: a directory opens or closes; a file moves to the viewer.
    pub fn files_open(&mut self) {
        if !self.toggle_files_dir() && self.files_tab.selected().is_some_and(Row::viewable) {
            self.focus = Focus::Diff;
        }
    }

    /// `v`: show or hide the selected secret file.
    pub fn toggle_reveal(&mut self) {
        if !self.files_tab.shown.as_deref().is_some_and(is_secret) {
            return;
        }
        self.files_tab.reveal = !self.files_tab.reveal;
        if self.files_tab.reveal {
            self.files_tab.viewing = Viewing::Loading;
            self.request_file();
        } else {
            // hide again: the content is dropped and an in-flight read is cancelled
            self.files_tab.viewing = Viewing::Nothing;
            self.files_tab.vscroll = 0;
            self.files_tab.hscroll = 0;
            self.files_tab.widest = 0;
            self.files_tab.view_gen = Gens::bump(&self.gens.files_view);
            self.hl_pending.clear();
        }
    }

    /// `e` and double-click: open the selected file in the editor (a masked secret too: it is the
    /// user's own editor).
    pub fn files_edit(&mut self) {
        let Some(row) = self.files_tab.selected() else { return };
        if !row.viewable() {
            self.toast = Some(Toast { what: "Select a file to edit it".into(), detail: String::new(), error: false });
            return;
        }
        let Some(root) = &self.workdir else { return };
        let abs = root.join(&row.path);
        self.external = Some(External::Edit { path: abs, line: None });
    }

    /// Lines of the file the viewer has loaded.
    pub fn view_lines(&self) -> usize {
        match &self.files_tab.viewing {
            Viewing::Ready(FileView::Text { text, .. }) => text.len() as usize,
            _ => 0,
        }
    }

    /// Syntax spans of the file in the viewer, once highlighted.
    pub fn view_highlights(&mut self) -> Option<std::sync::Arc<gitty_highlight::Highlights>> {
        let Viewing::Ready(FileView::Text { key, .. }) = &self.files_tab.viewing else { return None };
        let key = key.clone();
        self.hl_cache.get(&key).cloned().flatten()
    }

    /// Click or double-click on the tree: the clicked row (None outside the rows).
    pub(super) fn files_row_at(&self, x: u16, y: u16) -> Option<usize> {
        let r = self.hits.files_rows.filter(|r| r.contains(ratatui::layout::Position { x, y }))?;
        Some(self.hits.files_first + (y - r.y) as usize).filter(|&i| i < self.files_tab.rows.len())
    }

    /// A mouse click on the Files tab.
    pub(super) fn files_click(&mut self, x: u16, y: u16) {
        if let Some(i) = self.files_row_at(x, y) {
            self.focus = Focus::Files;
            self.select_files_row(i);
            self.toggle_files_dir();
        } else if self.hits.panes.diff.is_some_and(|r| r.contains(ratatui::layout::Position { x, y })) {
            self.focus = Focus::Diff;
        }
    }

    /// A double-click: select without toggling, then edit a file.
    pub(super) fn files_double_click(&mut self, x: u16, y: u16) {
        if let Some(i) = self.files_row_at(x, y) {
            self.focus = Focus::Files;
            self.select_files_row(i);
        }
        if self.files_tab.selected().is_some_and(Row::viewable) {
            self.files_edit();
        }
    }
}
