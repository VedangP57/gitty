#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::Command;

/// A throwaway repository with deterministic identity and dates.
pub struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("repo")).unwrap();
        let f = Fixture { dir };
        f.git(&["init", "-q", "-b", "main"]);
        f.git(&["config", "user.name", "Test User"]);
        f.git(&["config", "user.email", "test@example.com"]);
        f.git(&["config", "commit.gpgsign", "false"]);
        f.git(&["config", "core.autocrlf", "false"]);
        f
    }
    pub fn path(&self) -> PathBuf {
        self.dir.path().join("repo")
    }
    pub fn git(&self, args: &[&str]) -> String {
        self.git_env(args, &[])
    }
    pub fn git_env(&self, args: &[&str], env: &[(&str, String)]) -> String {
        let mut c = Command::new("git");
        c.current_dir(self.path()).args(args);
        c.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1");
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }
    pub fn write(&self, rel: &str, contents: impl AsRef<[u8]>) {
        let p = self.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }
    /// Commit everything with a fixed author+committer time. Returns the full hex id.
    pub fn commit(&self, msg: &str, epoch: i64) -> String {
        self.git(&["add", "-A"]);
        let date = format!("{epoch} +0000");
        self.git_env(
            &["commit", "-q", "--allow-empty", "-m", msg],
            &[("GIT_AUTHOR_DATE", date.clone()), ("GIT_COMMITTER_DATE", date)],
        );
        self.git(&["rev-parse", "HEAD"])
    }
    /// Creates a bare repo as `origin` next to the work repo, pushes main, sets upstream.
    pub fn add_bare_upstream(&self) -> PathBuf {
        let bare = self.dir.path().join("origin.git");
        let out = Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&bare).output().unwrap();
        assert!(out.status.success());
        self.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
        self.git(&["push", "-q", "-u", "origin", "main"]);
        bare
    }
}

pub fn path_of(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

impl Fixture {
    /// Shallow clone (`--depth`) of this repo into a sibling directory, returned as a Fixture-like path.
    pub fn shallow_clone(&self, depth: u32) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("file://{}", self.path().display());
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["clone", "-q", "--no-local", &format!("--depth={depth}"), &url])
            .arg(dir.path().join("repo"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        dir
    }
    /// Writes a raw commit object (bypassing validation) and points `branch` at it.
    pub fn raw_commit(&self, tree: &str, parent: &str, committer_line: &str, branch: &str) -> String {
        let body = format!("tree {tree}\nparent {parent}\nauthor A <a@a> 1700000000 +0000\n{committer_line}\n\nraw commit\n");
        let mut c = Command::new("git");
        c.current_dir(self.path()).args(["hash-object", "-t", "commit", "--literally", "-w", "--stdin"]);
        c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let id = String::from_utf8(out.stdout).unwrap().trim().to_string();
        self.git(&["update-ref", &format!("refs/heads/{branch}"), &id]);
        id
    }
}

/// Another person's clone of `bare` commits `file` and pushes main.
pub fn push_as_someone_else(bare: &Path, file: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let o = tmp.path().join("o");
    let git = |dir: &Path, args: &[&str]| {
        let out = Command::new("git").current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(tmp.path(), &["clone", "-q", bare.to_str().unwrap(), "o"]);
    std::fs::write(o.join(file), "theirs\n").unwrap();
    git(&o, &["add", "-A"]);
    git(&o, &["-c", "user.name=O", "-c", "user.email=o@example.com", "commit", "-qm", "theirs"]);
    git(&o, &["push", "-q", "origin", "main"]);
}

impl Fixture {
    /// A remote `name` whose transport is the shell script `body` (`ext::`), for hangs and failures.
    pub fn script_remote(&self, name: &str, body: &str) {
        let p = self.dir.path().join(format!("{name}.sh"));
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        self.git(&["config", "protocol.ext.allow", "always"]);
        self.git(&["remote", "add", name, &format!("ext::{}", p.display())]);
    }
}
