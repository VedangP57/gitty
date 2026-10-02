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
