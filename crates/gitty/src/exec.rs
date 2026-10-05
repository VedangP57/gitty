//! Worker-side execution of [`Request`]s. The same function backs the thread pools and the tests.

use std::cell::RefCell;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::UNIX_EPOCH;

use gitty_core::Handle;

use crate::msg::{DiffKey, FilesOf, Gens, Msg, Request};

/// History entries appended per write-lock hold; the first chunk is small so the first screen
/// of rows appears quickly even without a commit-graph.
const WALK_FIRST_CHUNK: usize = 256;
const WALK_CHUNK: usize = 4096;
/// Line stats are sent in batches of this many files; staleness is checked as often.
const STATS_CHUNK: usize = 32;
/// A search chunk checks for a newer query this often (rows).
const SEARCH_CHECK: usize = 1024;

thread_local! {
    static HIGHLIGHTER: RefCell<gitty_highlight::Highlighter> = RefCell::new(gitty_highlight::Highlighter::new());
}

fn error(what: impl Into<String>, e: &anyhow::Error) -> Msg {
    Msg::Error { what: what.into(), detail: format!("{e:#}") }
}

type ChangeDiffParts = (DiffKey, Arc<gitty_core::diff::FileDiff>, gitty_core::stage::Texts, Option<Vec<bool>>, bool);

/// HEAD → worktree diff of a status entry plus its staged lines (Changes tab).
fn change_diff(h: &Handle, e: &gitty_core::status::StatusEntry, opts: gitty_core::diff::DiffOptions, force_text: bool) -> anyhow::Result<ChangeDiffParts> {
    use gitty_core::diff::ops::WsMode;
    let texts = h.stage_texts(e)?;
    let mut diff = gitty_core::diff::FileDiff::from_bytes(
        &e.path,
        e.orig_path.as_deref(),
        texts.head.bytes().to_vec(),
        texts.wt.bytes().to_vec(),
        e.head_mode,
        texts.wt_mode,
        opts,
    );
    if force_text {
        diff = diff.force_text();
    }
    // symlinks, submodules and type changes are whole-file only (an untracked symlink shows only in wt_mode)
    let lines_ok = diff.is_text() && opts.ws == WsMode::Show && e.line_stageable() && texts.wt_mode & 0o170000 != 0o120000;
    let derived = lines_ok.then(|| gitty_core::stage::staged_set(&texts, &diff.ops));
    let divergent = matches!(derived, Some(None));
    let key = DiffKey {
        old: e.head_blob,
        new: (texts.wt_mode != 0).then_some(texts.wt_blob),
        path: e.path.clone(),
        old_path: e.orig_path.clone(),
        old_mode: e.head_mode,
        new_mode: texts.wt_mode,
        opts,
        force_text,
    };
    Ok((key, Arc::new(diff), texts, derived.flatten(), divergent))
}

/// A clean status this slow usually means racily clean index entries being re-hashed each run.
const SLOW_STATUS: std::time::Duration = std::time::Duration::from_millis(100);

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
        Request::Search { generation, query, paths, history, range } => {
            let ids = {
                let h = history.read().unwrap_or_else(PoisonError::into_inner);
                h.ids(range.start.min(h.len())..range.end.min(h.len()))
            };
            let mut hits = Vec::new();
            for (n, id) in ids.into_iter().enumerate() {
                if n % SEARCH_CHECK == 0 && !Gens::is(&gens.search, generation) {
                    return;
                }
                if paths.as_ref().is_some_and(|p| !p.contains(&id)) {
                    continue;
                }
                // a path-only query needs no decoding
                if query.text.is_none() || h.decode_row(id).is_ok_and(|r| query.matches(&r)) {
                    hits.push(range.start + n);
                }
            }
            sink(Msg::SearchHits { generation, range, hits });
        }
        Request::SearchPath { generation, tips, path } => {
            if !Gens::is(&gens.search, generation) {
                return;
            }
            let cli = gitty_core::git_cli::GitCli::new(h.owner());
            let stale = || !Gens::is(&gens.search, generation);
            let result = gitty_core::search::path_commits(&cli, &tips, &path, &stale).map(Arc::new).map_err(|e| format!("{e:#}"));
            // a cancelled lookup answers nothing: its search is gone
            if !stale() {
                sink(Msg::SearchPaths { generation, result });
            }
        }
        Request::RangeCount { generation, oldest, newest, history, rows } => {
            let stale = || !Gens::is(&gens.commit, generation);
            // a failed or cancelled count only leaves the note out
            if !stale()
                && let Ok(extra) = gitty_core::git_cli::GitCli::new(h.owner()).range_extra(oldest, newest, {
                    // one read guard for the whole range: the walker may be appending
                    let h = history.read().unwrap_or_else(std::sync::PoisonError::into_inner);
                    rows.filter(|&i| i < h.len()).map(|i| h.id(i)).collect()
                }, &stale)
            {
                sink(Msg::RangeCount { oldest, newest, extra });
            }
        }
        Request::CommitRows { ids } => {
            let rows = ids.into_iter().filter_map(|id| h.decode_row(id).ok()).collect();
            sink(Msg::CommitRows { rows });
        }
        Request::Compare { generation, head, other } => {
            let cli = gitty_core::git_cli::GitCli::new(h.owner());
            let result = gitty_core::compare::compare(&cli, head, other).map_err(|e| format!("{e:#}"));
            sink(Msg::Compare { generation, result });
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
        Request::Files { generation, of, prefetch } => {
            let stale = || !prefetch && !Gens::is(&gens.commit, generation);
            if stale() {
                return;
            }
            let listed = match of {
                FilesOf::Commit(id) => h.commit_files(id, true),
                FilesOf::Range { oldest, newest } => h.range_files(oldest, newest, true),
                FilesOf::Between { from, to } => h.diff_commits(from, to, true),
            };
            let files = match listed {
                Ok(f) => Arc::new(f),
                Err(e) => return sink(Msg::FilesError { generation, of, prefetch, detail: format!("{e:#}") }),
            };
            sink(Msg::Files { generation, of, files: files.clone(), prefetch });
            if prefetch {
                // prefetch fills the file-list cache only; stats are computed on selection
                return;
            }
            let mut start = 0;
            for chunk in files.chunks(STATS_CHUNK) {
                if stale() {
                    return;
                }
                let stats = chunk.iter().map(|f| h.line_stats(f).ok()).collect();
                let done = start + chunk.len() == files.len();
                sink(Msg::Stats { of, start, stats, done });
                start += chunk.len();
            }
            if files.is_empty() {
                sink(Msg::Stats { of, start: 0, stats: Vec::new(), done: true });
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
        Request::Highlight { generation, key, text } => {
            let stale = || !Gens::is(&gens.file, generation);
            let spans = HIGHLIGHTER.with_borrow_mut(|hl| hl.highlight(&key.path, text.bytes(), &stale));
            let cancelled = spans.is_none() && stale();
            sink(Msg::Highlighted { key, spans: spans.map(Arc::new), cancelled });
        }
        Request::Status { generation, mark } => {
            if let Some(mark) = mark {
                mark.note();
            }
            let t = std::time::Instant::now();
            let result = gitty_core::git_cli::GitCli::new(h.owner()).status().map_err(|e| format!("{e:#}"));
            let slow = result.is_ok() && t.elapsed() >= SLOW_STATUS;
            sink(Msg::Status { generation, result });
            if slow {
                sink(Msg::StatusSlow);
            }
        }
        Request::ChangeDiff { generation, entry, opts, force_text } => match change_diff(h, &entry, opts, force_text) {
            Ok((key, diff, texts, staged, divergent)) => sink(Msg::ChangeDiff { generation, entry, key, diff, texts, staged, divergent }),
            Err(e) => sink(Msg::ChangeDiffError { generation, path: entry.path, detail: format!("{e:#}") }),
        },
        Request::Write(op) => {
            let result = {
                let _write = crate::write::lock();
                crate::write::run(h, &op, &mut |line| sink(Msg::WriteLog { line: line.to_string() })).map_err(|e| format!("{e:#}"))
            };
            let stale = match &result {
                Err(e) if !matches!(op, crate::msg::WriteOp::RemoveIndexLock { .. }) => crate::write::stale_index_lock(h, e),
                _ => None,
            };
            sink(Msg::WriteDone { op, result });
            if let Some(seen) = stale {
                sink(Msg::StaleIndexLock { seen });
            }
        }
        Request::Net { op, mode, background } => crate::netjob::run(h, op, mode, background, sink),
        Request::Tune { history_len, th } => {
            let actions = gitty_core::tune::plan(h, history_len, th);
            let (applied, error) = match gitty_core::tune::apply(h, &actions) {
                Ok(done) => (done, None),
                Err(e) => (Vec::new(), Some(format!("{e:#}"))),
            };
            sink(Msg::Tuned { applied, error });
        }
        Request::HeadMessage => {
            let result = gitty_core::git_cli::GitCli::new(h.owner()).head_message().map_err(|e| format!("{e:#}"));
            sink(Msg::HeadMessage { result });
        }
    }
}
