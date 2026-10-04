//! Working-tree status from `git status --porcelain=v2 -z` (spec §5.4).

use crate::commit_files::BlobId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Ordinary,
    Renamed,
    Copied,
    Unmerged,
    Untracked,
}

/// Index state of a file as shown by its checkbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Every change is in the index.
    Staged,
    /// Nothing is in the index.
    Unstaged,
    /// Some changes are staged, some are not.
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    /// Source path of a staged rename or copy.
    pub orig_path: Option<String>,
    /// Index status (`.` unchanged, `M`, `A`, `D`, `R`, `C`, `T`, `U`; `?` untracked).
    pub x: char,
    /// Worktree status, same letters.
    pub y: char,
    pub kind: EntryKind,
    pub head_mode: u32,
    pub index_mode: u32,
    pub wt_mode: u32,
    pub head_blob: Option<BlobId>,
    pub index_blob: Option<BlobId>,
}

impl StatusEntry {
    pub fn check(&self) -> Check {
        match self.kind {
            EntryKind::Untracked | EntryKind::Unmerged => Check::Unstaged,
            _ if self.x == '.' => Check::Unstaged,
            _ if self.y == '.' => Check::Staged,
            _ => Check::Partial,
        }
    }
    pub fn is_conflicted(&self) -> bool {
        self.kind == EntryKind::Unmerged
    }
    /// The letter shown in the file list: the worktree side wins, as the diff shown is HEAD →
    /// worktree.
    pub fn letter(&self) -> char {
        match self.kind {
            EntryKind::Untracked => 'A',
            EntryKind::Unmerged => 'U',
            EntryKind::Renamed => 'R',
            EntryKind::Copied => 'C',
            EntryKind::Ordinary => match (self.x, self.y) {
                (_, 'D') | ('D', _) => 'D',
                ('A', _) => 'A',
                (_, 'T') | ('T', _) => 'T',
                _ => 'M',
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Current branch; None when detached.
    pub branch: Option<String>,
    /// HEAD commit; None on an unborn branch.
    pub head: Option<String>,
    pub entries: Vec<StatusEntry>,
}

fn blob(hex: &str) -> Option<BlobId> {
    BlobId::from_hex(hex)
}

fn mode(s: &str) -> u32 {
    u32::from_str_radix(s, 8).unwrap_or(0)
}

/// Parses `git status --porcelain=v2 -z --branch` output. Unknown records are skipped.
pub fn parse(out: &[u8]) -> Status {
    let mut st = Status::default();
    let mut recs = out.split(|&b| b == 0).map(|r| String::from_utf8_lossy(r).into_owned());
    while let Some(r) = recs.next() {
        let xy = |f: &str| {
            let mut c = f.chars();
            (c.next().unwrap_or('.'), c.next().unwrap_or('.'))
        };
        if let Some(h) = r.strip_prefix("# ") {
            if let Some(b) = h.strip_prefix("branch.head ") {
                st.branch = (b != "(detached)").then(|| b.to_string());
            } else if let Some(o) = h.strip_prefix("branch.oid ") {
                st.head = (o != "(initial)").then(|| o.to_string());
            }
        } else if let Some(rest) = r.strip_prefix("1 ") {
            let f: Vec<&str> = rest.splitn(8, ' ').collect();
            if f.len() < 8 {
                continue;
            }
            let (x, y) = xy(f[0]);
            st.entries.push(StatusEntry {
                path: f[7].to_string(),
                orig_path: None,
                x,
                y,
                kind: EntryKind::Ordinary,
                head_mode: mode(f[2]),
                index_mode: mode(f[3]),
                wt_mode: mode(f[4]),
                head_blob: blob(f[5]),
                index_blob: blob(f[6]),
            });
        } else if let Some(rest) = r.strip_prefix("2 ") {
            let f: Vec<&str> = rest.splitn(9, ' ').collect();
            let orig = recs.next();
            if f.len() < 9 {
                continue;
            }
            let (x, y) = xy(f[0]);
            st.entries.push(StatusEntry {
                path: f[8].to_string(),
                orig_path: orig,
                x,
                y,
                kind: if f[7].starts_with('C') { EntryKind::Copied } else { EntryKind::Renamed },
                head_mode: mode(f[2]),
                index_mode: mode(f[3]),
                wt_mode: mode(f[4]),
                head_blob: blob(f[5]),
                index_blob: blob(f[6]),
            });
        } else if let Some(rest) = r.strip_prefix("u ") {
            // XY sub m1 m2 m3 mW h1 h2 h3 path
            let f: Vec<&str> = rest.splitn(10, ' ').collect();
            if f.len() < 10 {
                continue;
            }
            let (x, y) = xy(f[0]);
            st.entries.push(StatusEntry {
                path: f[9].to_string(),
                orig_path: None,
                x,
                y,
                kind: EntryKind::Unmerged,
                head_mode: mode(f[2]),
                index_mode: mode(f[3]),
                wt_mode: mode(f[5]),
                head_blob: blob(f[6]),
                index_blob: blob(f[7]),
            });
        } else if let Some(p) = r.strip_prefix("? ") {
            st.entries.push(StatusEntry {
                path: p.to_string(),
                orig_path: None,
                x: '?',
                y: '?',
                kind: EntryKind::Untracked,
                head_mode: 0,
                index_mode: 0,
                wt_mode: 0o100644,
                head_blob: None,
                index_blob: None,
            });
        }
    }
    st.entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "c1b0730e0133447badcfd47fd144e254807b06e1";
    const H2: &str = "78981922613b2afb6025042ff6bd878ac1994e85";

    fn z(recs: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        for r in recs {
            v.extend_from_slice(r.as_bytes());
            v.push(0);
        }
        v
    }

    #[test]
    fn parses_every_record_kind_and_sorts() {
        let out = z(&[
            "# branch.oid 954af7ad394db009c21ebb83fb1ca55e9d536223",
            "# branch.head main",
            &format!("2 R. N... 100644 100644 100644 {H1} {H1} R100 new name.txt"),
            "old name.txt",
            &format!("1 .M N... 100644 100644 100755 {H2} {H2} sp ace.txt"),
            &format!("u UU N... 100644 100644 100644 100644 {H1} {H2} {H1} conflict.c"),
            "? dir/untracked\nwith newline",
            &format!("1 A. N... 000000 100644 100644 {} {H2} added", "0".repeat(40)),
        ]);
        let st = parse(&out);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert!(st.head.is_some());
        let paths: Vec<&str> = st.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["added", "conflict.c", "dir/untracked\nwith newline", "new name.txt", "sp ace.txt"]);
        let e = &st.entries[3];
        assert_eq!((e.kind, e.orig_path.as_deref(), e.x, e.y), (EntryKind::Renamed, Some("old name.txt"), 'R', '.'));
        assert_eq!(e.check(), Check::Staged);
        let e = &st.entries[4];
        assert_eq!((e.check(), e.wt_mode, e.head_blob.is_some()), (Check::Unstaged, 0o100755, true));
        assert_eq!(st.entries[0].head_blob, None, "zero oid means absent");
        assert_eq!(st.entries[0].letter(), 'A');
        assert!(st.entries[1].is_conflicted());
        assert_eq!(st.entries[2].kind, EntryKind::Untracked);
    }

    #[test]
    fn unborn_and_detached_heads() {
        let st = parse(&z(&["# branch.oid (initial)", "# branch.head main"]));
        assert_eq!((st.head, st.branch.as_deref()), (None, Some("main")));
        let st = parse(&z(&[&format!("# branch.oid {H1}"), "# branch.head (detached)"]));
        assert_eq!(st.branch, None);
    }

    #[test]
    fn partial_check() {
        let out = z(&[&format!("1 MM N... 100644 100644 100644 {H1} {H2} f")]);
        assert_eq!(parse(&out).entries[0].check(), Check::Partial);
    }

    #[test]
    fn truncated_records_are_skipped() {
        let st = parse(&z(&["1 .M N... 100644", "2 R.", "u UU", "garbage"]));
        assert!(st.entries.is_empty());
    }
}
