//! The diff engine: blobs → `FileDiff` (classification + op lists) → `DiffView` rows.

pub mod classify;
pub mod intraline;
pub mod ops;
pub mod text;
pub mod view;

use std::ops::Range;
use std::sync::{Arc, OnceLock};

use classify::FileClass;
use intraline::BlockHighlights;
use ops::{DiffAlgorithm, Op, WsMode};
use text::{EolStyle, Text};
use view::DiffView;

use crate::commit_files::FileChange;
use crate::repo::Handle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct DiffOptions {
    pub algorithm: DiffAlgorithm,
    pub ws: WsMode,
}

pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub class: FileClass,
    pub old: Arc<Text>,
    pub new: Arc<Text>,
    pub ops: Vec<Op>,
    /// The `Change` ops in order; `change` indices in rows refer to this list.
    pub changes: Vec<(Range<u32>, Range<u32>)>,
    pub added: u32,
    pub removed: u32,
    /// Whole-file line-ending style change, e.g. LF → CRLF.
    pub eol_change: Option<(EolStyle, EolStyle)>,
    /// Changed lines contain bidirectional control characters (possible "Trojan Source").
    pub bidi_warning: bool,
    pub options: DiffOptions,
    old_mode: u32,
    new_mode: u32,
    intraline: Vec<OnceLock<BlockHighlights>>,
}

impl FileDiff {
    pub fn from_bytes(
        path: &str,
        old_path: Option<&str>,
        old: Vec<u8>,
        new: Vec<u8>,
        old_mode: u32,
        new_mode: u32,
        options: DiffOptions,
    ) -> FileDiff {
        let class = classify::classify_pre(&classify::ClassifyInput {
            path,
            old: &old,
            new: &new,
            old_mode,
            new_mode,
            same_content: old == new,
        });
        let keep_text = matches!(class, FileClass::Text | FileClass::Generated { .. } | FileClass::LargeText { .. });
        let (old, new) = if keep_text { (old, new) } else { (Vec::new(), Vec::new()) };
        let mut d = FileDiff {
            path: path.to_string(),
            old_path: old_path.map(str::to_string),
            class,
            old: Arc::new(Text::new(old)),
            new: Arc::new(Text::new(new)),
            ops: Vec::new(),
            changes: Vec::new(),
            added: 0,
            removed: 0,
            eol_change: None,
            bidi_warning: false,
            options,
            old_mode,
            new_mode,
            intraline: Vec::new(),
        };
        if matches!(d.class, FileClass::Text) {
            d.compute();
            d.class = classify::classify_post(std::mem::replace(&mut d.class, FileClass::Text), d.added + d.removed);
        }
        d
    }

    fn compute(&mut self) {
        self.ops = ops::compute_ops(&self.old, &self.new, self.options.algorithm, self.options.ws);
        self.changes = self
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::Change { old, new } => Some((old.clone(), new.clone())),
                Op::Equal { .. } => None,
            })
            .collect();
        (self.added, self.removed) = ops::change_counts(&self.ops);
        self.intraline = (0..self.changes.len()).map(|_| OnceLock::new()).collect();
        let (os, ns) = (self.old.eol_style(), self.new.eol_style());
        if !self.old.is_empty() && !self.new.is_empty() && os != ns && os != EolStyle::None && ns != EolStyle::None {
            self.eol_change = Some((os, ns));
        }
        self.bidi_warning = self.changes.iter().any(|(o, n)| {
            o.clone().any(|i| has_bidi(self.old.line(i))) || n.clone().any(|i| has_bidi(self.new.line(i)))
        });
    }

    /// Word-level highlights for change block `change`, computed on first use.
    pub fn intraline(&self, change: usize) -> &BlockHighlights {
        self.intraline[change].get_or_init(|| {
            let (o, n) = &self.changes[change];
            let dels: Vec<&[u8]> = o.clone().map(|i| self.old.line(i)).collect();
            let adds: Vec<&[u8]> = n.clone().map(|i| self.new.line(i)).collect();
            intraline::block_highlights(&dels, &adds)
        })
    }

    /// A fresh view with default context. Split-view pairing is applied lazily, per visible
    /// change block, with [`FileDiff::apply_pairing`].
    pub fn view(&self) -> DiffView {
        DiffView::new(&self.ops, &self.old, &self.new)
    }

    /// Whether intraline (and so pairing) for `change` has been computed.
    pub fn is_paired(&self, change: usize) -> bool {
        self.intraline.get(change).is_some_and(|c| c.get().is_some())
    }

    /// Computes intraline for `changes` (typically the blocks near the viewport) and applies
    /// their line pairing to `view` in one rebuild.
    pub fn apply_pairing(&self, view: &mut DiffView, changes: Range<usize>) {
        let end = changes.end.min(self.changes.len());
        let start = changes.start.min(end);
        view.set_pairings((start..end).map(|c| (c, self.intraline(c).pair_of_del.as_slice())));
    }

    pub fn is_text(&self) -> bool {
        matches!(self.class, FileClass::Text)
    }

    /// Diff a collapsed Generated/LargeText file anyway (the user asked to see it).
    pub fn force_text(mut self) -> FileDiff {
        if matches!(self.class, FileClass::Generated { .. } | FileClass::LargeText { .. }) {
            self.class = FileClass::Text;
            self.compute();
        }
        self
    }

    pub fn modes(&self) -> (u32, u32) {
        (self.old_mode, self.new_mode)
    }
}

fn has_bidi(l: &[u8]) -> bool {
    l.windows(3).any(|w| w[0] == 0xE2 && ((w[1] == 0x80 && (0xAA..=0xAE).contains(&w[2])) || (w[1] == 0x81 && (0xA6..=0xA9).contains(&w[2]))))
}

impl Handle {
    /// Loads both sides of a commit file change and diffs them.
    pub fn file_diff(&self, change: &FileChange, options: DiffOptions) -> anyhow::Result<FileDiff> {
        use crate::commit_files::MODE_SUBMODULE;
        let (om, nm) = (change.old_mode, change.new_mode);
        let side = |blob: Option<crate::commit_files::BlobId>, mode: u32| -> anyhow::Result<Vec<u8>> {
            let Some(b) = blob else { return Ok(Vec::new()) };
            if mode == MODE_SUBMODULE {
                return Ok(crate::types::CommitId(b.0).to_hex().into_bytes());
            }
            self.blob_bytes(Some(b))
        };
        let header_size = |blob: Option<crate::commit_files::BlobId>, mode: u32| -> anyhow::Result<u64> {
            Ok(match blob {
                Some(b) if mode != MODE_SUBMODULE => self.gix().find_header(b.oid())?.size(),
                _ => 0,
            })
        };
        let (osz, nsz) = (header_size(change.old_blob, om)?, header_size(change.new_blob, nm)?);
        if osz > classify::TOO_LARGE || nsz > classify::TOO_LARGE {
            let mut d = FileDiff::from_bytes(&change.path, change.old_path.as_deref(), Vec::new(), Vec::new(), om, nm, options);
            d.class = FileClass::TooLarge { old_size: osz, new_size: nsz };
            return Ok(d);
        }
        let old = side(change.old_blob, om)?;
        let new = side(change.new_blob, nm)?;
        Ok(FileDiff::from_bytes(&change.path, change.old_path.as_deref(), old, new, om, nm, options))
    }
}
