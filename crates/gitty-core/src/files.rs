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
pub(crate) fn relative(rel: &Path) -> anyhow::Result<()> {
    if rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        Ok(())
    } else {
        anyhow::bail!("{} is not a path inside the working tree", rel.display())
    }
}

/// Refuses a path that goes through a symlink in `dirs` (each prefix of `rel`, and `rel` itself
/// when `whole`): a link swapped in for a directory must not lead the listing or the reader out of
/// the work tree. A check by `lstat` per level, so a swap between the check and the open is a
/// narrow race that the final `O_NOFOLLOW` open only covers for the last component.
pub(crate) fn no_symlinks(root: &Path, rel: &Path, whole: bool) -> anyhow::Result<()> {
    let n = rel.components().count();
    let mut p = root.to_path_buf();
    for (i, c) in rel.components().enumerate() {
        p.push(c);
        if i + 1 == n && !whole {
            break;
        }
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => anyhow::bail!("{} goes through a symlink", rel.display()),
            Ok(_) => {}
            Err(e) => anyhow::bail!("reading {}: {e}", rel.display()),
        }
    }
    Ok(())
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
        no_symlinks(root, rel, true)?;
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

/// Directories whose whole content is secret.
const SECRET_DIRS: [&str; 6] = [".env", "secrets", ".secrets", ".ssh", ".aws", ".gnupg"];
/// Names with these stripped (one after another) are matched as what they back up.
const BACKUP_SUFFIXES: [&str; 8] = ["~", ".bak", ".orig", ".old", ".swp", ".swo", ".save", ".tmp"];
const EXAMPLES: [&str; 4] = [".env.example", ".env.sample", ".env.template", ".env.dist"];

/// The lowercase name and every form it takes while trailing spaces and dots, editor lock forms
/// (`#name#`, `.#name`) and backup suffixes are taken off one at a time. A name is secret if any
/// of them is: `secrets.bak` is one although its stripped form `secrets` alone would not be.
fn forms(name: &str) -> Vec<String> {
    let mut n = name.to_lowercase();
    let mut out = vec![n.clone()];
    // one step at a time; each change is a form of its own
    let next = |n: &str| -> Option<String> {
        let t = n.trim_end_matches([' ', '.']);
        if t.len() != n.len() {
            return Some(t.to_string());
        }
        if n.len() > 2 && n.starts_with('#') && n.ends_with('#') {
            return Some(n[1..n.len() - 1].to_string());
        }
        if let Some(r) = n.strip_prefix(".#") {
            return Some(r.to_string());
        }
        BACKUP_SUFFIXES.iter().find_map(|s| n.strip_suffix(s).filter(|r| !r.is_empty()).map(str::to_string))
    };
    while let Some(m) = next(&n) {
        out.push(m.clone());
        n = m;
    }
    out
}

fn secret_name(n: &str) -> bool {
    const EXT: [&str; 14] = [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".kdbx", ".ppk", ".tfvars", ".gpg", ".token", ".secret", ".secrets", ".env"];
    if n == ".env" || n == ".envrc" || n == "secrets" || [".env.", ".env-", ".env_", ".envrc."].iter().any(|p| n.starts_with(p)) || n.contains(".env.") {
        return true;
    }
    if EXT.iter().any(|e| n.ends_with(e)) || n.contains(".tfstate") || (n.ends_with(".asc") && n.contains("secret")) {
        return true;
    }
    // a public key is masked too: cheaper to press `v` than to guess wrong
    let stem = n.strip_suffix(".pub").unwrap_or(n);
    if matches!(stem, "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519") || (stem.starts_with("id_") && stem.ends_with("_sk")) {
        return true;
    }
    matches!(n, ".netrc" | ".npmrc" | ".pypirc" | ".git-credentials" | "credentials" | ".pgpass" | ".htpasswd" | ".vault-token")
        || n.starts_with("credentials.")
        || n.starts_with("secrets.")
        || (n.starts_with("service-account") && n.ends_with(".json"))
}

/// Whether a path says its file holds secrets (keys, tokens, `.env`). Case-insensitive; the file
/// name is matched after dropping backup and editor suffixes, and a file inside a secret
/// directory (`.ssh`, `.aws`, `secrets`, …) is secret whatever it is called. Only the exact names
/// `.env.example`, `.env.sample`, `.env.template` and `.env.dist` are let through. Limits: a secret
/// under an innocent name is not recognised. The Files tab never reads a secret file for display
/// until the user asks.
pub fn is_secret(path: &Path) -> bool {
    let names: Vec<String> = path.components().filter_map(|c| if let Component::Normal(n) = c { Some(n.to_string_lossy().into_owned()) } else { None }).collect();
    let Some((name, dirs)) = names.split_last() else { return false };
    let dir_forms: Vec<Vec<String>> = dirs.iter().map(|d| forms(d)).collect();
    // a directory is secret by its name like a file is, or is one of the known secret places
    if dir_forms.iter().flatten().any(|d| SECRET_DIRS.contains(&d.as_str()) || d == ".kube" || secret_name(d)) {
        return true;
    }
    let name_forms = forms(name);
    if dir_forms.iter().any(|d| d.iter().any(|f| f == ".docker")) && name_forms.iter().any(|f| f == "config.json") {
        return true;
    }
    !EXAMPLES.contains(&name_forms[0].as_str()) && name_forms.iter().any(|f| secret_name(f))
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
/// `reveal`. Only regular files are opened (without following a link, and without blocking on a
/// FIFO), the type is checked on the open handle, and at most [`MAX_VIEW_BYTES`] are read.
pub fn read_file(root: &Path, rel: &Path, reveal: bool) -> anyhow::Result<FileContent> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    relative(rel)?;
    if is_secret(rel) && !reveal {
        return Ok(FileContent::Masked);
    }
    no_symlinks(root, rel, false)?;
    let abs = root.join(rel);
    let meta = std::fs::symlink_metadata(&abs).map_err(|e| anyhow::anyhow!("reading {}: {e}", rel.display()))?;
    if meta.file_type().is_symlink() {
        return Ok(FileContent::Symlink { target: std::fs::read_link(&abs).unwrap_or_default() });
    }
    if !meta.file_type().is_file() {
        return Ok(FileContent::Special);
    }
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(&abs).map_err(|e| match e.raw_os_error() {
        Some(libc::ELOOP) | Some(libc::ENXIO) => anyhow::anyhow!("{} changed while it was being opened", rel.display()),
        _ => anyhow::anyhow!("reading {}: {e}", rel.display()),
    })?;
    // what was opened, not what the path was a moment ago
    let meta = file.metadata().map_err(|e| anyhow::anyhow!("reading {}: {e}", rel.display()))?;
    if !meta.file_type().is_file() {
        return Ok(FileContent::Special);
    }
    if meta.size() > MAX_VIEW_BYTES {
        return Ok(FileContent::TooLarge { size: meta.size() });
    }
    // the file may have grown since the stat: `take` keeps the read bounded anyway
    let mut bytes = Vec::with_capacity(meta.size() as usize);
    file.take(MAX_VIEW_BYTES + 1).read_to_end(&mut bytes).map_err(|e| anyhow::anyhow!("reading {}: {e}", rel.display()))?;
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
