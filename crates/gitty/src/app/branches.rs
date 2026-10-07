//! Branches (spec: branches and stash): `B` opens a picker to switch, create, rename or delete.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gitty_core::refs::{Target, TargetKind};
use gitty_core::status::EntryKind;

use super::compare::fuzzy_rank;
use super::{App, Overlay, Toast};
use crate::editor::Editor;
use crate::msg::WriteOp;

/// What a [`Overlay::NameInput`] is typing a name for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameKind {
    /// A new branch at HEAD.
    Create,
    Rename { old: String },
    /// A stash message; empty means the default.
    Stash,
}

fn say(what: &str) -> Option<Toast> {
    Some(Toast { what: what.into(), detail: String::new(), error: false })
}

/// Edits `query` for one key: letters, Backspace, Ctrl-U. True when the key was an edit.
fn edit(query: &mut Editor, k: KeyEvent) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Backspace => query.backspace(),
        KeyCode::Char('u') if ctrl => query.set(""),
        KeyCode::Char(c) if !ctrl => {
            let mut b = [0; 4];
            query.insert(c.encode_utf8(&mut b));
        }
        _ => return false,
    }
    true
}

impl App {
    /// The picker's rows: current branch, local, remote-only; a query narrows and ranks them.
    pub fn switcher_matches(&self, query: &str) -> Vec<Target> {
        let Some(refs) = &self.refs else { return Vec::new() };
        let all = refs.switch_targets();
        if query.is_empty() {
            return all;
        }
        let names: Vec<String> = all.iter().map(|t| t.name.clone()).collect();
        fuzzy_rank(query, &names).into_iter().map(|i| all[i].clone()).collect()
    }

    pub fn open_switcher(&mut self) {
        self.overlay = Some(Overlay::Switcher { query: Editor::single(), sel: 0 });
    }

    /// Tracked changes git could refuse to carry across a switch (untracked files never block).
    fn tree_has_tracked_changes(&self) -> bool {
        self.changes.entries().iter().any(|e| e.kind != EntryKind::Untracked)
    }

    fn switch_to(&mut self, t: Target) {
        if t.kind == TargetKind::Current {
            return;
        }
        let remote = t.kind == TargetKind::Remote;
        if self.tree_has_tracked_changes() {
            self.overlay = Some(Overlay::DirtySwitch { name: t.name, remote });
        } else {
            self.write(WriteOp::SwitchBranch { name: t.name, remote });
        }
    }

    /// Keys while the picker is open.
    pub(super) fn switcher_key(&mut self, mut query: Editor, mut sel: usize, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let matches = self.switcher_matches(query.text());
        let n = matches.len();
        let picked = matches.get(sel).cloned();
        match k.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                if let Some(t) = picked {
                    self.switch_to(t);
                    return;
                }
            }
            KeyCode::Down | KeyCode::Tab => sel = (sel + 1).min(n.saturating_sub(1)),
            KeyCode::Up | KeyCode::BackTab => sel = sel.saturating_sub(1),
            KeyCode::Char('n') if ctrl => {
                let mut input = Editor::single();
                input.set(query.text());
                self.overlay = Some(Overlay::NameInput { kind: NameKind::Create, input });
                return;
            }
            KeyCode::Char('r') if ctrl => match picked {
                Some(t) if t.kind != TargetKind::Remote => {
                    let mut input = Editor::single();
                    input.set(&t.name);
                    self.overlay = Some(Overlay::NameInput { kind: NameKind::Rename { old: t.name }, input });
                    return;
                }
                Some(_) => self.toast = say("Only local branches can be renamed"),
                None => {}
            },
            KeyCode::Char('d') if ctrl => match picked {
                Some(Target { kind: TargetKind::Current, name }) => self.toast = say(&format!("`{name}` is checked out: switch to another branch first")),
                Some(Target { kind: TargetKind::Remote, .. }) => self.toast = say("Only local branches can be deleted"),
                Some(Target { name, .. }) => {
                    self.overlay = Some(Overlay::Confirm {
                        title: "Delete branch".into(),
                        body: format!("Delete the branch `{name}`?"),
                        op: WriteOp::DeleteBranch { name, force: false },
                    });
                    return;
                }
                None => {}
            },
            _ => {
                if edit(&mut query, k) {
                    sel = 0;
                }
            }
        }
        self.overlay = Some(Overlay::Switcher { query, sel });
    }

    /// Keys while a branch name is being typed.
    pub(super) fn name_key(&mut self, kind: NameKind, mut input: Editor, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                let name = input.text().trim().to_string();
                match kind {
                    NameKind::Stash => {
                        let message = if name.is_empty() { format!("gitty: stash on {}", self.head_name()) } else { name };
                        self.write(WriteOp::StashPush { message });
                        return;
                    }
                    _ if name.is_empty() => {}
                    NameKind::Create => {
                        self.write(WriteOp::CreateBranch { name });
                        return;
                    }
                    NameKind::Rename { old } => {
                        self.write(WriteOp::RenameBranch { old, new: name });
                        return;
                    }
                }
            }
            _ => {
                edit(&mut input, k);
            }
        }
        self.overlay = Some(Overlay::NameInput { kind, input });
    }

    /// Keys at the "uncommitted changes" prompt.
    pub(super) fn dirty_key(&mut self, name: String, remote: bool, k: KeyEvent) {
        match k.code {
            KeyCode::Char('s') => {
                let message = format!("gitty: auto-stash from {}", self.head_name());
                self.write(WriteOp::StashAndSwitch { name, remote, message });
            }
            KeyCode::Char('w') => self.write(WriteOp::SwitchBranch { name, remote }),
            KeyCode::Esc | KeyCode::Char('n' | 'q') => {}
            _ => self.overlay = Some(Overlay::DirtySwitch { name, remote }),
        }
    }

    fn head_name(&self) -> String {
        self.refs.as_ref().and_then(|r| r.head_branch()).unwrap_or("HEAD").to_string()
    }

    pub fn open_stashes(&mut self) {
        self.outbox.push(crate::msg::Request::StashList);
        self.overlay = Some(Overlay::Stashes { sel: 0 });
    }

    pub fn open_stash_name(&mut self) {
        self.overlay = Some(Overlay::NameInput { kind: NameKind::Stash, input: Editor::single() });
    }

    /// Keys in the stash list.
    pub(super) fn stashes_key(&mut self, mut sel: usize, k: KeyEvent) {
        let n = self.stashes.len();
        let cur = self.stashes.get(sel).map(|s| s.index);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return,
            KeyCode::Down | KeyCode::Char('j') => sel = (sel + 1).min(n.saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') => sel = sel.saturating_sub(1),
            KeyCode::Char('a') => {
                if let Some(index) = cur {
                    self.write(WriteOp::StashApply { index });
                    return;
                }
            }
            KeyCode::Char('p') => {
                if let Some(index) = cur {
                    self.write(WriteOp::StashPop { index });
                    return;
                }
            }
            KeyCode::Char('d') => {
                if let Some(index) = cur {
                    self.overlay = Some(Overlay::Confirm {
                        title: "Drop stash".into(),
                        body: format!("Drop stash@{{{index}}}? Its changes are lost."),
                        op: WriteOp::StashDrop { index },
                    });
                    return;
                }
            }
            KeyCode::Char('n') => {
                self.open_stash_name();
                return;
            }
            _ => {}
        }
        self.overlay = Some(Overlay::Stashes { sel });
    }

    /// A delete git refused for unmerged commits becomes a second, explicit question.
    pub(super) fn offer_force_delete(&mut self, op: &WriteOp, detail: &str) -> bool {
        let WriteOp::DeleteBranch { name, force: false } = op else { return false };
        if !detail.contains("not fully merged") {
            return false;
        }
        self.overlay = Some(Overlay::Confirm {
            title: "Delete branch".into(),
            body: format!("`{name}` has commits no other branch has. Delete it anyway?"),
            op: WriteOp::DeleteBranch { name: name.clone(), force: true },
        });
        true
    }
}

impl App {
    pub(super) fn handle_stash_msg(&mut self, m: crate::msg::Msg) -> Option<crate::msg::Msg> {
        match m {
            crate::msg::Msg::StashList { result } => {
                match result {
                    Ok(list) => self.stashes = list,
                    Err(detail) => self.toast = Some(Toast { what: "listing stashes".into(), detail, error: true }),
                }
                if let Some(Overlay::Stashes { sel }) = &mut self.overlay {
                    *sel = (*sel).min(self.stashes.len().saturating_sub(1));
                }
                None
            }
            m => Some(m),
        }
    }
}
