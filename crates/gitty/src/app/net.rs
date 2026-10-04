//! Network jobs in the app: starting fetch/pull/push, progress in the top bar, cancel, prompts
//! from the askpass trampoline, and what each outcome shows.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use gitty_core::net::{Cancel, Mode, Outcome};

use super::{App, Overlay, Toast};
use crate::askpass::{Ask, AskKind, Secret};
use crate::editor::Editor;
use crate::msg::{Msg, NetOp, Request};

/// The running network job.
#[derive(Debug, Clone)]
pub struct NetJob {
    pub op: NetOp,
    pub label: String,
    pub fraction: Option<f32>,
    pub cancel: Option<Cancel>,
    pub background: bool,
    pub remote: Option<String>,
}

impl App {
    /// `exe` (this binary, run as the askpass helper) and the trampoline socket, set at startup.
    pub fn set_askpass(&mut self, exe: PathBuf, sock: PathBuf) {
        self.askpass = Some((exe, sock));
    }

    fn net_mode(&self, background: bool) -> Mode {
        match (&self.askpass, background) {
            (Some((exe, sock)), false) => Mode::Interactive { exe: exe.clone(), sock: sock.clone() },
            _ => Mode::Background,
        }
    }

    pub fn start_net(&mut self, op: NetOp) {
        if let Some(j) = &self.net {
            let what = if j.cancel.is_some() { format!("Wait for {} (x cancels)", j.label.to_lowercase()) } else { format!("Wait for {}", j.label.to_lowercase()) };
            self.toast = Some(Toast { what, detail: String::new(), error: false });
            return;
        }
        let label = match op {
            NetOp::Fetch => "Fetching",
            NetOp::Pull => "Pulling",
            NetOp::PullMerge => "Merging",
            NetOp::PullRebase => "Rebasing",
            NetOp::Push => "Pushing",
        };
        self.net = Some(NetJob { op, label: label.into(), fraction: None, cancel: None, background: false, remote: None });
        let mode = self.net_mode(false);
        self.outbox.push(Request::Net { op, mode, background: false });
    }

    /// `x`: cancel the running job when it can be cancelled.
    pub fn cancel_net(&mut self) {
        match self.net.as_ref().and_then(|j| j.cancel.clone()) {
            Some(c) => c.cancel(),
            None if self.net.is_some() => self.toast = Some(Toast { what: "This step cannot be cancelled; it finishes in a moment".into(), detail: String::new(), error: false }),
            None => {}
        }
    }

    /// The top-bar text while a job runs.
    pub fn net_bar(&self) -> Option<String> {
        let j = self.net.as_ref()?;
        if j.background {
            return Some("fetching…".into());
        }
        let pct = j.fraction.map(|f| format!(" {}%", (f * 100.0).floor() as u32)).unwrap_or_else(|| "…".into());
        let cancel = if j.cancel.is_some() { " · x cancel" } else { "" };
        Some(format!("{}{pct}{cancel}", j.label))
    }

    /// Network and prompt messages. Returns the message when it is not one of them.
    pub(super) fn handle_net_msg(&mut self, m: Msg) -> Option<Msg> {
        match m {
            Msg::NetStarted { op, label, remote, cancel } => {
                let background = self.net.as_ref().is_some_and(|j| j.background);
                self.net = Some(NetJob { op, label, fraction: None, cancel, background, remote });
            }
            Msg::NetProgress { op, fraction } => {
                if let Some(j) = self.net.as_mut().filter(|j| j.op == op) {
                    j.fraction = Some(j.fraction.map_or(fraction, |f| f.max(fraction)));
                }
            }
            Msg::NetDone { op, background, outcome } => self.net_done(op, background, outcome),
            Msg::Ask(a) => {
                self.asks.push_back(a);
                self.next_ask();
            }
            m => return Some(m),
        }
        None
    }

    fn net_done(&mut self, op: NetOp, background: bool, outcome: Outcome) {
        let job = self.net.take();
        let remote = job.as_ref().and_then(|j| j.remote.clone());
        let label = job.map_or_else(|| op.verb().to_string(), |j| j.label);
        if matches!(op, NetOp::Fetch | NetOp::Pull) && matches!(outcome, Outcome::Ok { .. }) {
            self.last_fetch = self.clock;
            self.needs_auth = None;
        }
        self.outbox.push(Request::Refs);
        self.request_status();
        let toast = |what: String, detail: String, error: bool| Some(Toast { what, detail, error });
        if background {
            self.background_done(outcome, remote);
            return;
        }
        self.toast = match outcome {
            Outcome::Ok { summary } => {
                let what = match op {
                    NetOp::Fetch => label.replacen("Fetching", "Fetched", 1),
                    NetOp::Pull | NetOp::PullMerge | NetOp::PullRebase => "Pulled".to_string(),
                    NetOp::Push => label.replacen("Pushing", "Pushed", 1),
                };
                toast(what, summary, false)
            }
            Outcome::Cancelled => toast(format!("{} cancelled", op.verb()), String::new(), false),
            Outcome::Diverged => {
                self.overlay = Some(Overlay::Diverged);
                None
            }
            Outcome::Rejected { refs } => {
                let detail = refs.iter().map(|r| format!("{} → {}: {}", r.local, r.remote, r.summary)).collect::<Vec<_>>().join("\n");
                toast("Push rejected: the remote has commits you don't have; pull first (p)".into(), detail, true)
            }
            Outcome::NeedsAuth { detail } => toast(format!("{} failed: the remote refused the credentials", op.verb()), detail, true),
            Outcome::Failed { detail } => {
                let first = detail.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
                let what = if first.is_empty() || first.len() > 90 { format!("{} failed", op.verb()) } else { format!("{} failed: {}", op.verb(), first.trim_start_matches("fatal: ")) };
                toast(what, detail, true)
            }
        };
    }

    /// Background (auto-fetch) results are quiet; credentials it cannot supply turn it off.
    fn background_done(&mut self, outcome: Outcome, remote: Option<String>) {
        if let Outcome::NeedsAuth { .. } = outcome {
            self.needs_auth = Some(remote.unwrap_or_else(|| "the remote".into()));
        }
    }

    /// When auto-fetch runs next: focused, enabled, idle, with an upstream to fetch.
    pub fn auto_fetch_deadline(&self) -> Option<Instant> {
        let mins = self.config.auto_fetch_minutes;
        let upstream = self.refs.as_ref().is_some_and(|r| r.upstream.is_some());
        if !self.focused || mins == 0 || self.needs_auth.is_some() || self.net.is_some() || !upstream {
            return None;
        }
        Some(self.last_fetch + Duration::from_secs(60 * u64::from(mins)))
    }

    pub(super) fn tick_net(&mut self, at: Instant) {
        if self.auto_fetch_deadline().is_some_and(|d| d <= at) {
            self.last_fetch = at;
            self.net = Some(NetJob { op: NetOp::Fetch, label: "Fetching".into(), fraction: None, cancel: None, background: true, remote: None });
            self.outbox.push(Request::Net { op: NetOp::Fetch, mode: Mode::Background, background: true });
        }
    }

    /// Opens the next queued prompt when nothing else is on screen.
    pub(super) fn next_ask(&mut self) {
        if self.overlay.is_some() {
            return;
        }
        if let Some(ask) = self.asks.pop_front() {
            let mut input = Editor::single();
            input.reserve(256);
            self.overlay = Some(Overlay::Prompt { ask, input });
        }
    }

    fn answer(&mut self, id: u64, reply: Option<Secret>) {
        if let Some(h) = &self.ask_handle {
            h.answer(id, reply);
        }
    }

    /// Keys while a prompt is open. Every printable key is input.
    pub(super) fn prompt_key(&mut self, ask: Ask, mut input: Editor, k: KeyEvent) {
        let yes_no = ask.kind == AskKind::YesNo;
        match k.code {
            KeyCode::Esc => self.answer(ask.id, None),
            KeyCode::Char('y') if yes_no => self.answer(ask.id, Some(Secret::new("yes"))),
            KeyCode::Char('n') if yes_no => self.answer(ask.id, Some(Secret::new("no"))),
            KeyCode::Enter if !yes_no => self.answer(ask.id, Some(Secret::new(input.take()))),
            KeyCode::Backspace if !yes_no => {
                input.backspace();
                self.overlay = Some(Overlay::Prompt { ask, input });
            }
            KeyCode::Char(c) if !yes_no => {
                let mut b = [0; 4];
                input.insert(c.encode_utf8(&mut b));
                self.overlay = Some(Overlay::Prompt { ask, input });
            }
            _ => self.overlay = Some(Overlay::Prompt { ask, input }),
        }
    }

    /// Bracketed paste into an open prompt (a pasted token).
    pub(super) fn paste_into_prompt(&mut self, s: &str) -> bool {
        match &mut self.overlay {
            Some(Overlay::Prompt { ask, input }) if ask.kind != AskKind::YesNo => {
                input.insert(s.trim_end_matches(['\r', '\n']));
                true
            }
            _ => false,
        }
    }
}
