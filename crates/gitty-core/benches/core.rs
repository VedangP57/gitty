use criterion::{criterion_group, criterion_main, Criterion};
use gitty_core::refs::HistoryScope;
use gitty_core::Repo;

fn benches(c: &mut Criterion) {
    let Ok(path) = std::env::var("GITTY_BENCH_REPO") else {
        eprintln!("GITTY_BENCH_REPO unset; skipping");
        return;
    };
    let repo = Repo::open(&path).unwrap();
    let h = repo.handle();
    let refs = h.refs().unwrap();
    let tips = refs.tips(HistoryScope::AllRefs);
    c.bench_function("refs", |b| b.iter(|| h.refs().unwrap()));
    c.bench_function("walk_first_500", |b| {
        b.iter(|| {
            let mut w = h.walker(&tips).unwrap();
            let mut hist = w.new_history();
            w.step(&h, &mut hist, 500).unwrap();
            hist.len()
        })
    });
    let mut w = h.walker(&tips).unwrap();
    let mut hist = w.new_history();
    w.step(&h, &mut hist, 200).unwrap();
    let ids = hist.ids(0..hist.len());
    c.bench_function("decode_row", |b| {
        let mut i = 0;
        b.iter(|| {
            i = (i + 1) % ids.len();
            h.decode_row(ids[i]).unwrap()
        })
    });
    c.bench_function("commit_files", |b| {
        let mut i = 0;
        b.iter(|| {
            i = (i + 1) % ids.len();
            h.commit_files(ids[i], false).unwrap()
        })
    });
}

criterion_group!(g, benches);
criterion_main!(g);
