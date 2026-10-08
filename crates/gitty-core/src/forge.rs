//! Forge links: the GitHub "open a pull request" page for a branch, and the state of the branch's
//! pull request. The URL half is pure.

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
    /// The "new pull request" page for `branch` on the remote and under the name `P` would push
    /// it to, and that remote branch's name. Whether the remote branch exists is not looked at.
    fn pr_remote(&self, branch: &str) -> anyhow::Result<(String, String, String)> {
        self.check_branch_name(branch)?;
        let Ok(target) = push_target(self, branch) else {
            return Err(ForgeError::NoRemote.into());
        };
        let to = target.refspec.split_once(':').map_or(target.refspec.as_str(), |(_, to)| to);
        let name = to.strip_prefix("refs/heads/").unwrap_or(to);
        let remote = target.remote;
        let out = self.quiet(Kind::Read, &["remote", "get-url", "--push", "--", &remote], None)?;
        let compare = github_pr_url(String::from_utf8_lossy(&out).trim(), name)?;
        Ok((remote, name.to_string(), compare))
    }

    /// [`GitCli::pr_remote`] for a branch that has been pushed.
    fn pr_target(&self, branch: &str) -> anyhow::Result<(String, String)> {
        let (remote, name, compare) = self.pr_remote(branch)?;
        if self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", &format!("refs/remotes/{remote}/{name}")], None).is_err() {
            return Err(ForgeError::NotPushed.into());
        }
        Ok((compare, name))
    }

    /// The "open a pull request" page of a pushed branch.
    pub fn pr_url(&self, branch: &str) -> anyhow::Result<String> {
        let (compare, name) = self.pr_target(branch)?;
        Ok(open_pr_url("gh", &compare, &name).unwrap_or(compare))
    }

    /// The newest pull request whose head is `branch` on its remote, whatever its state, even
    /// when the remote branch is gone (merged and deleted). `Ok(None)`: there is none, or no
    /// GitHub remote to have one. `Err`: gh could not say (missing, logged out, offline, timed
    /// out, an answer it should not give).
    pub fn pr_badge(&self, branch: &str) -> Result<Option<PrInfo>, PrUnknown> {
        self.pr_badge_with("gh", branch)
    }

    /// [`GitCli::pr_badge`] asking `program` instead of `gh`.
    pub fn pr_badge_with(&self, program: &str, branch: &str) -> Result<Option<PrInfo>, PrUnknown> {
        let (_, name, compare) = match self.pr_remote(branch) {
            Ok(t) => t,
            Err(e) if matches!(e.downcast_ref::<ForgeError>(), Some(ForgeError::NotGithub | ForgeError::NoRemote)) => return Ok(None),
            Err(_) => return Err(PrUnknown),
        };
        pr_info(program, &compare, &name)
    }
}

/// Where a pull request stands, as GitHub shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

/// gh could not say whether there is a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrUnknown;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub state: PrState,
    /// `https://github.com/{owner}/{repo}/pull/{number}`, checked.
    pub url: String,
}

/// Runs `program` (gh) with `args`: stdout up to `limit` bytes if it exits successfully within a
/// few seconds. No terminal, no prompts, no stderr.
fn run_gh(program: &str, args: &[&str], limit: u64) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
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
    child.stdout.take()?.take(limit).read_to_string(&mut out).ok()?;
    Some(out)
}

/// The page of the open pull request for `branch`, asked of the `gh` CLI. `compare` is the
/// `/pull/new/` URL, which names the repository. `None` on any trouble: gh missing, not logged
/// in, no open PR, an odd answer or no answer within a few seconds.
fn open_pr_url(program: &str, compare: &str, branch: &str) -> Option<String> {
    let (repo, _) = compare.strip_prefix("https://github.com/")?.split_once("/pull/new/")?;
    let out = run_gh(program, &["pr", "list", "-R", repo, "--head", branch, "--state", "open", "--limit", "20", "--json", "url,isCrossRepository"], 16 * 1024)?;
    pick_pr_url(own_pr(&out).ok()??.get("url")?.as_str()?, repo)
}

/// The newest pull request for `branch` on the repository `compare` names, asked of gh.
fn pr_info(program: &str, compare: &str, branch: &str) -> Result<Option<PrInfo>, PrUnknown> {
    let (repo, _) = compare.strip_prefix("https://github.com/").and_then(|c| c.split_once("/pull/new/")).ok_or(PrUnknown)?;
    let out = run_gh(program, &["pr", "list", "-R", repo, "--head", branch, "--state", "all", "--limit", "20", "--json", "number,state,isDraft,isCrossRepository,url"], 64 * 1024).ok_or(PrUnknown)?;
    match own_pr(&out)? {
        None => Ok(None),
        Some(pr) => parse_pr(&pr, repo).map(Some).ok_or(PrUnknown),
    }
}

/// The first pull request of gh's JSON list that comes from the repository itself: `--head`
/// matches the branch name only, so pull requests from forks of the same name are in the list too.
/// `Err` when it is not a list; `Ok(None)` when none qualifies.
fn own_pr(json: &str) -> Result<Option<serde_json::Value>, PrUnknown> {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).map_err(|_| PrUnknown)?;
    Ok(list.into_iter().find(|pr| pr.get("isCrossRepository").and_then(|c| c.as_bool()) == Some(false)))
}

#[cfg(test)]
/// [`parse_pr`] of the first pull request of the repository itself in gh's JSON list.
fn parse_pr_list(json: &str, repo: &str) -> Option<PrInfo> {
    parse_pr(&own_pr(json).ok()??, repo)
}

/// A pull request if its fields are what gh documents and its URL is that repository's page for
/// that number.
fn parse_pr(pr: &serde_json::Value, repo: &str) -> Option<PrInfo> {
    let number = pr.get("number")?.as_u64()?;
    let draft = pr.get("isDraft")?.as_bool()?;
    let state = match (pr.get("state")?.as_str()?, draft) {
        ("OPEN", false) => PrState::Open,
        ("OPEN", true) => PrState::Draft,
        ("MERGED", _) => PrState::Merged,
        ("CLOSED", _) => PrState::Closed,
        _ => return None,
    };
    let url = pick_pr_url(pr.get("url")?.as_str()?, repo)?;
    (url.rsplit('/').next() == Some(number.to_string().as_str())).then_some(PrInfo { number, state, url })
}

/// `gh`'s output if it is exactly one line holding `https://github.com/{repo}/pull/{number}`;
/// gh writes the repository's canonical case, which the remote URL may not have.
fn pick_pr_url(output: &str, repo: &str) -> Option<String> {
    let line = output.strip_suffix('\n').unwrap_or(output);
    let rest = line.strip_prefix("https://github.com/")?;
    let number = rest.get(repo.len()..).filter(|_| rest.get(..repo.len()).is_some_and(|r| r.eq_ignore_ascii_case(repo)))?.strip_prefix("/pull/")?;
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

    fn pr(number: u64, state: PrState) -> Option<PrInfo> {
        Some(PrInfo { number, state, url: format!("https://github.com/o/r/pull/{number}") })
    }

    #[test]
    fn parses_each_state() {
        let one = |state: &str, draft: bool| format!(r#"[{{"number":7,"state":"{state}","isCrossRepository":false,"isDraft":{draft},"url":"https://github.com/o/r/pull/7"}}]"#);
        assert_eq!(parse_pr_list(&one("OPEN", false), "o/r"), pr(7, PrState::Open));
        assert_eq!(parse_pr_list(&one("OPEN", true), "o/r"), pr(7, PrState::Draft));
        assert_eq!(parse_pr_list(&one("MERGED", false), "o/r"), pr(7, PrState::Merged));
        assert_eq!(parse_pr_list(&one("CLOSED", false), "o/r"), pr(7, PrState::Closed));
        // a draft that was closed is closed
        assert_eq!(parse_pr_list(&one("CLOSED", true), "o/r"), pr(7, PrState::Closed));
    }

    #[test]
    fn takes_the_first_pull_request_and_ignores_extra_fields() {
        let json = r#"[{"number":9,"state":"MERGED","isCrossRepository":false,"isDraft":false,"url":"https://github.com/o/r/pull/9","title":"x"},{"number":3,"state":"OPEN","isCrossRepository":false,"isDraft":false,"url":"https://github.com/o/r/pull/3"}]"#;
        assert_eq!(parse_pr_list(json, "o/r"), pr(9, PrState::Merged));
    }

    #[test]
    fn skips_pull_requests_from_forks() {
        let entry = |n: u64, cross: &str| format!(r#"{{"number":{n},"state":"OPEN","isDraft":false,"isCrossRepository":{cross},"url":"https://github.com/o/r/pull/{n}"}}"#);
        let list = |entries: &[String]| format!("[{}]", entries.join(","));
        assert_eq!(parse_pr_list(&list(&[entry(9, "true"), entry(5, "false"), entry(3, "false")]), "o/r"), pr(5, PrState::Open));
        assert_eq!(parse_pr_list(&list(&[entry(9, "true")]), "o/r"), None);
        // without the field, nothing says it is the repository's own
        assert_eq!(parse_pr_list(r#"[{"number":9,"state":"OPEN","isDraft":false,"url":"https://github.com/o/r/pull/9"}]"#, "o/r"), None);
        assert!(matches!(own_pr(&list(&[entry(9, "true")])), Ok(None)));
        assert!(own_pr("nope").is_err());
        let dir = tempfile::tempdir().unwrap();
        let compare = "https://github.com/o/r/pull/new/main";
        let forks = fake_gh(dir.path(), "forks", &format!("echo '{}'", list(&[entry(9, "true")])));
        assert_eq!(pr_info(&forks, compare, "main"), Ok(None));
        let mixed = fake_gh(dir.path(), "mixed", &format!("echo '{}'", list(&[entry(9, "true"), entry(5, "false")])));
        assert_eq!(pr_info(&mixed, compare, "main"), Ok(pr(5, PrState::Open)));
        // `R` opens the repository's own pull request, not a fork's
        assert_eq!(open_pr_url(&mixed, compare, "main").as_deref(), Some("https://github.com/o/r/pull/5"));
        assert_eq!(open_pr_url(&forks, compare, "main"), None);
    }

    #[test]
    fn the_repository_is_compared_case_insensitively() {
        let url = "https://github.com/acme/widgets/pull/12\n";
        assert_eq!(pick_pr_url(url, "Acme/Widgets").as_deref(), Some("https://github.com/acme/widgets/pull/12"));
        assert_eq!(pick_pr_url(url, "acme/widgets").as_deref(), Some("https://github.com/acme/widgets/pull/12"));
        assert_eq!(pick_pr_url(url, "acme/gadgets"), None);
        assert_eq!(pick_pr_url(url, "acme/widget"), None);
        assert_eq!(pick_pr_url(url, "acme/widgets-x"), None);
        assert_eq!(pick_pr_url("https://github.com/acme/widgets/pull/1a", "Acme/Widgets"), None);
        assert_eq!(pick_pr_url("http://github.com/acme/widgets/pull/1", "acme/widgets"), None);
    }

    #[test]
    fn refuses_odd_pull_request_lists() {
        let with = |number: &str, state: &str, draft: &str, url: &str| format!(r#"[{{"number":{number},"state":"{state}","isCrossRepository":false,"isDraft":{draft},"url":"{url}"}}]"#);
        let good = "https://github.com/o/r/pull/7";
        for bad in [
            with("7", "OPEN", "false", "https://github.com/o/other/pull/7"),
            with("7", "OPEN", "false", "https://evil.com/o/r/pull/7"),
            with("7", "OPEN", "false", "https://github.com/o/r/pull/7/files"),
            with("7", "OPEN", "false", "https://github.com/o/r/pull/7?x=1"),
            with("7", "OPEN", "false", "https://github.com/o/r/pull/8"),
            with("7", "OPEN", "false", "https://github.com/o/r/pull/7\\nhttps://evil.com"),
            with("7", "OPEN", "false", "javascript:alert(1)"),
            with("7", "WEIRD", "false", good),
            with("7", "open", "false", good),
            with("-7", "OPEN", "false", good),
            with("\"7\"", "OPEN", "false", good),
            with("7", "OPEN", "\"no\"", good),
            "[]".to_string(),
            "[{}]".to_string(),
            "[null]".to_string(),
            r#"{"number":7,"state":"OPEN","isCrossRepository":false,"isDraft":false,"url":"https://github.com/o/r/pull/7"}"#.to_string(),
            String::new(),
            "not json".to_string(),
            "[{\"number\":7".to_string(),
        ] {
            assert_eq!(parse_pr_list(&bad, "o/r"), None, "{bad:?}");
        }
    }

    /// A stand-in for gh: a script running `body` whatever it is asked.
    fn fake_gh(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn asks_gh_and_reads_its_answer() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let compare = "https://github.com/o/r/pull/new/feat/x";
        let args = r#"[ "$*" = "pr list -R o/r --head feat/x --state all --limit 20 --json number,state,isDraft,isCrossRepository,url" ] || exit 1"#;
        let ok = fake_gh(d, "ok", &format!(r#"{args}; echo '[{{"number":4,"state":"OPEN","isCrossRepository":false,"isDraft":true,"url":"https://github.com/o/r/pull/4"}}]'"#));
        assert_eq!(pr_info(&ok, compare, "feat/x"), Ok(pr(4, PrState::Draft)));
        // gh is told never to prompt
        let env = fake_gh(d, "env", r#"[ "$GH_PROMPT_DISABLED" = 1 ] || exit 1; echo '[{"number":4,"state":"MERGED","isCrossRepository":false,"isDraft":false,"url":"https://github.com/o/r/pull/4"}]'"#);
        assert_eq!(pr_info(&env, compare, "feat/x"), Ok(pr(4, PrState::Merged)));
        let fails = fake_gh(d, "fails", r#"echo '[{"number":4,"state":"OPEN","isCrossRepository":false,"isDraft":false,"url":"https://github.com/o/r/pull/4"}]'; exit 1"#);
        assert_eq!(pr_info(&fails, compare, "feat/x"), Err(PrUnknown));
        assert_eq!(pr_info("gitty-no-such-gh", compare, "feat/x"), Err(PrUnknown));
        assert_eq!(pr_info(&ok, "https://example.com/o/r/pull/new/x", "x"), Err(PrUnknown));
        // gh answering "none" is not a failure; an answer it should not give is
        let none = fake_gh(d, "none", "echo '[]'");
        assert_eq!(pr_info(&none, compare, "feat/x"), Ok(None));
        let odd = fake_gh(d, "odd", r#"echo '[{"number":4,"state":"OPEN","isCrossRepository":false,"isDraft":false,"url":"https://evil.com/o/r/pull/4"}]'"#);
        assert_eq!(pr_info(&odd, compare, "feat/x"), Err(PrUnknown));
    }

    #[test]
    fn a_hanging_gh_is_given_up_on() {
        let dir = tempfile::tempdir().unwrap();
        let slow = fake_gh(dir.path(), "slow", "exec sleep 30");
        let t = Instant::now();
        assert_eq!(pr_info(&slow, "https://github.com/o/r/pull/new/x", "x"), Err(PrUnknown));
        assert!(t.elapsed() < Duration::from_secs(10));
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
