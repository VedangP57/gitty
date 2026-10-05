//! Thread pools: one walker, `cores - 2` (min 2) readers, two diff workers, two highlighters and
//! one writer. Each thread owns a
//! [`gitty_core::Handle`]; the UI thread never does.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, select, unbounded};
use gitty_core::Repo;

use crate::exec::exec;
use crate::msg::{Gens, Msg, Request};

struct Pool {
    high: Sender<Request>,
    low: Sender<Request>,
}

pub struct Workers {
    walker: Pool,
    readers: Pool,
    differs: Pool,
    highlighters: Pool,
    writer: Pool,
    net: Pool,
    maintenance: Pool,
}

fn next(high: &Receiver<Request>, low: &Receiver<Request>) -> Option<Request> {
    if let Ok(r) = high.try_recv() {
        return Some(r);
    }
    select! {
        recv(high) -> r => r.ok(),
        recv(low) -> r => r.ok(),
    }
}

fn pool(name: &str, n: usize, warm: bool, repo: &Repo, gens: &Arc<Gens>, tx: &Sender<Msg>) -> Pool {
    let (high, high_rx) = unbounded::<Request>();
    let (low, low_rx) = unbounded::<Request>();
    for i in 0..n {
        let (high_rx, low_rx, repo, gens, wtx) = (high_rx.clone(), low_rx.clone(), repo.clone(), gens.clone(), tx.clone());
        let spawned = std::thread::Builder::new().name(format!("gitty-{name}-{i}")).spawn(move || {
            crate::term::mark_thread_panics_caught();
            let h = repo.handle();
            if warm {
                h.warm();
            }
            while let Some(req) = next(&high_rx, &low_rx) {
                let files_req = match &req {
                    Request::Files { generation, of, prefetch } => Some((*generation, *of, *prefetch)),
                    _ => None,
                };
                // a write that panics still reports done, so the app stops waiting for it
                let write_req = match &req {
                    Request::Write(op) => Some(op.clone()),
                    _ => None,
                };
                let net_req = match &req {
                    Request::Net { op, background, .. } => Some((*op, *background)),
                    _ => None,
                };
                let r = catch_unwind(AssertUnwindSafe(|| {
                    exec(&h, req, &mut |m| {
                        let _ = wtx.send(m);
                    }, &gens)
                }));
                if let Err(p) = r {
                    let detail = p
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    let _ = match (files_req, write_req, net_req) {
                        (Some((generation, of, prefetch)), _, _) => wtx.send(Msg::FilesError { generation, of, prefetch, detail }),
                        (_, Some(op), _) => wtx.send(Msg::WriteDone { op, result: Err(format!("internal error: {detail}")) }),
                        (_, _, Some((op, background))) => {
                            wtx.send(Msg::NetDone { op, background, outcome: gitty_core::net::Outcome::Failed { detail: format!("internal error: {detail}") } })
                        }
                        _ => wtx.send(Msg::Error { what: "internal error in a worker".into(), detail }),
                    };
                }
            }
        });
        if let Err(e) = spawned {
            let _ = tx.send(Msg::Error { what: "starting worker threads".into(), detail: e.to_string() });
        }
    }
    Pool { high, low }
}

impl Workers {
    pub fn spawn(repo: Repo, gens: Arc<Gens>, tx: Sender<Msg>) -> Workers {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Workers {
            walker: pool("walker", 1, false, &repo, &gens, &tx),
            readers: pool("reader", cores.saturating_sub(2).max(2), true, &repo, &gens, &tx),
            differs: pool("diff", 2, false, &repo, &gens, &tx),
            highlighters: pool("highlight", 2, false, &repo, &gens, &tx),
            // one thread: writes run in the order they were asked for
            writer: pool("writer", 1, false, &repo, &gens, &tx),
            // one network job at a time, like Desktop
            net: pool("net", 1, false, &repo, &gens, &tx),
            // commit-graph writes can take seconds on huge repos: never on the writer
            maintenance: pool("maintenance", 1, false, &repo, &gens, &tx),
        }
    }

    pub fn submit(&self, req: Request) {
        let pool = match req {
            Request::Walk { .. } => &self.walker,
            Request::Diff { .. } | Request::Intraline { .. } => &self.differs,
            Request::Highlight { .. } => &self.highlighters,
            Request::Write(_) => &self.writer,
            Request::Net { .. } => &self.net,
            Request::Tune { .. } => &self.maintenance,
            _ => &self.readers,
        };
        let q = if req.is_background() { &pool.low } else { &pool.high };
        let _ = q.send(req);
    }
}
