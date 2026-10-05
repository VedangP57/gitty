//! Compare to a branch (spec §11.2): the commits on each side and their merge base.

use crate::git_cli::{GitCli, Kind};
use crate::types::CommitId;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Compare {
    /// In `other`, not in HEAD (newest first, topological).
    pub behind: Vec<CommitId>,
    /// In HEAD, not in `other`.
    pub ahead: Vec<CommitId>,
    /// None for unrelated histories.
    pub merge_base: Option<CommitId>,
}

pub fn compare(cli: &GitCli, head: CommitId, other: CommitId) -> anyhow::Result<Compare> {
    let range = format!("{head}...{other}");
    let out = cli.run(cli.cmd(Kind::Read, &["rev-list", "--left-right", "--topo-order", &range, "--"]), None, &mut |_| {})?;
    let mut c = Compare::default();
    for line in String::from_utf8_lossy(&out).lines() {
        let (side, hex) = line.split_at(line.len().min(1));
        let Some(id) = CommitId::from_hex(hex.trim()) else { continue };
        match side {
            "<" => c.ahead.push(id),
            ">" => c.behind.push(id),
            _ => {}
        }
    }
    // exit 1 with no output means no common ancestor
    let (h, o) = (head.to_string(), other.to_string());
    c.merge_base = cli.run(cli.cmd(Kind::Read, &["merge-base", &h, &o]), None, &mut |_| {}).ok().and_then(|b| CommitId::from_hex(String::from_utf8_lossy(&b).trim()));
    Ok(c)
}
