//! What the UI asks workers for ([`Request`]) and what comes back ([`Msg`]).
//!
//! Requests carry the generation they were made under; workers stop early and the app drops
//! results once the matching counter in [`Gens`] has moved on.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use gitty_core::ahead_behind::AheadBehind;
use gitty_core::commit_files::{BlobId, FileChange, LineStats};
use gitty_core::diff::text::Text;
use gitty_core::diff::{DiffOptions, FileDiff};
use gitty_highlight::Highlights;
use gitty_core::history::{CommitDetail, CommitRow, History};
use gitty_core::refs::RefsSnapshot;
use gitty_core::CommitId;

pub type SharedHistory = Arc<RwLock<History>>;

/// Current generations: `session` (history walk), `commit` (selected commit), `file` (diff).
#[derive(Debug, Default)]
pub struct Gens {
    pub session: AtomicU64,
    pub commit: AtomicU64,
    pub file: AtomicU64,
}

impl Gens {
    pub fn bump(counter: &AtomicU64) -> u64 {
        counter.fetch_add(1, Ordering::SeqCst) + 1
    }
    pub fn is(counter: &AtomicU64, generation: u64) -> bool {
        counter.load(Ordering::SeqCst) == generation
    }
}

/// Identity of a computed diff (cache key). Never contains timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiffKey {
    pub old: Option<BlobId>,
    pub new: Option<BlobId>,
    pub path: String,
    pub old_path: Option<String>,
    pub old_mode: u32,
    pub new_mode: u32,
    pub opts: DiffOptions,
    pub force_text: bool,
}

impl DiffKey {
    pub fn of(f: &FileChange, opts: DiffOptions, force_text: bool) -> DiffKey {
        DiffKey {
            old: f.old_blob,
            new: f.new_blob,
            path: f.path.clone(),
            old_path: f.old_path.clone(),
            old_mode: f.old_mode,
            new_mode: f.new_mode,
            opts,
            force_text,
        }
    }
}

/// Identity of one side's syntax highlighting: the blob, and the path that picks the language.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HlKey {
    pub blob: BlobId,
    pub path: String,
}

pub enum Request {
    Refs,
    Walk { session: u64, tips: Vec<CommitId> },
    AheadBehind { local: CommitId, upstream: CommitId },
    Rows { session: u64, ids: Vec<(usize, CommitId)> },
    Detail { generation: u64, id: CommitId },
    /// File list then line stats. Prefetches are never cancelled (they fill the cache).
    Files { generation: u64, id: CommitId, prefetch: bool },
    Diff { generation: u64, file: FileChange, opts: DiffOptions, force_text: bool },
    /// Finish intraline for a cached diff whose computation was cut short.
    Intraline { generation: u64, key: DiffKey, diff: Arc<FileDiff> },
    /// Whole-file syntax highlighting of one side; cancelled when the file generation moves on.
    Highlight { generation: u64, key: HlKey, text: Arc<Text> },
}

impl Request {
    /// Prefetches go to the low-priority queue.
    pub fn is_background(&self) -> bool {
        matches!(self, Request::Files { prefetch: true, .. })
    }
}

pub enum Msg {
    Refs { refs: RefsSnapshot, fetched_at: Option<i64> },
    HistoryStarted { session: u64, history: SharedHistory },
    HistoryProgress { session: u64, len: usize, done: bool },
    Rows { session: u64, rows: Vec<(usize, CommitRow)> },
    AheadBehind { local: CommitId, upstream: CommitId, ab: AheadBehind },
    Detail { generation: u64, detail: CommitDetail },
    Files { generation: u64, id: CommitId, files: Arc<Vec<FileChange>>, prefetch: bool },
    FilesError { generation: u64, id: CommitId, prefetch: bool, detail: String },
    /// Stats for `files[start..start + stats.len()]` of commit `id`.
    /// `None` where a blob could not be read (e.g. a partial clone).
    Stats { id: CommitId, start: usize, stats: Vec<Option<LineStats>>, done: bool },
    Diff { generation: u64, key: DiffKey, diff: Arc<FileDiff> },
    IntralineDone { key: DiffKey },
    DiffError { generation: u64, key: DiffKey, detail: String },
    /// `spans` is None for unknown languages and files over the limits; `cancelled` results are
    /// not cached.
    Highlighted { key: HlKey, spans: Option<Arc<Highlights>>, cancelled: bool },
    Error { what: String, detail: String },
}

impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Msg::Refs { fetched_at, .. } => write!(f, "Refs {{ fetched_at: {fetched_at:?} }}"),
            Msg::HistoryStarted { session, .. } => write!(f, "HistoryStarted {{ session: {session} }}"),
            Msg::HistoryProgress { session, len, done } => write!(f, "HistoryProgress {{ session: {session}, len: {len}, done: {done} }}"),
            Msg::Rows { session, rows } => write!(f, "Rows {{ session: {session}, n: {} }}", rows.len()),
            Msg::AheadBehind { ab, .. } => write!(f, "AheadBehind {{ ahead: {}, behind: {} }}", ab.ahead.len(), ab.behind.len()),
            Msg::Detail { generation, detail } => write!(f, "Detail {{ generation: {generation}, id: {:?} }}", detail.row.id),
            Msg::Files { generation, id, files, prefetch } => write!(f, "Files {{ generation: {generation}, id: {id:?}, n: {}, prefetch: {prefetch} }}", files.len()),
            Msg::FilesError { id, prefetch, detail, .. } => write!(f, "FilesError {{ id: {id:?}, prefetch: {prefetch}, {detail} }}"),
            Msg::Stats { id, start, stats, done } => write!(f, "Stats {{ id: {id:?}, start: {start}, n: {}, done: {done} }}", stats.len()),
            Msg::Diff { generation, key, .. } => write!(f, "Diff {{ generation: {generation}, path: {} }}", key.path),
            Msg::IntralineDone { key } => write!(f, "IntralineDone {{ path: {} }}", key.path),
            Msg::DiffError { key, detail, .. } => write!(f, "DiffError {{ {}: {detail} }}", key.path),
            Msg::Highlighted { key, spans, cancelled } => write!(f, "Highlighted {{ {}: {}, cancelled: {cancelled} }}", key.path, spans.is_some()),
            Msg::Error { what, detail } => write!(f, "Error {{ {what}: {detail} }}"),
        }
    }
}
