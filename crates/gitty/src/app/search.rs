//! History search (spec §11.2): `/` opens a bar at the bottom; matches stream in from the reader
//! pool chunk by chunk and the list stays unfiltered. `n`/`N` move between the matches found
//! so far.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gitty_core::CommitId;
use gitty_core::search::Query;

use super::{App, Toast};
use crate::editor::Editor;
use crate::msg::{Gens, Msg, Request};

/// Rows matched per request.
pub const SEARCH_CHUNK: usize = 20_000;

#[derive(Default)]
pub struct Search {
    /// Open while the query is being typed; it takes every key.
    pub bar: Option<Editor>,
    /// The query as typed, for the bottom bar.
    pub input: String,
    pub query: Option<Arc<Query>>,
    pub generation: u64,
    pub hits: BTreeSet<usize>,
    paths: Option<Arc<HashSet<CommitId>>>,
    /// The `path:` filter has not been resolved yet; no chunk is matched before it is.
    waiting_paths: bool,
    /// Rows `..requested` have been sent for matching, `scanned` of them answered.
    requested: usize,
    scanned: usize,
    /// (row the search started from, row the search moved the selection to). Cleared once the
    /// user moves on their own.
    jump: Option<(usize, usize)>,
}

impl App {
    pub fn open_search(&mut self) {
        self.search.bar = Some(Editor::single());
    }

    /// Keys while the search bar is open: everything is input except Enter and Esc.
    pub(super) fn search_bar_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let Some(bar) = self.search.bar.as_mut() else { return };
        match k.code {
            KeyCode::Esc => {
                self.search.bar = None;
                self.clear_search();
            }
            KeyCode::Enter => {
                let input = bar.take();
                self.search.bar = None;
                self.run_search(input);
            }
            KeyCode::Backspace => bar.backspace(),
            KeyCode::Delete => bar.delete(),
            KeyCode::Left => bar.left(),
            KeyCode::Right => bar.right(),
            KeyCode::Home => bar.home(),
            KeyCode::End => bar.end(),
            KeyCode::Char('a') if ctrl => bar.home(),
            KeyCode::Char('e') if ctrl => bar.end(),
            KeyCode::Char('u') if ctrl => bar.set(""),
            KeyCode::Char(c) if !ctrl => {
                let mut b = [0; 4];
                bar.insert(c.encode_utf8(&mut b));
            }
            _ => {}
        }
    }

    pub(super) fn paste_into_search(&mut self, s: &str) -> bool {
        let Some(bar) = self.search.bar.as_mut() else { return false };
        bar.insert(s.lines().next().unwrap_or(""));
        true
    }

    pub fn search_active(&self) -> bool {
        self.search.query.is_some()
    }

    /// Stops the running search (workers see the bumped generation) and forgets its matches.
    pub fn clear_search(&mut self) {
        let generation = Gens::bump(&self.gens.search);
        let bar = self.search.bar.take();
        self.search = super::search::Search { generation, bar, ..Default::default() };
    }

    fn run_search(&mut self, input: String) {
        self.clear_search();
        let Some(query) = Query::parse(&input) else { return };
        self.search.input = input;
        self.search.jump = Some((self.selected, self.selected));
        self.start_search(Arc::new(query));
    }

    /// Runs `input` again without moving the selection (leaving compare).
    pub(super) fn resume_search(&mut self, input: String) {
        self.clear_search();
        let Some(query) = Query::parse(&input) else { return };
        self.search.input = input;
        self.start_search(Arc::new(query));
    }

    /// Starts matching `query` from the first row (also after the history restarts).
    fn start_search(&mut self, query: Arc<Query>) {
        if let Some(path) = &query.path {
            let tips = self.refs.as_ref().map(|r| r.tips(self.scope)).unwrap_or_default();
            self.search.waiting_paths = true;
            self.outbox.push(Request::SearchPath { generation: self.search.generation, tips, path: path.clone() });
        }
        self.search.query = Some(query);
        self.request_search_chunks();
    }

    /// The history was walked again (new tips): matches are found again under a new
    /// generation; the selection is kept.
    pub(super) fn restart_search(&mut self) {
        let Some(query) = self.search.query.clone() else { return };
        let input = std::mem::take(&mut self.search.input);
        self.clear_search();
        self.search.input = input;
        self.start_search(query);
    }

    /// Sends the rows not yet sent, in chunks; called again as the history walk grows.
    pub(super) fn request_search_chunks(&mut self) {
        let (Some(query), Some(history)) = (self.search.query.clone(), self.history.clone()) else { return };
        if self.search.waiting_paths {
            return;
        }
        let chunk = self.search_chunk.max(1);
        while self.search.requested < self.history_len {
            let start = self.search.requested;
            let end = (start + chunk).min(self.history_len);
            self.search.requested = end;
            self.outbox.push(Request::Search {
                generation: self.search.generation,
                query: query.clone(),
                paths: self.search.paths.clone(),
                history: history.clone(),
                range: start..end,
            });
        }
    }

    /// Search replies; everything else is passed back.
    pub(super) fn handle_search_msg(&mut self, m: Msg) -> Option<Msg> {
        match m {
            Msg::SearchHits { generation, range, hits } => {
                if generation == self.search.generation && self.search.query.is_some() {
                    self.search.scanned += range.len();
                    self.search.hits.extend(hits);
                    self.follow_search();
                }
                None
            }
            Msg::SearchPaths { generation, result } => {
                if generation != self.search.generation {
                    return None;
                }
                match result {
                    Ok(ids) => {
                        self.search.paths = Some(ids);
                        self.search.waiting_paths = false;
                        self.request_search_chunks();
                    }
                    Err(detail) => {
                        self.clear_search();
                        self.toast = Some(Toast { what: "searching by path".into(), detail, error: true });
                    }
                }
                None
            }
            m => Some(m),
        }
    }

    fn search_complete(&self) -> bool {
        !self.search.waiting_paths && self.history_done && self.search.scanned >= self.history_len
    }

    /// Moves the selection to the first match at or after where the search started, while the
    /// user has not moved: chunks answer in any order, so an earlier match can still arrive.
    fn follow_search(&mut self) {
        let Some((origin, at)) = self.search.jump else { return };
        if self.selected != at {
            self.search.jump = None;
            return;
        }
        let complete = self.search_complete();
        let hits = &self.search.hits;
        let target = hits.range(origin..).next().or(if complete { hits.first() } else { None }).copied();
        if let Some(t) = target.filter(|&t| t != self.selected) {
            self.select(t);
            self.search.jump = Some((origin, t));
        }
        if complete {
            self.search.jump = None;
        }
    }

    /// `n` (forward) / `N`: the next match found so far, wrapping around.
    pub fn search_step(&mut self, forward: bool) {
        let hits = &self.search.hits;
        let cur = self.selected;
        let t = if forward { hits.range(cur + 1..).next().or(hits.first()) } else { hits.range(..cur).next_back().or(hits.last()) };
        if let Some(&t) = t {
            self.search.jump = None;
            self.select(t);
        }
    }

    /// The bottom bar text for an active search: `/query  k/N · searching… x%`.
    pub fn search_label(&self) -> Option<String> {
        let s = &self.search;
        if let Some(bar) = &s.bar {
            return Some(format!("/{}", bar.text()));
        }
        s.query.as_ref()?;
        let n = s.hits.len();
        let mut out = format!("/{}  ", s.input);
        if self.search_complete() && n == 0 {
            out.push_str("no matches");
            return Some(out);
        }
        match s.hits.iter().position(|&h| h == self.selected) {
            Some(k) => out.push_str(&format!("{}/{n}", k + 1)),
            None => out.push_str(&format!("-/{n}")),
        }
        if !self.search_complete() {
            let pct = if s.waiting_paths { 0 } else { s.scanned * 100 / self.history_len.max(1) };
            out.push_str(&format!(" · searching… {}%", pct.min(99)));
        }
        Some(out)
    }
}
