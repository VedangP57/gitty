//! Resolve the real `git` binary once. `/usr/bin/git` on macOS is an xcrun shim (+~4 ms/spawn).

use std::path::{Path, PathBuf};
use std::process::Command;

const SHIM: &str = "/usr/bin/git";

#[derive(Debug, Clone)]
pub struct GitBin {
    pub path: PathBuf,
}

/// First candidate that exists and is not the xcrun shim; else the shim if present.
pub fn pick_git(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let mut shim = None;
    for c in candidates {
        if !c.is_file() {
            continue;
        }
        if c == Path::new(SHIM) {
            shim.get_or_insert(c);
            continue;
        }
        return Some(c);
    }
    shim
}

impl GitBin {
    pub fn resolve() -> GitBin {
        let mut cands: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).map(|d| d.join("git")).collect())
            .unwrap_or_default();
        if cfg!(target_os = "macos")
            && let Ok(out) = Command::new("xcrun").args(["-f", "git"]).output()
            && out.status.success()
        {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                cands.push(PathBuf::from(s));
            }
        }
        cands.push(PathBuf::from(SHIM));
        GitBin { path: pick_git(cands).unwrap_or_else(|| PathBuf::from("git")) }
    }

    pub fn command(&self) -> Command {
        Command::new(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_non_shim() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("git");
        std::fs::write(&real, "").unwrap();
        let got = pick_git([PathBuf::from(SHIM), real.clone()]);
        assert_eq!(got, Some(real));
    }

    #[test]
    fn skips_missing() {
        assert_eq!(pick_git([PathBuf::from("/nonexistent/git")]), None);
    }

    #[test]
    fn resolve_runs_git() {
        let g = GitBin::resolve();
        let out = g.command().arg("--version").output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version"));
    }
}
