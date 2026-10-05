//! Line staging into the real index (spec §12.2).
//!
//! One primitive serves stage, unstage and partial states: the Changes tab shows the HEAD →
//! worktree diff and a staged flag per changed line. Any edit of those flags yields a desired
//! index text `build(HEAD, WT, flags)`, written to the index as a plain `index → desired` patch.
//! The worktree is compared in its git form (clean filters, autocrlf), so the patch applies to
//! the index blob exactly.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use crate::commit_files::BlobId;
use crate::diff::ops::{DiffAlgorithm, Op, WsMode, compute_ops};
use crate::diff::text::Text;
use crate::repo::Handle;
use crate::status::{EntryKind, StatusEntry};

/// The three versions of one file.
#[derive(Clone)]
pub struct Texts {
    pub head: Arc<Text>,
    pub index: Arc<Text>,
    /// The worktree file in git form.
    pub wt: Arc<Text>,
    /// The worktree file's raw bytes equal its git form (no conversion applies), so the
    /// worktree may be rewritten from these texts (line discard).
    pub wt_is_raw: bool,
    /// Git mode of the worktree file (0 when it is missing).
    pub wt_mode: u32,
    /// Blob id of the worktree file's git form.
    pub wt_blob: BlobId,
}

/// One changed line of a HEAD → worktree diff: a deleted HEAD line or an added worktree line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeLine {
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// Index of the change block in the ops' `Change` order.
    pub change: usize,
}

/// Changed lines in the order flags are kept: per change block, its deletions then additions.
pub fn change_lines(ops: &[Op]) -> Vec<ChangeLine> {
    let mut v = Vec::new();
    let mut change = 0;
    for op in ops {
        if let Op::Change { old, new } = op {
            v.extend(old.clone().map(|o| ChangeLine { old: Some(o), new: None, change }));
            v.extend(new.clone().map(|n| ChangeLine { old: None, new: Some(n), change }));
            change += 1;
        }
    }
    v
}

/// The text with the flagged changes of `head → wt` applied. Within a change block, unstaged
/// deletions stay, then staged additions follow. A line without a final newline that ends up
/// followed by another line gets one, so `a\nb` + `c` never becomes `a\nbc`.
pub fn build(head: &Text, wt: &Text, ops: &[Op], staged: &[bool]) -> Vec<u8> {
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut k = 0;
    for op in ops {
        match op {
            Op::Equal { old, len, .. } => lines.extend((*old..old + len).map(|i| head.raw_line(i))),
            Op::Change { old, new } => {
                for i in old.clone() {
                    if !staged.get(k).copied().unwrap_or(false) {
                        lines.push(head.raw_line(i));
                    }
                    k += 1;
                }
                for i in new.clone() {
                    if staged.get(k).copied().unwrap_or(false) {
                        lines.push(wt.raw_line(i));
                    }
                    k += 1;
                }
            }
        }
    }
    let mut out = Vec::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for (i, l) in lines.iter().enumerate() {
        out.extend_from_slice(l);
        if i + 1 < lines.len() && !l.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    out
}

fn ops_of(a: &Text, b: &Text) -> Vec<Op> {
    compute_ops(a, b, DiffAlgorithm::Myers, WsMode::Show)
}

/// Explored (item, index line) states before giving up and calling the file divergent.
const MAX_STATES: usize = 4_000_000;

/// Staged flags of `head_wt_ops`'s changed lines, derived from the index. None when the index
/// cannot be expressed as HEAD plus a subset of those changes (it holds content of its own).
///
/// The index must be the HEAD → worktree items in order, with every unchanged line present,
/// each deletion present unless staged, and each addition present only if staged. That is a
/// subsequence match, searched depth-first with a memo of failed states. Matching tolerates the
/// newline `build` adds after a no-EOL line.
pub fn staged_set(t: &Texts, head_wt_ops: &[Op]) -> Option<Vec<bool>> {
    #[derive(Clone, Copy, PartialEq)]
    enum K {
        Same,
        Del,
        Add,
    }
    let mut items: Vec<(K, &[u8])> = Vec::new();
    for op in head_wt_ops {
        match op {
            Op::Equal { old, len, .. } => items.extend((*old..old + len).map(|i| (K::Same, t.head.raw_line(i)))),
            Op::Change { old, new } => {
                items.extend(old.clone().map(|i| (K::Del, t.head.raw_line(i))));
                items.extend(new.clone().map(|i| (K::Add, t.wt.raw_line(i))));
            }
        }
    }
    let idx: Vec<&[u8]> = (0..t.index.len()).map(|i| t.index.raw_line(i)).collect();
    let (n, m) = (items.len(), idx.len());
    // exactly what `build` emits at index line `p`: a no-EOL line gains a newline unless last
    let eq = |a: &[u8], p: usize| {
        let b = idx[p];
        if a.ends_with(b"\n") || p + 1 == m {
            a == b
        } else {
            b.len() == a.len() + 1 && b.starts_with(a) && b.ends_with(b"\n")
        }
    };
    let mut failed = std::collections::HashSet::new();
    // (item, index line, alternatives tried): alternative 0 consumes an index line, 1 skips
    let mut stack: Vec<(usize, usize, u8)> = vec![(0, 0, 0)];
    let flags_of = |stack: &[(usize, usize, u8)]| -> Vec<bool> {
        stack
            .iter()
            .filter(|(i, ..)| *i < n && items[*i].0 != K::Same)
            .map(|&(i, _, alt)| match items[i].0 {
                // alt is one past the alternative taken: 1 = consumed, 2 = skipped
                K::Del => alt == 2,
                _ => alt == 1,
            })
            .collect()
    };
    loop {
        let &mut (i, p, ref mut alt) = stack.last_mut()?;
        if i == n {
            if p == m {
                // matching mirrors `build` line for line, so this holds; checked to stay safe
                let flags = flags_of(&stack);
                return (build(&t.head, &t.wt, head_wt_ops, &flags) == t.index.bytes()).then_some(flags);
            }
            stack.pop();
            continue;
        }
        let kind = items[i].0;
        let alts = if kind == K::Same { 1 } else { 2 };
        if *alt >= alts || failed.len() > MAX_STATES {
            failed.insert((i, p));
            stack.pop();
            if failed.len() > MAX_STATES {
                return None;
            }
            continue;
        }
        let this = *alt;
        *alt += 1;
        if this == 0 {
            if p < m && eq(items[i].1, p) && !failed.contains(&(i + 1, p + 1)) {
                stack.push((i + 1, p + 1, 0));
            }
        } else if !failed.contains(&(i + 1, p)) {
            stack.push((i + 1, p, 0));
        }
    }
}

/// What to run to make the index hold `build(head, wt, staged)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Nothing,
    /// Every change is staged: `git add -A -- paths`.
    StageFile(Vec<String>),
    /// No change is staged: `git restore --staged -- paths`.
    UnstageFile(Vec<String>),
    /// `git apply --cached` this patch, after checking the index entry is still `expect`.
    /// `target` is the blob the index must hold afterwards.
    Patch { patch: Vec<u8>, expect: Option<BlobId>, target: BlobId },
}

pub fn plan(e: &StatusEntry, t: &Texts, head_wt_ops: &[Op], staged: &[bool]) -> Plan {
    let target = build(&t.head, &t.wt, head_wt_ops, staged);
    let paths = || {
        let mut v = vec![e.path.clone()];
        v.extend(e.orig_path.clone());
        v
    };
    if target == t.index.bytes() {
        Plan::Nothing
    } else if target == t.wt.bytes() {
        Plan::StageFile(paths())
    } else if target == t.head.bytes() {
        Plan::UnstageFile(paths())
    } else {
        // the path is absent from the index for untracked files and staged deletions
        let create = (e.index_blob.is_none()).then(|| {
            let m = if e.kind == EntryKind::Untracked { t.wt_mode } else { e.head_mode.max(t.wt_mode) };
            if m == 0 { 0o100644 } else { m }
        });
        Plan::Patch { patch: unified_patch(&e.path, &t.index, &target, create), expect: e.index_blob, target: BlobId::hash_of(&target) }
    }
}

/// Git's C-style quoting for a header path (prefix included), only when needed.
pub fn quote_path(p: &str) -> String {
    if !p.bytes().any(|b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\') {
        return p.to_string();
    }
    let mut s = String::from("\"");
    for c in p.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\t' => s.push_str("\\t"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => s.push_str(&format!("\\{:03o}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

fn push_line(out: &mut Vec<u8>, prefix: u8, raw: &[u8]) {
    out.push(prefix);
    out.extend_from_slice(raw);
    if !raw.ends_with(b"\n") {
        out.extend_from_slice(b"\n\\ No newline at end of file\n");
    }
}

/// A unified diff (3 lines of context) turning `old` into `new` at `path`. `create`: the path
/// is not in the index yet, so the patch creates it with this mode. Empty when nothing differs.
pub fn unified_patch(path: &str, old: &Text, new: &[u8], create: Option<u32>) -> Vec<u8> {
    const CTX: u32 = 3;
    let new = Text::new(new.to_vec());
    let ops = ops_of(old, &new);
    let changes: Vec<(std::ops::Range<u32>, std::ops::Range<u32>)> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Change { old, new } => Some((old.clone(), new.clone())),
            Op::Equal { .. } => None,
        })
        .collect();
    let mut out = Vec::new();
    if changes.is_empty() && create.is_none() {
        return out;
    }
    let (a, b) = (quote_path(&format!("a/{path}")), quote_path(&format!("b/{path}")));
    out.extend_from_slice(format!("diff --git {a} {b}\n").as_bytes());
    match create {
        Some(mode) => out.extend_from_slice(format!("new file mode {mode:o}\n--- /dev/null\n+++ {b}\n").as_bytes()),
        None => out.extend_from_slice(format!("--- {a}\n+++ {b}\n").as_bytes()),
    }
    let mut i = 0;
    while i < changes.len() {
        let mut j = i;
        while j + 1 < changes.len() && changes[j + 1].0.start - changes[j].0.end <= 2 * CTX {
            j += 1;
        }
        let pre = changes[i].0.start.min(CTX);
        let post = (old.len() - changes[j].0.end).min(CTX);
        let (os, ns) = (changes[i].0.start - pre, changes[i].1.start - pre);
        let (oc, nc) = (changes[j].0.end + post - os, changes[j].1.end + post - ns);
        // a zero-length side is addressed by the line before it
        let at = |s: u32, c: u32| if c == 0 { s } else { s + 1 };
        out.extend_from_slice(format!("@@ -{},{oc} +{},{nc} @@\n", at(os, oc), at(ns, nc)).as_bytes());
        let mut cur = os;
        for (o, n) in &changes[i..=j] {
            for k in cur..o.start {
                push_line(&mut out, b' ', old.raw_line(k));
            }
            for k in o.clone() {
                push_line(&mut out, b'-', old.raw_line(k));
            }
            for k in n.clone() {
                push_line(&mut out, b'+', new.raw_line(k));
            }
            cur = o.end;
        }
        for k in cur..changes[j].0.end + post {
            push_line(&mut out, b' ', old.raw_line(k));
        }
        i = j + 1;
    }
    out
}

impl Handle {
    fn blob_text(&self, b: Option<BlobId>) -> anyhow::Result<Arc<Text>> {
        Ok(Arc::new(Text::new(self.blob_bytes(b)?)))
    }

    /// The blob (or a submodule's commit) `path` has in HEAD; None when HEAD lacks it or is
    /// unborn. In process: a git call here costs as much as the rest of a line toggle.
    pub fn head_blob(&self, path: &str) -> anyhow::Result<Option<BlobId>> {
        let Ok(commit) = self.repo.head_commit() else { return Ok(None) };
        let entry = commit.tree()?.lookup_entry_by_path(path)?;
        Ok(entry.filter(|e| !e.mode().is_tree()).map(|e| BlobId::from_oid(e.oid())))
    }

    /// `path`'s stage-0 index entry as status reports it: None when absent or intent-to-add
    /// (`git add -N` lists the empty blob, but has no index side). Read from disk each call, in
    /// process: on a 120k-entry index this is ~8 ms against ~12 ms for `git ls-files`.
    pub fn index_entry_blob(&self, path: &str) -> anyhow::Result<Option<BlobId>> {
        use gix::index::entry::{Flags, Stage};
        let idx = self.repo.open_index()?;
        let e = idx.entry_by_path_and_stage(path.into(), Stage::Unconflicted);
        Ok(e.filter(|e| !e.flags.contains(Flags::INTENT_TO_ADD)).map(|e| BlobId::from_oid(&e.id)))
    }

    /// Applies a patch to the index after checking `path`'s entry is still `expect` (spec §12.2
    /// TOCTOU guard): a mismatch means the index changed since the diff was made.
    /// Afterwards the entry must be `target`: anything else is reported, not left silent.
    pub fn apply_cached(&self, patch: &[u8], path: &str, expect: Option<BlobId>, target: BlobId) -> anyhow::Result<()> {
        if self.index_entry_blob(path)? != expect {
            anyhow::bail!("{path} changed in the index since its diff was loaded; refreshing");
        }
        crate::git_cli::GitCli::new(self.owner()).apply_to_index(patch)?;
        let got = self.index_entry_blob(path)?;
        if got != Some(target) {
            let got = got.map_or("no entry".to_string(), |b| b.to_string());
            anyhow::bail!("the index now has {path} as {got}, not what was selected ({target}); check `git diff --cached -- {path}`");
        }
        Ok(())
    }

    /// HEAD, index and worktree (git form) versions of a status entry's file.
    pub fn stage_texts(&self, e: &StatusEntry) -> anyhow::Result<Texts> {
        if [e.head_mode, e.index_mode, e.wt_mode].contains(&crate::commit_files::MODE_SUBMODULE) {
            return self.submodule_texts(e);
        }
        let head = self.blob_text(e.head_blob)?;
        let index = self.blob_text(e.index_blob)?;
        let root = self.owner().workdir().ok_or_else(|| anyhow::anyhow!("bare repository"))?;
        let full = root.join(&e.path);
        use std::os::unix::fs::PermissionsExt;
        let (raw, convert, wt_mode) = match std::fs::symlink_metadata(&full) {
            Ok(m) if m.file_type().is_symlink() => (std::fs::read_link(&full)?.into_os_string().into_encoded_bytes(), false, 0o120000),
            Ok(m) if m.is_file() => {
                let exec = m.permissions().mode() & 0o111 != 0;
                (std::fs::read(&full)?, true, if exec { 0o100755 } else { 0o100644 })
            }
            _ => (Vec::new(), false, 0),
        };
        let git_form = if convert { self.to_git_form(&e.path, &raw)? } else { raw.clone() };
        let wt_is_raw = git_form == raw;
        let wt_blob = BlobId::hash_of(&git_form);
        Ok(Texts { head, index, wt: Arc::new(Text::new(git_form)), wt_is_raw, wt_mode, wt_blob })
    }

    /// A submodule's "texts" are the commit ids it points at, which the diff shows as a card:
    /// HEAD's and the index's gitlinks, and the checked-out commit of the submodule itself.
    fn submodule_texts(&self, e: &StatusEntry) -> anyhow::Result<Texts> {
        let id = |b: Option<BlobId>| Arc::new(Text::new(b.map(|b| b.to_string().into_bytes()).unwrap_or_default()));
        let root = self.owner().workdir().ok_or_else(|| anyhow::anyhow!("bare repository"))?;
        let checked_out = gix::open(root.join(&e.path)).ok().and_then(|r| r.head_id().ok().map(|h| h.to_string()));
        let wt = checked_out.map(String::into_bytes).unwrap_or_default();
        let wt_mode = if wt.is_empty() { 0 } else { crate::commit_files::MODE_SUBMODULE };
        let wt_blob = BlobId::hash_of(&wt);
        Ok(Texts { head: id(e.head_blob), index: id(e.index_blob), wt: Arc::new(Text::new(wt)), wt_is_raw: true, wt_mode, wt_blob })
    }

    /// Applies clean filters, eol and autocrlf conversion as `git add` would.
    fn to_git_form(&self, path: &str, raw: &[u8]) -> anyhow::Result<Vec<u8>> {
        let (mut pipe, index) = self.repo.filter_pipeline(None)?;
        let mut outcome = pipe.convert_to_git(raw, Path::new(path), &index)?;
        let mut v = Vec::with_capacity(raw.len());
        outcome.read_to_end(&mut v)?;
        Ok(v)
    }
}
