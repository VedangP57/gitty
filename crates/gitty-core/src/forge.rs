//! Forge links: the GitHub "open a pull request" page for a branch. The URL half is pure.

use anyhow::bail;

use crate::git_cli::{GitCli, Kind};
use crate::net::push_target;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ForgeError {
    #[error("Only GitHub remotes are supported")]
    NotGithub,
    #[error("Could not read the remote URL")]
    Unparsable,
    #[error("Not a valid branch name for a link")]
    BadBranch,
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
            bail!("No remote to open");
        };
        let to = target.refspec.split_once(':').map_or(target.refspec.as_str(), |(_, to)| to);
        let name = to.strip_prefix("refs/heads/").unwrap_or(to);
        let remote = target.remote;
        if self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", &format!("refs/remotes/{remote}/{name}")], None).is_err() {
            bail!("Push the branch first (P)");
        }
        let out = self.quiet(Kind::Read, &["remote", "get-url", "--", &remote], None)?;
        Ok(github_pr_url(String::from_utf8_lossy(&out).trim(), name)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(url: &str, branch: &str) -> String {
        github_pr_url(url, branch).unwrap()
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
    }
}
