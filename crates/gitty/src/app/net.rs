//! Network jobs in the app: starting fetch/pull/push, progress in the top bar, cancel, prompts
//! from the askpass trampoline, and what each outcome shows.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use gitty_core::net::{Cancel, ForcePush, Mode, Outcome};
use gitty_core::tune::Action;

use super::{App, Overlay, Toast};
use crate::askpass::{Ask, AskKind, Secret};
use crate::editor::Editor;
use crate::msg::{Msg, NetOp, Request};

const TUNE_EVERY: Duration = Duration::from_secs(600);

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
        // a pull would merge or rebase on top of the one already open
        if let Some(s) = self.op.as_ref().filter(|_| matches!(op, NetOp::Pull | NetOp::PullMerge | NetOp::PullRebase)) {
            self.toast = Some(Toast { what: format!("finish or abort the {} first: m", s.op.name()), detail: String::new(), error: false });
            return;
        }
        let label = match op {
            // started with a plan, by start_force_push
            NetOp::ForcePush => return,
            NetOp::Fetch => "Fetching",
            NetOp::Pull => "Pulling",
            NetOp::PullMerge => "Merging",
            NetOp::PullRebase => "Rebasing",
            NetOp::Push => "Pushing",
        };
        self.net = Some(NetJob { op, label: label.into(), fraction: None, cancel: None, background: false, remote: None });
        self.prompt_cancelled = false;
        let mode = self.net_mode(false);
        self.outbox.push(Request::Net { op, mode, background: false, force: None });
    }

    /// Enter on the force-push question: `plan` was captured when the rejection came.
    pub(super) fn start_force_push(&mut self, plan: ForcePush) {
        if self.net.is_some() {
            // an auto-fetch started meanwhile: the question stays
            self.overlay = Some(Overlay::ForcePush { plan });
            self.toast = Some(Toast { what: "Fetch in progress, try again in a moment".into(), detail: String::new(), error: false });
            return;
        }
        self.net = Some(NetJob { op: NetOp::ForcePush, label: "Force pushing".into(), fraction: None, cancel: None, background: false, remote: None });
        self.prompt_cancelled = false;
        let mode = self.net_mode(false);
        self.outbox.push(Request::Net { op: NetOp::ForcePush, mode, background: false, force: Some(plan) });
    }

    /// `q`: with a job running in the foreground, ask first; a background fetch is just cancelled.
    pub fn request_quit(&mut self) {
        match &self.net {
            Some(j) if !j.background => self.overlay = Some(Overlay::Quit { label: j.label.clone() }),
            _ => self.quit_now(),
        }
    }

    /// Quits, cancelling the running job so git does not outlive gitty.
    pub fn quit_now(&mut self) {
        if let Some(c) = self.net.as_ref().and_then(|j| j.cancel.clone()) {
            c.cancel();
        }
        self.quit = true;
    }

    pub fn pending_asks(&self) -> usize {
        self.asks.len()
    }

    /// The top bar's note about the last auto-fetch, when it failed.
    pub fn background_problem(&self) -> Option<String> {
        self.bg_failure.as_ref().map(|_| "auto-fetch failed · !".to_string())
    }

    /// `!` with no error toast: the last auto-fetch failure's details.
    pub(super) fn show_background_problem(&mut self) {
        if let Some(detail) = self.bg_failure.clone() {
            self.toast = Some(Toast { what: "auto-fetch failed".into(), detail, error: true });
        }
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
            Msg::ForceOffer(result) => self.force_offer(result),
            Msg::Tuned { applied, error } => self.tuned(applied, error),
            Msg::Ask(a) => {
                self.asks.push_back(a);
                self.next_ask();
            }
            m => return Some(m),
        }
        None
    }

    fn net_done(&mut self, op: NetOp, background: bool, outcome: Outcome) {
        self.finish_net(op, background, outcome);
        // questions held while the job ran (or queued behind its prompt) can open now
        self.next_ask();
    }

    fn finish_net(&mut self, op: NetOp, background: bool, outcome: Outcome) {
        let job = self.net.take();
        let remote = job.as_ref().and_then(|j| j.remote.clone());
        let label = job.map_or_else(|| op.verb().to_string(), |j| j.label);
        // the job's prompts went with it
        if let Some(Overlay::Prompt { ask, .. }) = &self.overlay {
            let id = ask.id;
            self.overlay = None;
            self.answer(id, None);
        }
        for a in std::mem::take(&mut self.asks) {
            self.answer(a.id, None);
        }
        let prompt_cancelled = std::mem::take(&mut self.prompt_cancelled);
        let ok = matches!(outcome, Outcome::Ok { .. });
        if matches!(op, NetOp::Fetch | NetOp::Pull) && ok {
            self.last_fetch = self.clock;
            self.needs_auth = None;
            self.bg_failure = None;
        }
        if ok && !matches!(op, NetOp::Push | NetOp::ForcePush) {
            if op != NetOp::Fetch {
                // HEAD moved: check now, not at the next 10-minute slot
                self.last_tune = None;
            }
            // new commits may be missing from the commit-graph
            self.request_tune();
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
                    NetOp::ForcePush => label.replacen("Force pushing", "Force pushed", 1),
                };
                toast(what, summary, false)
            }
            Outcome::Cancelled => toast(format!("{} cancelled", op.verb()), String::new(), false),
            Outcome::Diverged => {
                // behind whatever is open (help, a confirmation): asked when it closes
                if self.overlay.is_some() {
                    self.pending_diverged = true;
                } else {
                    self.overlay = Some(Overlay::Diverged);
                }
                None
            }
            Outcome::Rejected { refs, detail } => {
                let stale = op == NetOp::ForcePush && refs.iter().any(|r| r.is_stale());
                let lines = refs.iter().filter(|r| r.flag == '!').map(|r| format!("{} → {}: {}", r.local, r.remote, r.summary)).collect::<Vec<_>>().join("\n");
                let detail = format!("{lines}\n{detail}").trim().to_string();
                let fetch = self.keymap.keys_of(crate::keymap::Action::Fetch).first().map_or_else(|| "f".to_string(), crate::keymap::Key::label);
                if stale {
                    toast(format!("The remote has new commits you haven't seen. Fetch first ({fetch}) and look at them."), detail, true)
                } else if refs.iter().any(|r| r.is_fetch_first()) {
                    toast(format!("The remote has new commits. Fetch first ({fetch})"), detail, true)
                } else if refs.iter().any(|r| r.needs_pull()) {
                    toast("Push rejected: the remote has commits you don't have; pull first (p)".into(), detail, true)
                } else {
                    // "[remote rejected] (pre-receive hook declined)" → "pre-receive hook declined"
                    let summary = refs.iter().find(|r| r.flag == '!').map_or("", |r| r.summary.as_str());
                    let why = summary.rsplit_once('(').and_then(|(_, r)| r.strip_suffix(')')).unwrap_or(summary);
                    toast(format!("Push refused by the remote: {why}"), detail, true)
                }
            }
            Outcome::NeedsAuth { .. } if prompt_cancelled => toast(format!("{} cancelled at the prompt", op.verb()), String::new(), false),
            Outcome::NeedsAuth { detail } => toast(format!("{} failed: the remote refused the credentials", op.verb()), detail, true),
            // found open on disk after the job: a question, not a failure
            Outcome::Conflicts { files, state, .. } => {
                let upstream = self.refs.as_ref().and_then(|r| r.upstream.as_ref()).map_or_else(|| "the upstream".to_string(), |(u, _)| u.clone());
                let doing = if op == NetOp::PullRebase { format!("Rebasing onto {upstream}") } else { format!("Pulling {upstream}") };
                self.offer_resolve(doing, files, state, None)
            }
            Outcome::Failed { detail } => {
                let first = detail.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
                let what = if first.is_empty() || first.len() > 90 { format!("{} failed", op.verb()) } else { format!("{} failed: {}", op.verb(), first.trim_start_matches("fatal: ")) };
                toast(what, detail, true)
            }
        };
    }

    /// The push was rejected because the remote moved on: ask whether to force push with a lease,
    /// or say why not. The rejection's toast stays (under the question, or with the reason added).
    fn force_offer(&mut self, result: Result<ForcePush, String>) {
        match result {
            Ok(plan) if self.overlay.is_none() => self.overlay = Some(Overlay::ForcePush { plan }),
            Ok(_) => {
                let push = self.keymap.keys_of(crate::keymap::Action::Push).first().map_or_else(|| "P".to_string(), crate::keymap::Key::label);
                self.toast = Some(Toast { what: format!("Push was rejected; press {push} to see the force push option"), detail: String::new(), error: true });
            }
            Err(notice) => {
                if let Some(t) = self.toast.as_mut() {
                    t.what = format!("{} · {notice}", t.what);
                }
            }
        }
    }

    /// Background (auto-fetch) results are quiet: credentials it cannot supply turn it off,
    /// other failures show in the top bar (details on `!`), and a cancel says so.
    fn background_done(&mut self, outcome: Outcome, remote: Option<String>) {
        match outcome {
            Outcome::NeedsAuth { .. } => self.needs_auth = Some(remote.unwrap_or_else(|| "the remote".into())),
            Outcome::Failed { detail } => self.bg_failure = Some(detail),
            Outcome::Cancelled => self.toast = Some(Toast { what: "Auto-fetch cancelled".into(), detail: String::new(), error: false }),
            _ => {}
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
            self.outbox.push(Request::Net { op: NetOp::Fetch, mode: Mode::Background, background: true, force: None });
        }
    }

    /// Checks (and applies) auto-tuning, at most every 10 minutes: after the first full walk and
    /// after fetches, pulls and commits.
    pub fn request_tune(&mut self) {
        if !self.config.auto_tune || self.history_len == 0 || self.last_tune.is_some_and(|t| self.clock.saturating_duration_since(t) < TUNE_EVERY) {
            return;
        }
        self.last_tune = Some(self.clock);
        self.outbox.push(Request::Tune { history_len: self.history_len, th: self.tune_thresholds });
    }

    /// One notice per session, and again whenever gitty changes the repo's config.
    fn tuned(&mut self, applied: Vec<Action>, error: Option<String>) {
        if let Some(detail) = error {
            if !self.tune_announced {
                self.tune_announced = true;
                self.toast = Some(Toast { what: "Auto-tuning this repository failed".into(), detail, error: true });
            }
            return;
        }
        let config = applied.iter().any(|a| *a != Action::CommitGraph);
        if applied.is_empty() || (self.tune_announced && !config) {
            return;
        }
        self.tune_announced = true;
        let list = applied.iter().map(|a| a.describe()).collect::<Vec<_>>().join(", ");
        self.toast = Some(Toast { what: format!("Tuned this large repository ({list}) · `gitty untune` undoes it"), detail: String::new(), error: false });
    }

    /// Opens the next queued prompt when nothing else is on screen.
    pub(super) fn next_ask(&mut self) {
        if self.overlay.is_some() {
            return;
        }
        // a running git waits on its prompt; the questions below wait on nothing
        if let Some(ask) = self.asks.pop_front() {
            let mut input = Editor::single();
            input.reserve(256);
            self.overlay = Some(Overlay::Prompt { ask, input });
            return;
        }
        // between two prompts of one git (username, then password) nothing else may open, and
        // "no git process is running" is not true until the job ends (net_done asks again)
        if self.net.is_some() {
            return;
        }
        if std::mem::take(&mut self.pending_diverged) {
            self.overlay = Some(Overlay::Diverged);
            return;
        }
        // a lock removed (or replaced) while the offer waited is no longer the question
        if let Some(seen) = self.pending_stale_lock.take().filter(|s| s.still_there()) {
            self.overlay = Some(Overlay::Confirm {
                title: "Remove the stale .git/index.lock?".into(),
                body: "A git command failed because the index is locked, and no git process is running. The lock was probably left by a git that crashed.".into(),
                op: crate::msg::WriteOp::RemoveIndexLock { seen },
            });
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
            KeyCode::Esc => {
                self.prompt_cancelled = true;
                self.answer(ask.id, None);
            }
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
