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
    /// Replies to ReadDir requests of an older generation are dropped.
    dir_gen: u64,
    /// The file the viewer is for, and what it has of it.
    pub shown: Option<PathBuf>,
    pub viewing: Viewing,
    /// `v`: the shown secret file is revealed. Never persisted; ends with the selection.
    pub reveal: bool,
    pub vscroll: usize,
    pub hscroll: u16,
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
            shown: None,
            viewing: Viewing::Nothing,
            reveal: false,
            vscroll: 0,
            hscroll: 0,
        }
    }
}

impl FilesState {
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

    fn push_dir(&mut self, rel: &Path, depth: u16) {
        match self.dirs.get(rel) {
            Some(Ok(entries)) => {
                if entries.is_empty() && depth > 0 {
                    self.rows.push(Row { path: rel.join("(empty)"), name: "(empty)".into(), depth, kind: RowKind::Note { error: false }, ignored: false, secret: false });
                }
                let entries = entries.clone();
                for e in &entries {
                    let path = rel.join(&e.name);
                    let name = e.name.to_string_lossy().into_owned();
                    let secret = matches!(e.kind, EntryKind::File | EntryKind::Symlink { .. }) && is_secret(&path);
                    let kind = match &e.kind {
                        EntryKind::File => RowKind::File,
                        EntryKind::Dir => RowKind::Dir { open: self.expanded.contains(&path), loading: self.loading.contains(&path) },
                        EntryKind::Symlink { target } => RowKind::Symlink { target: target.to_string_lossy().into_owned() },
                        EntryKind::Submodule => RowKind::Submodule,
                    };
                    let open = matches!(kind, RowKind::Dir { open: true, .. });
                    self.rows.push(Row { path: path.clone(), name, depth, kind, ignored: e.ignored, secret });
                    if open {
                        self.push_dir(&path, depth + 1);
                    }
                }
            }
            Some(Err(e)) => {
                let text = format!("cannot read: {}", e.lines().next().unwrap_or(""));
                self.rows.push(Row { path: rel.join("(error)"), name: text, depth, kind: RowKind::Note { error: true }, ignored: false, secret: false });
            }
            None => {}
        }
    }

    /// Rebuilds `rows` from the loaded directories, keeping the selection on the same path.
    fn rebuild(&mut self) {
        let keep = self.selected().map(|r| r.path.clone());
        self.rows.clear();
        self.push_dir(Path::new(""), 0);
        self.sel = keep.and_then(|p| self.row_at(&p)).unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
    }
}

impl App {
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
    }

    /// Lists the root and every expanded directory again (the old listing stays on screen until
    /// the new one arrives), and re-reads the open file.
    pub fn refresh_files(&mut self) {
        self.files_tab.dir_gen += 1;
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
        self.file_gen = Gens::bump(&self.gens.file);
        self.outbox.push(Request::ReadFile { generation: self.file_gen, path, reveal: self.files_tab.reveal });
    }

    pub(super) fn handle_files_msg(&mut self, m: Msg) -> Option<Msg> {
        match m {
            Msg::Dir { generation, dir, result } => {
                if generation == self.files_tab.dir_gen && self.tab == Tab::Files {
                    self.files_tab.loading.remove(&dir);
                    self.files_tab.dirs.insert(dir, result);
                    self.files_tab.rebuild();
                    self.ensure_files_visible_sel();
                    self.sync_viewer();
                }
            }
            Msg::File { generation, path, result } => {
                // a secret that is not revealed never takes content, whatever arrives
                if generation == self.file_gen && self.tab == Tab::Files && self.files_tab.shown.as_ref() == Some(&path) && !self.files_tab.masked() {
                    self.files_tab.viewing = match result {
                        Ok(view) => {
                            if let FileView::Text { text, key } = &view {
                                self.request_file_highlight(key.clone(), text.clone());
                            }
                            Viewing::Ready(view)
                        }
                        Err(e) => Viewing::Failed(e),
                    };
                }
            }
            m => return Some(m),
        }
        None
    }

    fn request_file_highlight(&mut self, key: HlKey, text: std::sync::Arc<gitty_core::diff::text::Text>) {
        if !self.hl_cache.contains(&key) && self.hl_pending.insert(key.clone()) {
            self.outbox.push(Request::Highlight { generation: self.file_gen, key, text });
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
        self.files_tab.viewing = Viewing::Nothing;
        self.files_tab.shown = want;
        // the bump also cancels the highlight of the file left behind
        self.file_gen = Gens::bump(&self.gens.file);
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
            self.file_gen = Gens::bump(&self.gens.file);
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
