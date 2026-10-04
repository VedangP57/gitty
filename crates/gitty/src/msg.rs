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
use gitty_core::stage::Texts;
use gitty_core::status::{Status, StatusEntry};
use gitty_core::watch::Changed;
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

/// A mutating git operation, run in order on the writer thread.
#[derive(Clone)]
pub enum WriteOp {
    /// `git add -A` for the paths.
    Stage(Vec<String>),
    /// Restore the paths' index entries to HEAD.
    Unstage(Vec<String>),
    StageAll,
    UnstageAll,
    /// Make the index hold exactly the flagged changes of `diff` (HEAD → worktree).
    SetStaged { entry: StatusEntry, texts: Texts, diff: Arc<FileDiff>, flags: Vec<bool> },
    /// Replace the worktree file's bytes (line discard); the file keeps its mode.
    WriteFile { path: String, bytes: Vec<u8>, expect: gitty_core::commit_files::BlobId },
    /// Discard every change to the paths: `restore` paths go back to HEAD (index and worktree),
    /// `remove` paths (not in HEAD) leave the index and the disk. Each file is copied to the
    /// Trash first.
    DiscardFiles { restore: Vec<String>, remove: Vec<String> },
    Commit { message: String, amend: bool },
    UndoCommit,
    /// `git update-index -q --refresh`: saves fresh stat data so later read-only statuses stop
    /// re-hashing racily clean files.
    RefreshIndex,
    /// Runs in order and stops at the first failure (a line discard that unstages first).
    Seq(Vec<WriteOp>),
}

impl WriteOp {
    pub fn label(&self) -> &'static str {
        match self {
            WriteOp::Stage(_) | WriteOp::StageAll => "staging",
            WriteOp::Unstage(_) | WriteOp::UnstageAll => "unstaging",
            WriteOp::SetStaged { .. } => "staging lines",
            WriteOp::WriteFile { .. } | WriteOp::DiscardFiles { .. } => "discarding",
            WriteOp::Commit { amend: false, .. } => "committing",
            WriteOp::Commit { amend: true, .. } => "amending",
            WriteOp::UndoCommit => "undoing the commit",
            WriteOp::RefreshIndex => "refreshing the index",
            WriteOp::Seq(ops) => ops.last().map_or("writing", WriteOp::label),
        }
    }
    /// Commits and undo move HEAD; refs and history refresh after them.
    pub fn moves_head(&self) -> bool {
        matches!(self, WriteOp::Commit { .. } | WriteOp::UndoCommit)
    }
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
    /// Working-tree status.
    Status { generation: u64 },
    /// HEAD → worktree diff of one status entry, with its staged lines.
    ChangeDiff { generation: u64, entry: StatusEntry, opts: DiffOptions, force_text: bool },
    Write(WriteOp),
    /// HEAD's full message, for amend.
    HeadMessage,
    /// A network job, on the single network thread. `background` jobs (auto-fetch) report quietly.
    Net { op: NetOp, mode: gitty_core::net::Mode, background: bool },
}

/// What the user asked the network thread to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetOp {
    Fetch,
    /// Fetch the upstream remote, then fast-forward.
    Pull,
    /// After a diverged pull: merge or rebase onto the upstream.
    PullMerge,
    PullRebase,
    Push,
}

impl NetOp {
    pub fn verb(self) -> &'static str {
        match self {
            NetOp::Fetch => "Fetch",
            NetOp::Pull => "Pull",
            NetOp::PullMerge => "Merge",
            NetOp::PullRebase => "Rebase",
            NetOp::Push => "Push",
        }
    }
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
    Status { generation: u64, result: Result<Status, String> },
    /// `staged`: per changed line of `diff`; None when lines cannot be staged individually
    /// (binary and other whole-file classes, whitespace hidden, conflicts, or an index holding
    /// content of its own: `divergent`).
    ChangeDiff { generation: u64, entry: StatusEntry, key: DiffKey, diff: Arc<FileDiff>, texts: Texts, staged: Option<Vec<bool>>, divergent: bool },
    ChangeDiffError { generation: u64, path: String, detail: String },
    /// A line of hook or git output from the running write.
    WriteLog { line: String },
    /// `Ok(Some(head))` after a commit; `Ok(Some(message))` after an undo: the undone commit's message.
    WriteDone { op: WriteOp, result: Result<Option<String>, String> },
    /// The watcher saw these kinds of change.
    Changed(Changed),
    /// A network step started; `cancel` is None for local steps that must not be interrupted.
    NetStarted { op: NetOp, label: String, cancel: Option<gitty_core::net::Cancel> },
    NetProgress { op: NetOp, fraction: f32 },
    NetDone { op: NetOp, background: bool, outcome: gitty_core::net::Outcome },
    /// git or ssh asks for a username, password, passphrase or yes/no through the trampoline.
    Ask(crate::askpass::Ask),
    /// A status run was slow enough that refreshing the index is worth a try.
    StatusSlow,
    HeadMessage { result: Result<String, String> },
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
            Msg::Status { generation, result } => match result {
                Ok(s) => write!(f, "Status {{ generation: {generation}, n: {} }}", s.entries.len()),
                Err(e) => write!(f, "Status {{ generation: {generation}, error: {e} }}"),
            },
            Msg::ChangeDiff { generation, entry, staged, divergent, .. } => {
                write!(f, "ChangeDiff {{ generation: {generation}, {}: staged {:?}, divergent: {divergent} }}", entry.path, staged.as_ref().map(|s| s.iter().filter(|b| **b).count()))
            }
            Msg::ChangeDiffError { path, detail, .. } => write!(f, "ChangeDiffError {{ {path}: {detail} }}"),
            Msg::WriteLog { line } => write!(f, "WriteLog {{ {line} }}"),
            Msg::WriteDone { op, result } => write!(f, "WriteDone {{ {}: {:?} }}", op.label(), result.as_ref().map(|m| m.is_some())),
            Msg::Changed(c) => write!(f, "Changed({:#x})", c.0),
            Msg::StatusSlow => write!(f, "StatusSlow"),
            Msg::NetStarted { op, label, cancel } => write!(f, "NetStarted {{ {op:?}: {label}, cancellable: {} }}", cancel.is_some()),
            Msg::NetProgress { op, fraction } => write!(f, "NetProgress {{ {op:?}: {fraction:.2} }}"),
            Msg::NetDone { op, background, outcome } => write!(f, "NetDone {{ {op:?}, background: {background}, {outcome:?} }}"),
            Msg::Ask(a) => write!(f, "Ask {{ {}: {:?} }}", a.prompt, a.kind),
            Msg::HeadMessage { result } => write!(f, "HeadMessage {{ ok: {} }}", result.is_ok()),
            Msg::Error { what, detail } => write!(f, "Error {{ {what}: {detail} }}"),
        }
    }
}
