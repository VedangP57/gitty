//! Network-thread side of a [`NetOp`]: picks the remote and refspec, runs the git steps, and
//! reports start (with a cancel handle), throttled progress and the outcome.

use std::time::{Duration, Instant};

use gitty_core::Handle;
use gitty_core::git_cli::{GitCli, Kind};
use gitty_core::net::{ForcePush, Job, Mode, NetCmd, Outcome, force_push_plan, push_target, remote_of};

use crate::msg::{Msg, NetOp};

/// One progress message per frame at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(16);

fn current_branch(cli: &GitCli) -> Option<String> {
    let out = cli.run(cli.cmd(Kind::Read, &["symbolic-ref", "--short", "-q", "HEAD"]), None, &mut |_| {}).ok()?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn has_upstream(cli: &GitCli, branch: &str) -> bool {
    cli.run(cli.cmd(Kind::Read, &["config", "--get", &format!("branch.{branch}.merge")]), None, &mut |_| {}).is_ok()
}

fn failed(detail: impl Into<String>) -> Outcome {
    Outcome::Failed { detail: detail.into() }
}

/// Runs one git step; `cancellable` steps hand their cancel handle to the UI.
fn step(cli: &GitCli, op: NetOp, cmd: NetCmd, label: String, cancellable: bool, mode: &Mode, sink: &mut dyn FnMut(Msg)) -> Outcome {
    let remote = match &cmd {
        NetCmd::Fetch { remote } => Some(remote.clone()),
        NetCmd::Push(t) => Some(t.remote.clone()),
        NetCmd::ForcePush(f) => Some(f.target.remote.clone()),
        _ => None,
    };
    // fast-forward, merge and rebase write the index and worktree: never alongside the writer
    let local = matches!(cmd, NetCmd::FfMerge | NetCmd::Merge | NetCmd::Rebase);
    let _write = local.then(crate::write::lock);
    let job = match Job::spawn(cli, cmd, mode.clone()) {
        Ok(j) => j,
        Err(e) => return failed(format!("{e:#}")),
    };
    let cancel = cancellable.then(|| job.cancel_handle());
    sink(Msg::NetStarted { op, label, remote, cancel });
    let mut last: Option<Instant> = None;
    job.wait(&mut |fraction| {
        if last.is_none_or(|t| t.elapsed() >= PROGRESS_EVERY || fraction >= 1.0) {
            last = Some(Instant::now());
            sink(Msg::NetProgress { op, fraction });
        }
    })
}

pub fn run(h: &Handle, op: NetOp, mode: Mode, background: bool, force: Option<ForcePush>, sink: &mut dyn FnMut(Msg)) {
    let outcome = outcome(h, op, &mode, force, sink);
    // The lease is read now, at the rejection: a fetch before the confirm must make the push
    // stale, not move what it is checked against.
    let offer = match (&outcome, op) {
        (Outcome::Rejected { refs, .. }, NetOp::Push) if refs.iter().any(|r| r.needs_pull()) => {
            let cli = GitCli::new(h.owner());
            current_branch(&cli).map(|b| (force_push_plan(&cli, &b).map_err(|e| format!("{e:#}")), b))
        }
        _ => None,
    };
    sink(Msg::NetDone { op, background, outcome });
    if let Some((result, branch)) = offer {
        sink(Msg::ForceOffer { branch, result });
    }
}

fn outcome(h: &Handle, op: NetOp, mode: &Mode, force: Option<ForcePush>, sink: &mut dyn FnMut(Msg)) -> Outcome {
    let cli = GitCli::new(h.owner());
    let branch = current_branch(&cli);
    let remote = || remote_of(&cli, branch.as_deref());
    match op {
        NetOp::Fetch => match remote() {
            Some(r) => step(&cli, op, NetCmd::Fetch { remote: r.clone() }, format!("Fetching {r}"), true, mode, sink),
            None => failed("This repository has no remote to fetch from"),
        },
        NetOp::Pull => {
            let Some(b) = branch.as_deref() else { return failed("HEAD is detached: check out a branch to pull") };
            if !has_upstream(&cli, b) {
                return failed(format!("{b} has no upstream branch: push it first (P)"));
            }
            let Some(r) = remote() else { return failed("This repository has no remote to pull from") };
            match step(&cli, op, NetCmd::Fetch { remote: r.clone() }, format!("Pulling {r}"), true, mode, sink) {
                Outcome::Ok { .. } => step(&cli, op, NetCmd::FfMerge, format!("Updating {b}"), false, mode, sink),
                o => o,
            }
        }
        NetOp::PullMerge => step(&cli, op, NetCmd::Merge, "Merging the upstream".into(), false, mode, sink),
        NetOp::PullRebase => step(&cli, op, NetCmd::Rebase, "Rebasing onto the upstream".into(), false, mode, sink),
        NetOp::ForcePush => match (branch.as_deref(), force) {
            (Some(b), Some(f)) => {
                let label = format!("Force pushing {b} to {}", f.target.remote);
                step(&cli, op, NetCmd::ForcePush(f), label, true, mode, sink)
            }
            _ => failed("HEAD is detached: check out a branch to push"),
        },
        NetOp::Push => {
            let Some(b) = branch.as_deref() else { return failed("HEAD is detached: check out a branch to push") };
            match push_target(&cli, b) {
                Ok(t) => {
                    let label = format!("Pushing {b} to {}", t.remote);
                    step(&cli, op, NetCmd::Push(t), label, true, mode, sink)
                }
                Err(e) => failed(format!("{e:#}")),
            }
        }
    }
}
