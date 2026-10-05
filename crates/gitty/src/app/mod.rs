//! Application state. Pure: inputs and worker messages go in, [`Request`]s collect in an
//! outbox, and `ui::draw` renders. Nothing here touches git.

pub mod changes;
pub mod commit;
pub mod compare;
pub mod net;
pub mod diffstate;
mod input;
pub mod search;
mod tools;
pub mod tree;

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use gitty_core::CommitId;
use gitty_core::commit_files::{FileChange, LineStats};
use gitty_core::diff::DiffOptions;
use gitty_core::diff::ops::WsMode;
use gitty_core::history::{CommitDetail, CommitRow};
use gitty_highlight::Highlights;
use gitty_core::refs::{HistoryScope, RefsSnapshot};
use ratatui::layout::Rect;

use crate::config::{Config, Density, UiState};
use crate::dates::{DateMode, next_threshold};
use crate::msg::{DiffKey, FilesOf, Gens, HlKey, Msg, Request, SharedHistory};
use crate::theme::{ColorDepth, Registry, Theme};
use crate::ui::layout::{self, LayoutInput, Mode, Panes, Sep};
use diffstate::{DiffState, Wrap};

pub use crate::ui::layout::Focus;

/// Debounce for diff requests while the selection is moving.
pub const DIFF_DEBOUNCE: Duration = Duration::from_millis(30);
/// A selection change this long after the previous diff request fires immediately.
const IDLE_BEFORE_LEADING_EDGE: Duration = Duration::from_millis(100);
const NEIGHBOURS: usize = 10;
const MAX_PREFETCH_IN_FLIGHT: usize = 20;
const ROWS_BATCH: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Changes,
    History,
}

pub enum Overlay {
    ThemePicker { sel: usize, original: Theme },
    Help { scroll: usize },
    ErrorDetail,
    /// A destructive write waiting for Enter (or `y`); Esc cancels.
    Confirm { title: String, body: String, op: crate::msg::WriteOp },
    /// Full output of a failed write (hooks), shown until dismissed.
    Log { title: String, body: String },
    /// git or ssh asks for input (masked unless it is a username).
    Prompt { ask: crate::askpass::Ask, input: crate::editor::Editor },
    /// A pull found local and upstream commits: merge, rebase or cancel.
    Diverged,
    /// `b`: pick the branch to compare with.
    BranchPicker { query: crate::editor::Editor, sel: usize },
    /// `q` while a network job runs.
    Quit { label: String },
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub what: String,
    pub detail: String,
    pub error: bool,
}

/// Screen regions recorded by the last draw, for mouse hit-testing.
#[derive(Debug, Clone, Default)]
pub struct Hits {
    pub panes: Panes,
    pub history_rows: Option<Rect>,
    pub history_first: usize,
    pub history_row_h: u16,
    pub files_rows: Option<Rect>,
    pub files_first: usize,
    pub diff_rows: Option<Rect>,
    /// Row index of each screen line of `diff_rows` (wrapped rows repeat).
    pub diff_lines: Vec<usize>,
    /// (x, width) of the old and new line-number gutters of the (left) diff side.
    pub diff_old_gutter: (u16, u16),
    pub diff_new_gutter: (u16, u16),
    pub tabs: Vec<(Rect, Tab)>,
    pub commit_fields: Vec<(Rect, commit::Field)>,
    pub commit_button: Option<Rect>,
    pub dragging: Option<Sep>,
}

/// A tiny LRU: eviction scans for the oldest stamp (caches here hold ≤ ~1k entries).
pub struct Lru<K, V> {
    map: HashMap<K, (V, u64)>,
    cap: usize,
    tick: u64,
}

impl<K: Hash + Eq + Clone, V> Lru<K, V> {
    pub fn new(cap: usize) -> Self {
        Lru { map: HashMap::new(), cap, tick: 0 }
    }
    pub fn get(&mut self, k: &K) -> Option<&mut V> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(k).map(|e| {
            e.1 = t;
            &mut e.0
        })
    }
    pub fn contains(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }
    pub fn insert(&mut self, k: K, v: V) {
        self.tick += 1;
        if self.map.len() >= self.cap && !self.map.contains_key(&k)
            && let Some(old) = self.map.iter().min_by_key(|(_, e)| e.1).map(|(k, _)| k.clone()) {
                self.map.remove(&old);
            }
        self.map.insert(k, (v, self.tick));
    }
}

#[derive(Clone)]
pub struct CachedFiles {
    pub files: Arc<Vec<FileChange>>,
    pub stats: Vec<Option<LineStats>>,
    pub done: bool,
}

pub struct AppInit {
    pub repo_name: String,
    pub config: Config,
    pub registry: Registry,
    pub theme: Theme,
    pub depth: ColorDepth,
    pub ui_state: UiState,
    pub config_path: Option<PathBuf>,
    pub state_path: Option<PathBuf>,
    pub gens: Arc<Gens>,
    pub now: i64,
    pub clock: Instant,
    pub size: (u16, u16),
}

pub struct App {
    pub repo_name: String,
    pub config: Config,
    pub registry: Registry,
    pub theme: Theme,
    pub depth: ColorDepth,
    pub ui_state: UiState,
    pub config_path: Option<PathBuf>,
    pub state_path: Option<PathBuf>,
    pub gens: Arc<Gens>,
    /// Wall clock (epoch seconds) for date text.
    pub now: i64,
    /// Monotonic clock for debouncing; set by the main loop (or tests).
    pub clock: Instant,
    pub size: (u16, u16),

    pub tab: Tab,
    pub focus: Focus,
    pub fullscreen: bool,

    pub refs: Option<RefsSnapshot>,
    pub fetched_at: Option<i64>,
    pub ahead: HashSet<CommitId>,
    pub behind: HashSet<CommitId>,

    pub scope: HistoryScope,
    pub session: u64,
    pub history: Option<SharedHistory>,
    pub history_len: usize,
    pub history_done: bool,
    pub rows: HashMap<usize, CommitRow>,
    requested_rows: HashSet<usize>,
    pub selected: usize,
    pub list_scroll: usize,
    selected_id: Option<CommitId>,
    reselect: Option<CommitId>,
    /// A file to select by path when the next file list is installed (leaving compare).
    files_restore: Option<String>,
    /// A range anchor to restore by commit once its row is known (leaving compare).
    anchor_restore: Option<CommitId>,
    /// Commits covered by a range's diff, by its ends.
    range_count: Option<((CommitId, CommitId), usize)>,
    commit_gen: u64,

    pub detail: Option<CommitDetail>,
    pub header_expanded: bool,
    pub files: Option<Arc<Vec<FileChange>>>,
    /// The list `files` shows, and the one the selection wants (they differ while loading).
    files_of: Option<FilesOf>,
    files_wanted: Option<FilesOf>,
    /// `V` (or Shift-click): the other end of a contiguous range ending at `selected`.
    pub range_anchor: Option<usize>,
    pub stats: Vec<Option<LineStats>>,
    pub stats_done: bool,
    /// Listing the selected commit's files failed.
    pub files_error: Option<String>,
    pub file_sel: usize,
    pub file_scroll: usize,
    /// Tree view: the directory row the cursor rests on (None: on the selected file).
    tree_dir: Option<String>,
    /// Collapsed directories of the tree view, by path.
    collapsed: HashSet<String>,
    /// Rows of the files pane (see `tree.rs`), rebuilt when the list or view changes.
    file_rows: Vec<tree::FileRow>,
    file_cache: Lru<FilesOf, CachedFiles>,
    prefetching: HashSet<CommitId>,

    pub diff: Option<DiffState>,
    /// The diff the selection wants; differs from `diff.key` while loading.
    pub diff_wanted: Option<DiffKey>,
    pub diff_error: Option<(DiffKey, String)>,
    diff_cache: Lru<DiffKey, Arc<gitty_core::diff::FileDiff>>,
    diff_deadline: Option<Instant>,
    last_diff_request: Option<Instant>,
    file_gen: u64,
    force_text: bool,
    /// Syntax spans by blob; None = no colour (unknown language, over the limits).
    hl_cache: Lru<HlKey, Option<Arc<Highlights>>>,
    hl_pending: HashSet<HlKey>,

    pub date_mode: DateMode,
    pub density: Density,
    pub split_pref: Option<bool>,
    /// `W`: wrap long diff lines instead of scrolling horizontally.
    pub wrap: bool,
    pub ws: WsMode,

    pub overlay: Option<Overlay>,
    pub toast: Option<Toast>,
    pub quit: bool,
    pub suspend: bool,
    pub dirty: bool,
    outbox: Vec<Request>,
    /// Escape sequences (OSC 52) for main to write after the next frame.
    pub osc_out: Vec<String>,
    pub hits: Hits,

    pub changes: changes::Changes,
    /// The terminal has focus (FocusGained/FocusLost); the status backstop runs only then.
    pub focused: bool,
    index_mark: Option<gitty_core::watch::IndexMark>,
    pub net: Option<net::NetJob>,
    /// (gitty executable, trampoline socket) for interactive network jobs.
    pub askpass: Option<(PathBuf, PathBuf)>,
    pub ask_handle: Option<crate::askpass::AskHandle>,
    asks: std::collections::VecDeque<crate::askpass::Ask>,
    /// The last fetch (or launch); auto-fetch runs `auto_fetch_minutes` after it.
    last_fetch: Instant,
    /// A remote whose credentials auto-fetch could not supply; auto-fetch is off until a manual
    /// fetch succeeds.
    pub needs_auth: Option<String>,
    /// When a repository counts as large enough to tune (lowered in tests).
    pub tune_thresholds: gitty_core::tune::Thresholds,
    last_tune: Option<Instant>,
    tune_announced: bool,
    /// The user dismissed a credential prompt of the running job.
    prompt_cancelled: bool,
    /// A Diverged question waiting for the open overlay to close.
    pending_diverged: bool,
    /// The stale index.lock offer, waiting for the open overlay to close.
    pending_stale_lock: Option<crate::write::LockId>,
    /// The last auto-fetch failed: its details, until a fetch works.
    bg_failure: Option<String>,
    pub search: search::Search,
    /// Compare mode (`b`); the history pane lists its commits instead.
    pub compare: Option<compare::CompareMode>,
    /// A tool for the main loop to run with the terminal handed over.
    pub external: Option<crate::external::External>,
    /// The working tree, for opening files in the editor (set by the main loop).
    pub workdir: Option<PathBuf>,
    last_click: Option<(Instant, u16, u16)>,
    compare_gen: u64,
    /// Rows per search request (lowered in tests).
    pub search_chunk: usize,
    pub keymap: crate::keymap::Keymap,
    /// Problems in `[keys]`, for the startup toast.
    pub key_warnings: Vec<String>,
}

impl App {
    pub fn new(i: AppInit) -> App {
        let (keymap, key_warnings) = crate::keymap::Keymap::from_config(&i.config.keys);
        let scope = if i.ui_state.scope_all { HistoryScope::AllRefs } else { HistoryScope::HeadAndUpstream };
        let mut app = App {
            repo_name: i.repo_name,
            date_mode: i.config.date_mode,
            density: i.config.density,
            ws: i.config.whitespace,
            config: i.config,
            registry: i.registry,
            theme: i.theme,
            depth: i.depth,
            ui_state: i.ui_state,
            config_path: i.config_path,
            state_path: i.state_path,
            gens: i.gens,
            now: i.now,
            clock: i.clock,
            size: i.size,
            tab: Tab::History,
            focus: Focus::History,
            fullscreen: false,
            refs: None,
            fetched_at: None,
            ahead: HashSet::new(),
            behind: HashSet::new(),
            scope,
            session: 0,
            history: None,
            history_len: 0,
            history_done: false,
            rows: HashMap::new(),
            requested_rows: HashSet::new(),
            selected: 0,
            list_scroll: 0,
            selected_id: None,
            reselect: None,
            files_restore: None,
            anchor_restore: None,
            range_count: None,
            commit_gen: 0,
            detail: None,
            header_expanded: false,
            files: None,
            files_of: None,
            files_wanted: None,
            range_anchor: None,
            stats: Vec::new(),
            stats_done: false,
            files_error: None,
            file_sel: 0,
            file_scroll: 0,
            tree_dir: None,
            collapsed: HashSet::new(),
            file_rows: Vec::new(),
            file_cache: Lru::new(1024),
            prefetching: HashSet::new(),
            diff: None,
            diff_wanted: None,
            diff_error: None,
            diff_cache: Lru::new(200),
            hl_cache: Lru::new(64),
            hl_pending: HashSet::new(),
            diff_deadline: None,
            last_diff_request: None,
            file_gen: 0,
            force_text: false,
            split_pref: None,
            wrap: false,
            overlay: None,
            toast: None,
            quit: false,
            suspend: false,
            dirty: true,
            outbox: vec![Request::Refs],
            osc_out: Vec::new(),
            hits: Hits::default(),
            changes: changes::Changes::default(),
            focused: false,
            index_mark: None,
            net: None,
            askpass: None,
            ask_handle: None,
            asks: Default::default(),
            last_fetch: i.clock,
            needs_auth: None,
            tune_thresholds: gitty_core::tune::Thresholds::DEFAULT,
            last_tune: None,
            tune_announced: false,
            prompt_cancelled: false,
            pending_diverged: false,
            pending_stale_lock: None,
            bg_failure: None,
            search: Default::default(),
            compare: None,
            external: None,
            workdir: None,
            last_click: None,
            compare_gen: 0,
            search_chunk: search::SEARCH_CHUNK,
            keymap,
            key_warnings,
        };
        app.request_status();
        app
    }

    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.outbox)
    }
    pub fn take_requests_peek(&self) -> &[Request] {
        &self.outbox
    }
    pub fn selected_id(&self) -> Option<CommitId> {
        self.selected_id
    }
    /// The commit whose file list is shown (None for a range).
    pub fn files_for(&self) -> Option<CommitId> {
        match self.files_of? {
            FilesOf::Commit(id) => Some(id),
            FilesOf::Range { .. } | FilesOf::Between { .. } => None,
        }
    }
    pub fn files_of(&self) -> Option<FilesOf> {
        self.files_of
    }
    /// (oldest, newest) history indices of the selected range; None without a range.
    pub fn selected_range(&self) -> Option<(usize, usize)> {
        let a = self.range_anchor.filter(|&a| a < self.history_len)?;
        Some((a.max(self.selected), a.min(self.selected)))
    }
    pub fn selected_row(&self) -> Option<&CommitRow> {
        self.rows.get(&self.selected).filter(|r| Some(r.id) == self.selected_id)
    }
    /// How many commits outside the selected rows the range's diff includes (merged side
    /// branches, or unrelated history in the all-refs scope); None when none or unknown.
    pub fn range_extra(&self) -> Option<usize> {
        let (oldest, newest) = self.selected_range()?;
        let ends = (self.history_id(oldest)?, self.history_id(newest)?);
        let (e, extra) = self.range_count?;
        (e == ends && extra > 0).then_some(extra)
    }
    pub fn current_file(&self) -> Option<&FileChange> {
        self.files.as_ref()?.get(self.file_sel)
    }
    /// The error for the wanted diff, if loading it failed.
    pub fn wanted_diff_error(&self) -> Option<&str> {
        self.diff_error.as_ref().filter(|(k, _)| Some(k) == self.diff_wanted.as_ref()).map(|(_, e)| e.as_str())
    }
    pub fn diff_loading(&self) -> bool {
        let files_pending = self.selected_id.is_some() && self.files.is_none() && self.files_error.is_none();
        let diff_pending = self.wanted_diff_error().is_none() && self.diff_wanted.is_some() && self.diff.as_ref().map(|d| &d.key) != self.diff_wanted.as_ref();
        files_pending || diff_pending
    }
    pub fn prefetch_in_flight(&self) -> usize {
        self.prefetching.len()
    }

    // ---- layout ----

    pub fn header_height(&self) -> u16 {
        if !self.header_expanded {
            return 3;
        }
        let body = self.detail.as_ref().map_or(0, |d| d.body.lines().count()).min(12) as u16;
        3 + body + u16::from(body > 0) + 1
    }

    pub fn panes(&self) -> Panes {
        let input = LayoutInput {
            width: self.size.0,
            height: self.size.1,
            focus: self.focus,
            fullscreen: self.fullscreen,
            header_height: self.header_height(),
            file_count: self.file_rows.len(),
            ui: &self.ui_state,
        };
        match self.tab {
            Tab::History => layout::compute(&input),
            Tab::Changes => layout::compute_changes(&input),
        }
    }

    pub fn mode(&self) -> Mode {
        Mode::of(self.size.0)
    }

    pub fn row_height(&self) -> usize {
        match self.density {
            Density::Compact => 1,
            Density::Comfortable => 2,
        }
    }

    /// Commits visible in the history pane (one title row).
    pub fn list_capacity(&self) -> usize {
        let p = self.panes();
        let h = p.history.map_or_else(|| self.size.1.saturating_sub(3), |r| r.height.saturating_sub(1));
        (h as usize / self.row_height()).max(1)
    }

    pub fn files_capacity(&self) -> usize {
        let p = self.panes();
        p.files.map_or(1, |r| r.height.saturating_sub(1) as usize).max(1)
    }

    pub fn diff_capacity(&self) -> usize {
        let p = self.panes();
        let banners = self.diff.as_ref().map_or(0, |d| d.banners().len()) + usize::from(self.changes_notice().is_some());
        p.diff.map_or(1, |r| (r.height as usize).saturating_sub(1 + banners)).max(1)
    }

    /// Width of one split half's line-number gutter plus marker.
    fn split_fits(&self) -> bool {
        let Some(diff) = self.panes().diff else { return false };
        let digits = self.diff.as_ref().map_or(4, |d| digits(d.diff.old.len().max(d.diff.new.len())));
        let half = digits + 3 + 50;
        diff.width as usize > 2 * half
    }

    pub fn split_active(&self) -> bool {
        self.split_pref.unwrap_or_else(|| self.size.0 >= self.config.split_threshold && self.split_fits())
    }

    fn diff_opts(&self) -> DiffOptions {
        DiffOptions { algorithm: self.config.diff_algorithm, ws: self.ws }
    }

    // ---- messages ----

    pub fn handle_msg(&mut self, m: Msg) {
        self.dirty = true;
        let Some(m) = self.handle_changes_msg(m) else { return };
        let Some(m) = self.handle_net_msg(m) else { return };
        let Some(m) = self.handle_search_msg(m) else { return };
        let Some(m) = self.handle_compare_msg(m) else { return };
        match m {
            Msg::Refs { refs, fetched_at } => {
                if let (Some(local), Some((_, upstream))) = (refs.head_id(), refs.upstream.clone()) {
                    self.outbox.push(Request::AheadBehind { local, upstream });
                }
                let moved = self.refs.as_ref().is_none_or(|old| old.tips(self.scope) != refs.tips(self.scope));
                self.refs = Some(refs);
                self.fetched_at = fetched_at;
                if moved {
                    // a refresh after a commit or fetch keeps the selected commit selected
                    if self.history.is_some() {
                        self.reselect = self.history_selection().or(self.reselect);
                    }
                    self.start_walk();
                }
            }
            Msg::HistoryStarted { session, history } => {
                if session == self.session {
                    self.history = Some(history);
                }
            }
            Msg::HistoryProgress { session, len, done } => {
                if session != self.session {
                    return;
                }
                let old = self.history_len;
                let finished = done && !self.history_done;
                self.history_len = len;
                self.history_done = done;
                if finished {
                    self.request_tune();
                }
                if let Some(want) = self.reselect {
                    let found = self.history.as_ref().and_then(|h| {
                        let h = h.read().unwrap_or_else(PoisonError::into_inner);
                        (old..len.min(h.len())).find(|&i| h.id(i) == want)
                    });
                    if let Some(i) = found {
                        self.reselect = None;
                        self.restore_anchor();
                        self.select_at(i);
                    } else if done {
                        // gone (amended, reset): show row 0 rather than whatever was on screen
                        self.reselect = None;
                        self.anchor_restore = None;
                        self.select_at(0);
                    }
                }
                if self.reselect.is_none() && self.selected_id.is_none() && len > 0 {
                    self.select_at(0);
                }
                self.request_visible_rows();
                self.request_search_chunks();
            }
            Msg::Rows { session, rows } => {
                if session == self.session {
                    self.rows.extend(rows);
                }
            }
            Msg::AheadBehind { local, upstream, ab } => {
                let current = self.refs.as_ref().map(|r| (r.head_id(), r.upstream.as_ref().map(|u| u.1)));
                if current == Some((Some(local), Some(upstream))) {
                    self.ahead = ab.ahead.into_iter().collect();
                    self.behind = ab.behind.into_iter().collect();
                }
            }
            Msg::Detail { generation, detail } => {
                if generation == self.commit_gen && Some(detail.row.id) == self.selected_id {
                    self.detail = Some(detail);
                }
            }
            Msg::Files { generation, of, files, prefetch } => {
                if let (true, FilesOf::Commit(id)) = (prefetch, of) {
                    self.prefetching.remove(&id);
                }
                if !self.file_cache.contains(&of) {
                    let n = files.len();
                    self.file_cache.insert(of, CachedFiles { files: files.clone(), stats: vec![None; n], done: n == 0 });
                }
                if !prefetch && generation == self.commit_gen && Some(of) == self.files_wanted && self.files_of != Some(of) {
                    let cached = self.file_cache.get(&of).cloned();
                    if let Some(c) = cached {
                        self.install_files(of, c);
                    }
                }
            }
            Msg::FilesError { generation, of, prefetch, detail } => {
                if prefetch {
                    if let FilesOf::Commit(id) = of {
                        self.prefetching.remove(&id);
                    }
                } else if generation == self.commit_gen && Some(of) == self.files_wanted {
                    self.files_error = Some(detail);
                }
            }
            Msg::Stats { of, start, stats, done } => {
                if let Some(c) = self.file_cache.get(&of) {
                    for (i, s) in stats.iter().enumerate() {
                        if let Some(slot) = c.stats.get_mut(start + i) {
                            *slot = *s;
                        }
                    }
                    c.done |= done;
                }
                if self.files_of == Some(of) {
                    for (i, s) in stats.into_iter().enumerate() {
                        if let Some(slot) = self.stats.get_mut(start + i) {
                            *slot = s;
                        }
                    }
                    self.stats_done |= done;
                }
            }
            Msg::Diff { generation, key, diff } => {
                self.diff_cache.insert(key.clone(), diff.clone());
                if generation == self.file_gen && Some(&key) == self.diff_wanted.as_ref() {
                    self.install_diff(key, diff);
                }
            }
            Msg::IntralineDone { key } => {
                let split = self.split_active();
                if let Some(d) = self.diff.as_mut().filter(|d| d.key == key) {
                    d.apply_ready_pairing(split);
                }
            }
            Msg::DiffError { generation, key, detail } => {
                if generation == self.file_gen && Some(&key) == self.diff_wanted.as_ref() {
                    self.diff_error = Some((key, detail));
                }
            }
            Msg::Highlighted { key, spans, cancelled } => {
                self.hl_pending.remove(&key);
                if !cancelled {
                    self.hl_cache.insert(key, spans);
                }
            }
            Msg::Error { what, detail } => self.toast = Some(Toast { what, detail, error: true }),
            Msg::RangeCount { oldest, newest, extra } => self.range_count = Some(((oldest, newest), extra)),
            // handled by handle_changes_msg
            Msg::Status { .. } | Msg::ChangeDiff { .. } | Msg::ChangeDiffError { .. } | Msg::WriteLog { .. } | Msg::WriteDone { .. } | Msg::Changed(_) | Msg::HeadMessage { .. } | Msg::StatusSlow | Msg::StaleIndexLock { .. } => {}
            Msg::NetStarted { .. } | Msg::NetProgress { .. } | Msg::NetDone { .. } | Msg::Ask(_) | Msg::Tuned { .. } => {}
            Msg::SearchHits { .. } | Msg::SearchPaths { .. } | Msg::CommitRows { .. } | Msg::Compare { .. } => {}
        }
    }

    pub fn handle_resize(&mut self, w: u16, h: u16) {
        if self.size == (w, h) {
            return;
        }
        let before = self.split_active();
        self.size = (w, h);
        let after = self.split_active();
        if let Some(d) = self.diff.as_mut() {
            d.remap_cursor(before, after);
        }
        self.ensure_list_visible();
        self.ensure_diff_visible();
        self.request_visible_rows();
        self.dirty = true;
    }

    /// Fires the debounced diff request once its deadline has passed.
    pub fn tick(&mut self, at: Instant) {
        self.clock = at;
        if self.diff_deadline.is_some_and(|d| d <= at) {
            self.fire_diff();
        }
        self.tick_changes(at);
        self.tick_net(at);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [self.diff_deadline, self.status_deadline(), self.auto_fetch_deadline()].into_iter().flatten().min()
    }

    /// Earliest epoch second at which a visible relative date changes.
    pub fn date_refresh_at(&self) -> Option<i64> {
        if self.date_mode == DateMode::Absolute {
            return None;
        }
        let n = self.list_capacity();
        (self.list_scroll..(self.list_scroll + n).min(self.history_len))
            .filter_map(|i| self.rows.get(&i))
            .filter_map(|r| next_threshold(r.author.time, self.now))
            .min()
    }

    // ---- history ----

    fn start_walk(&mut self) {
        let Some(refs) = &self.refs else { return };
        let tips = refs.tips(self.scope);
        self.session = Gens::bump(&self.gens.session);
        // a count's excluded rows belong to the old walk's list
        self.range_count = None;
        self.history = None;
        self.history_len = 0;
        self.history_done = false;
        self.rows.clear();
        self.requested_rows.clear();
        // indices of the old walk mean nothing in the new one
        self.range_anchor = None;
        self.outbox.push(Request::Walk { session: self.session, tips });
        self.restart_search();
    }

    pub fn toggle_scope(&mut self) {
        self.scope = match self.scope {
            HistoryScope::HeadAndUpstream => HistoryScope::AllRefs,
            HistoryScope::AllRefs => HistoryScope::HeadAndUpstream,
        };
        self.ui_state.scope_all = self.scope == HistoryScope::AllRefs;
        self.save_state();
        // a second toggle before the first walk found the commit keeps the original target
        self.reselect = self.selected_id.take().or(self.reselect.take());
        self.selected = 0;
        self.detail = None;
        self.files = None;
        self.file_rows.clear();
        self.files_of = None;
        self.files_wanted = None;
        self.range_anchor = None;
        self.files_error = None;
        self.stats.clear();
        self.diff = None;
        self.diff_wanted = None;
        self.diff_deadline = None;
        self.commit_gen = Gens::bump(&self.gens.commit);
        self.file_gen = Gens::bump(&self.gens.file);
        self.list_scroll = 0;
        self.start_walk();
    }

    /// Selects the file saved by path once the wanted list is installed; a list still loading
    /// keeps the request for its arrival.
    pub(super) fn apply_files_restore(&mut self) {
        if self.files_of.is_none() || self.files_of != self.files_wanted {
            return;
        }
        if let Some(path) = self.files_restore.take()
            && let Some(i) = self.files.as_ref().and_then(|f| f.iter().position(|f| f.path == path))
        {
            self.select_file(i);
        }
    }

    /// Sets the range anchor saved by commit, if its row is in the walk so far.
    pub(super) fn restore_anchor(&mut self) {
        let Some(id) = self.anchor_restore.take() else { return };
        let Some(h) = self.history.clone() else { return };
        let h = h.read().unwrap_or_else(PoisonError::into_inner);
        self.range_anchor = (0..h.len()).find(|&i| h.id(i) == id);
    }

    /// The commit at history row `i`.
    pub fn history_id_at(&self, i: usize) -> Option<CommitId> {
        self.history_id(i)
    }

    fn history_id(&self, i: usize) -> Option<CommitId> {
        let h = self.history.as_ref()?.read().unwrap_or_else(PoisonError::into_inner);
        (i < h.len()).then(|| h.id(i))
    }

    /// `V`: starts a range at the selection, or ends the current one.
    pub fn toggle_range(&mut self) {
        self.range_anchor = match self.range_anchor {
            Some(_) => None,
            None => Some(self.selected),
        };
        self.load_files();
    }

    pub fn end_range(&mut self) {
        if self.range_anchor.take().is_some() {
            self.load_files();
        }
    }

    /// Shift- or Ctrl-click: extends a range (anchored at the selection) to `idx`.
    pub fn extend_range(&mut self, idx: usize) {
        if self.range_anchor.is_none() {
            self.range_anchor = Some(self.selected);
        }
        self.select(idx);
    }

    /// Selects history row `idx` on the user's behalf (cancels a pending re-selection).
    pub fn select(&mut self, idx: usize) {
        self.reselect = None;
        self.files_restore = None;
        self.anchor_restore = None;
        self.select_at(idx);
    }

    fn select_at(&mut self, idx: usize) {
        if self.history_len == 0 {
            return;
        }
        let idx = idx.min(self.history_len - 1);
        let Some(id) = self.history_id(idx) else { return };
        self.selected = idx;
        self.ensure_list_visible();
        self.request_visible_rows();
        if self.compare.is_some() {
            // compare mode shows its own commits; the history row waits for Esc
            return;
        }
        self.show_commit(id);
    }

    /// Makes `id` the commit whose detail, files and diff are shown.
    fn show_commit(&mut self, id: CommitId) {
        if Some(id) != self.selected_id {
            self.selected_id = Some(id);
            self.commit_gen = Gens::bump(&self.gens.commit);
            self.detail = None;
            self.outbox.push(Request::Detail { generation: self.commit_gen, id });
        }
        self.load_files();
    }

    /// The file list the selection asks for: the range when one is selected, else the commit.
    fn files_target(&self) -> Option<FilesOf> {
        if let Some(c) = &self.compare {
            return match c.tab {
                compare::CompareTab::Files => self.compare_files(),
                _ => self.selected_id.map(FilesOf::Commit),
            };
        }
        let id = self.selected_id?;
        match self.selected_range() {
            Some((oldest, newest)) if oldest != newest => {
                Some(FilesOf::Range { oldest: self.history_id(oldest)?, newest: self.history_id(newest)? })
            }
            _ => Some(FilesOf::Commit(id)),
        }
    }

    /// Shows (or requests) the wanted file list when it changed.
    fn load_files(&mut self) {
        let Some(of) = self.files_target() else { return };
        if self.files_wanted == Some(of) {
            return;
        }
        self.files_wanted = Some(of);
        if let FilesOf::Range { oldest, newest } = of
            && self.range_count.is_none_or(|(ends, _)| ends != (oldest, newest))
        {
            let rows = self.selected_range().map_or_else(Vec::new, |(o, n)| (n..=o).filter_map(|i| self.history_id(i)).collect());
            self.outbox.push(Request::RangeCount { generation: self.commit_gen, oldest, newest, rows });
        }
        self.files = None;
        self.file_rows.clear();
        self.files_of = None;
        self.stats.clear();
        self.stats_done = false;
        self.files_error = None;
        self.file_sel = 0;
        self.file_scroll = 0;
        self.force_text = false;
        match self.file_cache.get(&of).cloned() {
            Some(c) => {
                if !c.done {
                    self.outbox.push(Request::Files { generation: self.commit_gen, of, prefetch: false });
                }
                self.install_files(of, c);
            }
            None => {
                // nothing to show for this commit yet: drop the previous commit's diff
                self.diff = None;
                self.diff_wanted = None;
                self.diff_deadline = None;
                self.file_gen = Gens::bump(&self.gens.file);
                self.outbox.push(Request::Files { generation: self.commit_gen, of, prefetch: false });
            }
        }
    }

    fn install_files(&mut self, of: FilesOf, c: CachedFiles) {
        self.files_of = Some(of);
        self.stats = c.stats;
        self.stats_done = c.done;
        let empty = c.files.is_empty();
        self.files = Some(c.files);
        self.refresh_file_rows();
        self.tree_dir = None;
        self.file_sel = self.first_file_row();
        // every file under a folded directory: the cursor rests on the first directory
        if !self.file_rows.iter().any(|r| matches!(r, tree::FileRow::File { .. })) {
            self.tree_dir = self.file_rows.first().and_then(|r| match r {
                tree::FileRow::Dir { path, .. } => Some(path.clone()),
                tree::FileRow::File { .. } => None,
            });
        }
        self.file_scroll = 0;
        if empty {
            self.diff = None;
            self.diff_wanted = None;
            self.diff_deadline = None;
            Gens::bump(&self.gens.file);
        } else {
            self.schedule_diff();
        }
        self.apply_files_restore();
        self.prefetch_neighbours();
    }

    fn prefetch_neighbours(&mut self) {
        let lo = self.selected.saturating_sub(NEIGHBOURS);
        let hi = (self.selected + NEIGHBOURS + 1).min(self.history_len);
        let ids: Vec<CommitId> = {
            let Some(h) = &self.history else { return };
            let h = h.read().unwrap_or_else(PoisonError::into_inner);
            (lo..hi.min(h.len())).filter(|&i| i != self.selected).map(|i| h.id(i)).collect()
        };
        for id in ids {
            if self.prefetching.len() >= MAX_PREFETCH_IN_FLIGHT {
                break;
            }
            if self.file_cache.contains(&FilesOf::Commit(id)) || self.prefetching.contains(&id) {
                continue;
            }
            self.prefetching.insert(id);
            self.outbox.push(Request::Files { generation: 0, of: FilesOf::Commit(id), prefetch: true });
        }
    }

    pub fn ensure_list_visible(&mut self) {
        let cap = self.list_capacity();
        if self.selected < self.list_scroll {
            self.list_scroll = self.selected;
        } else if self.selected >= self.list_scroll + cap {
            self.list_scroll = self.selected + 1 - cap;
        }
        self.list_scroll = self.list_scroll.min(self.history_len.saturating_sub(1));
    }

    pub fn ensure_files_visible(&mut self) {
        let cap = self.files_capacity();
        let cur = self.file_cursor();
        if cur < self.file_scroll {
            self.file_scroll = cur;
        } else if cur >= self.file_scroll + cap {
            self.file_scroll = cur + 1 - cap;
        }
    }

    pub fn ensure_diff_visible(&mut self) {
        let (cap, split, wrap) = (self.diff_capacity(), self.split_active(), self.diff_wrap());
        if let Some(d) = self.diff.as_mut() {
            d.ensure_visible(cap, split, wrap);
        }
    }

    /// Text widths of wrapped diff rows, matching `ui::diff`'s layout; None when not wrapping.
    pub fn diff_wrap(&self) -> Option<Wrap> {
        if !self.wrap {
            return None;
        }
        let width = u32::from(self.panes().diff?.width);
        let gutter = digits(self.diff.as_ref().map_or(1, |d| d.diff.old.len().max(d.diff.new.len()))) as u32 + 2;
        // `ui::diff` draws gutters, then the +/- marker and one space, then the text
        let text = |w: u32, gutters: u32| w.saturating_sub(gutters * gutter + 2).max(1);
        let tab = self.config.tab_size;
        Some(if self.split_active() {
            let left = width.saturating_sub(1) / 2;
            Wrap { left: text(left, 1), right: text(width.saturating_sub(left + 1), 1), tab }
        } else {
            let w = text(width, 2);
            Wrap { left: w, right: w, tab }
        })
    }

    /// Requests decoding for undecoded rows in and around the viewport.
    pub fn request_visible_rows(&mut self) {
        let Some(h) = &self.history else { return };
        let page = self.list_capacity();
        let lo = self.list_scroll.saturating_sub(page);
        let hi = (self.list_scroll + 2 * page).min(self.history_len);
        let ids: Vec<(usize, CommitId)> = {
            let h = h.read().unwrap_or_else(PoisonError::into_inner);
            (lo..hi.min(h.len()))
                .filter(|i| !self.rows.contains_key(i) && !self.requested_rows.contains(i))
                .map(|i| (i, h.id(i)))
                .collect()
        };
        for chunk in ids.chunks(ROWS_BATCH) {
            self.requested_rows.extend(chunk.iter().map(|(i, _)| *i));
            self.outbox.push(Request::Rows { session: self.session, ids: chunk.to_vec() });
        }
    }

    // ---- files and diff ----

    pub fn select_file(&mut self, i: usize) {
        let Some(n) = self.files.as_ref().map(|f| f.len()) else { return };
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        self.tree_dir = None;
        self.ensure_files_visible();
        if i == self.file_sel && self.diff_wanted.is_some() {
            return;
        }
        self.file_sel = i;
        self.force_text = false;
        self.ensure_files_visible();
        self.schedule_diff();
    }

    fn schedule_diff(&mut self) {
        self.file_gen = Gens::bump(&self.gens.file);
        // the bump cancels in-flight highlights; their replies only clear pending entries
        self.hl_pending.clear();
        self.diff_wanted = self.current_file().map(|f| DiffKey::of(f, self.diff_opts(), self.force_text));
        let idle = self.last_diff_request.is_none_or(|t| self.clock.saturating_duration_since(t) >= IDLE_BEFORE_LEADING_EDGE);
        self.last_diff_request = Some(self.clock);
        if idle {
            self.fire_diff();
        } else {
            self.diff_deadline = Some(self.clock + DIFF_DEBOUNCE);
        }
    }

    fn fire_diff(&mut self) {
        self.diff_deadline = None;
        let Some(file) = self.current_file().cloned() else { return };
        let opts = self.diff_opts();
        let key = DiffKey::of(&file, opts, self.force_text);
        self.diff_wanted = Some(key.clone());
        if self.diff.as_ref().is_some_and(|d| d.key == key) {
            self.request_highlights();
            return;
        }
        if let Some(d) = self.diff_cache.get(&key).cloned() {
            self.install_diff(key, d);
            return;
        }
        self.outbox.push(Request::Diff { generation: self.file_gen, file, opts, force_text: self.force_text });
    }

    fn install_diff(&mut self, key: DiffKey, diff: Arc<gitty_core::diff::FileDiff>) {
        let complete = (0..diff.changes.len()).all(|c| diff.intraline_ready(c).is_some());
        if !complete {
            self.outbox.push(Request::Intraline { generation: self.file_gen, key: key.clone(), diff: diff.clone() });
        }
        self.diff = Some(DiffState::new(key, diff));
        self.request_highlights();
    }

    fn hl_keys(key: &DiffKey) -> (Option<HlKey>, Option<HlKey>) {
        let old_path = key.old_path.as_ref().unwrap_or(&key.path);
        (key.old.map(|blob| HlKey { blob, path: old_path.clone() }), key.new.map(|blob| HlKey { blob, path: key.path.clone() }))
    }

    /// Highlights the new side, and the old side when its lines are shown (deletions, or
    /// context that may differ under a whitespace mode).
    fn request_highlights(&mut self) {
        let Some(d) = self.diff.as_ref().filter(|d| d.diff.is_text()) else { return };
        let (fd, (old, new)) = (d.diff.clone(), Self::hl_keys(&d.key));
        let old = old.filter(|_| fd.removed > 0 || self.ws != WsMode::Show);
        // spec §7: files past the large-text thresholds stay uncoloured even when shown
        if d.key.force_text && (is_large(&fd.old) || is_large(&fd.new)) {
            return;
        }
        for (key, text) in [(old, &fd.old), (new, &fd.new)] {
            let Some(key) = key else { continue };
            if text.is_empty() || self.hl_cache.contains(&key) || !self.hl_pending.insert(key.clone()) {
                continue;
            }
            self.outbox.push(Request::Highlight { generation: self.file_gen, key, text: text.clone() });
        }
    }

    /// Syntax spans for the installed diff's (old, new) sides, when ready.
    pub fn diff_highlights(&mut self) -> (Option<Arc<Highlights>>, Option<Arc<Highlights>>) {
        let Some(d) = &self.diff else { return (None, None) };
        let (old, new) = Self::hl_keys(&d.key);
        let mut get = |k: Option<HlKey>| k.and_then(|k| self.hl_cache.get(&k).cloned().flatten());
        (get(old), get(new))
    }

    /// Re-requests the current file's diff with new options (whitespace mode, force text).
    pub fn refresh_diff(&mut self) {
        if self.tab == Tab::Changes {
            self.request_change_diff();
        } else if self.current_file().is_some() {
            self.schedule_diff();
        }
    }

    pub fn force_show(&mut self) {
        let hidden = self.diff.as_ref().is_some_and(|d| {
            matches!(d.diff.class, gitty_core::diff::classify::FileClass::LargeText { .. } | gitty_core::diff::classify::FileClass::Generated { .. })
        });
        if hidden && !self.force_text {
            self.force_text = true;
            self.last_diff_request = None;
            self.schedule_diff();
        }
    }

    // ---- persistence and output ----

    pub fn save_state(&mut self) {
        if let Some(p) = &self.state_path
            && let Err(e) = self.ui_state.save(p) {
                self.toast = Some(Toast { what: "saving UI state".into(), detail: e.to_string(), error: true });
            }
    }

    pub fn copy(&mut self, text: &str) {
        self.osc_out.push(format!("\x1b]52;c;{}\x07", base64(text.as_bytes())));
        self.toast = Some(Toast { what: format!("Copied {text}"), detail: String::new(), error: false });
    }
}

fn is_large(t: &gitty_core::diff::text::Text) -> bool {
    use gitty_core::diff::classify::{LARGE_TEXT, LONG_LINE};
    t.bytes().len() as u64 > LARGE_TEXT || (0..t.len()).any(|i| t.line(i).len() > LONG_LINE as usize)
}

pub fn digits(n: u32) -> usize {
    n.max(1).ilog10() as usize + 1
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn lru_evicts_oldest() {
        let mut l = Lru::new(2);
        l.insert(1, "a");
        l.insert(2, "b");
        l.get(&1);
        l.insert(3, "c");
        assert!(l.contains(&1) && l.contains(&3) && !l.contains(&2));
    }

    #[test]
    fn digits_of() {
        assert_eq!((digits(0), digits(9), digits(10), digits(12345)), (1, 1, 2, 5));
    }
}
