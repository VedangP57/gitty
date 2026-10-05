//! Every git CLI call goes through here (spec §12.1): the resolved binary, no terminal prompts,
//! optional locks off for reads, and each child in its own session so hooks and helpers cannot
//! draw on the TUI's terminal and the whole process group can be killed.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::Context;

use crate::GitError;
use crate::commit_files::BlobId;
use crate::repo::Repo;
use crate::status::{self, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Never takes the index lock (`GIT_OPTIONAL_LOCKS=0`).
    Read,
    Write,
}

#[derive(Clone)]
pub struct GitCli {
    git: PathBuf,
    dir: PathBuf,
}

impl GitCli {
    pub fn new(repo: &Repo) -> GitCli {
        GitCli { git: repo.git().path.clone(), dir: repo.workdir().unwrap_or(repo.git_dir()).to_path_buf() }
    }

    pub fn cmd(&self, kind: Kind, args: &[&str]) -> Command {
        let mut c = Command::new(&self.git);
        c.current_dir(&self.dir).env("GIT_TERMINAL_PROMPT", "0").args(["-c", "core.quotepath=false"]).args(args);
        if kind == Kind::Read {
            c.env("GIT_OPTIONAL_LOCKS", "0");
        }
        c.stdin(Stdio::null());
        // SAFETY: setsid is async-signal-safe and only affects the child.
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        c
    }

    /// Runs `cmd`, feeding `stdin`, calling `on_stderr` for each stderr line (split on `\r` and
    /// `\n`) as it arrives. Returns stdout; a non-zero exit is a [`GitError`] with all of stderr.
    pub fn run(&self, mut cmd: Command, stdin: Option<&[u8]>, on_stderr: &mut dyn FnMut(&str)) -> anyhow::Result<Vec<u8>> {
        let args: Vec<String> = cmd.get_args().skip(2).map(|a| a.to_string_lossy().into_owned()).collect();
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        let mut child = cmd.spawn().with_context(|| format!("running git {args:?}"))?;
        let mut out = child.stdout.take().expect("piped");
        let reader = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = out.read_to_end(&mut v);
            v
        });
        let writer = match (stdin, child.stdin.take()) {
            (Some(data), Some(mut w)) => {
                let data = data.to_vec();
                Some(std::thread::spawn(move || {
                    let _ = w.write_all(&data);
                }))
            }
            _ => None,
        };
        let mut stderr = String::new();
        let mut err = BufReader::new(child.stderr.take().expect("piped"));
        let mut buf = Vec::new();
        loop {
            buf.clear();
            // split on '\n'; progress output uses '\r' within a line
            match err.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let s = String::from_utf8_lossy(&buf);
                    stderr.push_str(&s);
                    for part in s.split(['\r', '\n']).filter(|p| !p.is_empty()) {
                        on_stderr(part);
                    }
                }
            }
        }
        if let Some(w) = writer {
            let _ = w.join();
        }
        let status = child.wait()?;
        let stdout = reader.join().unwrap_or_default();
        if !status.success() {
            return Err(GitError { args, code: status.code(), stderr: stderr.trim_end().to_string() }.into());
        }
        Ok(stdout)
    }

    fn quiet(&self, kind: Kind, args: &[&str], stdin: Option<&[u8]>) -> anyhow::Result<Vec<u8>> {
        self.run(self.cmd(kind, args), stdin, &mut |_| {})
    }

    /// Paths as NUL-separated literal pathspecs on stdin.
    fn with_paths(&self, args: &[&str], paths: &[String]) -> anyhow::Result<()> {
        let mut input = Vec::new();
        for p in paths {
            input.extend_from_slice(p.as_bytes());
            input.push(0);
        }
        let mut full = vec!["--literal-pathspecs"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--pathspec-from-file=-", "--pathspec-file-nul"]);
        self.quiet(Kind::Write, &full, Some(&input)).map(|_| ())
    }

    pub fn status(&self) -> anyhow::Result<Status> {
        let out = self.quiet(
            Kind::Read,
            &["--no-optional-locks", "status", "--porcelain=v2", "-z", "--branch", "--no-ahead-behind", "--untracked-files=all"],
            None,
        )?;
        Ok(status::parse(&out))
    }

    fn head_born(&self) -> bool {
        self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", "HEAD"], None).is_ok()
    }

    /// `git add -A` for the paths (new, modified and deleted alike).
    pub fn stage_paths(&self, paths: &[String]) -> anyhow::Result<()> {
        self.with_paths(&["add", "-A"], paths)
    }

    /// Restores the paths' index entries to HEAD (removes them when HEAD lacks them).
    pub fn unstage_paths(&self, paths: &[String]) -> anyhow::Result<()> {
        if self.head_born() {
            self.with_paths(&["restore", "--staged"], paths)
        } else {
            self.with_paths(&["rm", "--cached", "-q", "-r", "--ignore-unmatch"], paths)
        }
    }

    pub fn stage_all(&self) -> anyhow::Result<()> {
        self.quiet(Kind::Write, &["add", "-A"], None).map(|_| ())
    }

    pub fn unstage_all(&self) -> anyhow::Result<()> {
        if self.head_born() {
            self.quiet(Kind::Write, &["reset", "-q"], None).map(|_| ())
        } else {
            self.quiet(Kind::Write, &["rm", "--cached", "-q", "-r", "--ignore-unmatch", "."], None).map(|_| ())
        }
    }

    /// The index entry (stage 0) for `path`, if any.
    pub fn index_blob(&self, path: &str) -> anyhow::Result<Option<BlobId>> {
        let out = self.quiet(Kind::Read, &["--literal-pathspecs", "ls-files", "-s", "-z", "--", path], None)?;
        let rec = out.split(|&b| b == 0).next().unwrap_or(&[]);
        let s = String::from_utf8_lossy(rec);
        // "<mode> <hex> <stage>\t<path>"
        let blob = s.split(' ').nth(1).and_then(BlobId::from_hex);
        // an intent-to-add entry (`git add -N`) lists the empty blob, but status (which the
        // entry's index_blob came from) says it has no index side
        if blob == Some(BlobId::hash_of(b"")) && self.intent_to_add(path)? {
            return Ok(None);
        }
        Ok(blob)
    }

    /// The blob `path` has in HEAD; None when HEAD lacks it or is unborn.
    pub fn head_blob(&self, path: &str) -> anyhow::Result<Option<BlobId>> {
        let Ok(out) = self.quiet(Kind::Read, &["--literal-pathspecs", "ls-tree", "-z", "--full-tree", "HEAD", "--", path], None) else {
            return Ok(None);
        };
        let rec = String::from_utf8_lossy(out.split(|&b| b == 0).next().unwrap_or(&[])).into_owned();
        // "<mode> <type> <hex>\t<path>"
        Ok(rec.split('\t').next().and_then(|m| m.split(' ').nth(2)).and_then(BlobId::from_hex))
    }

    fn intent_to_add(&self, path: &str) -> anyhow::Result<bool> {
        let out = self.quiet(Kind::Read, &["--literal-pathspecs", "status", "--porcelain=v2", "-z", "--untracked-files=no", "--", path], None)?;
        let rec = String::from_utf8_lossy(out.split(|&b| b == 0).next().unwrap_or(&[])).into_owned();
        // "1 XY sub mH mI mW hH hI path": an all-zero index mode is the intent-to-add marker
        Ok(rec.strip_prefix("1 ").and_then(|r| r.split(' ').nth(3)).is_some_and(|m| m == "000000"))
    }

    /// Applies a patch to the index after checking `path`'s entry is still `expect` (spec §12.2
    /// TOCTOU guard): a mismatch means the index changed since the diff was made.
    pub fn apply_cached(&self, patch: &[u8], path: &str, expect: Option<BlobId>) -> anyhow::Result<()> {
        if self.index_blob(path)? != expect {
            anyhow::bail!("{path} changed in the index since its diff was loaded; refreshing");
        }
        self.quiet(Kind::Write, &["apply", "--cached", "--whitespace=nowarn", "-"], Some(patch)).map(|_| ())
    }

    /// `git commit -F -`; hook output streams through `on_stderr`.
    pub fn commit(&self, message: &str, amend: bool, on_stderr: &mut dyn FnMut(&str)) -> anyhow::Result<()> {
        let mut args = vec!["commit", "-q", "-F", "-"];
        if amend {
            args.push("--amend");
        }
        self.run(self.cmd(Kind::Write, &args), Some(message.as_bytes()), on_stderr).map(|_| ())
    }

    /// Full message of HEAD.
    pub fn head_message(&self) -> anyhow::Result<String> {
        let out = self.quiet(Kind::Read, &["log", "-1", "--format=%B"], None)?;
        let s = String::from_utf8_lossy(&out).into_owned();
        // `%B` plus log's own newline
        Ok(s.strip_suffix('\n').unwrap_or(&s).to_string())
    }

    /// Undoes the latest commit, keeping its changes staged, when HEAD is still `expect` and
    /// the upstream does not have it (spec §12.3). Returns its message.
    pub fn undo_commit(&self, expect: &str) -> anyhow::Result<String> {
        let head = self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", "HEAD"], None)?;
        if String::from_utf8_lossy(&head).trim() != expect {
            anyhow::bail!("HEAD is no longer the commit made here; nothing was undone");
        }
        if self.quiet(Kind::Read, &["merge-base", "--is-ancestor", "HEAD", "@{upstream}"], None).is_ok() {
            anyhow::bail!("that commit is already pushed; nothing was undone");
        }
        let msg = self.head_message()?;
        let has_parent = self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", "HEAD^"], None).is_ok();
        if has_parent {
            self.quiet(Kind::Write, &["reset", "--soft", "-q", "HEAD^"], None)?;
        } else {
            self.quiet(Kind::Write, &["update-ref", "-d", "HEAD"], None)?;
        }
        Ok(msg)
    }
}
