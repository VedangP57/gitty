//! Times each worker request against a repository: `cargo run --release --example trace -- PATH`.

use std::time::Instant;

use gitty::exec::exec;
use gitty::msg::{FilesOf, Gens, Msg, Request};
use gitty_core::Repo;
use gitty_core::refs::HistoryScope;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let t = Instant::now();
    let repo = Repo::open(&path).unwrap();
    let h = repo.handle();
    let gens = Gens::default();
    println!("open+handle      {:>8.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let t = Instant::now();
    let mut refs = None;
    exec(&h, Request::Refs, &mut |m| if let Msg::Refs { refs: r, .. } = m { refs = Some(r) }, &gens);
    println!("refs             {:>8.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let tips = refs.unwrap().tips(HistoryScope::HeadAndUpstream);
    let mut hist = None;
    // the graph view lists git's topological order; without it, commit-time order
    for (topo, label) in [(true, "topo"), (false, "walk")] {
        let t = Instant::now();
        let mut first = None;
        let mut msgs = 0;
        exec(&h, Request::Walk { session: 0, tips: tips.clone(), topo }, &mut |m| {
            msgs += 1;
            match m {
                Msg::HistoryStarted { history, .. } => hist = Some(history),
                Msg::HistoryProgress { len, .. } if first.is_none() => first = Some((len, t.elapsed())),
                _ => {}
            }
        }, &gens);
        let (n, d) = first.unwrap();
        println!("{label} first chunk {:>8.1} ms ({n} rows)", d.as_secs_f64() * 1e3);
        let len = hist.as_ref().unwrap().read().unwrap().len();
        println!("{label} total       {:>8.1} ms ({len} rows, {msgs} msgs)", t.elapsed().as_secs_f64() * 1e3);
    }
    for rows in [256, 4096, 65536] {
        let t = Instant::now();
        exec(&h, Request::Graph { session: 0, tips: tips.clone(), rows }, &mut |_| {}, &gens);
        println!("graph {rows:>6} rows {:>8.1} ms", t.elapsed().as_secs_f64() * 1e3);
    }
    let hist = hist.unwrap();
    let len = hist.read().unwrap().len();
    let ids: Vec<_> = { let h = hist.read().unwrap(); (0..100).map(|i| (i, h.id(i))).collect() };
    let t = Instant::now();
    exec(&h, Request::Rows { session: 0, ids: ids.clone() }, &mut |_| {}, &gens);
    println!("decode 100 rows  {:>8.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let t = Instant::now();
    exec(&h, Request::Files { generation: 0, of: FilesOf::Commit(ids[0].1), prefetch: true }, &mut |_| {}, &gens);
    println!("files+stats HEAD {:>8.1} ms", t.elapsed().as_secs_f64() * 1e3);
    // one search chunk on this thread; the app spreads chunks over its 2-thread search pool
    let query = std::sync::Arc::new(gitty_core::search::Query::parse("zzz-no-such-text").unwrap());
    let n = len.min(gitty::app::search::SEARCH_CHUNK);
    let t = Instant::now();
    exec(&h, Request::Search { generation: 0, query, paths: None, history: hist.clone(), range: 0..n }, &mut |_| {}, &gens);
    println!("search chunk     {:>8.1} ms ({n} rows)", t.elapsed().as_secs_f64() * 1e3);
}
