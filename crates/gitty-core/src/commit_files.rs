//! The changed files of a commit (first-parent tree diff) and their lazily computed +/- stats.

use crate::repo::{to_oid, Handle};
use crate::types::CommitId;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlobId(pub [u8; 20]);

impl BlobId {
    pub(crate) fn from_oid(o: &gix::oid) -> BlobId {
        let mut b = [0u8; 20];
        b.copy_from_slice(o.as_bytes());
        BlobId(b)
    }
    pub(crate) fn oid(&self) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed { similarity: Option<u8> },
    Copied,
    TypeChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// New path; the old path for deletions.
    pub path: String,
    /// Source path of a rename or copy.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub old_blob: Option<BlobId>,
    pub new_blob: Option<BlobId>,
    /// Git mode bits (e.g. 0o100644); 0 when that side is absent.
    pub old_mode: u32,
    pub new_mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineStats {
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
}

pub const MODE_SUBMODULE: u32 = 0o160000;

impl Handle {
    /// Files changed by `commit` relative to its first parent (empty tree for a root commit).
    pub fn commit_files(&self, commit: CommitId, detect_renames: bool) -> anyhow::Result<Vec<FileChange>> {
        let repo = self.gix();
        let c = repo.find_commit(to_oid(commit))?;
        let new_tree = c.tree()?;
        let old_tree = self.first_parent_tree(&c)?;
        self.diff_trees(&old_tree, &new_tree, detect_renames)
    }

    /// Files changed across `oldest^..newest`.
    pub fn range_files(&self, oldest: CommitId, newest: CommitId, detect_renames: bool) -> anyhow::Result<Vec<FileChange>> {
        let repo = self.gix();
        let old_tree = self.first_parent_tree(&repo.find_commit(to_oid(oldest))?)?;
        let new_tree = repo.find_commit(to_oid(newest))?.tree()?;
        self.diff_trees(&old_tree, &new_tree, detect_renames)
    }

    fn first_parent_tree<'r>(&'r self, c: &gix::Commit<'r>) -> anyhow::Result<gix::Tree<'r>> {
        Ok(match self.parents_of(c).first() {
            Some(p) => self.gix().find_commit(*p)?.tree()?,
            None => self.gix().empty_tree(),
        })
    }

    fn diff_trees(&self, old: &gix::Tree<'_>, new: &gix::Tree<'_>, detect_renames: bool) -> anyhow::Result<Vec<FileChange>> {
        use gix::object::tree::diff::Change;
        let mut out = Vec::new();
        let mut plat = old.changes()?;
        plat.options(|o| {
            o.track_path();
            o.track_rewrites(if detect_renames { Some(Default::default()) } else { None });
        });
        let mut cache_slot = self.tree_diff_cache.borrow_mut();
        if cache_slot.is_none() {
            *cache_slot = Some(self.gix().diff_resource_cache_for_tree_diff()?);
        }
        let cache = cache_slot.as_mut().expect("just set");
        let res = plat.for_each_to_obtain_tree_with_cache(new, cache, |ch| {
            if ch.entry_mode().is_tree() {
                return Ok::<_, gix::Exn>(gix::object::tree::diff::Action::Continue(()));
            }
            let mode = |m: gix::object::tree::EntryMode| m.value() as u32;
            let fc = match ch {
                Change::Addition { location, entry_mode, id, .. } => FileChange {
                    path: location.to_string(),
                    old_path: None,
                    status: FileStatus::Added,
                    old_blob: None,
                    new_blob: Some(BlobId::from_oid(&id)),
                    old_mode: 0,
                    new_mode: mode(entry_mode),
                },
                Change::Deletion { location, entry_mode, id, .. } => FileChange {
                    path: location.to_string(),
                    old_path: None,
                    status: FileStatus::Deleted,
                    old_blob: Some(BlobId::from_oid(&id)),
                    new_blob: None,
                    old_mode: mode(entry_mode),
                    new_mode: 0,
                },
                Change::Modification { location, previous_entry_mode, previous_id, entry_mode, id, .. } => {
                    let type_change = previous_entry_mode.kind() != entry_mode.kind()
                        && !(previous_entry_mode.is_blob() && entry_mode.is_blob());
                    FileChange {
                        path: location.to_string(),
                        old_path: None,
                        status: if type_change { FileStatus::TypeChange } else { FileStatus::Modified },
                        old_blob: Some(BlobId::from_oid(&previous_id)),
                        new_blob: Some(BlobId::from_oid(&id)),
                        old_mode: mode(previous_entry_mode),
                        new_mode: mode(entry_mode),
                    }
                }
                Change::Rewrite { source_location, source_entry_mode, source_id, entry_mode, id, location, copy, diff, .. } => {
                    FileChange {
                        path: location.to_string(),
                        old_path: Some(source_location.to_string()),
                        status: if copy {
                            FileStatus::Copied
                        } else {
                            FileStatus::Renamed { similarity: diff.map(|d| (d.similarity * 100.0).round() as u8) }
                        },
                        old_blob: Some(BlobId::from_oid(&source_id)),
                        new_blob: Some(BlobId::from_oid(&id)),
                        old_mode: mode(source_entry_mode),
                        new_mode: mode(entry_mode),
                    }
                }
            };
            out.push(fc);
            Ok(gix::object::tree::diff::Action::Continue(()))
        });
        cache.clear_resource_cache_keep_allocation();
        res?;
        out.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        Ok(out)
    }

    /// +/- line counts for one file. Binary files (NUL in the first 8000 bytes) report `binary`.
    pub fn line_stats(&self, ch: &FileChange) -> anyhow::Result<LineStats> {
        if ch.old_mode == MODE_SUBMODULE || ch.new_mode == MODE_SUBMODULE {
            return Ok(LineStats::default());
        }
        let old = self.blob_bytes(ch.old_blob)?;
        let new = self.blob_bytes(ch.new_blob)?;
        if is_binary(&old) || is_binary(&new) {
            return Ok(LineStats { binary: true, ..Default::default() });
        }
        let (added, removed) = crate::diff_lines::count(&old, &new);
        Ok(LineStats { added, removed, binary: false })
    }

    pub(crate) fn blob_bytes(&self, b: Option<BlobId>) -> anyhow::Result<Vec<u8>> {
        Ok(match b {
            Some(b) => self.gix().find_object(b.oid())?.detach().data,
            None => Vec::new(),
        })
    }
}

pub(crate) fn is_binary(d: &[u8]) -> bool {
    d[..d.len().min(8000)].contains(&0)
}
