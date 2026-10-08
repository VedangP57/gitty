//! The Files tab's data: one directory of the working tree at a time, merged from the index
//! (what is tracked, which paths are submodules) and the filesystem (what is there), and the
//! content of one file, read with limits. Nothing here walks a whole tree.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use gix::bstr::ByteSlice;

use crate::diff::classify::{is_binary, parse_lfs};
use crate::ignores::Ignores;
use crate::repo::Handle;

/// Files bigger than this are never loaded (spec: the viewer reads at most 2 MiB).
pub const MAX_VIEW_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// Never followed, even when it points at a directory.
    Symlink { target: PathBuf },
    /// A gitlink in the index or a directory with a `.git` of its own: never descended.
    Submodule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: OsString,
    pub kind: EntryKind,
    /// In the index (for a directory: something below it is).
    pub tracked: bool,
    /// Matched by an ignore rule and not tracked.
    pub ignored: bool,
    /// Bytes of a file; 0 for everything else.
    pub size: u64,
}

impl DirEntry {
    /// Directories and submodules sort first.
    pub fn is_dir_like(&self) -> bool {
        matches!(self.kind, EntryKind::Dir | EntryKind::Submodule)
    }
}

/// A path inside the work tree: no `..`, no root, no prefix.
fn relative(rel: &Path) -> anyhow::Result<()> {
    if rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        Ok(())
    } else {
        anyhow::bail!("{} is not a path inside the working tree", rel.display())
    }
}

/// What the index knows about the children of one directory: (name, is a gitlink, is a directory).
fn tracked_children(h: &Handle, rel: &Path) -> Vec<(Vec<u8>, bool, bool)> {
    use gix::bstr::BString;
    use gix::index::entry::Mode;
    // the index is read from disk for each listing; a failure (no index yet) means nothing is tracked
    let Ok(idx) = h.repo.open_index() else { return Vec::new() };
    let mut prefix = BString::from(gix::path::into_bstr(rel).as_ref().to_vec());
    if !prefix.is_empty() {
        prefix.push(b'/');
    }
    let Some(range) = idx.prefixed_entries_range(prefix.as_ref()) else { return Vec::new() };
    let entries = &idx.entries()[range.clone()];
    let mut out = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let path = entries[i].path(&idx);
        let rest = &path[prefix.len()..];
        match rest.find_byte(b'/') {
            Some(slash) => {
                let name = rest[..slash].to_vec();
                // everything below `name/` is contiguous: skip it with a binary search
                let mut end = prefix.clone();
                end.extend_from_slice(&name);
                end.push(b'/' + 1);
                i += entries[i..].partition_point(|e| e.path(&idx) < end.as_bstr()).max(1);
                out.push((name, false, true));
            }
            None => {
                out.push((rest.to_vec(), entries[i].mode == Mode::COMMIT, false));
                i += 1;
            }
        }
    }
    // first appearance is path order, where `a.txt` precedes `a/x`; lookups want name order
    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    out
}

impl Handle {
    /// The entries of `rel` (a directory of the work tree; empty for its root), directories
    /// first, each group by name without regard to case. `.git` is left out. A directory that
    /// cannot be read is an error; entries that vanish meanwhile are skipped.
    pub fn list_dir(&self, rel: &Path) -> anyhow::Result<Vec<DirEntry>> {
        use std::os::unix::ffi::OsStrExt;
        relative(rel)?;
        let root = self.owner().workdir().ok_or_else(|| anyhow::anyhow!("bare repository"))?;
        let abs = root.join(rel);
        let tracked = tracked_children(self, rel);
        let is_tracked = |name: &OsString| tracked.binary_search_by(|(n, ..)| n.as_slice().cmp(name.as_bytes())).is_ok();
        let gitlink = |name: &OsString| tracked.binary_search_by(|(n, ..)| n.as_slice().cmp(name.as_bytes())).is_ok_and(|i| tracked[i].1);
        let mut out = Vec::new();
        for ent in std::fs::read_dir(&abs).map_err(|e| anyhow::anyhow!("reading {}: {e}", abs.display()))? {
            let Ok(ent) = ent else { continue };
            let name = ent.file_name();
            if name == ".git" {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(ent.path()) else { continue };
            let ft = meta.file_type();
            let kind = if ft.is_symlink() {
                EntryKind::Symlink { target: std::fs::read_link(ent.path()).unwrap_or_default() }
            } else if ft.is_dir() {
                if gitlink(&name) || std::fs::symlink_metadata(ent.path().join(".git")).is_ok() { EntryKind::Submodule } else { EntryKind::Dir }
            } else {
                EntryKind::File
            };
            let size = if ft.is_file() { meta.len() } else { 0 };
            out.push(DirEntry { tracked: is_tracked(&name), name, kind, ignored: false, size });
        }
        let names: Vec<_> = out.iter().map(|e| (e.name.clone(), e.is_dir_like())).collect();
        let verdicts = Ignores::new(self.repo.clone()).children(rel, &names);
        for (e, ignored) in out.iter_mut().zip(verdicts) {
            e.ignored = ignored && !e.tracked;
        }
        out.sort_by_cached_key(|e| (!e.is_dir_like(), e.name.to_string_lossy().to_lowercase(), e.name.as_bytes().to_vec()));
        Ok(out)
    }
}

/// Whether a file's name says it holds secrets (keys, tokens, `.env`). Case-insensitive, on the
/// last component only. The Files tab never reads such a file for display until the user asks.
pub fn is_secret(path: &Path) -> bool {
    let Some(name) = path.file_name() else { return false };
    let n = name.to_string_lossy().to_lowercase();
    if n == ".env" {
        return true;
    }
    if n.starts_with(".env.") {
        return !matches!(n.as_str(), ".env.example" | ".env.sample" | ".env.template" | ".env.dist");
    }
    const EXT: [&str; 7] = [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".kdbx"];
    if EXT.iter().any(|e| n.ends_with(e)) {
        return true;
    }
    // a public key is masked too: cheaper to press `v` than to guess wrong
    let stem = n.strip_suffix(".pub").unwrap_or(&n);
    if matches!(stem, "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519") {
        return true;
    }
    matches!(n.as_str(), ".netrc" | ".npmrc" | ".pypirc" | ".git-credentials" | "credentials")
        || n.starts_with("credentials.")
        || n.starts_with("secrets.")
        || (n.starts_with("service-account") && n.ends_with(".json"))
}

/// What the viewer shows for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    Text(Vec<u8>),
    Binary { size: u64 },
    TooLarge { size: u64 },
    /// A Git LFS pointer: the real object is not checked out.
    Lfs { size: u64 },
    Symlink { target: PathBuf },
    /// A FIFO, device or socket: never opened.
    Special,
    /// Secret by name and not revealed: the disk was not touched.
    Masked,
}

/// Reads `rel` of the work tree at `root` for display. A secret file is not opened unless
/// `reveal`. Only regular files are opened, and at most [`MAX_VIEW_BYTES`] are read.
pub fn read_file(root: &Path, rel: &Path, reveal: bool) -> anyhow::Result<FileContent> {
    use std::io::Read;
    relative(rel)?;
    if is_secret(rel) && !reveal {
        return Ok(FileContent::Masked);
    }
    let abs = root.join(rel);
    let meta = std::fs::symlink_metadata(&abs).map_err(|e| anyhow::anyhow!("reading {}: {e}", rel.display()))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Ok(FileContent::Symlink { target: std::fs::read_link(&abs).unwrap_or_default() });
    }
    if !ft.is_file() {
        return Ok(FileContent::Special);
    }
    if meta.len() > MAX_VIEW_BYTES {
        return Ok(FileContent::TooLarge { size: meta.len() });
    }
    // the file may have grown since the stat: `take` keeps the read bounded anyway
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(&abs)
        .and_then(|f| f.take(MAX_VIEW_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", rel.display()))?;
    let size = bytes.len() as u64;
    Ok(if size > MAX_VIEW_BYTES {
        FileContent::TooLarge { size }
    } else if let Some(p) = parse_lfs(&bytes) {
        FileContent::Lfs { size: p.size }
    } else if is_binary(&bytes) {
        FileContent::Binary { size }
    } else {
        FileContent::Text(bytes)
    })
}
