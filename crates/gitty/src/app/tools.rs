//! External tools from the UI: double-click opens `$EDITOR` at the line, `O` the difftool.
//! The app only describes the command ([`External`]); the main loop runs it.

use std::time::Duration;

use gitty_core::diff::view::{Row, SplitRow};

use super::diffstate::VRow;
use super::{App, Tab, Toast};
use crate::external::External;
use crate::msg::Request;

/// Two clicks on the same cell within this are a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

fn new_line(v: &VRow) -> Option<u32> {
    match v {
        VRow::Row(Row::Context { new, .. } | Row::Add { new, .. }) | VRow::Split(SplitRow::Context { new, .. } | SplitRow::Change { new: Some(new), .. }) => Some(*new),
        _ => None,
    }
}

impl App {
    /// Records a click; true when it completes a double-click.
    pub(super) fn note_click(&mut self, x: u16, y: u16) -> bool {
        let double = self.last_click.is_some_and(|(t, lx, ly)| (lx, ly) == (x, y) && self.clock.saturating_duration_since(t) < DOUBLE_CLICK);
        // a third click starts over
        self.last_click = if double { None } else { Some((self.clock, x, y)) };
        double
    }

    /// 1-based line in the new file for diff row `i`: its own new line, else the next row's
    /// (a deleted line opens where it was).
    fn editor_line(&self, i: usize) -> Option<u32> {
        let split = self.split_active();
        let d = self.diff.as_ref()?;
        let n = d.rows(split);
        (i..n).chain((0..i).rev()).find_map(|j| d.vrow(j, split).as_ref().and_then(new_line)).map(|l| l + 1)
    }

    /// The first changed line of the shown diff.
    fn first_change_line(&self) -> Option<u32> {
        let split = self.split_active();
        let d = self.diff.as_ref()?;
        let first = (0..d.rows(split)).find(|&j| {
            matches!(d.vrow(j, split), Some(VRow::Row(Row::Add { .. } | Row::Del { .. }) | VRow::Split(SplitRow::Change { .. })))
        })?;
        self.editor_line(first)
    }

    /// Double-click on the diff (`on_diff`) or a file row: open the shown file in the editor.
    pub(super) fn open_shown_file(&mut self, on_diff: bool) {
        let shown = self.diff.as_ref().map(|d| d.key.path.clone());
        // a file row is the file just selected, whose diff may not have arrived yet
        let path = if on_diff {
            shown.clone()
        } else if self.tab == Tab::Changes {
            self.changes.selected().map(|e| e.path.clone())
        } else if self.tree_dir.is_some() {
            None
        } else {
            self.current_file().map(|f| f.path.clone())
        };
        let Some(path) = path else { return };
        let line = match () {
            _ if shown.as_deref() != Some(path.as_str()) => None,
            _ if on_diff => self.diff.as_ref().and_then(|d| self.editor_line(d.cursor)),
            _ => self.first_change_line(),
        };
        let Some(root) = &self.workdir else { return };
        let abs = root.join(&path);
        if !abs.exists() {
            self.toast = Some(Toast { what: format!("{path} is not in the working tree"), detail: String::new(), error: false });
            return;
        }
        self.external = Some(External::Edit { path: abs, line });
    }

    /// `O`: the shown diff's two sides in the configured difftool.
    pub fn open_difftool(&mut self) {
        let Some(d) = &self.diff else { return };
        if self.config.difftool.as_deref().is_none_or(|t| t.trim().is_empty()) {
            self.toast = Some(Toast { what: "Set difftool in config.toml to use O".into(), detail: "e.g. difftool = \"delta\"".into(), error: false });
            return;
        }
        let (old, new) = (d.diff.old.bytes().to_vec(), d.diff.new.bytes().to_vec());
        self.external = Some(External::Diff { path: d.key.path.clone(), old, new });
    }

    /// `R`: asks for the current branch's pull-request page; the main loop opens it.
    pub fn open_pr(&mut self) {
        match self.refs.as_ref().and_then(|r| r.head_branch()) {
            Some(branch) => self.outbox.push(Request::PrUrl { branch: branch.to_string() }),
            None => self.toast = Some(Toast { what: "No branch checked out.".into(), detail: String::new(), error: false }),
        }
    }

    /// After the main loop ran the tool: `Err` could not start it, `Ok(code)` its exit status.
    pub fn external_done(&mut self, result: Result<Option<i32>, String>) {
        self.dirty = true;
        match result {
            Ok(Some(0)) => {}
            Ok(code) => {
                let what = code.map_or("the tool was stopped by a signal".to_string(), |c| format!("the tool exited with status {c}"));
                self.toast = Some(Toast { what, detail: String::new(), error: false });
            }
            Err(detail) => self.toast = Some(Toast { what: "could not run the tool".into(), detail, error: true }),
        }
        // an editor may have changed files
        self.request_status();
    }
}
