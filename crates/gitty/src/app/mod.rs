//! Application state. Pure: inputs and worker messages go in, [`Request`]s collect in an
//! outbox, and `ui::draw` renders. Nothing here touches git.

pub mod diffstate;
mod input;

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
use gitty_core::refs::{HistoryScope, RefsSnapshot};
use ratatui::layout::Rect;

use crate::config::{Config, Density, UiState};
use crate::dates::{DateMode, next_threshold};
use crate::msg::{DiffKey, Gens, Msg, Request, SharedHistory};
use crate::theme::{ColorDepth, Registry, Theme};
use crate::ui::layout::{self, LayoutInput, Mode, Panes, Sep};
use diffstate::DiffState;

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
    Help,
    ErrorDetail,
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
    pub diff_first: usize,
    /// (x, width) of the old and new line-number gutters of the (left) diff side.
    pub diff_old_gutter: (u16, u16),
    pub diff_new_gutter: (u16, u16),
    pub tabs: Vec<(Rect, Tab)>,
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
        if self.map.len() >= self.cap && !self.map.contains_key(&k) {
            if let Some(old) = self.map.iter().min_by_key(|(_, e)| e.1).map(|(k, _)| k.clone()) {
                self.map.remove(&old);
            }
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
    commit_gen: u64,

    pub detail: Option<CommitDetail>,
    pub header_expanded: bool,
    pub files: Option<Arc<Vec<FileChange>>>,
    files_id: Option<CommitId>,
    pub stats: Vec<Option<LineStats>>,
    pub stats_done: bool,
    pub file_sel: usize,
    pub file_scroll: usize,
    file_cache: Lru<CommitId, CachedFiles>,
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

    pub date_mode: DateMode,
    pub density: Density,
    pub split_pref: Option<bool>,
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
}

impl App {
    pub fn new(i: AppInit) -> App {
        let scope = if i.ui_state.scope_all { HistoryScope::AllRefs } else { HistoryScope::HeadAndUpstream };
        App {
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
            commit_gen: 0,
            detail: None,
            header_expanded: false,
            files: None,
            files_id: None,
            stats: Vec::new(),
            stats_done: false,
            file_sel: 0,
            file_scroll: 0,
            file_cache: Lru::new(1024),
            prefetching: HashSet::new(),
            diff: None,
            diff_wanted: None,
            diff_error: None,
            diff_cache: Lru::new(200),
            diff_deadline: None,
            last_diff_request: None,
            file_gen: 0,
            force_text: false,
            split_pref: None,
            overlay: None,
            toast: None,
            quit: false,
            suspend: false,
            dirty: true,
            outbox: vec![Request::Refs],
            osc_out: Vec::new(),
            hits: Hits::default(),
        }
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
    /// The commit whose file list is shown.
    pub fn files_for(&self) -> Option<CommitId> {
        self.files_id
    }
    pub fn selected_row(&self) -> Option<&CommitRow> {
        self.rows.get(&self.selected).filter(|r| Some(r.id) == self.selected_id)
    }
    pub fn current_file(&self) -> Option<&FileChange> {
        self.files.as_ref()?.get(self.file_sel)
    }
    /// The error for the wanted diff, if loading it failed.
    pub fn wanted_diff_error(&self) -> Option<&str> {
        self.diff_error.as_ref().filter(|(k, _)| Some(k) == self.diff_wanted.as_ref()).map(|(_, e)| e.as_str())
    }
    pub fn diff_loading(&self) -> bool {
        self.wanted_diff_error().is_none() && self.diff_wanted.is_some() && self.diff.as_ref().map(|d| &d.key) != self.diff_wanted.as_ref()
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
        layout::compute(&LayoutInput {
            width: self.size.0,
            height: self.size.1,
            focus: self.focus,
            fullscreen: self.fullscreen,
            header_height: self.header_height(),
            file_count: self.files.as_ref().map_or(0, |f| f.len()),
            ui: &self.ui_state,
        })
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
        let banners = self.diff.as_ref().map_or(0, |d| d.banners().len());
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
        match m {
            Msg::Refs { refs, fetched_at } => {
                if let (Some(local), Some((_, upstream))) = (refs.head_id(), refs.upstream.clone()) {
                    self.outbox.push(Request::AheadBehind { local, upstream });
                }
                self.refs = Some(refs);
                self.fetched_at = fetched_at;
                self.start_walk();
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
                self.history_len = len;
                self.history_done = done;
                if let Some(want) = self.reselect {
                    let found = self.history.as_ref().and_then(|h| {
                        let h = h.read().unwrap_or_else(PoisonError::into_inner);
                        (old..len.min(h.len())).find(|&i| h.id(i) == want)
                    });
                    if let Some(i) = found {
                        self.reselect = None;
                        self.select(i);
                    } else if done {
                        self.reselect = None;
                    }
                }
                if self.reselect.is_none() && self.selected_id.is_none() && len > 0 {
                    self.select(0);
                }
                self.request_visible_rows();
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
            Msg::Files { generation, id, files, prefetch } => {
                if prefetch {
                    self.prefetching.remove(&id);
                }
                if !self.file_cache.contains(&id) {
                    let n = files.len();
                    self.file_cache.insert(id, CachedFiles { files: files.clone(), stats: vec![None; n], done: n == 0 });
                }
                if !prefetch && generation == self.commit_gen && Some(id) == self.selected_id && self.files_id != Some(id) {
                    let cached = self.file_cache.get(&id).cloned();
                    if let Some(c) = cached {
                        self.install_files(id, c);
                    }
                }
            }
            Msg::Stats { id, start, stats, done } => {
                if let Some(c) = self.file_cache.get(&id) {
                    for (i, s) in stats.iter().enumerate() {
                        if let Some(slot) = c.stats.get_mut(start + i) {
                            *slot = *s;
                        }
                    }
                    c.done |= done;
                }
                if self.files_id == Some(id) {
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
                if let Some(d) = self.diff.as_mut().filter(|d| d.key == key) {
                    d.apply_ready_pairing();
                }
            }
            Msg::DiffError { generation, key, detail } => {
                if generation == self.file_gen && Some(&key) == self.diff_wanted.as_ref() {
                    self.diff_error = Some((key, detail));
                }
            }
            Msg::Error { what, detail } => self.toast = Some(Toast { what, detail, error: true }),
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
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.diff_deadline
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
        self.history = None;
        self.history_len = 0;
        self.history_done = false;
        self.rows.clear();
        self.requested_rows.clear();
        self.outbox.push(Request::Walk { session: self.session, tips });
    }

    pub fn toggle_scope(&mut self) {
        self.scope = match self.scope {
            HistoryScope::HeadAndUpstream => HistoryScope::AllRefs,
            HistoryScope::AllRefs => HistoryScope::HeadAndUpstream,
        };
        self.ui_state.scope_all = self.scope == HistoryScope::AllRefs;
        self.save_state();
        self.reselect = self.selected_id.take();
        self.selected = 0;
        self.list_scroll = 0;
        self.start_walk();
    }

    fn history_id(&self, i: usize) -> Option<CommitId> {
        let h = self.history.as_ref()?.read().unwrap_or_else(PoisonError::into_inner);
        (i < h.len()).then(|| h.id(i))
    }

    pub fn select(&mut self, idx: usize) {
        if self.history_len == 0 {
            return;
        }
        let idx = idx.min(self.history_len - 1);
        let Some(id) = self.history_id(idx) else { return };
        self.selected = idx;
        self.ensure_list_visible();
        self.request_visible_rows();
        if Some(id) == self.selected_id {
            return;
        }
        self.selected_id = Some(id);
        self.commit_gen = Gens::bump(&self.gens.commit);
        self.detail = None;
        self.files = None;
        self.files_id = None;
        self.stats.clear();
        self.stats_done = false;
        self.file_sel = 0;
        self.file_scroll = 0;
        self.force_text = false;
        self.outbox.push(Request::Detail { generation: self.commit_gen, id });
        match self.file_cache.get(&id).cloned() {
            Some(c) => {
                if !c.done {
                    self.outbox.push(Request::Files { generation: self.commit_gen, id, prefetch: false });
                }
                self.install_files(id, c);
            }
            None => self.outbox.push(Request::Files { generation: self.commit_gen, id, prefetch: false }),
        }
    }

    fn install_files(&mut self, id: CommitId, c: CachedFiles) {
        self.files_id = Some(id);
        self.stats = c.stats;
        self.stats_done = c.done;
        let empty = c.files.is_empty();
        self.files = Some(c.files);
        self.file_sel = 0;
        self.file_scroll = 0;
        if empty {
            self.diff = None;
            self.diff_wanted = None;
            self.diff_deadline = None;
            Gens::bump(&self.gens.file);
        } else {
            self.schedule_diff();
        }
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
            if self.file_cache.contains(&id) || self.prefetching.contains(&id) {
                continue;
            }
            self.prefetching.insert(id);
            self.outbox.push(Request::Files { generation: 0, id, prefetch: true });
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
        if self.file_sel < self.file_scroll {
            self.file_scroll = self.file_sel;
        } else if self.file_sel >= self.file_scroll + cap {
            self.file_scroll = self.file_sel + 1 - cap;
        }
    }

    pub fn ensure_diff_visible(&mut self) {
        let cap = self.diff_capacity();
        if let Some(d) = self.diff.as_mut() {
            d.ensure_visible(cap);
        }
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
    }

    /// Re-requests the current file's diff with new options (whitespace mode, force text).
    pub fn refresh_diff(&mut self) {
        if self.current_file().is_some() {
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
        if let Some(p) = &self.state_path {
            if let Err(e) = self.ui_state.save(p) {
                self.toast = Some(Toast { what: "saving UI state".into(), detail: e.to_string(), error: true });
            }
        }
    }

    pub fn copy(&mut self, text: &str) {
        self.osc_out.push(format!("\x1b]52;c;{}\x07", base64(text.as_bytes())));
        self.toast = Some(Toast { what: format!("Copied {text}"), detail: String::new(), error: false });
    }
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
