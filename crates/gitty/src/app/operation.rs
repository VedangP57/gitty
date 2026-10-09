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
        RepoOp::Rebase => "The rebase is undone and the branch goes back to where it was. Your resolved conflicts are discarded.".to_string(),
        _ => format!("Your resolved conflicts and everything done by the {n} are discarded; your own earlier work is kept."),
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
                None => self.write(WriteOp::ContinueOp { op: state.op }),
            },
            KeyCode::Char('a') => {
                let (title, body) = abort_question(state.op);
                self.overlay = Some(Overlay::Confirm { title, body, op: WriteOp::AbortOp { op: state.op } });
            }
            _ => self.overlay = Some(Overlay::InProgress),
        }
    }
}
