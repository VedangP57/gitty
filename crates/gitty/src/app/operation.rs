//! A merge, rebase, cherry-pick or revert in progress (`m`): continue it once everything is
//! resolved and staged, or abort it. The state is read with every status run, so one started in
//! another terminal shows up too.

use crossterm::event::{KeyCode, KeyEvent};
use gitty_core::op_state::{OpState, RepoOp};

use super::{App, Overlay, Toast};
use crate::msg::WriteOp;

fn say(what: &str) -> Option<Toast> {
    Some(Toast { what: what.into(), detail: String::new(), error: false })
}

/// Why Continue is not offered yet; None when it is.
pub fn continue_blocked(s: &OpState) -> Option<String> {
    let n = s.conflicts;
    (n > 0).then(|| if n == 1 { "1 file still conflicts: resolve it and stage it first".to_string() } else { format!("{n} files still conflict: resolve them and stage them first") })
}

/// The question before an abort, in words about what is lost and what is kept.
fn abort_question(op: RepoOp) -> (String, String) {
    let n = op.name();
    let body = match op {
        RepoOp::Rebase => "The branch goes back to where it was before the rebase; any changes or commits made during the rebase are discarded.".to_string(),
        _ => format!("Your resolved conflicts and everything done by the {n} are discarded. Changes you had before it started are kept where git can restore them."),
    };
    (format!("Abort the {n}?"), body)
}

impl App {
    /// `m`.
    pub fn open_operation(&mut self) {
        if self.op.is_some() {
            self.overlay = Some(Overlay::InProgress);
        } else {
            self.toast = say("Nothing in progress");
        }
    }

    /// Git found staged files that still contain conflict markers: committing them is the user's
    /// call. Enter continues with exactly these files accepted; the writer looks again.
    pub(super) fn offer_continue_anyway(&mut self, op: RepoOp, id: String, files: Vec<String>) {
        let shown = files.iter().take(3).map(String::as_str).collect::<Vec<_>>().join(", ");
        let more = if files.len() > 3 { format!(" and {} more", files.len() - 3) } else { String::new() };
        let (n, s) = (files.len(), if files.len() == 1 { "" } else { "s" });
        self.overlay = Some(Overlay::Confirm {
            title: "Continue anyway?".into(),
            body: format!("{n} staged file{s} still contain{} conflict markers ({shown}{more}). Continuing commits them as they are.", if n == 1 { "s" } else { "" }),
            op: WriteOp::ContinueOp { op, id, accepted: files },
        });
    }

    /// A merge, pull or rebase (`doing`, "Merging a into main") was left open on conflicts: ask
    /// whether to resolve them now. Another overlay stays: the banner shows the state, and a toast
    /// points at `m`. Returns that toast, if any.
    pub(super) fn offer_resolve(&mut self, doing: String, files: Vec<String>, state: OpState, stash: Option<String>) -> Option<Toast> {
        if self.overlay.is_some() {
            return say("Conflicts: press m");
        }
        let n = files.len();
        let shown = files.iter().take(3).map(String::as_str).collect::<Vec<_>>().join(", ");
        let more = if n > 3 { format!(", +{}", n - 3) } else { String::new() };
        let mut body = format!("{doing} hit conflicts in {n} file{} ({shown}{more}).", if n == 1 { "" } else { "s" });
        if stash.is_some() {
            body.push_str(" Your uncommitted changes are in the stash (stash@{0}): pop them after you finish the merge.");
        }
        self.overlay = Some(Overlay::Resolve { body, state, stash });
        None
    }

    /// Keys in the "resolve now?" prompt: Enter goes to the conflicts, `a` aborts (putting the
    /// stashed changes back), Esc decides later.
    pub(super) fn resolve_key(&mut self, ov: Overlay, k: KeyEvent) {
        let Overlay::Resolve { state, stash, .. } = &ov else { return };
        match k.code {
            KeyCode::Enter => self.resolve_now(),
            KeyCode::Char('a') => {
                let (op, id) = (state.op, state.id.clone());
                self.write(match stash {
                    Some(pushed) => WriteOp::AbortAndUnstash { op, id, pushed: pushed.clone() },
                    None => WriteOp::AbortOp { op, id },
                });
            }
            KeyCode::Esc | KeyCode::Char('q') => self.toast = say(&format!("The {} stays open: press m to continue or abort it", state.op.name())),
            _ => self.overlay = Some(ov),
        }
    }

    /// Keys in the dialog: `c` continue, `a` abort, Esc close.
    pub(super) fn operation_key(&mut self, k: KeyEvent) {
        let Some(state) = self.op.clone() else { return };
        match k.code {
            KeyCode::Esc | KeyCode::Char('q' | 'm') => {}
            KeyCode::Char('c') => match continue_blocked(&state) {
                Some(why) => {
                    self.toast = say(&why);
                    self.overlay = Some(Overlay::InProgress);
                }
                None => self.write(WriteOp::ContinueOp { op: state.op, id: state.id, accepted: Vec::new() }),
            },
            KeyCode::Char('a') => {
                let (title, body) = abort_question(state.op);
                self.overlay = Some(Overlay::Confirm { title, body, op: WriteOp::AbortOp { op: state.op, id: state.id } });
            }
            _ => self.overlay = Some(Overlay::InProgress),
        }
    }
}
