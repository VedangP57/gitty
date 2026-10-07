//! What the UI asks workers for ([`Request`]) and what comes back ([`Msg`]).
//!
//! Requests carry the generation they were made under; workers stop early and the app drops
//! results once the matching counter in [`Gens`] has moved on.

use std::collections::HashSet;
use std::ops::Range;
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

/// Where a file list comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilesOf {
    /// One commit against its first parent.
    Commit(CommitId),
    /// A contiguous run of history: `oldest^..newest` (the empty tree for a root commit).
    Range { oldest: CommitId, newest: CommitId },
    /// `from..to` trees (from = None: the empty tree), e.g. compare's `merge-base..other`.
    Between { from: Option<CommitId>, to: CommitId },
}

/// Current generations: `session` (history walk), `commit` (selected commit), `file` (diff),
/// `search` (history search).
#[derive(Debug, Default)]
pub struct Gens {
    pub session: AtomicU64,
    pub commit: AtomicU64,
    pub file: AtomicU64,
    pub search: AtomicU64,
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
    /// `expect` is the file's blob on disk and `head` its blob in HEAD (at `head_path`, the
    /// original path of a rename) when the diff was made.
    WriteFile { path: String, bytes: Vec<u8>, expect: gitty_core::commit_files::BlobId, head_path: String, head: Option<gitty_core::commit_files::BlobId> },
    /// Discard every change to the paths: `restore` paths go back to HEAD (index and worktree),
    /// `remove` paths (not in HEAD) leave the index and the disk. Each file is copied to the
    /// Trash first.
    DiscardFiles { restore: Vec<String>, remove: Vec<String> },
    Commit { message: String, amend: bool },
    /// Undo the commit gitty made (`expect`, its id), unless HEAD moved or it was pushed.
    UndoCommit { expect: String },
    /// Delete the git dir's `index.lock` left by a git that died, once no git is running.
    RemoveIndexLock { seen: crate::write::LockId },
    /// `git update-index -q --refresh`: saves fresh stat data so later read-only statuses stop
    /// re-hashing racily clean files.
    RefreshIndex,
    /// `git switch`; `remote`: `name` is `origin/x` and a local branch tracking it is created.
    SwitchBranch { name: String, remote: bool },
    /// Create a branch at HEAD and switch to it.
    CreateBranch { name: String },
    RenameBranch { old: String, new: String },
    /// `-d`, or `-D` with `force` (after the user confirmed losing unmerged commits).
    DeleteBranch { name: String, force: bool },
    /// `git stash push -u`. Nothing to stash is a note, not an error.
    StashPush { message: String },
    /// `expect` is the stash commit the user saw at `index`; a different one there means the list changed.
    StashApply { index: usize, expect: String },
    StashPop { index: usize, expect: String },
    StashDrop { index: usize, expect: String },
    /// Stash the changes, then switch; if the switch fails the stash is popped back.
    StashAndSwitch { name: String, remote: bool, message: String },
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
            WriteOp::UndoCommit { .. } => "undoing the commit",
            WriteOp::RemoveIndexLock { .. } => "removing index.lock",
            WriteOp::RefreshIndex => "refreshing the index",
            WriteOp::SwitchBranch { .. } => "switching branch",
            WriteOp::CreateBranch { .. } => "creating the branch",
            WriteOp::RenameBranch { .. } => "renaming the branch",
            WriteOp::DeleteBranch { .. } => "deleting the branch",
            WriteOp::StashPush { .. } => "stashing",
            WriteOp::StashApply { .. } | WriteOp::StashPop { .. } => "applying the stash",
            WriteOp::StashDrop { .. } => "dropping the stash",
            WriteOp::StashAndSwitch { .. } => "stashing and switching branch",
            WriteOp::Seq(ops) => ops.last().map_or("writing", WriteOp::label),
        }
    }
    /// Commits, undo and branch changes move HEAD or refs; refs and history refresh after them.
    pub fn moves_head(&self) -> bool {
        matches!(self, WriteOp::Commit { .. } | WriteOp::UndoCommit { .. } | WriteOp::SwitchBranch { .. } | WriteOp::CreateBranch { .. } | WriteOp::RenameBranch { .. } | WriteOp::DeleteBranch { .. } | WriteOp::StashAndSwitch { .. })
    }

    /// The stash list changes after these.
    pub fn touches_stash(&self) -> bool {
        matches!(self, WriteOp::StashPush { .. } | WriteOp::StashApply { .. } | WriteOp::StashPop { .. } | WriteOp::StashDrop { .. } | WriteOp::StashAndSwitch { .. })
    }
}

pub enum Request {
    Refs,
    Walk { session: u64, tips: Vec<CommitId> },
    /// `upstream: None` is a branch never pushed: ahead is what pushing it would publish.
    AheadBehind { local: CommitId, upstream: Option<CommitId> },
    Rows { session: u64, ids: Vec<(usize, CommitId)> },
    /// Match history rows `range` against `query`, keeping only `paths` when set.
    Search { generation: u64, query: Arc<gitty_core::search::Query>, paths: Option<Arc<HashSet<CommitId>>>, history: SharedHistory, range: Range<usize> },
    /// The commits reachable from `tips` that touch `path` (the `path:` filter).
    SearchPath { generation: u64, tips: Vec<CommitId>, path: String },
    /// Decodes rows by id (compare lists, which are not history indices).
    CommitRows { ids: Vec<CommitId> },
    /// How many commits a range's diff covers.
    /// `rows`: the selected history rows (indices into `history`), which the note does not count;
    /// their ids are collected on the worker, not the main thread.
    RangeCount { generation: u64, oldest: CommitId, newest: CommitId, history: SharedHistory, rows: Range<usize> },
    /// Both sides of HEAD vs `other`.
    Compare { generation: u64, head: CommitId, other: CommitId },
    Detail { generation: u64, id: CommitId },
    /// File list then line stats. Prefetches are never cancelled (they fill the cache).
    Files { generation: u64, of: FilesOf, prefetch: bool },
    Diff { generation: u64, file: FileChange, opts: DiffOptions, force_text: bool },
    /// Finish intraline for a cached diff whose computation was cut short.
    Intraline { generation: u64, key: DiffKey, diff: Arc<FileDiff> },
    /// Whole-file syntax highlighting of one side; cancelled when the file generation moves on.
    Highlight { generation: u64, key: HlKey, text: Arc<Text> },
    /// Working-tree status. `mark` is noted just before status reads the index, so the watcher
    /// drops the event for exactly that state and nothing later.
    Status { generation: u64, mark: Option<gitty_core::watch::IndexMark> },
    /// HEAD → worktree diff of one status entry, with its staged lines.
    ChangeDiff { generation: u64, entry: StatusEntry, opts: DiffOptions, force_text: bool },
    Write(WriteOp),
    /// HEAD's full message, for amend.
    HeadMessage,
    /// The stash entries, newest first.
    StashList,
    /// A network job, on the single network thread. `background` jobs (auto-fetch) report quietly.
    Net { op: NetOp, mode: gitty_core::net::Mode, background: bool },
    /// Auto-tuning check (and apply) on the maintenance thread.
    Tune { history_len: usize, th: gitty_core::tune::Thresholds },
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
        matches!(self, Request::Files { prefetch: true, .. } | Request::Search { .. } | Request::SearchPath { .. })
    }
}

pub enum Msg {
    Refs { refs: RefsSnapshot, fetched_at: Option<i64> },
    HistoryStarted { session: u64, history: SharedHistory },
    HistoryProgress { session: u64, len: usize, done: bool },
    Rows { session: u64, rows: Vec<(usize, CommitRow)> },
    /// History indices in `range` that match, ascending.
    SearchHits { generation: u64, range: Range<usize>, hits: Vec<usize> },
    SearchPaths { generation: u64, result: Result<Arc<HashSet<CommitId>>, String> },
    CommitRows { rows: Vec<CommitRow> },
    RangeCount { oldest: CommitId, newest: CommitId, extra: usize },
    Compare { generation: u64, result: Result<gitty_core::compare::Compare, String> },
    AheadBehind { local: CommitId, upstream: Option<CommitId>, ab: AheadBehind },
    Detail { generation: u64, detail: CommitDetail },
    Files { generation: u64, of: FilesOf, files: Arc<Vec<FileChange>>, prefetch: bool },
    FilesError { generation: u64, of: FilesOf, prefetch: bool, detail: String },
    /// Stats for `files[start..start + stats.len()]` of the list `of`.
    /// `None` where a blob could not be read (e.g. a partial clone).
    Stats { of: FilesOf, start: usize, stats: Vec<Option<LineStats>>, done: bool },
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
    NetStarted { op: NetOp, label: String, remote: Option<String>, cancel: Option<gitty_core::net::Cancel> },
    NetProgress { op: NetOp, fraction: f32 },
    NetDone { op: NetOp, background: bool, outcome: gitty_core::net::Outcome },
    /// git or ssh asks for a username, password, passphrase or yes/no through the trampoline.
    Ask(crate::askpass::Ask),
    /// What auto-tuning applied (empty when nothing was needed).
    Tuned { applied: Vec<gitty_core::tune::Action>, error: Option<String> },
    /// A status run was slow enough that refreshing the index is worth a try.
    StatusSlow,
    /// A write failed on an `index.lock` while no git process runs: offer to remove it.
    StaleIndexLock { seen: crate::write::LockId },
    HeadMessage { result: Result<String, String> },
    StashList { result: Result<Vec<gitty_core::stash::StashEntry>, String> },
    Error { what: String, detail: String },
}

impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Msg::Refs { fetched_at, .. } => write!(f, "Refs {{ fetched_at: {fetched_at:?} }}"),
            Msg::HistoryStarted { session, .. } => write!(f, "HistoryStarted {{ session: {session} }}"),
            Msg::HistoryProgress { session, len, done } => write!(f, "HistoryProgress {{ session: {session}, len: {len}, done: {done} }}"),
            Msg::Rows { session, rows } => write!(f, "Rows {{ session: {session}, n: {} }}", rows.len()),
            Msg::SearchHits { generation, range, hits } => write!(f, "SearchHits {{ generation: {generation}, {range:?}: {} }}", hits.len()),
            Msg::SearchPaths { generation, result } => write!(f, "SearchPaths {{ generation: {generation}, {:?} }}", result.as_ref().map(|s| s.len())),
            Msg::CommitRows { rows } => write!(f, "CommitRows {{ n: {} }}", rows.len()),
            Msg::RangeCount { extra, .. } => write!(f, "RangeCount {{ extra: {extra} }}"),
            Msg::Compare { generation, result } => write!(f, "Compare {{ generation: {generation}, {:?} }}", result.as_ref().map(|c| (c.behind.len(), c.ahead.len()))),
            Msg::AheadBehind { ab, .. } => write!(f, "AheadBehind {{ ahead: {}, behind: {} }}", ab.ahead.len(), ab.behind.len()),
            Msg::Detail { generation, detail } => write!(f, "Detail {{ generation: {generation}, id: {:?} }}", detail.row.id),
            Msg::Files { generation, of, files, prefetch } => write!(f, "Files {{ generation: {generation}, {of:?}, n: {}, prefetch: {prefetch} }}", files.len()),
            Msg::FilesError { of, prefetch, detail, .. } => write!(f, "FilesError {{ {of:?}, prefetch: {prefetch}, {detail} }}"),
            Msg::Stats { of, start, stats, done } => write!(f, "Stats {{ {of:?}, start: {start}, n: {}, done: {done} }}", stats.len()),
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
            Msg::StaleIndexLock { .. } => write!(f, "StaleIndexLock"),
            Msg::NetStarted { op, label, cancel, .. } => write!(f, "NetStarted {{ {op:?}: {label}, cancellable: {} }}", cancel.is_some()),
            Msg::NetProgress { op, fraction } => write!(f, "NetProgress {{ {op:?}: {fraction:.2} }}"),
            Msg::NetDone { op, background, outcome } => write!(f, "NetDone {{ {op:?}, background: {background}, {outcome:?} }}"),
            Msg::Tuned { applied, error } => write!(f, "Tuned {{ {applied:?}, error: {} }}", error.is_some()),
            Msg::Ask(a) => write!(f, "Ask {{ {}: {:?} }}", a.prompt, a.kind),
            Msg::HeadMessage { result } => write!(f, "HeadMessage {{ ok: {} }}", result.is_ok()),
            Msg::StashList { result } => write!(f, "StashList {{ {:?} }}", result.as_ref().map(Vec::len)),
            Msg::Error { what, detail } => write!(f, "Error {{ {what}: {detail} }}"),
        }
    }
}
