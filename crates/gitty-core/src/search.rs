//! History search (spec §8.4): a parsed query matched against decoded rows, plus the
//! `path:` filter, which asks git for the commits touching a path.

use std::collections::HashSet;

use crate::git_cli::{GitCli, Kind};
use crate::history::CommitRow;
use crate::types::CommitId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// Words to find in the summary, author name or email; `None` when the query is only a path.
    pub text: Option<String>,
    pub path: Option<String>,
    /// Smart case: sensitive only when the text has an uppercase letter.
    case_sensitive: bool,
    /// `text` lowercased once, for insensitive matching.
    folded: String,
}

impl Query {
    /// `fix crash path:src/ui` or `path:"dir with space/f"`. `None` when there is nothing to find.
    pub fn parse(input: &str) -> Option<Query> {
        let mut words = Vec::new();
        let mut path = None;
        let mut rest = input.trim_start();
        while !rest.is_empty() {
            if let Some(p) = rest.strip_prefix("path:") {
                let (value, after) = match p.strip_prefix('"') {
                    Some(q) => match q.find('"') {
                        Some(end) => (&q[..end], &q[end + 1..]),
                        None => (q, ""),
                    },
                    None => p.split_at(p.find(char::is_whitespace).unwrap_or(p.len())),
                };
                if !value.is_empty() {
                    path = Some(value.to_string());
                }
                rest = after.trim_start();
                continue;
            }
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            words.push(&rest[..end]);
            rest = rest[end..].trim_start();
        }
        let text = (!words.is_empty()).then(|| words.join(" "));
        if text.is_none() && path.is_none() {
            return None;
        }
        let case_sensitive = text.as_deref().is_some_and(|t| t.chars().any(char::is_uppercase));
        let folded = text.as_deref().map(str::to_lowercase).unwrap_or_default();
        Some(Query { text, path, case_sensitive, folded })
    }

    /// Whether `row`'s summary, author name or email contains the text. A path-only query
    /// matches every row (the path filter is applied separately).
    pub fn matches(&self, row: &CommitRow) -> bool {
        let Some(text) = self.text.as_deref() else { return true };
        let hay = [row.summary.as_str(), row.author.name.as_str(), row.author.email.as_str()];
        if self.case_sensitive {
            hay.iter().any(|h| h.contains(text))
        } else {
            hay.iter().any(|h| contains_folded(h, &self.folded))
        }
    }
}

/// `needle` is already lowercase. ASCII haystacks skip the allocation of `to_lowercase`.
fn contains_folded(hay: &str, needle: &str) -> bool {
    if hay.is_ascii() && needle.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle.as_bytes());
        return n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    hay.to_lowercase().contains(needle)
}

/// Commits reachable from `tips` that touch `path` (a file or a directory), matched literally:
/// no globs, no pathspec magic.
pub fn path_commits(cli: &GitCli, tips: &[CommitId], path: &str) -> anyhow::Result<HashSet<CommitId>> {
    if tips.is_empty() {
        return Ok(HashSet::new());
    }
    let hex: Vec<String> = tips.iter().map(|t| t.to_string()).collect();
    let mut args = vec!["log", "--format=%H"];
    args.extend(hex.iter().map(String::as_str));
    args.extend(["--", path]);
    let mut cmd = cli.cmd(Kind::Read, &args);
    cmd.env("GIT_LITERAL_PATHSPECS", "1");
    let out = cli.run(cmd, None, &mut |_| {})?;
    Ok(String::from_utf8_lossy(&out).lines().filter_map(|l| CommitId::from_hex(l.trim())).collect())
}
