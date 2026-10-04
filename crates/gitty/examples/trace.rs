//! Times each worker request against a repository: `cargo run --release --example trace -- PATH`.

use std::time::Instant;

use gitty::exec::exec;
use gitty::msg::{Gens, Msg, Request};
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
    let t = Instant::now();
    let mut first = None;
    let mut hist = None;
    let mut msgs = 0;
    exec(&h, Request::Walk { session: 0, tips }, &mut |m| {
        msgs += 1;
        match m {
            Msg::HistoryStarted { history, .. } => hist = Some(history),
            Msg::HistoryProgress { len, .. } if first.is_none() => first = Some((len, t.elapsed())),
            _ => {}
        }
    }, &gens);
    let (n, d) = first.unwrap();
    println!("walk first chunk {:>8.1} ms ({n} rows)", d.as_secs_f64() * 1e3);
    let hist = hist.unwrap();
    let len = hist.read().unwrap().len();
    println!("walk total       {:>8.1} ms ({len} rows, {msgs} msgs)", t.elapsed().as_secs_f64() * 1e3);
    let ids: Vec<_> = { let h = hist.read().unwrap(); (0..100).map(|i| (i, h.id(i))).collect() };
    let t = Instant::now();
    exec(&h, Request::Rows { session: 0, ids: ids.clone() }, &mut |_| {}, &gens);
    println!("decode 100 rows  {:>8.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let t = Instant::now();
    exec(&h, Request::Files { generation: 0, id: ids[0].1, prefetch: true }, &mut |_| {}, &gens);
    println!("files+stats HEAD {:>8.1} ms", t.elapsed().as_secs_f64() * 1e3);
}
