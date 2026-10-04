//! Worker-side execution of [`Request`]s. The same function backs the thread pools and the tests.

use std::sync::{Arc, PoisonError, RwLock};
use std::time::UNIX_EPOCH;

use gitty_core::Handle;

use crate::msg::{DiffKey, Gens, Msg, Request};

/// History entries appended per write-lock hold; the first chunk is small so the first screen
/// of rows appears quickly even without a commit-graph.
const WALK_FIRST_CHUNK: usize = 256;
const WALK_CHUNK: usize = 4096;
/// Line stats are sent in batches of this many files; staleness is checked as often.
const STATS_CHUNK: usize = 32;

fn error(what: impl Into<String>, e: &anyhow::Error) -> Msg {
    Msg::Error { what: what.into(), detail: format!("{e:#}") }
}

pub fn exec(h: &Handle, req: Request, sink: &mut dyn FnMut(Msg), gens: &Gens) {
    match req {
        Request::Refs => match h.refs() {
            Ok(refs) => {
                let fetched_at = std::fs::metadata(h.owner().git_dir().join("FETCH_HEAD"))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64);
                sink(Msg::Refs { refs, fetched_at });
            }
            Err(e) => sink(error("reading refs", &e)),
        },
        Request::Walk { session, tips } => {
            let done = |sink: &mut dyn FnMut(Msg), len| sink(Msg::HistoryProgress { session, len, done: true });
            let mut walker = match h.walker(&tips) {
                Ok(w) => w,
                Err(e) => {
                    sink(error("walking history", &e));
                    return done(sink, 0);
                }
            };
            let history = Arc::new(RwLock::new(walker.new_history()));
            sink(Msg::HistoryStarted { session, history: history.clone() });
            let mut len = 0;
            loop {
                if !Gens::is(&gens.session, session) {
                    return done(sink, len);
                }
                // walk into a private chunk; hold the lock only to publish it
                let mut chunk = walker.new_history();
                let more = walker.step(h, &mut chunk, if len == 0 { WALK_FIRST_CHUNK } else { WALK_CHUNK });
                {
                    let mut shared = history.write().unwrap_or_else(PoisonError::into_inner);
                    shared.append(&mut chunk);
                    len = shared.len();
                }
                match more {
                    Ok(true) => sink(Msg::HistoryProgress { session, len, done: false }),
                    Ok(false) => return done(sink, len),
                    Err(e) => {
                        sink(error("walking history", &e));
                        return done(sink, len);
                    }
                }
            }
        }
        Request::AheadBehind { local, upstream } => match h.ahead_behind(local, upstream) {
            Ok(ab) => sink(Msg::AheadBehind { local, upstream, ab }),
            Err(e) => sink(error("computing ahead/behind", &e)),
        },
        Request::Rows { session, ids } => {
            let rows = ids.into_iter().filter_map(|(i, id)| h.decode_row(id).ok().map(|r| (i, r))).collect();
            sink(Msg::Rows { session, rows });
        }
        Request::Detail { generation, id } => {
            if !Gens::is(&gens.commit, generation) {
                return;
            }
            match h.commit_detail(id) {
                Ok(detail) => sink(Msg::Detail { generation, detail }),
                Err(e) => sink(error(format!("reading commit {}", id.short(7)), &e)),
            }
        }
        Request::Files { generation, id, prefetch } => {
            let stale = || !prefetch && !Gens::is(&gens.commit, generation);
            if stale() {
                return;
            }
            let files = match h.commit_files(id, true) {
                Ok(f) => Arc::new(f),
                Err(e) => return sink(error(format!("listing files of {}", id.short(7)), &e)),
            };
            sink(Msg::Files { generation, id, files: files.clone(), prefetch });
            let mut start = 0;
            for chunk in files.chunks(STATS_CHUNK) {
                if stale() {
                    return;
                }
                let stats = chunk.iter().map(|f| h.line_stats(f).ok()).collect();
                let done = start + chunk.len() == files.len();
                sink(Msg::Stats { id, start, stats, done });
                start += chunk.len();
            }
            if files.is_empty() {
                sink(Msg::Stats { id, start: 0, stats: Vec::new(), done: true });
            }
        }
        Request::Diff { generation, file, opts, force_text } => {
            if !Gens::is(&gens.file, generation) {
                return;
            }
            let key = DiffKey::of(&file, opts, force_text);
            let diff = match h.file_diff(&file, opts) {
                Ok(d) => Arc::new(if force_text { d.force_text() } else { d }),
                Err(e) => return sink(Msg::DiffError { generation, key, detail: format!("{e:#}") }),
            };
            sink(Msg::Diff { generation, key: key.clone(), diff: diff.clone() });
            exec(h, Request::Intraline { generation, key, diff }, sink, gens);
        }
        Request::Intraline { generation, key, diff } => {
            for c in 0..diff.changes.len() {
                if !Gens::is(&gens.file, generation) {
                    return;
                }
                diff.intraline(c);
            }
            sink(Msg::IntralineDone { key });
        }
    }
}
