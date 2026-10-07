//! Forge links: the GitHub "open a pull request" page for a branch. The URL half is pure.

use crate::git_cli::{GitCli, Kind};
use crate::net::push_target;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ForgeError {
    #[error("Only GitHub remotes are supported")]
    NotGithub,
    #[error("Could not read the remote URL")]
    Unparsable,
    #[error("Not a valid branch name for a link")]
    BadBranch,
    #[error("No remote to open")]
    NoRemote,
    #[error("Push the branch first (P)")]
    NotPushed,
}

impl ForgeError {
    /// Something the user can act on (a notice), as opposed to a failure to read the repository.
    pub fn is_guidance(self) -> bool {
        matches!(self, ForgeError::NotGithub | ForgeError::NoRemote | ForgeError::NotPushed)
    }
}

/// `https://github.com/{owner}/{repo}/pull/new/{branch}` for a GitHub remote URL (scp-like, ssh,
/// git, http or https). Only `github.com` and `www.github.com` count; owner and repo are checked
/// so a crafted URL cannot smuggle `?`, `#` or `..` into the result.
pub fn github_pr_url(remote_url: &str, branch: &str) -> Result<String, ForgeError> {
    if branch.is_empty() || branch.starts_with('/') || branch.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        return Err(ForgeError::BadBranch);
    }
    let url = remote_url.trim();
    let (authority, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            if !["http", "https", "ssh", "git"].iter().any(|s| scheme.eq_ignore_ascii_case(s)) {
                return Err(ForgeError::Unparsable);
            }
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            if authority.contains('\\') {
                return Err(ForgeError::NotGithub);
            }
            let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            match host_port.rsplit_once(':') {
                Some((_, port)) if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) => return Err(ForgeError::Unparsable),
                Some((h, _)) => (h, path),
                None => (host_port, path),
            }
        }
        None => {
            let (authority, path) = url.split_once(':').ok_or(ForgeError::Unparsable)?;
            (authority.rsplit_once('@').map_or(authority, |(_, h)| h), path.strip_prefix('/').unwrap_or(path))
        }
    };
    if authority.is_empty() {
        return Err(ForgeError::Unparsable);
    }
    if !authority.eq_ignore_ascii_case("github.com") && !authority.eq_ignore_ascii_case("www.github.com") {
        return Err(ForgeError::NotGithub);
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments: Vec<&str> = path.split('/').collect();
    let [owner, repo] = segments.as_slice() else {
        return Err(ForgeError::Unparsable);
    };
    let valid = |s: &str| !s.is_empty() && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'));
    if !valid(owner) || !valid(repo) {
        return Err(ForgeError::Unparsable);
    }
    Ok(format!("https://github.com/{owner}/{repo}/pull/new/{}", encode_branch(branch)))
}

/// Percent-encodes everything but `/` and the RFC 3986 unreserved set.
fn encode_branch(branch: &str) -> String {
    let mut out = String::with_capacity(branch.len());
    for b in branch.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl GitCli {
    /// The "open a pull request" page of a pushed branch, on the remote and under the name `P`
    /// would push it to.
    pub fn pr_url(&self, branch: &str) -> anyhow::Result<String> {
        self.check_branch_name(branch)?;
        let Ok(target) = push_target(self, branch) else {
            return Err(ForgeError::NoRemote.into());
        };
        let to = target.refspec.split_once(':').map_or(target.refspec.as_str(), |(_, to)| to);
        let name = to.strip_prefix("refs/heads/").unwrap_or(to);
        let remote = target.remote;
        if self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", &format!("refs/remotes/{remote}/{name}")], None).is_err() {
            return Err(ForgeError::NotPushed.into());
        }
        let out = self.quiet(Kind::Read, &["remote", "get-url", "--push", "--", &remote], None)?;
        let compare = github_pr_url(String::from_utf8_lossy(&out).trim(), name)?;
        Ok(open_pr_url("gh", &compare, name).unwrap_or(compare))
    }
}

/// The page of the open pull request for `branch`, asked of the `gh` CLI. `compare` is the
/// `/pull/new/` URL, which names the repository. `None` on any trouble: gh missing, not logged
/// in, no open PR, an odd answer or no answer within a few seconds.
fn open_pr_url(program: &str, compare: &str, branch: &str) -> Option<String> {
    let (repo, _) = compare.strip_prefix("https://github.com/")?.split_once("/pull/new/")?;
    let mut child = Command::new(program)
        .args(["pr", "list", "-R", repo, "--head", branch, "--state", "open", "--json", "url", "--jq", ".[0].url"])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(4);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    child.stdout.take()?.take(4096).read_to_string(&mut out).ok()?;
    pick_pr_url(&out, repo)
}

/// `gh`'s output if it is exactly one line holding `https://github.com/{repo}/pull/{number}`.
fn pick_pr_url(output: &str, repo: &str) -> Option<String> {
    let line = output.strip_suffix('\n').unwrap_or(output);
    let number = line.strip_prefix("https://github.com/")?.strip_prefix(repo)?.strip_prefix("/pull/")?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(url: &str, branch: &str) -> String {
        github_pr_url(url, branch).unwrap()
    }

    #[test]
    fn picks_only_a_plain_pr_url() {
        let pick = |o: &str| pick_pr_url(o, "o/r");
        assert_eq!(pick("https://github.com/o/r/pull/12\n").as_deref(), Some("https://github.com/o/r/pull/12"));
        assert_eq!(pick("https://github.com/o/r/pull/12").as_deref(), Some("https://github.com/o/r/pull/12"));
        for bad in [
            "",
            "\n",
            "https://example.com/o/r/pull/12\n",
            "https://github.com/o/other/pull/12\n",
            "https://github.com/o/r2/pull/12\n",
            "https://github.com/o/r/pull/12\nhttps://github.com/o/r/pull/13\n",
            "https://github.com/o/r/pull/12\n\n",
            "https://github.com/o/r/pull/\n",
            "https://github.com/o/r/pull/1a\n",
            "https://github.com/o/r/pull/12/files\n",
            "https://github.com/o/r/pull/new/x\n",
            "null\n",
        ] {
            assert_eq!(pick(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn missing_gh_falls_back() {
        assert_eq!(open_pr_url("gitty-no-such-gh", "https://github.com/o/r/pull/new/b", "b"), None);
    }

    const BASE: &str = "https://github.com/owner/repo/pull/new/";

    #[test]
    fn every_url_shape() {
        for url in [
            "git@github.com:owner/repo.git",
            "git@github.com:owner/repo",
            "https://github.com/owner/repo.git",
            "https://github.com/owner/repo/",
            "http://github.com/owner/repo",
            "https://user@github.com/owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "ssh://git@github.com:22/owner/repo.git",
            "git://github.com/owner/repo.git",
            "  https://github.com/owner/repo.git\n",
            "HTTPS://GitHub.COM/owner/repo",
            "https://www.github.com/owner/repo",
            "https://github.com/owner/repo.git/",
        ] {
            assert_eq!(ok(url, "main"), format!("{BASE}main"), "{url}");
        }
    }

    #[test]
    fn branch_is_percent_encoded_per_segment() {
        let u = "git@github.com:owner/repo.git";
        assert_eq!(ok(u, "feat/x"), format!("{BASE}feat/x"));
        assert_eq!(ok(u, "a b"), format!("{BASE}a%20b"));
        assert_eq!(ok(u, "a#b"), format!("{BASE}a%23b"));
        assert_eq!(ok(u, "a?b"), format!("{BASE}a%3Fb"));
        assert_eq!(ok(u, "a%b&c"), format!("{BASE}a%25b%26c"));
        assert_eq!(ok(u, "feat/ünï"), format!("{BASE}feat/%C3%BCn%C3%AF"));
        assert_eq!(ok(u, "v1.2-rc_3~x"), format!("{BASE}v1.2-rc_3~x"));
    }

    #[test]
    fn not_github() {
        for url in [
            "git@gitlab.com:owner/repo.git",
            "https://github.com.evil.com/owner/repo",
            "https://notgithub.com/owner/repo",
            "https://github.example.com/owner/repo",
            "git@github.com.evil.com:owner/repo.git",
            "https://github.com@evil.com/o/r",
            "https://evil.com/github.com/o/r",
            "https://evil.com\\@github.com/o/r",
        ] {
            assert_eq!(github_pr_url(url, "m"), Err(ForgeError::NotGithub), "{url}");
        }
    }

    #[test]
    fn unparsable() {
        for url in [
            "",
            "https://github.com/owner",
            "https://github.com/a/b/c",
            "https://github.com/../repo",
            "https://github.com/owner/..",
            "https://github.com/./repo",
            "https://github.com//repo",
            "https://github.com/ow?ner/repo",
            "https://github.com/owner/re#po",
            "git@github.com:owner",
            "https://github.com/owner/.git",
            "https://github.com:abc/owner/repo",
            "https://github.com:/owner/repo",
        ] {
            assert_eq!(github_pr_url(url, "m"), Err(ForgeError::Unparsable), "{url}");
        }
    }

    #[test]
    fn bad_branches_are_refused() {
        let u = "git@github.com:owner/repo.git";
        for b in ["", "/x", "a//b", "a/", "./x", "a/./b", "..", "a/../b"] {
            assert_eq!(github_pr_url(u, b), Err(ForgeError::BadBranch), "{b:?}");
        }
    }

    #[test]
    fn messages_suit_a_toast() {
        assert_eq!(ForgeError::NotGithub.to_string(), "Only GitHub remotes are supported");
        assert_eq!(ForgeError::Unparsable.to_string(), "Could not read the remote URL");
        assert!(ForgeError::NotPushed.is_guidance() && ForgeError::NoRemote.is_guidance() && !ForgeError::Unparsable.is_guidance());
    }
}
