//! Worker-side execution of [`Request`]s. The same function backs the thread pools and the tests.

use std::cell::RefCell;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::UNIX_EPOCH;

use gitty_core::Handle;

use crate::msg::{ConflictBody, DiffKey, FileView, FilesOf, Gens, HlKey, Msg, Request};

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

fn file_view(c: gitty_core::files::FileContent, path: &std::path::Path) -> FileView {
    use gitty_core::files::FileContent as C;
    match c {
        C::Text(bytes) => FileView::Text { key: HlKey { blob: gitty_core::commit_files::BlobId::hash_of(&bytes), path: path.to_string_lossy().into_owned() }, text: Arc::new(gitty_core::diff::text::Text::new(bytes)) },
        C::Binary { size } => FileView::Binary { size },
        C::TooLarge { size } => FileView::TooLarge { size },
        C::Lfs { size } => FileView::Lfs { size },
        C::Symlink { target } => FileView::Symlink { target },
        C::Special => FileView::Special,
        C::Masked => FileView::Masked,
    }
}

/// Bytes of conflicted files read for the file list's counts in one request.
const CONFLICT_COUNT_BUDGET: usize = 16 << 20;

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
        Request::Walk { session, tips, topo: true, head } => {
            let history = Arc::new(RwLock::new(h.empty_history()));
            sink(Msg::HistoryStarted { session, history: history.clone() });
            let mut chunk = history.read().unwrap_or_else(PoisonError::into_inner).empty_like();
            let mut len = 0;
            // fill a private chunk; hold the lock only to publish it
            let publish = |chunk: &mut gitty_core::history::History| {
                let mut shared = history.write().unwrap_or_else(PoisonError::into_inner);
                shared.append(chunk);
                shared.len()
            };
            let stale = || !Gens::is(&gens.session, session);
            let cli = gitty_core::git_cli::GitCli::new(h.owner());
            // the lanes are laid out as the commits stream in: each row is ready with its id
            let mut lanes = gitty_core::graph::Lanes::new(head);
            let walked = gitty_core::graph::topo_walk(&cli, &tips, &stale, &mut |id, parents| {
                chunk.push_laid_out(id, parents, &mut lanes);
                if chunk.len() >= if len == 0 { WALK_FIRST_CHUNK } else { WALK_CHUNK } {
                    len = publish(&mut chunk);
                    sink(Msg::HistoryProgress { session, len, done: false });
                }
                true
            });
            len = publish(&mut chunk);
            if let Err(e) = walked {
                sink(error("walking history", &e));
            }
            sink(Msg::HistoryProgress { session, len, done: true });
        }
        Request::Walk { session, tips, topo: false, .. } => {
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
        Request::AheadBehind { local, upstream } => match upstream.map_or_else(|| h.unpublished(local).map(|ahead| gitty_core::ahead_behind::AheadBehind { ahead, behind: Vec::new() }), |u| h.ahead_behind(local, u)) {
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
        Request::Highlight { generation, key, text, files_view } => {
            let stale = || !Gens::is(if files_view { &gens.files_view } else { &gens.file }, generation);
            let spans = HIGHLIGHTER.with_borrow_mut(|hl| hl.highlight(&key.path, text.bytes(), &stale));
            let cancelled = spans.is_none() && stale();
            sink(Msg::Highlighted { key, spans: spans.map(Arc::new), cancelled });
        }
        Request::Status { generation, mark } => {
            if let Some(mark) = mark {
                mark.note();
            }
            let t = std::time::Instant::now();
            let cli = gitty_core::git_cli::GitCli::new(h.owner());
            let status = cli.status();
            let slow = status.is_ok() && t.elapsed() >= SLOW_STATUS;
            // a state started or ended anywhere (the watcher saw MERGE_HEAD, rebase-merge…) arrives
            // with the status it changes. A failed status run sends none: the last known state stays
            // on screen, which is safer than hiding an operation that is half done.
            let op = status.as_ref().ok().map(|st| cli.op_state(st));
            sink(Msg::Status { generation, result: status.map_err(|e| format!("{e:#}")) });
            if let Some(state) = op {
                sink(Msg::OpState { generation, state });
            }
            if slow {
                sink(Msg::StatusSlow);
            }
        }
        Request::ChangeDiff { generation, entry, opts, force_text } => match change_diff(h, &entry, opts, force_text) {
            Ok((key, diff, texts, staged, divergent)) => sink(Msg::ChangeDiff { generation, entry, key, diff, texts, staged, divergent }),
            Err(e) => sink(Msg::ChangeDiffError { generation, path: entry.path, detail: format!("{e:#}") }),
        },
        Request::ConflictFile { generation, entry } => {
            use gitty_core::conflicts::Loaded;
            let cli = gitty_core::git_cli::GitCli::new(h.owner());
            let sides = cli.conflict_sides();
            let result = match h.owner().workdir() {
                Some(root) => gitty_core::conflicts::read(root, std::path::Path::new(&entry.path), cli.conflict_style(&entry.path))
                    .map(|loaded| match loaded {
                        Loaded::Text { bytes, conflicts, unknown } => {
                            let key = HlKey { blob: gitty_core::commit_files::BlobId::hash_of(&bytes), path: entry.path.clone() };
                            ConflictBody::Text { text: Arc::new(gitty_core::diff::text::Text::new(bytes)), key, conflicts: Arc::new(conflicts), unknown }
                        }
                        Loaded::Other(why) => ConflictBody::Other(why),
                    })
                    .map_err(|e| format!("{e:#}")),
                None => Err("bare repository".to_string()),
            };
            sink(Msg::ConflictFile { generation, entry, sides, result });
        }
        Request::ConflictCounts { paths } => {
            use std::os::unix::fs::MetadataExt;
            let mut counts = Vec::new();
            let cli = gitty_core::git_cli::GitCli::new(h.owner());
            if let Some(root) = h.owner().workdir() {
                // a tree of hundreds of conflicted big files must not hold a reader for long
                let mut budget = CONFLICT_COUNT_BUDGET;
                for (p, known) in &paths {
                    let Ok(meta) = std::fs::symlink_metadata(root.join(p)) else { continue };
                    let stamp = (meta.len(), i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()));
                    if *known == Some(stamp) {
                        continue;
                    }
                    if budget == 0 {
                        break;
                    }
                    // every read is charged, a file that is not text included
                    budget = budget.saturating_sub(meta.len().min(gitty_core::files::MAX_VIEW_BYTES) as usize);
                    let n = match gitty_core::conflicts::read(root, std::path::Path::new(p), cli.conflict_style(p)) {
                        // markers that were not understood still count: staging them is not harmless
                        Ok(gitty_core::conflicts::Loaded::Text { conflicts, unknown, .. }) => Some(conflicts.len().max(usize::from(unknown))),
                        _ => None,
                    };
                    counts.push((p.clone(), n, stamp));
                }
            }
            sink(Msg::ConflictCounts { asked: paths.into_iter().map(|(p, _)| p).collect(), counts });
        }
        Request::ReadDir { generation, dir } => {
            if !Gens::is(&gens.files_dirs, generation) {
                return;
            }
            let result = h.list_dir(&dir).map_err(|e| format!("{e:#}"));
            sink(Msg::Dir { generation, dir, result });
        }
        Request::ReadFile { generation, path, reveal } => {
            if !Gens::is(&gens.files_view, generation) {
                return;
            }
            let result = match h.owner().workdir() {
                Some(root) => gitty_core::files::read_file(root, &path, reveal).map(|c| file_view(c, &path)).map_err(|e| format!("{e:#}")),
                None => Err("bare repository".to_string()),
            };
            sink(Msg::File { generation, path, result });
        }
        Request::Write(op) => {
            let ran = {
                let _write = crate::write::lock();
                crate::write::run(h, &op, &mut |line| sink(Msg::WriteLog { line: line.to_string() }))
            };
            // a continue that found staged conflict markers is a question, not a failure
            let markers = match (&op, &ran) {
                (crate::msg::WriteOp::ContinueOp { op, id, .. }, Err(e)) => e.downcast_ref::<gitty_core::op_state::StagedMarkers>().map(|m| Msg::StagedMarkers { op: *op, id: id.clone(), files: m.0.clone() }),
                _ => None,
            };
            // a merge left open on conflicts is a question too
            let stopped = match &ran {
                Err(e) => e.downcast_ref::<crate::write::MergeConflicts>().map(|c| Msg::Conflicted { doing: c.doing.clone(), files: c.files.clone(), state: c.state.clone(), stash: c.stash.clone() }),
                Ok(_) => None,
            };
            let ask = markers.or(stopped);
            let result = if ask.is_some() { Ok(None) } else { ran.map_err(|e| format!("{e:#}")) };
            if let Some(ask) = ask {
                sink(ask);
            }
            let stale = match &result {
                Err(e) if !matches!(op, crate::msg::WriteOp::RemoveIndexLock { .. }) => crate::write::stale_index_lock(h, e),
                _ => None,
            };
            sink(Msg::WriteDone { op, result });
            if let Some(seen) = stale {
                sink(Msg::StaleIndexLock { seen });
            }
        }
        Request::Net { op, mode, background, force } => crate::netjob::run(h, op, mode, background, force, sink),
        Request::Tune { history_len, th } => {
            let actions = gitty_core::tune::plan(h, history_len, th);
            let (applied, error) = match gitty_core::tune::apply(h, &actions) {
                Ok(done) => (done, None),
                Err(e) => (Vec::new(), Some(format!("{e:#}"))),
            };
            sink(Msg::Tuned { applied, error });
        }
        Request::StashList => {
            let result = gitty_core::git_cli::GitCli::new(h.owner()).stash_list().map_err(|e| format!("{e:#}"));
            sink(Msg::StashList { result });
        }
        Request::PrUrl { branch } => {
            let result = gitty_core::git_cli::GitCli::new(h.owner()).pr_url(&branch).map_err(|e| match e.downcast_ref::<gitty_core::forge::ForgeError>() {
                Some(f) if f.is_guidance() => crate::msg::PrUrlError::Notice(f.to_string()),
                _ => crate::msg::PrUrlError::Failed(format!("{e:#}")),
            });
            sink(Msg::PrUrl { result });
        }
        Request::PrBadge { branch } => {
            let result = gitty_core::git_cli::GitCli::new(h.owner()).pr_badge(&branch);
            sink(Msg::PrBadge { branch, result });
        }
        Request::HeadMessage => {
            let result = gitty_core::git_cli::GitCli::new(h.owner()).head_message().map_err(|e| format!("{e:#}"));
            sink(Msg::HeadMessage { result });
        }
    }
}
