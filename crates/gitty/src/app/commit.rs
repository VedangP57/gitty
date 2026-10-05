//! The commit box: summary, description and co-authors, amend, hooks and undo.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gitty_core::status::Check;

use super::{App, Focus, Overlay, Toast};
use crate::editor::Editor;
use crate::msg::{Request, WriteOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Field {
    #[default]
    Summary,
    Body,
    CoAuthors,
}

/// A commit gitty made this session, offered for undo while it is still HEAD.
#[derive(Debug, Clone)]
pub struct Committed {
    /// HEAD right after the commit.
    pub head: String,
    /// A status has shown `head`; from then on, any other HEAD withdraws the offer. (Statuses
    /// already running when the commit finished still show the parent.)
    seen: bool,
}

#[derive(Debug, Clone)]
pub struct CommitBox {
    pub summary: Editor,
    pub body: Editor,
    pub coauthors: Editor,
    pub field: Field,
    pub amend: bool,
    /// The message being written before amend loaded HEAD's.
    draft: Option<(String, String, String)>,
    pub committing: bool,
    pub committed: Option<Committed>,
}

impl Default for CommitBox {
    fn default() -> Self {
        Self {
            summary: Editor::single(),
            body: Editor::multi(),
            coauthors: Editor::single(),
            field: Field::Summary,
            amend: false,
            draft: None,
            committing: false,
            committed: None,
        }
    }
}

const TRAILER: &str = "Co-authored-by:";

impl CommitBox {
    pub fn editor(&mut self) -> &mut Editor {
        match self.field {
            Field::Summary => &mut self.summary,
            Field::Body => &mut self.body,
            Field::CoAuthors => &mut self.coauthors,
        }
    }

    /// Splits a commit message into the three fields; trailing co-author trailers go to the last.
    pub fn load(&mut self, message: &str) {
        let message = message.trim_end();
        let (summary, rest) = message.split_once('\n').unwrap_or((message, ""));
        let mut body: Vec<&str> = rest.lines().collect();
        let mut authors = Vec::new();
        while let Some(l) = body.last() {
            if let Some(a) = l.strip_prefix(TRAILER) {
                authors.push(a.trim().to_string());
                body.pop();
            } else if l.trim().is_empty() {
                body.pop();
            } else {
                break;
            }
        }
        authors.reverse();
        self.summary.set(summary.trim());
        self.body.set(body.join("\n").trim_matches('\n'));
        self.coauthors.set(&authors.join(", "));
        self.field = Field::Summary;
    }

    fn snapshot(&self) -> (String, String, String) {
        (self.summary.text().into(), self.body.text().into(), self.coauthors.text().into())
    }

    fn restore(&mut self, (s, b, c): (String, String, String)) {
        self.summary.set(&s);
        self.body.set(&b);
        self.coauthors.set(&c);
    }

    /// The message to commit, with `summary` used when the summary field is empty.
    pub fn message(&self, summary: &str) -> String {
        let mut m = summary.trim().to_string();
        let body = self.body.text().trim_end();
        if !body.trim().is_empty() {
            m.push_str("\n\n");
            m.push_str(body);
        }
        let authors: Vec<&str> = self.coauthors.text().split([',', '\n']).map(str::trim).filter(|a| !a.is_empty()).collect();
        if !authors.is_empty() {
            m.push_str("\n\n");
            m.push_str(&authors.iter().map(|a| format!("{TRAILER} {a}")).collect::<Vec<_>>().join("\n"));
        }
        m.push('\n');
        m
    }
}

impl App {
    fn staged_paths(&self) -> Vec<&str> {
        self.changes.entries().iter().filter(|e| e.check() != Check::Unstaged).map(|e| e.path.as_str()).collect()
    }

    /// Shown in an empty summary, and used as the summary when committing one file.
    pub fn commit_placeholder(&self) -> String {
        match self.staged_paths().as_slice() {
            [one] => format!("Update {}", one.rsplit('/').next().unwrap_or(one)),
            _ => "Summary (required)".into(),
        }
    }

    pub fn commit_button(&self) -> String {
        if self.changes.commit.amend {
            return "Amend last commit".into();
        }
        let n = self.staged_paths().len();
        let files = if n == 1 { "file" } else { "files" };
        match self.changes.status.as_ref().and_then(|s| s.branch.as_deref()) {
            Some(b) => format!("Commit {n} {files} to {b}"),
            None => format!("Commit {n} {files} (detached HEAD)"),
        }
    }

    /// The line under the commit box: hook progress, or the undo offer.
    pub fn commit_bar(&self) -> Option<String> {
        let c = &self.changes.commit;
        if c.committing {
            let last = self.changes.log.last().map(String::as_str).unwrap_or("");
            return Some(format!("Committing… {last}").trim_end().to_string());
        }
        self.undo_offered().then(|| "Committed just now · [u] Undo".into())
    }

    /// The commit gitty made can be undone while it is HEAD and the upstream does not have
    /// it (spec §12.3). Here that is the upstream's tip; the write checks ancestry too.
    fn undo_offered(&self) -> bool {
        let Some(c) = &self.changes.commit.committed else { return false };
        let upstream = self.refs.as_ref().and_then(|r| r.upstream.as_ref()).map(|u| u.1.to_string());
        upstream.as_deref() != Some(c.head.as_str())
    }

    pub fn focus_commit(&mut self) {
        self.focus = Focus::Commit;
        self.changes.commit.field = Field::Summary;
    }

    pub(super) fn focus_commit_or_true(&mut self) -> bool {
        self.focus_commit();
        true
    }

    pub fn commit(&mut self) {
        let c = &self.changes.commit;
        if c.committing {
            return;
        }
        let staged = self.staged_paths().len();
        if staged == 0 && !c.amend {
            self.toast = Some(Toast { what: "Nothing staged: tick files or lines to commit".into(), detail: String::new(), error: false });
            return;
        }
        let summary = match c.summary.text().trim() {
            "" if staged == 1 && !c.amend => self.commit_placeholder(),
            "" => {
                self.toast = Some(Toast { what: "Enter a summary".into(), detail: String::new(), error: false });
                return;
            }
            s => s.to_string(),
        };
        let op = WriteOp::Commit { message: c.message(&summary), amend: c.amend };
        self.changes.commit.committing = true;
        self.write(op);
    }

    pub fn toggle_amend(&mut self) {
        let c = &mut self.changes.commit;
        if c.amend {
            c.amend = false;
            if let Some(d) = c.draft.take() {
                c.restore(d);
            }
            return;
        }
        if self.changes.status.as_ref().is_some_and(|s| s.head.is_none()) {
            self.toast = Some(Toast { what: "No commit to amend yet".into(), detail: String::new(), error: false });
            return;
        }
        c.amend = true;
        c.draft = Some(c.snapshot());
        self.outbox.push(Request::HeadMessage);
    }

    pub fn undo_commit(&mut self) {
        if !self.undo_offered() || self.changes.commit.committing {
            self.toast = Some(Toast { what: "Nothing to undo: only a commit made here can be undone".into(), detail: String::new(), error: false });
            return;
        }
        let Some(expect) = self.changes.commit.committed.as_ref().map(|c| c.head.clone()) else { return };
        self.write(WriteOp::UndoCommit { expect });
    }

    pub(super) fn install_head_message(&mut self, result: Result<String, String>) {
        match result {
            Ok(m) if self.changes.commit.amend => self.changes.commit.load(&m),
            Ok(_) => {}
            Err(detail) => {
                self.changes.commit.amend = false;
                self.changes.commit.draft = None;
                self.toast = Some(Toast { what: "Reading the last commit's message failed".into(), detail, error: true });
            }
        }
    }

    /// Commit and undo results. Returns true when the result was handled here.
    pub(super) fn commit_done(&mut self, op: &WriteOp, result: &Result<Option<String>, String>) -> bool {
        match (op, result) {
            (WriteOp::Commit { .. }, Ok(head)) => {
                self.request_tune();
                let committed = head.clone().map(|head| Committed { head, seen: false });
                self.changes.commit = CommitBox { committed, ..CommitBox::default() };
                if self.focus == Focus::Commit {
                    self.focus = Focus::Files;
                }
                true
            }
            (WriteOp::Commit { .. }, Err(detail)) => {
                self.changes.commit.committing = false;
                let mut body = self.changes.log.join("\n");
                if !body.contains(detail.trim()) {
                    if !body.is_empty() {
                        body.push_str("\n\n");
                    }
                    body.push_str(detail);
                }
                self.overlay = Some(Overlay::Log { title: "Commit failed".into(), body });
                true
            }
            (WriteOp::UndoCommit { .. }, Ok(Some(m))) => {
                let c = &mut self.changes.commit;
                c.committed = None;
                c.amend = false;
                c.draft = None;
                c.load(m);
                false
            }
            _ => false,
        }
    }

    /// The HEAD a status run saw: the undo offer lasts while HEAD is the commit gitty made.
    pub(super) fn track_committed_head(&mut self, head: Option<&str>) {
        let Some(c) = self.changes.commit.committed.as_mut() else { return };
        if head == Some(c.head.as_str()) {
            c.seen = true;
        } else if c.seen {
            self.changes.commit.committed = None;
        }
    }

    /// Keys while the commit box has focus. Everything that is not an edit or a move is ignored,
    /// so typing never triggers shortcuts.
    pub(super) fn commit_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let c = &mut self.changes.commit;
        match k.code {
            KeyCode::Enter if ctrl || alt => self.commit(),
            KeyCode::Esc => self.focus = Focus::Files,
            KeyCode::Tab => {
                c.field = match c.field {
                    Field::Summary => Field::Body,
                    Field::Body => Field::CoAuthors,
                    Field::CoAuthors => Field::Summary,
                }
            }
            KeyCode::BackTab => {
                c.field = match c.field {
                    Field::Summary => Field::CoAuthors,
                    Field::Body => Field::Summary,
                    Field::CoAuthors => Field::Body,
                }
            }
            KeyCode::Enter => match c.field {
                Field::Summary => c.field = Field::Body,
                Field::Body => c.body.insert("\n"),
                Field::CoAuthors => {}
            },
            KeyCode::Up => {
                if !c.editor().up() {
                    c.field = match c.field {
                        Field::CoAuthors => Field::Body,
                        _ => Field::Summary,
                    };
                }
            }
            KeyCode::Down => {
                if !c.editor().down() {
                    c.field = match c.field {
                        Field::Summary => Field::Body,
                        _ => Field::CoAuthors,
                    };
                }
            }
            KeyCode::Left if ctrl || alt => c.editor().word_left(),
            KeyCode::Right if ctrl || alt => c.editor().word_right(),
            KeyCode::Char('b') if alt => c.editor().word_left(),
            KeyCode::Char('f') if alt => c.editor().word_right(),
            KeyCode::Left => c.editor().left(),
            KeyCode::Right => c.editor().right(),
            KeyCode::Home => c.editor().home(),
            KeyCode::End => c.editor().end(),
            KeyCode::Char('a') if ctrl => c.editor().home(),
            KeyCode::Char('e') if ctrl => c.editor().end(),
            KeyCode::Backspace => c.editor().backspace(),
            KeyCode::Delete => c.editor().delete(),
            KeyCode::Char(ch) if !ctrl && !alt => {
                let mut b = [0; 4];
                c.editor().insert(ch.encode_utf8(&mut b));
            }
            _ => {}
        }
    }

    /// Bracketed paste: into the commit box when it has focus.
    pub fn handle_paste(&mut self, s: &str) {
        if self.paste_into_prompt(s) || self.paste_into_search(s) {
            self.dirty = true;
            return;
        }
        if self.focus == Focus::Commit && self.overlay.is_none() {
            self.dirty = true;
            self.changes.commit.editor().insert(s);
        }
    }
}
