//! Network jobs (spec §12.4): fetch, the local half of pull, and push. Each runs in its own
//! process group so cancel kills ssh and remote helpers too; stderr is read as it arrives and
//! turned into a phase-weighted progress fraction.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};

use crate::git_cli::{GitCli, Kind};

/// `(title, Some((cur, total)))` for `Receiving objects:  42% (420/1000)`, `(title, None)` for a
/// bare count such as `Counting objects: 12`. `remote: ` prefixes are dropped.
pub fn parse_progress(line: &str) -> Option<(String, Option<(u64, u64)>)> {
    let line = line.strip_prefix("remote: ").unwrap_or(line).trim_start();
    let (title, rest) = line.split_once(": ")?;
    if title.is_empty() || title.len() > 64 {
        return None;
    }
    let rest = rest.trim_start();
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    let n = digits(rest);
    if n == 0 {
        return None;
    }
    let after = &rest[n..];
    if let Some(p) = after.strip_prefix("% (") {
        let c = digits(p);
        let cur: u64 = p[..c].parse().ok()?;
        let p = p[c..].strip_prefix('/')?;
        let t = digits(p);
        let tot: u64 = p[..t].parse().ok()?;
        p[t..].starts_with(')').then_some(())?;
        return Some((title.to_string(), Some((cur, tot))));
    }
    if n > 3 && after.starts_with('%') {
        return None;
    }
    Some((title.to_string(), None))
}

/// Phase weights (Desktop's), in the order git reports the phases.
#[derive(Debug, Clone, Copy)]
pub struct Weights(pub &'static [(&'static str, f32)]);

impl Weights {
    pub const FETCH: Weights = Weights(&[("Compressing objects", 0.1), ("Receiving objects", 0.7), ("Resolving deltas", 0.2)]);
    pub const PUSH: Weights = Weights(&[("Compressing objects", 0.2), ("Writing objects", 0.7), ("Resolving deltas", 0.1)]);
}

/// Turns progress lines into one fraction that never goes backwards.
#[derive(Debug, Clone)]
pub struct Tracker {
    weights: Weights,
    shown: f32,
}

impl Tracker {
    pub fn new(weights: Weights) -> Tracker {
        Tracker { weights, shown: 0.0 }
    }

    /// The new fraction when this line moves it forward.
    pub fn update(&mut self, line: &str) -> Option<f32> {
        let (title, Some((cur, tot))) = parse_progress(line)? else { return None };
        let phases = self.weights.0;
        let i = phases.iter().position(|(t, _)| title == *t)?;
        let done: f32 = phases[..i].iter().map(|(_, w)| w).sum();
        let frac = if tot == 0 { 1.0 } else { (cur as f32 / tot as f32).min(1.0) };
        let v = (done + phases[i].1 * frac).min(1.0);
        (v > self.shown).then(|| {
            self.shown = v;
            v
        })
    }
}

/// Where a push goes: `git push <remote> <refspec>`, with `-u` the first time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
    pub remote: String,
    pub refspec: String,
    pub set_upstream: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetCmd {
    Fetch { remote: String },
    /// `git merge --ff-only @{u}`: the local half of pull.
    FfMerge,
    Merge,
    Rebase,
    Push(PushTarget),
}

impl NetCmd {
    fn args(&self) -> Vec<String> {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match self {
            NetCmd::Fetch { remote } => v(&["fetch", "--progress", "--prune", remote]),
            NetCmd::FfMerge => v(&["merge", "--ff-only", "@{u}"]),
            NetCmd::Merge => v(&["merge", "--no-edit", "@{u}"]),
            NetCmd::Rebase => v(&["rebase", "@{u}"]),
            NetCmd::Push(t) => {
                let mut a = v(&["push", "--progress", "--porcelain"]);
                if t.set_upstream {
                    a.push("-u".into());
                }
                a.push(t.remote.clone());
                a.push(t.refspec.clone());
                a
            }
        }
    }

    fn weights(&self) -> Weights {
        match self {
            NetCmd::Push(_) => Weights::PUSH,
            _ => Weights::FETCH,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Mode {
    /// The user started it: prompts go through the askpass trampoline (`exe` re-run as a helper,
    /// talking to `sock`).
    Interactive { exe: PathBuf, sock: PathBuf },
    /// Auto-fetch: no prompts at all; a credential request fails.
    Background,
}

/// One line of `git push --porcelain`: `<flag>\t<from>:<to>\t<summary>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRef {
    pub flag: char,
    pub local: String,
    pub remote: String,
    pub summary: String,
}

impl PushRef {
    /// Rejected because the remote moved on (`fetch first`, `non-fast-forward`, a stale
    /// lease): pulling fixes it. Other rejections (hooks, protected branches) do not.
    pub fn needs_pull(&self) -> bool {
        self.flag == '!' && !self.summary.contains("remote rejected") && ["fetch first", "non-fast-forward", "stale info"].iter().any(|s| self.summary.contains(s))
    }
}

pub fn parse_push_porcelain(stdout: &str) -> Vec<PushRef> {
    stdout
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let flag = f.next()?.chars().next()?;
            let (local, remote) = f.next()?.split_once(':')?;
            let summary = f.next().unwrap_or("").to_string();
            " +-*!=".contains(flag).then(|| PushRef { flag, local: local.into(), remote: remote.into(), summary })
        })
        .collect()
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Ok { summary: String },
    Cancelled,
    /// `merge --ff-only` found local commits: offer merge or rebase.
    Diverged,
    /// `detail`: what the remote said (a hook's message).
    Rejected { refs: Vec<PushRef>, detail: String },
    NeedsAuth { detail: String },
    Failed { detail: String },
}

/// Credential failures, from the texts git, ssh and remote helpers print.
pub fn is_auth_failure(stderr: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "terminal prompts disabled",
        "could not read Username",
        "could not read Password",
        "Authentication failed",
        "Permission denied (publickey",
        "Permission denied, please try again",
        "Host key verification failed",
        "invalid credentials",
        "HTTP Basic: Access denied",
    ];
    PATTERNS.iter().any(|p| stderr.contains(p))
}

/// `git config <key>` or None.
pub(crate) fn config(cli: &GitCli, key: &str) -> Option<String> {
    let out = cli.run(cli.cmd(Kind::Read, &["config", "--get", key]), None, &mut |_| {}).ok()?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn remotes(cli: &GitCli) -> Vec<String> {
    let out = cli.run(cli.cmd(Kind::Read, &["remote"]), None, &mut |_| {}).unwrap_or_default();
    String::from_utf8_lossy(&out).lines().map(str::to_string).collect()
}

/// The branch's upstream remote, else `origin`, else the only remote.
pub fn remote_of(cli: &GitCli, branch: Option<&str>) -> Option<String> {
    if let Some(r) = branch.and_then(|b| config(cli, &format!("branch.{b}.remote"))).filter(|r| r != ".") {
        return Some(r);
    }
    let all = remotes(cli);
    if all.iter().any(|r| r == "origin") {
        return Some("origin".into());
    }
    match all.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// Where `P` pushes, as `git push` with the default `push.default=simple` would, but publishing
/// instead of refusing:
/// - the remote is `branch.<b>.pushRemote`, else `remote.pushDefault`, else the upstream's remote,
///   else [`remote_of`];
/// - the branch goes to its own name; only `push.default=upstream` sends it to a differently named
///   upstream (a branch made with `git checkout -b feature origin/main` must never land on main);
/// - a branch without an upstream gets `-u`.
pub fn push_target(cli: &GitCli, branch: &str) -> anyhow::Result<PushTarget> {
    let up_remote = config(cli, &format!("branch.{branch}.remote")).filter(|r| r != ".");
    let merge = config(cli, &format!("branch.{branch}.merge"));
    let push_remote = config(cli, &format!("branch.{branch}.pushRemote")).or_else(|| config(cli, "remote.pushDefault"));
    let Some(remote) = push_remote.clone().or_else(|| up_remote.clone()).or_else(|| remote_of(cli, None)) else {
        bail!("no remote to push to: add one with `git remote add`")
    };
    let own = format!("refs/heads/{branch}");
    let tracked = up_remote.is_some() && merge.is_some();
    let to = match merge {
        Some(m) if tracked && push_remote.is_none() && config(cli, "push.default").as_deref() == Some("upstream") => m,
        _ => own.clone(),
    };
    Ok(PushTarget { remote, refspec: format!("{own}:{to}"), set_upstream: !tracked })
}

/// Cancels a running job by killing its process group: SIGTERM, then SIGKILL after 2 s.
#[derive(Debug, Clone)]
pub struct Cancel {
    pgid: i32,
    cancelled: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
}

impl Cancel {
    pub fn pgid(&self) -> i32 {
        self.pgid
    }

    pub fn cancel(&self) {
        if self.exited.load(Ordering::SeqCst) || self.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        // SAFETY: plain syscalls on a group this job created with setsid
        unsafe { libc::killpg(self.pgid, libc::SIGTERM) };
        let (pgid, exited) = (self.pgid, self.exited.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            // once the leader is reaped its pid (the group id) can be reused: leave it alone.
            // Until then the group is ours; `wait` kills leftover helpers when the leader exits.
            if !exited.load(Ordering::SeqCst) {
                // SAFETY: as above
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
            }
        });
    }
}

/// Reads a pipe to its end on a thread; [`Collector::finish`] takes what arrived.
struct Collector {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
}

impl Collector {
    fn spawn(mut r: impl Read + Send + 'static) -> Collector {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Collector { rx }
    }

    /// Everything until end of file, or until `end` passes without it.
    fn finish(self, end: std::time::Instant) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            match self.rx.recv_timeout(end.saturating_duration_since(std::time::Instant::now())) {
                Ok(chunk) => out.extend(chunk),
                Err(_) => return out,
            }
        }
    }
}

pub struct Job {
    child: Child,
    cmd: NetCmd,
    cancel: Cancel,
}

const STDERR_CAP: usize = 64 * 1024;
/// How long stderr and stdout (together) may stay open after git itself exited (helpers it
/// left behind).
const STDERR_GRACE: Duration = Duration::from_secs(1);

/// Whether `pid` (our child) has exited, without reaping it: until it is reaped, its pid and
/// so its process group id cannot be reused.
fn exited_unreaped(pid: i32) -> bool {
    // SAFETY: waitid only writes the siginfo we pass; WNOWAIT leaves the child waitable
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
    r == 0 && si_pid(&info) != 0
}

#[cfg(target_os = "linux")]
fn si_pid(i: &libc::siginfo_t) -> i32 {
    // SAFETY: filled in by waitid for WEXITED
    unsafe { i.si_pid() }
}

#[cfg(not(target_os = "linux"))]
fn si_pid(i: &libc::siginfo_t) -> i32 {
    i.si_pid
}

impl Job {
    pub fn spawn(cli: &GitCli, cmd: NetCmd, mode: Mode) -> anyhow::Result<Job> {
        let args = cmd.args();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut c = cli.cmd(Kind::Write, &args);
        c.env("LANG", "C").env("LC_ALL", "C").env_remove("LANGUAGE");
        match &mode {
            Mode::Interactive { exe, sock } => {
                c.env("GIT_ASKPASS", exe).env("SSH_ASKPASS", exe).env("SSH_ASKPASS_REQUIRE", "force").env("GITTY_ASKPASS_SOCK", sock);
                if std::env::var_os("DISPLAY").is_none() {
                    c.env("DISPLAY", ":0");
                }
            }
            Mode::Background => {
                // empty, not unset: unset GIT_ASKPASS falls back to core.askPass (a GUI dialog)
                c.env("GIT_ASKPASS", "").env("SSH_ASKPASS", "").env("SSH_ASKPASS_REQUIRE", "never").env("GCM_INTERACTIVE", "never").env_remove("GITTY_ASKPASS_SOCK");
                if std::env::var_os("GIT_SSH_COMMAND").is_none() && config(cli, "core.sshCommand").is_none() {
                    c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
                }
            }
        }
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = c.spawn().with_context(|| format!("running git {args:?}"))?;
        let pgid = child.id() as i32;
        let cancel = Cancel { pgid, cancelled: Arc::new(AtomicBool::new(false)), exited: Arc::new(AtomicBool::new(false)) };
        Ok(Job { child, cmd, cancel })
    }

    pub fn cancel_handle(&self) -> Cancel {
        self.cancel.clone()
    }

    /// Streams progress (fractions in 0..=1, increasing) until git exits. stderr and stdout are read
    /// on their own threads: helpers that keep them open after git exits get one shared
    /// [`STDERR_GRACE`] for both, not the rest of their life.
    pub fn wait(mut self, on_progress: &mut dyn FnMut(f32)) -> Outcome {
        let reader = Collector::spawn(self.child.stdout.take().expect("piped"));
        let mut err = self.child.stderr.take().expect("piped");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut tracker = Tracker::new(self.cmd.weights());
        let mut stderr = String::new();
        let mut pending = Vec::new();
        let mut take = |chunk: Vec<u8>, stderr: &mut String| {
            pending.extend_from_slice(&chunk);
            while let Some(i) = pending.iter().position(|b| *b == b'\r' || *b == b'\n') {
                let line: Vec<u8> = pending.drain(..=i).collect();
                let repaint = line[line.len() - 1] == b'\r';
                let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
                if let Some(p) = tracker.update(&line) {
                    on_progress(p);
                }
                // keep finished lines only: `\r` ends a repaint (progress, "Rebasing (1/2)")
                if !repaint && !line.is_empty() && parse_progress(&line).is_none_or(|(_, n)| n.is_none_or(|(c, t)| c == t)) {
                    stderr.push_str(&line);
                    stderr.push('\n');
                    if stderr.len() > STDERR_CAP {
                        let cut = stderr.len() - STDERR_CAP / 2;
                        let cut = (cut..stderr.len()).find(|&i| stderr.is_char_boundary(i)).unwrap_or(stderr.len());
                        stderr.drain(..cut);
                    }
                }
            }
        };
        let pid = self.child.id() as i32;
        let mut grace: Option<std::time::Instant> = None;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => take(chunk, &mut stderr),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                // every holder of stderr closed it
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if grace.is_none() && exited_unreaped(pid) {
                if self.cancel.cancelled.load(Ordering::SeqCst) {
                    // the leader is a zombie, so the group id is still ours: finish the helpers
                    // SAFETY: plain syscall on the group this job created
                    unsafe { libc::killpg(self.cancel.pgid, libc::SIGKILL) };
                }
                grace = Some(std::time::Instant::now() + STDERR_GRACE);
            }
            if grace.is_some_and(|g| std::time::Instant::now() >= g) {
                break;
            }
        }
        while let Ok(chunk) = rx.try_recv() {
            take(chunk, &mut stderr);
        }
        let status = self.child.wait();
        self.cancel.exited.store(true, Ordering::SeqCst);
        // like stderr, stdout may be held by a helper git left behind: both share one grace
        let deadline = grace.unwrap_or_else(|| std::time::Instant::now() + STDERR_GRACE);
        let stdout = String::from_utf8_lossy(&reader.finish(deadline)).into_owned();
        let ok = status.as_ref().is_ok_and(|s| s.success());
        // a cancel that arrived after git finished does not undo what git did
        if self.cancel.cancelled.load(Ordering::SeqCst) && !ok {
            return Outcome::Cancelled;
        }
        let refs = matches!(self.cmd, NetCmd::Push(_)).then(|| parse_push_porcelain(&stdout)).unwrap_or_default();
        // merge and rebase explain conflicts on stdout
        let detail = match &self.cmd {
            NetCmd::Push(_) => stderr.trim_end().to_string(),
            _ => format!("{}\n{}", stdout.trim_end(), stderr.trim_end()).trim().to_string(),
        };
        if ok {
            let summary = match &self.cmd {
                NetCmd::Push(_) => refs.iter().map(|r| format!("{} {}", r.remote, r.summary)).collect::<Vec<_>>().join(", "),
                _ => detail.lines().last().unwrap_or("").to_string(),
            };
            return Outcome::Ok { summary };
        }
        if refs.iter().any(|r| r.flag == '!') {
            return Outcome::Rejected { refs, detail };
        }
        if is_auth_failure(&detail) {
            return Outcome::NeedsAuth { detail };
        }
        if self.cmd == NetCmd::FfMerge && (detail.contains("Not possible to fast-forward") || detail.contains("can't be fast-forwarded")) {
            return Outcome::Diverged;
        }
        Outcome::Failed { detail }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdout_collection_gives_up_on_a_holder_after_the_grace() {
        use std::io::Write;
        let (r, mut w) = std::io::pipe().unwrap();
        w.write_all(b"done\n").unwrap();
        let c = Collector::spawn(r);
        let t = std::time::Instant::now();
        // `w` stays open: a helper that kept git's stdout
        let got = c.finish(t + Duration::from_millis(200));
        assert_eq!(got, b"done\n");
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        drop(w);
    }

    #[test]
    fn stdout_gets_only_what_is_left_of_the_shared_grace() {
        let (r, w) = std::io::pipe().unwrap();
        let c = Collector::spawn(r);
        // stderr's wait already used most of the one grace both pipes share
        let deadline = std::time::Instant::now() + Duration::from_millis(400);
        std::thread::sleep(Duration::from_millis(300));
        let t = std::time::Instant::now();
        c.finish(deadline);
        assert!(t.elapsed() < Duration::from_millis(250), "waited {:?} past the shared deadline", t.elapsed());
        drop(w);
    }

    #[test]
    fn parses_progress_lines() {
        assert_eq!(parse_progress("Receiving objects:  42% (420/1000), 1.2 MiB | 3 MiB/s"), Some(("Receiving objects".into(), Some((420, 1000)))));
        assert_eq!(parse_progress("remote: Compressing objects: 100% (5/5), done."), Some(("Compressing objects".into(), Some((5, 5)))));
        assert_eq!(parse_progress("remote: Counting objects: 12, done."), Some(("Counting objects".into(), None)));
        assert_eq!(parse_progress("fatal: unable to access 'x'"), None);
        assert_eq!(parse_progress("remote: Résolution des deltas: 3"), Some(("Résolution des deltas".into(), None)));
        assert_eq!(parse_progress("To /tmp/x.git"), None);
        assert_eq!(parse_progress(": 1"), None);
    }

    #[test]
    fn tracker_weights_phases_and_never_goes_back() {
        let mut t = Tracker::new(Weights::FETCH);
        assert_eq!(t.update("remote: Compressing objects: 100% (4/4), done."), Some(0.1));
        let r = t.update("Receiving objects:  50% (5/10)").unwrap();
        assert!((r - 0.45).abs() < 1e-6, "{r}");
        assert_eq!(t.update("remote: Compressing objects:  50% (2/4)"), None, "an earlier phase repeating cannot lower it");
        assert_eq!(t.update("Unknown phase:  99% (99/100)"), None);
        assert_eq!(t.update("Resolving deltas: 100% (3/3), done."), Some(1.0));
        assert_eq!(t.update("Resolving deltas: 100% (0/0)"), None, "never above 1");
    }

    #[test]
    fn push_porcelain_lines() {
        let out = "To /tmp/o.git\n \trefs/heads/main:refs/heads/main\t1a2b..3c4d\n!\trefs/heads/x:refs/heads/x\t[rejected] (non-fast-forward)\nDone\n";
        let r = parse_push_porcelain(out);
        assert_eq!(r.len(), 2);
        assert_eq!((r[1].flag, r[1].remote.as_str()), ('!', "refs/heads/x"));
    }

    #[test]
    fn auth_failure_texts() {
        assert!(is_auth_failure("fatal: could not read Username for 'https://github.com': terminal prompts disabled"));
        assert!(is_auth_failure("git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository."));
        assert!(is_auth_failure("remote: HTTP Basic: Access denied"));
        assert!(!is_auth_failure("fatal: unable to access 'https://x/': Could not resolve host: x"));
    }
}
