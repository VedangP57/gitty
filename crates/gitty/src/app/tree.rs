//! The file list as a tree (`t`): directory rows, indented by depth, that collapse.

use std::collections::HashSet;

use gitty_core::commit_files::FileChange;

use super::App;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRow {
    /// `path` is the directory's full path (no trailing slash); `name` its last component.
    Dir { path: String, name: String, depth: usize, collapsed: bool },
    /// `idx` indexes the file list.
    File { idx: usize, depth: usize },
}

/// Rows for `files`: directories before files at each level, then by name. Rows under a
/// collapsed directory are left out.
pub fn build(files: &[FileChange], collapsed: &HashSet<String>) -> Vec<FileRow> {
    let key = |p: &str| -> Vec<(bool, String)> {
        let parts: Vec<&str> = p.split('/').collect();
        parts.iter().enumerate().map(|(i, c)| (i + 1 == parts.len(), c.to_string())).collect()
    };
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_cached_key(|&i| key(&files[i].path));
    let mut rows = Vec::new();
    let mut stack: Vec<&str> = Vec::new();
    for i in order {
        let parts: Vec<&str> = files[i].path.split('/').collect();
        let dirs = &parts[..parts.len() - 1];
        let common = stack.iter().zip(dirs).take_while(|(a, b)| a == b).count();
        stack.truncate(common);
        for d in &dirs[common..] {
            stack.push(d);
            let path = stack.join("/");
            if !hidden(&stack[..stack.len() - 1], collapsed) {
                let collapsed = collapsed.contains(&path);
                rows.push(FileRow::Dir { path, name: d.to_string(), depth: stack.len() - 1, collapsed });
            }
        }
        if !hidden(&stack, collapsed) {
            rows.push(FileRow::File { idx: i, depth: stack.len() });
        }
    }
    rows
}

/// Whether anything inside `dirs` (a path's directory components) is hidden by a collapse.
fn hidden(dirs: &[&str], collapsed: &HashSet<String>) -> bool {
    (1..=dirs.len()).any(|n| collapsed.contains(&dirs[..n].join("/")))
}

impl App {
    /// The files pane's rows: one per file, or the tree.
    pub fn file_rows(&self) -> &[FileRow] {
        &self.file_rows
    }

    /// Rebuilds the rows after the list, the view or a collapse changed.
    pub(super) fn refresh_file_rows(&mut self) {
        self.file_rows = match &self.files {
            None => Vec::new(),
            Some(files) if self.ui_state.tree_view => build(files, &self.collapsed),
            Some(files) => (0..files.len()).map(|idx| FileRow::File { idx, depth: 0 }).collect(),
        };
    }

    /// Row of the files pane cursor: the directory it rests on, else the selected file's row.
    pub fn file_cursor(&self) -> usize {
        let rows = self.file_rows();
        rows.iter()
            .position(|r| match (r, &self.tree_dir) {
                (FileRow::Dir { path, .. }, Some(d)) => path == d,
                (FileRow::File { idx, .. }, None) => *idx == self.file_sel,
                _ => false,
            })
            .unwrap_or(0)
    }

    /// Moves the files pane cursor to row `r`.
    pub fn select_file_row(&mut self, r: usize) {
        let rows = self.file_rows();
        let Some(row) = rows.get(r.min(rows.len().saturating_sub(1))).cloned() else { return };
        match &row {
            FileRow::File { idx, .. } => {
                self.tree_dir = None;
                self.select_file(*idx);
            }
            FileRow::Dir { path, .. } => self.tree_dir = Some(path.clone()),
        }
        self.ensure_files_visible();
    }

    /// `{` / `}`: the previous / next file row, skipping directories.
    pub fn step_file(&mut self, forward: bool) {
        let rows = self.file_rows();
        let cur = self.file_cursor();
        let is_file = |r: &&FileRow| matches!(r, FileRow::File { .. });
        let rows = rows.to_vec();
        let found = if forward {
            rows.iter().enumerate().skip(cur + 1).find(|(_, r)| is_file(r)).map(|(i, _)| i)
        } else {
            rows.iter().enumerate().take(cur).rev().find(|(_, r)| is_file(r)).map(|(i, _)| i)
        };
        if let Some(i) = found {
            self.select_file_row(i);
        }
    }

    /// Collapses or expands the directory under the cursor; false on a file row.
    pub fn toggle_dir(&mut self) -> bool {
        let Some(d) = self.tree_dir.clone() else { return false };
        if !self.collapsed.remove(&d) {
            self.collapsed.insert(d);
        }
        self.refresh_file_rows();
        self.ensure_files_visible();
        true
    }

    /// `t`: list ↔ tree. The selected file stays selected.
    pub fn toggle_tree(&mut self) {
        self.ui_state.tree_view = !self.ui_state.tree_view;
        self.tree_dir = None;
        if self.ui_state.tree_view {
            // the selected file may be under a collapsed directory: show it
            if let Some(p) = self.current_file().map(|f| f.path.clone()) {
                self.collapsed.retain(|d| !p.starts_with(&format!("{d}/")));
            }
        }
        self.refresh_file_rows();
        self.save_state();
        self.ensure_files_visible();
    }

    /// The file a fresh list starts on: the first file row.
    pub(super) fn first_file_row(&self) -> usize {
        self.file_rows().iter().find_map(|r| match r {
            FileRow::File { idx, .. } => Some(*idx),
            FileRow::Dir { .. } => None,
        }).unwrap_or(0)
    }
}
