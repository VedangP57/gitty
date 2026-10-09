//! Conflict markers in a file, and resolving one block by keeping a side (spec: conflict help).
//!
//! The parser and the slicing resolver follow the logic of the editor druk's `conflicts.ts`
//! (https://github.com/letstri/druk, MIT, Copyright (c) Valerii Strilets): marker lines are
//! recognised by their exact form, and a block is resolved by cutting the original text at line
//! offsets, so line endings and a missing final newline come out as they went in.

use std::ops::Range;
use std::path::Path;

use anyhow::{Context, bail};

use crate::commit_files::BlobId;
use crate::files::{MAX_VIEW_BYTES, is_secret, no_symlinks, relative};
use crate::git_cli::{GitCli, Kind};
use crate::op_state::RepoOp;

const OPEN: &str = "<<<<<<<";
const BASE: &str = "|||||||";
const SEP: &str = "=======";
const CLOSE: &str = ">>>>>>>";

/// One conflict block. Lines are 0-based and count like [`crate::diff::text::Text`] does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The `<<<<<<<` line and the `>>>>>>>` line.
    pub start_line: usize,
    pub end_line: usize,
    /// Content lines of each side (the marker lines are not in them). `base` exists in diff3 style.
    pub ours: Range<usize>,
    pub base: Option<Range<usize>>,
    pub theirs: Range<usize>,
    /// The text after `<<<<<<<` and `>>>>>>>` (git writes `HEAD` and a branch or commit name).
    pub ours_label: String,
    pub theirs_label: String,
}

impl Conflict {
    /// The `|||||||` line, when the block has a base.
    pub fn base_line(&self) -> Option<usize> {
        self.base.as_ref().map(|b| b.start - 1)
    }
    /// The `=======` line.
    pub fn sep_line(&self) -> usize {
        self.theirs.start - 1
    }
}

/// Which side of a block to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Ours,
    Theirs,
    /// Ours, then theirs.
    Both,
}

/// `<<<<<<<` and its kin: exactly seven characters, then the end of the line or a space and a label.
fn marker<'a>(line: &'a str, m: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(m)?;
    match rest.as_bytes().first() {
        None => Some(""),
        Some(b' ') => Some(&rest[1..]),
        _ => None,
    }
}

/// A line without its terminator (`\n` or `\r\n`).
fn content(raw: &str) -> &str {
    let l = raw.strip_suffix('\n').unwrap_or(raw);
    l.strip_suffix('\r').unwrap_or(l)
}

struct Open {
    start: usize,
    label: String,
    base: Option<usize>,
    sep: Option<usize>,
}

/// The conflict blocks of `text`, in order. Strict: a block needs its `=======` and its closing
/// `>>>>>>>`; a second `<<<<<<<` starts over; `|||||||` and `=======` count once per block and
/// outside a block they are text (a Markdown heading underline, say).
pub fn parse(text: &str) -> Vec<Conflict> {
    let mut out = Vec::new();
    if !text.contains(OPEN) {
        return out;
    }
    let mut open: Option<Open> = None;
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        let line = content(raw);
        // every marker starts with one of four ASCII characters: most lines are skipped here
        if !matches!(line.as_bytes().first(), Some(b'<' | b'|' | b'=' | b'>')) {
            continue;
        }
        if let Some(label) = marker(line, OPEN) {
            open = Some(Open { start: i, label: label.to_string(), base: None, sep: None });
        } else if let Some(o) = open.as_mut() {
            if marker(line, BASE).is_some() {
                if o.base.is_none() && o.sep.is_none() {
                    o.base = Some(i);
                }
            } else if line == SEP {
                o.sep.get_or_insert(i);
            } else if let (Some(label), Some(sep)) = (marker(line, CLOSE), o.sep) {
                let o = open.take().expect("matched above");
                out.push(Conflict {
                    start_line: o.start,
                    end_line: i,
                    ours: o.start + 1..o.base.unwrap_or(sep),
                    base: o.base.map(|b| b + 1..sep),
                    theirs: sep + 1..i,
                    ours_label: o.label,
                    theirs_label: label.to_string(),
                });
            }
        }
    }
    out
}

/// Byte offset of the start of each line, and of the end of the text.
fn line_starts(text: &str) -> Vec<usize> {
    let mut v: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    // a final newline does not begin a line
    if v.last() == Some(&text.len()) && v.len() > 1 {
        v.pop();
    }
    v.push(text.len());
    v
}

/// `text` with the block resolved to `choice`. None when the text is not what the block was
/// parsed from (its marker lines are not where the block says).
pub fn resolve(text: &str, c: &Conflict, choice: Choice) -> Option<String> {
    let starts = line_starts(text);
    let lines = starts.len() - 1;
    let line = |i: usize| (i < lines).then(|| &text[starts[i]..starts[i + 1]]);
    if !(line(c.start_line).is_some_and(|l| marker(content(l), OPEN).is_some()) && line(c.end_line).is_some_and(|l| marker(content(l), CLOSE).is_some()) && line(c.sep_line()).is_some_and(|l| content(l) == SEP)) {
        return None;
    }
    let side = |r: &Range<usize>| &text[starts[r.start]..starts[r.end]];
    let mut kept = String::new();
    if matches!(choice, Choice::Ours | Choice::Both) {
        kept.push_str(side(&c.ours));
    }
    if matches!(choice, Choice::Theirs | Choice::Both) {
        kept.push_str(side(&c.theirs));
    }
    // a closing marker with no newline ends the file: what replaces it does not gain one
    if !text[starts[c.end_line]..starts[c.end_line + 1]].ends_with('\n') {
        let cut = kept.strip_suffix('\n').map_or(kept.len(), |k| k.strip_suffix('\r').unwrap_or(k).len());
        kept.truncate(cut);
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..starts[c.start_line]]);
    out.push_str(&kept);
    out.push_str(&text[starts[c.end_line + 1]..]);
    Some(out)
}

/// What a conflicted file in the work tree holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// UTF-8 text, with its conflict blocks (none when the markers are gone from it).
    Text { bytes: Vec<u8>, conflicts: Vec<Conflict> },
    /// Not shown or resolved here, and why (binary, too large, a symlink, a secret…).
    Other(String),
}

/// Reads `rel` of the work tree at `root`, with the limits of the Files viewer, and parses it.
pub fn read(root: &Path, rel: &Path) -> anyhow::Result<Loaded> {
    use crate::files::{FileContent as C, read_file};
    Ok(match read_file(root, rel, false)? {
        C::Text(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) => {
                let conflicts = parse(text);
                Loaded::Text { bytes, conflicts }
            }
            Err(_) => Loaded::Other("The file is not UTF-8 text: open it in your editor".into()),
        },
        C::Binary { .. } => Loaded::Other("This is a binary file: it cannot be merged line by line".into()),
        C::TooLarge { size } => Loaded::Other(format!("The file is too large to resolve here ({:.1} MiB): open it in your editor", size as f64 / 1_048_576.0)),
        C::Lfs { .. } => Loaded::Other("This is an LFS pointer: it cannot be merged line by line".into()),
        C::Symlink { target } => Loaded::Other(format!("This is a symlink to {}: it cannot be merged line by line", target.display())),
        C::Special => Loaded::Other("This is not a regular file".into()),
        C::Masked => Loaded::Other("Hidden: this looks like a secret file. Open it in your editor".into()),
    })
}

/// Writes `bytes` over `rel` of the work tree at `root`, if the file still holds exactly what
/// hashes to `expect` (what the caller built `bytes` from). The write is a temporary file in the
/// same directory, renamed over the original with its permissions. Refused: a symlink, a path
/// through one or outside the tree, a secret file, a file over [`MAX_VIEW_BYTES`], a file changed since.
pub fn write_resolved(root: &Path, rel: &Path, expect: BlobId, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    relative(rel)?;
    no_symlinks(root, rel, false)?;
    let shown = rel.display();
    if is_secret(rel) {
        bail!("{shown} looks like a secret file: open it in your editor");
    }
    let abs = root.join(rel);
    let meta = std::fs::symlink_metadata(&abs).with_context(|| format!("reading {shown}"))?;
    if meta.file_type().is_symlink() {
        bail!("{shown} is a symlink: open it in your editor");
    }
    if !meta.file_type().is_file() {
        bail!("{shown} is not a regular file");
    }
    if meta.len() > MAX_VIEW_BYTES || bytes.len() as u64 > MAX_VIEW_BYTES {
        bail!("{shown} is too large to resolve here: open it in your editor");
    }
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(&abs).with_context(|| format!("reading {shown}"))?;
    let mut now = Vec::new();
    file.take(MAX_VIEW_BYTES + 1).read_to_end(&mut now).with_context(|| format!("reading {shown}"))?;
    if BlobId::hash_of(&now) != expect {
        bail!("The file changed on disk; reloaded");
    }
    let dir = abs.parent().context("no parent directory")?;
    let name = abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut tmp = None;
    for k in 0..100 {
        let p = dir.join(format!(".{name}.gitty-{}-{k}", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p) {
            Ok(f) => {
                tmp = Some((p, f));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("writing next to {shown}")),
        }
    }
    let (tmp, mut f) = tmp.context("no free temporary name")?;
    let written = f.write_all(bytes).and_then(|()| f.set_permissions(meta.permissions())).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, &abs)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {shown}"));
    }
    Ok(())
}

/// One side of a conflict as the user knows it: a role and the name of the branch or commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideName {
    pub title: &'static str,
    /// Empty when unknown.
    pub name: String,
}

impl SideName {
    /// `Current (main)`, or just the title.
    pub fn label(&self) -> String {
        if self.name.is_empty() { self.title.to_string() } else { format!("{} ({})", self.title, self.name) }
    }
}

/// Who "ours" and "theirs" are. Git's words are about the merge machinery, not about the user:
/// in a merge, ours is the checked-out branch and theirs the branch merged in. A rebase or a
/// cherry-pick replays commits on top of another base, so ours is the branch being built (the
/// base the commits land on) and theirs is the commit being replayed (the user's own work, in a
/// rebase); a revert's theirs is the change being undone. Marker blocks and `checkout --ours`
/// both follow git's words, so the labels below name the roles git gives each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sides {
    pub ours: SideName,
    pub theirs: SideName,
}

impl Sides {
    /// No operation to ask (a stash pop, say): the plain words, with the markers' own labels.
    pub fn generic() -> Sides {
        Sides { ours: SideName { title: "Current", name: String::new() }, theirs: SideName { title: "Incoming", name: String::new() } }
    }
}

impl GitCli {
    /// The sides of the conflicts of the operation in progress, read from the git dir now.
    pub fn conflict_sides(&self) -> Sides {
        let Some(state) = self.read_op(true) else { return Sides::generic() };
        let short = |id: String| id.chars().take(7).collect::<String>();
        let current = self.current_branch().or_else(|| self.head_id().map(short)).unwrap_or_default();
        let subject = |id: &str| {
            let hex = id.len() >= 7 && id.bytes().all(|b| b.is_ascii_hexdigit());
            let out = hex.then(|| self.quiet(Kind::Read, &["log", "-1", "--format=%s", id], None).ok()).flatten();
            out.map(|o| String::from_utf8_lossy(&o).trim().to_string()).unwrap_or_default()
        };
        let side = |title, name| SideName { title, name };
        match state.op {
            RepoOp::Merge => Sides { ours: side("Current", current), theirs: side("Incoming", state.detail) },
            RepoOp::Rebase => {
                let onto = ["rebase-merge/onto", "rebase-apply/onto"].iter().find_map(|f| self.git_file(f)).map(|id| self.merge_name(&id, &Default::default())).unwrap_or_default();
                let replayed = ["REBASE_HEAD", "rebase-merge/stopped-sha", "rebase-apply/original-commit"].iter().find_map(|f| self.git_file(f)).map(|id| subject(&id)).unwrap_or_default();
                Sides { ours: side("Base branch", onto), theirs: side("Your commit", replayed) }
            }
            RepoOp::CherryPick => Sides { ours: side("Current branch", current), theirs: side("Picked commit", subject(&state.id)) },
            RepoOp::Revert => Sides { ours: side("Current branch", current), theirs: side("Reverted change", subject(&state.id)) },
        }
    }

    /// The first line of a file of the git dir (`rev-parse --git-path`, so a worktree's own).
    fn git_file(&self, name: &str) -> Option<String> {
        let out = self.quiet(Kind::Read, &["rev-parse", "--git-path", name], None).ok()?;
        let path = self.dir().join(String::from_utf8_lossy(&out).trim());
        let text = std::fs::read_to_string(path).ok()?;
        text.lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
    }

    /// The index stages (1 base, 2 ours, 3 theirs) `path` is unmerged in; empty when it is not.
    pub fn unmerged_stages(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let out = self.quiet(Kind::Read, &["--literal-pathspecs", "ls-files", "-u", "-z", "--", path], None)?;
        // `<mode> <object> <stage>\t<path>`
        let stages = out.split(|&b| b == 0).filter_map(|r| r.splitn(2, |&b| b == b'\t').next()).filter_map(|h| h.last().filter(|b| b.is_ascii_digit()).map(|b| b - b'0'));
        Ok(stages.collect())
    }

    /// Settles a conflict that has no markers to resolve (a binary file, a file one side
    /// deleted) for the whole file: takes `--ours` or `--theirs` and stages the result, or with
    /// `delete` (the side has no such file) removes the path. `delete` is what the caller asked
    /// the user about: if the index no longer says so, nothing is done.
    pub fn take_side(&self, path: &str, theirs: bool, delete: bool) -> anyhow::Result<()> {
        let stages = self.unmerged_stages(path)?;
        if stages.is_empty() {
            anyhow::bail!("{path} is not conflicted any more");
        }
        if stages.contains(&if theirs { 3 } else { 2 }) == delete {
            anyhow::bail!("{path} changed in the index since it was shown; look again");
        }
        if delete {
            return self.quiet(Kind::Write, &["--literal-pathspecs", "rm", "-q", "--", path], None).map(|_| ());
        }
        self.quiet(Kind::Write, &["--literal-pathspecs", "checkout", if theirs { "--theirs" } else { "--ours" }, "--", path], None)?;
        self.quiet(Kind::Write, &["--literal-pathspecs", "add", "--", path], None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> Conflict {
        let mut v = parse(text);
        assert_eq!(v.len(), 1, "{text:?}");
        v.remove(0)
    }

    const BASIC: &str = "a\n<<<<<<< HEAD\nour\n=======\ntheir\n>>>>>>> topic\nz\n";

    #[test]
    fn parses_a_block_and_its_labels() {
        let c = one(BASIC);
        assert_eq!((c.start_line, c.end_line, c.ours.clone(), c.base.clone(), c.theirs.clone()), (1, 5, 2..3, None, 4..5));
        assert_eq!((c.ours_label.as_str(), c.theirs_label.as_str()), ("HEAD", "topic"));
        assert_eq!(c.sep_line(), 3);
    }

    #[test]
    fn resolves_each_choice() {
        let c = one(BASIC);
        assert_eq!(resolve(BASIC, &c, Choice::Ours).unwrap(), "a\nour\nz\n");
        assert_eq!(resolve(BASIC, &c, Choice::Theirs).unwrap(), "a\ntheir\nz\n");
        assert_eq!(resolve(BASIC, &c, Choice::Both).unwrap(), "a\nour\ntheir\nz\n");
    }

    #[test]
    fn diff3_keeps_the_base_out_of_every_choice() {
        let t = "<<<<<<< HEAD\nours\n||||||| merged common ancestors\nbase\n=======\ntheirs\n>>>>>>> topic\n";
        let c = one(t);
        assert_eq!((c.ours.clone(), c.base.clone(), c.theirs.clone()), (1..2, Some(3..4), 5..6));
        assert_eq!(c.base_line(), Some(2));
        assert_eq!(resolve(t, &c, Choice::Both).unwrap(), "ours\ntheirs\n");
        assert_eq!(resolve(t, &c, Choice::Theirs).unwrap(), "theirs\n");
    }

    #[test]
    fn marker_lines_must_be_exact_and_at_column_zero() {
        for t in [
            " <<<<<<< HEAD\nx\n=======\ny\n>>>>>>> t\n",
            "<<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> t\n",
            "<<<<<<<HEAD\nx\n=======\ny\n>>>>>>> t\n",
            "<<<<<<< HEAD\nx\n======\ny\n>>>>>>> t\n",
            "<<<<<<< HEAD\nx\n========\ny\n>>>>>>> t\n",
            "<<<<<<< HEAD\nx\n=======\ny\n>>>>>> t\n",
            "<<<<<<< HEAD\nx\n=======\ny\n>>>>>>>t\n",
        ] {
            assert!(parse(t).is_empty(), "{t:?}");
        }
        // no label is fine
        assert_eq!(parse("<<<<<<<\nx\n=======\ny\n>>>>>>>\n").len(), 1);
    }

    #[test]
    fn unterminated_and_separatorless_blocks_do_not_count() {
        assert!(parse("<<<<<<< HEAD\nx\n=======\ny\n").is_empty());
        assert!(parse("<<<<<<< HEAD\nx\ny\n>>>>>>> t\n").is_empty());
        assert!(parse("<<<<<<< HEAD\n").is_empty());
        assert!(parse("=======\n>>>>>>> t\n").is_empty());
    }

    #[test]
    fn a_new_open_marker_restarts_the_block() {
        let t = "<<<<<<< one\nx\n<<<<<<< two\nours\n=======\ntheirs\n>>>>>>> t\n";
        let c = one(t);
        assert_eq!((c.start_line, c.ours_label.as_str(), c.ours.clone()), (2, "two", 3..4));
    }

    #[test]
    fn only_the_first_separator_and_base_count() {
        let t = "<<<<<<< A\no\n=======\nt1\n=======\nt2\n>>>>>>> B\n";
        let c = one(t);
        assert_eq!((c.ours.clone(), c.theirs.clone()), (1..2, 3..6));
        // a base marker after the separator is text
        let t = "<<<<<<< A\no\n=======\nt1\n||||||| x\nt2\n>>>>>>> B\n";
        let c = one(t);
        assert_eq!((c.base.clone(), c.theirs.clone()), (None, 3..6));
        let t = "<<<<<<< A\no\n||||||| x\nb\n||||||| y\nc\n=======\nt\n>>>>>>> B\n";
        assert_eq!(one(t).base, Some(3..6));
    }

    #[test]
    fn markdown_underlines_are_not_markers() {
        let readme = "Title\n=======\n\ntext\n\nSub\n=======\n";
        assert!(parse(readme).is_empty());
        let t = format!("{readme}<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> t\nAfter\n=======\n");
        let c = one(&t);
        assert_eq!((c.start_line, c.end_line), (7, 11));
        assert_eq!(resolve(&t, &c, Choice::Ours).unwrap(), format!("{readme}x\nAfter\n=======\n"));
        // a heading underline inside a block is the separator, once; the real one is content
        let t = "<<<<<<< HEAD\nTitle\n=======\nOther\n=======\n>>>>>>> t\n";
        let c = one(t);
        assert_eq!((c.ours.clone(), c.theirs.clone()), (1..2, 3..5));
    }

    #[test]
    fn crlf_and_mixed_endings_survive() {
        let t = "a\r\n<<<<<<< HEAD\r\nour\r\n=======\r\ntheir\n>>>>>>> topic\r\nz";
        let c = one(t);
        assert_eq!((c.ours_label.as_str(), c.theirs_label.as_str()), ("HEAD", "topic"));
        assert_eq!(resolve(t, &c, Choice::Ours).unwrap(), "a\r\nour\r\nz");
        assert_eq!(resolve(t, &c, Choice::Theirs).unwrap(), "a\r\ntheir\nz");
        assert_eq!(resolve(t, &c, Choice::Both).unwrap(), "a\r\nour\r\ntheir\nz");
    }

    #[test]
    fn a_missing_final_newline_stays_missing() {
        let t = "a\n<<<<<<< HEAD\nour\n=======\ntheir\n>>>>>>> topic";
        let c = one(t);
        assert_eq!(resolve(t, &c, Choice::Ours).unwrap(), "a\nour");
        assert_eq!(resolve(t, &c, Choice::Theirs).unwrap(), "a\ntheir");
        assert_eq!(resolve(t, &c, Choice::Both).unwrap(), "a\nour\ntheir");
        let crlf = "<<<<<<< HEAD\r\nour\r\n=======\r\ntheir\r\n>>>>>>> topic";
        assert_eq!(resolve(crlf, &one(crlf), Choice::Both).unwrap(), "our\r\ntheir");
        // an empty side leaves nothing behind
        let e = "x\n<<<<<<< HEAD\n=======\n>>>>>>> topic";
        assert_eq!(resolve(e, &one(e), Choice::Ours).unwrap(), "x\n");
    }

    #[test]
    fn empty_sides() {
        let t = "<<<<<<< HEAD\n=======\ntheir\n>>>>>>> t\n";
        let c = one(t);
        assert_eq!(c.ours, 1..1);
        assert_eq!(resolve(t, &c, Choice::Ours).unwrap(), "");
        assert_eq!(resolve(t, &c, Choice::Both).unwrap(), "their\n");
        let t = "<<<<<<< HEAD\nour\n=======\n>>>>>>> t\n";
        assert_eq!(resolve(t, &one(t), Choice::Theirs).unwrap(), "");
    }

    #[test]
    fn adjacent_blocks_resolve_one_at_a_time() {
        let t = "<<<<<<< A\n1\n=======\n2\n>>>>>>> B\n<<<<<<< A\n3\n=======\n4\n>>>>>>> B\n";
        let cs = parse(t);
        assert_eq!(cs.len(), 2);
        let after = resolve(t, &cs[0], Choice::Theirs).unwrap();
        assert_eq!(after, "2\n<<<<<<< A\n3\n=======\n4\n>>>>>>> B\n");
        let again = parse(&after);
        assert_eq!(again.len(), 1);
        assert_eq!(resolve(&after, &again[0], Choice::Ours).unwrap(), "2\n3\n");
        // the stale block no longer matches the text it was parsed from
        assert_eq!(resolve(&after, &cs[1], Choice::Ours), None);
    }

    #[test]
    fn fast_path_and_a_huge_file() {
        assert!(parse("no markers here\n=======\n").is_empty());
        let mut big = String::with_capacity(2 << 20);
        let mut n = 0;
        while big.len() < 2 << 20 {
            big.push_str(&format!("line {n} of ordinary text, nothing special\n"));
            if n % 5000 == 0 {
                big.push_str("<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> topic\n");
            }
            n += 1;
        }
        let t = std::time::Instant::now();
        let cs = parse(&big);
        let r = resolve(&big, &cs[0], Choice::Both).unwrap();
        assert!(t.elapsed() < std::time::Duration::from_millis(500), "{:?}", t.elapsed());
        assert!(cs.len() >= 5);
        assert_eq!(parse(&r).len(), cs.len() - 1);
    }

    #[test]
    fn write_is_atomic_checked_and_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let p = root.join("f.txt");
        std::fs::write(&p, BASIC).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rel = Path::new("f.txt");
        let stale = BlobId::hash_of(b"something else");
        assert!(write_resolved(root, rel, stale, b"x").unwrap_err().to_string().contains("changed on disk"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), BASIC);
        write_resolved(root, rel, BlobId::hash_of(BASIC.as_bytes()), b"done\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "done\n");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        let leftovers: Vec<_> = std::fs::read_dir(root).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    }

    #[test]
    fn write_refuses_symlinks_big_files_secrets_and_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "keep").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let ok = BlobId::hash_of(b"keep");
        assert!(write_resolved(&root, Path::new("link"), ok, b"x").unwrap_err().to_string().contains("symlink"));
        assert!(write_resolved(&root, Path::new("../outside.txt"), ok, b"x").is_err());
        std::fs::create_dir(root.join("d")).unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("d/up")).unwrap();
        assert!(write_resolved(&root, Path::new("d/up/outside.txt"), ok, b"x").is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        let big = vec![b'a'; MAX_VIEW_BYTES as usize + 1];
        std::fs::write(root.join("big"), &big).unwrap();
        assert!(write_resolved(&root, Path::new("big"), BlobId::hash_of(&big), b"x").unwrap_err().to_string().contains("too large"));
        std::fs::write(root.join(".env"), "k=v").unwrap();
        assert!(write_resolved(&root, Path::new(".env"), BlobId::hash_of(b"k=v"), b"x").unwrap_err().to_string().contains("secret"));
    }
}
