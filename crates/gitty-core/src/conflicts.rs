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

/// How git writes the markers of a file: `conflict-marker-size` (`.gitattributes`) and whether it
/// adds the merge base (`merge.conflictStyle` diff3 or zdiff3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub size: usize,
    pub diff3: bool,
}

/// Git's default length of a marker.
pub const MARKER_SIZE: usize = 7;

impl Default for Style {
    fn default() -> Style {
        Style { size: MARKER_SIZE, diff3: false }
    }
}

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
    /// The length of the markers the block was parsed with.
    pub marker_size: usize,
    /// A line inside the block looks like a marker too (a second `=======` or `|||||||`, a bare
    /// `|||||||` outside diff3 style): where the sides split cannot be told, so the block is
    /// counted but never resolved from here.
    pub ambiguous: bool,
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

/// `<<<<<<<` and its kin: exactly `n` of `ch`, then the end of the line or a space and a label.
fn marker(line: &str, ch: u8, n: usize) -> Option<&str> {
    let b = line.as_bytes();
    if b.len() < n || !b[..n].iter().all(|&c| c == ch) {
        return None;
    }
    match b.get(n) {
        None => Some(""),
        Some(b' ') => Some(&line[n + 1..]),
        _ => None,
    }
}

fn is_sep(line: &str, n: usize) -> bool {
    line.len() == n && line.bytes().all(|c| c == b'=')
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
    ambiguous: bool,
}

/// The conflict blocks of `text`, in order. Strict: a block needs its `=======` and its closing
/// `>>>>>>>`; a second `<<<<<<<` starts over; `|||||||` and `=======` count once per block and
/// outside a block they are text (a Markdown heading underline, say). Inside a block a second
/// `=======`, a second `|||||||` or a bare `|||||||` outside diff3 style may be file content, so
/// the block is flagged [`Conflict::ambiguous`] (git always writes a label after the base marker).
pub fn parse(text: &str, style: Style) -> Vec<Conflict> {
    let mut out = Vec::new();
    let n = style.size;
    if !text.contains(&"<".repeat(n)) {
        return out;
    }
    let mut open: Option<Open> = None;
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        let line = content(raw);
        // every marker starts with one of four ASCII characters: most lines are skipped here
        if !matches!(line.as_bytes().first(), Some(b'<' | b'|' | b'=' | b'>')) {
            continue;
        }
        if let Some(label) = marker(line, b'<', n) {
            open = Some(Open { start: i, label: label.to_string(), base: None, sep: None, ambiguous: false });
        } else if let Some(o) = open.as_mut() {
            if let Some(label) = marker(line, b'|', n) {
                // after the separator a base marker is the second side's text
                if o.sep.is_none() {
                    if o.base.is_some() || (!style.diff3 && label.is_empty()) {
                        o.ambiguous = true;
                    }
                    o.base.get_or_insert(i);
                }
            } else if is_sep(line, n) {
                if o.sep.is_some() {
                    o.ambiguous = true;
                }
                o.sep.get_or_insert(i);
            } else if let (Some(label), Some(sep)) = (marker(line, b'>', n), o.sep) {
                let o = open.take().expect("matched above");
                out.push(Conflict {
                    start_line: o.start,
                    end_line: i,
                    ours: o.start + 1..o.base.unwrap_or(sep),
                    base: o.base.map(|b| b + 1..sep),
                    theirs: sep + 1..i,
                    ours_label: o.label,
                    theirs_label: label.to_string(),
                    marker_size: n,
                    ambiguous: o.ambiguous,
                });
            }
        }
    }
    out
}

/// A line that starts with seven `<` or `>`: a conflict marker of some kind. Used when [`parse`]
/// found nothing, to tell "markers git wrote that are not understood" from "markers all gone"
/// (`=======` alone is no sign: Markdown underlines are made of it).
pub fn has_stray_markers(text: &str) -> bool {
    (text.contains("<<<<<<<") || text.contains(">>>>>>>")) && text.lines().any(|l| matches!(l.as_bytes(), [c @ (b'<' | b'>'), rest @ ..] if rest.len() >= 6 && rest[..6].iter().all(|b| b == c)))
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
/// parsed from (its marker lines are not where the block says), and for an ambiguous block.
pub fn resolve(text: &str, c: &Conflict, choice: Choice) -> Option<String> {
    if c.ambiguous {
        return None;
    }
    let n = c.marker_size;
    let starts = line_starts(text);
    let lines = starts.len() - 1;
    let line = |i: usize| (i < lines).then(|| &text[starts[i]..starts[i + 1]]);
    if !(line(c.start_line).is_some_and(|l| marker(content(l), b'<', n).is_some()) && line(c.end_line).is_some_and(|l| marker(content(l), b'>', n).is_some()) && line(c.sep_line()).is_some_and(|l| is_sep(content(l), n))) {
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
    /// UTF-8 text, with its conflict blocks. None parsed and `unknown`: it has marker lines
    /// that were not understood; none and not `unknown`: the markers are gone from it.
    Text { bytes: Vec<u8>, conflicts: Vec<Conflict>, unknown: bool },
    /// Not shown or resolved here, and why (binary, too large, a symlink, a secret…).
    Other(String),
}

/// Reads `rel` of the work tree at `root`, with the limits of the Files viewer, and parses it.
pub fn read(root: &Path, rel: &Path, style: Style) -> anyhow::Result<Loaded> {
    use crate::files::{FileContent as C, read_file};
    Ok(match read_file(root, rel, false)? {
        C::Text(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) => {
                let conflicts = parse(text, style);
                let unknown = conflicts.is_empty() && has_stray_markers(text);
                Loaded::Text { bytes, conflicts, unknown }
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

/// Removes the temporary files of [`write_resolved`] that a crashed process left next to `name`.
/// Best effort: a name that is not ours, or a process still alive, is left alone.
fn remove_stale_temps(dir: &Path, name: &str) {
    let prefix = format!(".{name}.gitty-");
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = file.strip_prefix(&prefix).and_then(|r| r.split('-').next()).and_then(|p| p.parse::<i32>().ok()) else { continue };
        // SAFETY: signal 0 only checks that the process exists
        let alive = pid == std::process::id() as i32 || unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Writes `bytes` over `rel` of the work tree at `root`, if the file still holds exactly what
/// hashes to `expect` (what the caller built `bytes` from). The write is a temporary file in the
/// same directory, renamed over the original with its permissions. Refused: a symlink, a path
/// through one or outside the tree, a secret file, a file over [`MAX_VIEW_BYTES`], a file changed since.
///
/// The check and the rename are not one atomic step: an editor that saves between them loses its
/// save to the rename. The window is the time to write one temporary file; the callers hold the
/// write lock, which keeps gitty's own writes out of it, nothing keeps other programs out.
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
    remove_stale_temps(dir, &name);
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
    // make the rename itself durable; the file is already complete, so a failure here is not one
    let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
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
/// rebase). A revert merges with the reverted commit as the base: its theirs is the commit's
/// parent, the code as it was *without* the change, so taking theirs undoes it. Marker blocks and
/// `checkout --ours` both follow git's words, so the labels below name the roles git gives each side.
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
            RepoOp::Revert => Sides { ours: side("Current branch", current), theirs: side("Without change", subject(&state.id)) },
        }
    }

    /// The first line of a file of the git dir (`rev-parse --git-path`, so a worktree's own).
    fn git_file(&self, name: &str) -> Option<String> {
        let out = self.quiet(Kind::Read, &["rev-parse", "--git-path", name], None).ok()?;
        let path = self.dir().join(String::from_utf8_lossy(&out).trim());
        let text = std::fs::read_to_string(path).ok()?;
        text.lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
    }

    /// How git wrote the markers of `path`: `conflict-marker-size` from `.gitattributes` (an
    /// unset, unspecified or invalid value is the default 7; the rest clamped to git's 3..=64)
    /// and whether `merge.conflictStyle` adds the base.
    pub fn conflict_style(&self, path: &str) -> Style {
        let size = self.quiet(Kind::Read, &["check-attr", "-z", "conflict-marker-size", "--", path], None).ok().and_then(|o| {
            // `<path> NUL <attribute> NUL <value> NUL`
            let value = o.split(|&b| b == 0).nth(2)?;
            std::str::from_utf8(value).ok()?.parse::<usize>().ok()
        });
        let diff3 = self.quiet(Kind::Read, &["config", "--get", "merge.conflictStyle"], None).is_ok_and(|o| matches!(String::from_utf8_lossy(&o).trim().to_ascii_lowercase().as_str(), "diff3" | "zdiff3"));
        Style { size: size.map_or(MARKER_SIZE, |n| n.clamp(3, 64)), diff3 }
    }

    /// The index entries `path` is unmerged in (stage 1 base, 2 ours, 3 theirs); empty when it is not.
    pub fn unmerged_entries(&self, path: &str) -> anyhow::Result<Vec<Stage>> {
        let out = self.quiet(Kind::Read, &["--literal-pathspecs", "ls-files", "-u", "-z", "--", path], None)?;
        Ok(out.split(|&b| b == 0).filter_map(parse_stage).collect())
    }

    /// The index stages (1 base, 2 ours, 3 theirs) `path` is unmerged in; empty when it is not.
    pub fn unmerged_stages(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        Ok(self.unmerged_entries(path)?.into_iter().map(|e| e.stage).collect())
    }

    /// Every path the index holds unmerged, each once.
    pub fn unmerged_paths(&self) -> anyhow::Result<Vec<String>> {
        let out = self.quiet(Kind::Read, &["ls-files", "-u", "-z"], None)?;
        let mut paths: Vec<String> = out.split(|&b| b == 0).filter_map(|r| r.splitn(2, |&b| b == b'\t').nth(1)).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
        paths.dedup();
        Ok(paths)
    }

    /// Settles a conflict that has no markers to resolve (a binary file, a file one side
    /// deleted) for the whole file: takes `--ours` or `--theirs` and stages the result, or with
    /// `delete` (the side has no such file) removes the path. `delete` is what the caller asked
    /// the user about: if the index no longer says so, nothing is done. `backup` runs once that
    /// is checked and before anything changes (a copy of the file for the Trash).
    ///
    /// A submodule (a gitlink) is not checked out: `checkout --theirs` would leave the work tree
    /// alone and the `add` after it would stage the work tree's commit, ours. The chosen side's
    /// commit goes into the index directly, and a deletion leaves the work tree's directory.
    pub fn take_side(&self, path: &str, theirs: bool, delete: bool, backup: &mut dyn FnMut() -> anyhow::Result<()>) -> anyhow::Result<()> {
        let entries = self.unmerged_entries(path)?;
        if entries.is_empty() {
            anyhow::bail!("{path} is not conflicted any more");
        }
        let chosen = entries.iter().find(|e| e.stage == if theirs { 3 } else { 2 });
        if chosen.is_some() == delete {
            anyhow::bail!("{path} changed in the index since it was shown; look again");
        }
        let gitlink = entries.iter().any(|e| e.mode == 0o160000);
        backup()?;
        match (chosen, gitlink) {
            (None, true) => self.quiet(Kind::Write, &["--literal-pathspecs", "rm", "--cached", "-q", "--", path], None).map(|_| ()),
            (None, false) => self.quiet(Kind::Write, &["--literal-pathspecs", "rm", "-q", "--", path], None).map(|_| ()),
            (Some(c), true) => self.quiet(Kind::Write, &["update-index", "--add", "--cacheinfo", &format!("{:o},{},{path}", c.mode, c.sha)], None).map(|_| ()),
            (Some(_), false) => {
                self.quiet(Kind::Write, &["--literal-pathspecs", "checkout", if theirs { "--theirs" } else { "--ours" }, "--", path], None)?;
                self.quiet(Kind::Write, &["--literal-pathspecs", "add", "--", path], None).map(|_| ())
            }
        }
    }
}

/// One unmerged index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub stage: u8,
    pub mode: u32,
    pub sha: String,
}

/// `<mode> <object> <stage>\t<path>`
fn parse_stage(rec: &[u8]) -> Option<Stage> {
    let head = std::str::from_utf8(rec.splitn(2, |&b| b == b'\t').next()?).ok()?;
    let mut f = head.split(' ');
    let (mode, sha, stage) = (f.next()?, f.next()?, f.next()?);
    Some(Stage { stage: stage.parse().ok()?, mode: u32::from_str_radix(mode, 8).ok()?, sha: sha.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<Conflict> {
        super::parse(text, Style::default())
    }

    fn parse_diff3(text: &str) -> Vec<Conflict> {
        super::parse(text, Style { diff3: true, ..Style::default() })
    }

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
        let c = parse_diff3(t).remove(0);
        assert!(!c.ambiguous);
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
    fn only_the_first_separator_and_base_split_a_block_and_a_second_one_flags_it() {
        let t = "<<<<<<< A\no\n=======\nt1\n=======\nt2\n>>>>>>> B\n";
        let c = one(t);
        assert_eq!((c.ours.clone(), c.theirs.clone(), c.ambiguous), (1..2, 3..6, true));
        // a base marker after the separator is the second side's text
        let t = "<<<<<<< A\no\n=======\nt1\n||||||| x\nt2\n>>>>>>> B\n";
        let c = one(t);
        assert_eq!((c.base.clone(), c.theirs.clone(), c.ambiguous), (None, 3..6, false));
        let t = "<<<<<<< A\no\n||||||| x\nb\n||||||| y\nc\n=======\nt\n>>>>>>> B\n";
        let c = parse_diff3(t).remove(0);
        assert_eq!((c.base.clone(), c.ambiguous), (Some(3..6), true));
        // outside diff3 style git never writes a base: a bare base marker is the file's own line,
        // a labelled one is a base from `--conflict=diff3`
        let t = "<<<<<<< A\no\n|||||||\nb\n=======\nt\n>>>>>>> B\n";
        assert!(one(t).ambiguous && !parse_diff3(t)[0].ambiguous);
        let t = "<<<<<<< A\no\n||||||| anc\nb\n=======\nt\n>>>>>>> B\n";
        assert!(!one(t).ambiguous);
    }

    #[test]
    fn an_ambiguous_block_is_counted_but_no_choice_resolves_it() {
        // a setext underline under a seven-letter title is a line of the first side
        let t = "<<<<<<< HEAD\nHeading\n=======\nours\n=======\ntheirs\n>>>>>>> t\nafter\n";
        let c = one(t);
        assert!(c.ambiguous);
        for choice in [Choice::Ours, Choice::Theirs, Choice::Both] {
            assert_eq!(resolve(t, &c, choice), None);
        }
    }

    #[test]
    fn no_choice_of_a_clean_block_drops_a_line() {
        let ours = ["a", "=== not seven", "<<<<<< six", "||||||"];
        let theirs = ["b", ">>>>>> six", "== two"];
        let t = format!("pre\n<<<<<<< HEAD\n{}\n=======\n{}\n>>>>>>> t\npost\n", ours.join("\n"), theirs.join("\n"));
        let c = one(&t);
        assert!(!c.ambiguous);
        let all = |r: String| r.lines().map(str::to_string).collect::<Vec<_>>();
        let o = all(resolve(&t, &c, Choice::Ours).unwrap());
        let th = all(resolve(&t, &c, Choice::Theirs).unwrap());
        let both = all(resolve(&t, &c, Choice::Both).unwrap());
        assert_eq!(o.len(), 2 + ours.len());
        assert_eq!(th.len(), 2 + theirs.len());
        assert_eq!(both.len(), 2 + ours.len() + theirs.len());
    }

    #[test]
    fn marker_size_makes_markers_exactly_that_long() {
        let nine = |t: &str| super::parse(t, Style { size: 9, diff3: false });
        let t = "<<<<<<<<< HEAD\nours\n=========\ntheirs\n>>>>>>>>> topic\n";
        let c = nine(t).remove(0);
        assert_eq!((c.ours.clone(), c.theirs.clone(), c.marker_size), (1..2, 3..4, 9));
        assert_eq!(resolve(t, &c, Choice::Both).unwrap(), "ours\ntheirs\n");
        // sevens are text at size 9, and nines are not markers at size 7
        assert!(nine("<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> t\n").is_empty());
        assert!(parse(t).is_empty());
        // a size-9 block with a shorter line of `=` inside it is not ambiguous
        let t = "<<<<<<<<< HEAD\nTitle\n=======\n=========\nt\n>>>>>>>>> topic\n";
        let c = nine(t).remove(0);
        assert_eq!((c.ours.clone(), c.theirs.clone(), c.ambiguous), (1..3, 4..5, false));
    }

    #[test]
    fn stray_markers_are_told_from_markdown_underlines() {
        assert!(has_stray_markers("a\n<<<<<<<< HEAD\nb\n"));
        assert!(has_stray_markers(">>>>>>> x\n"));
        assert!(!has_stray_markers("Title\n=======\n"));
        assert!(!has_stray_markers("a <<<<<<< b\n"));
        assert!(!has_stray_markers("<<<<<< six\n"));
    }

    #[test]
    fn markdown_underlines_are_not_markers() {
        let readme = "Title\n=======\n\ntext\n\nSub\n=======\n";
        assert!(parse(readme).is_empty());
        let t = format!("{readme}<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> t\nAfter\n=======\n");
        let c = one(&t);
        assert_eq!((c.start_line, c.end_line), (7, 11));
        assert_eq!(resolve(&t, &c, Choice::Ours).unwrap(), format!("{readme}x\nAfter\n=======\n"));
        // inside a block an underline is the separator, once, and the block is flagged: the sides
        // cannot be told apart, so it is counted and left to the editor
        let t = "<<<<<<< HEAD\nTitle\n=======\nOther\n=======\n>>>>>>> t\n";
        let c = one(t);
        assert_eq!((c.ours.clone(), c.theirs.clone(), c.ambiguous), (1..2, 3..5, true));
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
        // what a crashed write left behind is cleaned up; a live process's temporary file is not
        let stale = root.join(".f.txt.gitty-2147483646-0");
        let live = root.join(format!(".f.txt.gitty-{}-77", std::process::id()));
        std::fs::write(&stale, "x").unwrap();
        std::fs::write(&live, "x").unwrap();
        write_resolved(root, rel, BlobId::hash_of(BASIC.as_bytes()), b"done\n").unwrap();
        assert!(!stale.exists() && live.exists());
        std::fs::remove_file(&live).unwrap();
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
