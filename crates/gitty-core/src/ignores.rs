//! gitignore checks shared by the watcher and the Files tab listing.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// gitignore checks with a cache of directory verdicts (a build in an ignored directory sends
/// thousands of events).
pub(crate) struct Ignores {
    repo: gix::Repository,
    dirs: HashMap<PathBuf, bool>,
}

impl Ignores {
    pub(crate) fn new(repo: gix::Repository) -> Ignores {
        Ignores { repo, dirs: HashMap::new() }
    }
    pub(crate) fn reset(&mut self) {
        self.dirs.clear();
    }
    /// Which of `names` (with whether each is a directory), children of the directory `dir`, are
    /// ignored. One exclude stack for all of them; an ignored directory makes every child ignored.
    pub(crate) fn children(&mut self, dir: &Path, names: &[(OsString, bool)]) -> Vec<bool> {
        if !dir.as_os_str().is_empty() && self.is_ignored(dir) {
            return vec![true; names.len()];
        }
        let Ok(index) = self.repo.index_or_empty() else { return vec![false; names.len()] };
        let Ok(mut stack) = self.repo.excludes(&index, None, Default::default()) else { return vec![false; names.len()] };
        use gix::index::entry::Mode;
        names.iter().map(|(n, is_dir)| stack.at_path(dir.join(n), Some(if *is_dir { Mode::DIR } else { Mode::FILE })).is_ok_and(|p| p.is_excluded())).collect()
    }
    pub(crate) fn is_ignored(&mut self, rel: &Path) -> bool {
        let comps: Vec<_> = rel.components().collect();
        let dirs = || {
            comps[..comps.len().saturating_sub(1)].iter().scan(PathBuf::new(), |d, c| {
                d.push(c);
                Some(d.clone())
            })
        };
        // fast path: an ignored ancestor already known (a build in `target/`)
        for d in dirs() {
            match self.dirs.get(&d) {
                Some(true) => return true,
                Some(false) => continue,
                None => break,
            }
        }
        let Ok(index) = self.repo.index_or_empty() else { return false };
        let Ok(mut stack) = self.repo.excludes(&index, None, Default::default()) else { return false };
        if self.dirs.len() > 50_000 {
            self.dirs.clear();
        }
        for d in dirs() {
            let v = match self.dirs.get(&d) {
                Some(&v) => v,
                None => {
                    let v = stack.at_path(&d, Some(gix::index::entry::Mode::DIR)).is_ok_and(|p| p.is_excluded());
                    self.dirs.insert(d, v);
                    v
                }
            };
            if v {
                return true;
            }
        }
        stack.at_path(rel, None).is_ok_and(|p| p.is_excluded())
    }
}
