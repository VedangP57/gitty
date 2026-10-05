//! Measures the core read path on a real repository. See bench/run.sh.
//!
//! `--check` compares each measurement with its spec §8 budget: a miss prints the budget and the
//! measured value, and the probe exits 2. `GITTY_BUDGET_SCALE` multiplies every budget (a tiny
//! value proves the failure path).

use std::cell::RefCell;
use std::time::Instant;

use gitty_core::refs::HistoryScope;
use gitty_core::{CommitId, Repo};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

thread_local! {
    static CHECK: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Records `value` against `budget` (ms) when `--check` is on.
fn budget(what: &str, value: f64, budget: f64) {
    let scale: f64 = std::env::var("GITTY_BUDGET_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let limit = budget * scale;
    CHECK.with_borrow_mut(|c| {
        if let Some(misses) = c {
            let ok = value < limit;
            println!("  budget {what}: {value:.2}ms < {limit:.2}ms {}", if ok { "ok" } else { "MISSED" });
            if !ok {
                misses.push(format!("{what}: measured {value:.2}ms, budget < {limit:.2}ms"));
            }
        }
    });
}

fn main() -> anyhow::Result<()> {
    let mut a: Vec<String> = std::env::args().collect();
    if let Some(i) = a.iter().position(|s| s == "--check") {
        a.remove(i);
        CHECK.with_borrow_mut(|c| *c = Some(Vec::new()));
    }
    run(&a)?;
    let misses = CHECK.with_borrow(|c| c.clone().unwrap_or_default());
    if !misses.is_empty() {
        for m in &misses {
            eprintln!("BUDGET MISSED {m}");
        }
        std::process::exit(2);
    }
    Ok(())
}

fn run(a: &[String]) -> anyhow::Result<()> {
    let cmd = a.get(1).map(String::as_str).unwrap_or("walk");
    let path = a.get(2).map(String::as_str).unwrap_or(".");
    let t0 = Instant::now();
    let repo = Repo::open(path)?;
    let h = repo.handle();
    let refs = h.refs()?;
    let t_refs = ms(t0);
    match cmd {
        "walk" => {
            let scope = if a.get(3).map(String::as_str) == Some("head") { HistoryScope::HeadAndUpstream } else { HistoryScope::AllRefs };
            let tips = refs.tips(scope);
            let mut w = h.walker(&tips)?;
            let mut hist = w.new_history();
            w.step(&h, &mut hist, 500)?;
            let first = ms(t0);
            let rows: Vec<_> = hist.ids(0..hist.len().min(60)).into_iter().map(|id| h.decode_row(id)).collect::<Result<_, _>>()?;
            let first_screen = ms(t0);
            std::hint::black_box(rows);
            while w.step(&h, &mut hist, 65_536)? {}
            println!(
                "walk scope={scope:?} refs={t_refs:.1}ms first500={first:.1}ms first_screen_decoded={first_screen:.1}ms total={:.1}ms count={} graph={} tips={}",
                ms(t0),
                hist.len(),
                w.uses_graph(),
                tips.len()
            );
            // spec §8 measures the walk with a commit-graph, which gitty writes on large repos
            if w.uses_graph() {
                budget("first 500 rows", first, 50.0);
                budget("full walk", ms(t0), 400.0);
            } else {
                println!("  no commit-graph: walk budgets not checked");
            }
        }
        "rows" => {
            let n: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(500);
            let mut w = h.walker(&refs.tips(HistoryScope::AllRefs))?;
            let mut hist = w.new_history();
            w.step(&h, &mut hist, n)?;
            let t = Instant::now();
            for id in hist.ids(0..hist.len()) {
                std::hint::black_box(h.decode_row(id)?);
            }
            println!("rows n={} per_row={:.1}us", hist.len(), t.elapsed().as_secs_f64() * 1e6 / hist.len().max(1) as f64);
        }
        "files" => {
            let n: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(500);
            let stats = a.get(4).map(String::as_str) == Some("stats");
            let mut w = h.walker(&refs.tips(HistoryScope::HeadAndUpstream))?;
            let mut hist = w.new_history();
            w.step(&h, &mut hist, n)?;
            let mut times = vec![];
            let mut nfiles = 0;
            for id in hist.ids(0..hist.len()) {
                let t = Instant::now();
                let files = h.commit_files(id, false)?;
                if stats {
                    for f in files.iter().take(40) {
                        std::hint::black_box(h.line_stats(f)?);
                    }
                }
                nfiles += files.len();
                times.push(ms(t));
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: usize| times[(times.len() * p / 100).min(times.len() - 1)];
            println!(
                "files stats={stats} commits={} files={nfiles} p50={:.3}ms p99={:.3}ms max={:.3}ms",
                times.len(),
                pct(50),
                pct(99),
                times.last().unwrap()
            );
            if !stats {
                budget("commit file list p50", pct(50), 5.0);
            }
        }
        "ab" => {
            let (Some(l), Some((name, u))) = (refs.head_id(), refs.upstream.clone()) else {
                println!("ab: no upstream, skipped");
                return Ok(());
            };
            let t = Instant::now();
            let ab = h.ahead_behind(l, u)?;
            println!("ab vs {name}: ahead={} behind={} {:.2}ms", ab.ahead.len(), ab.behind.len(), ms(t));
            budget("ahead/behind", ms(t), 150.0);
        }
        "abrefs" => {
            let l = rev(&repo, &a[3])?;
            let u = rev(&repo, &a[4])?;
            let t = Instant::now();
            let ab = h.ahead_behind(l, u)?;
            println!("ab {}...{}: ahead={} behind={} {:.2}ms", a[3], a[4], ab.ahead.len(), ab.behind.len(), ms(t));
            budget("ahead/behind", ms(t), 150.0);
        }
        "diffs" => {
            let n: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(300);
            let mut w = h.walker(&refs.tips(HistoryScope::HeadAndUpstream))?;
            let mut hist = w.new_history();
            w.step(&h, &mut hist, n)?;
            let (mut times, mut lines, mut changes) = (vec![], 0u64, 0usize);
            let t_all = Instant::now();
            for id in hist.ids(0..hist.len()) {
                for fc in h.commit_files(id, false)? {
                    let t = Instant::now();
                    let d = h.file_diff(&fc, gitty_core::diff::DiffOptions::default())?;
                    for c in 0..d.changes.len() {
                        std::hint::black_box(d.intraline(c));
                    }
                    let v = d.view();
                    std::hint::black_box(v.rows(0..v.row_count().min(80)));
                    times.push(ms(t));
                    lines += (d.added + d.removed) as u64;
                    changes += d.changes.len();
                }
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: usize| times[(times.len() * p / 100).min(times.len() - 1)];
            println!(
                "diffs files={} changes={changes} lines={lines} p50={:.3}ms p99={:.3}ms max={:.2}ms total={:.0}ms",
                times.len(), pct(50), pct(99), times.last().unwrap(), ms(t_all)
            );
            budget("file diff p50", pct(50), 10.0);
        }
        "status" => {
            // gitty refreshes stale stat data itself when a status is slow; start from that state
            let cli = gitty_core::git_cli::GitCli::new(&repo);
            repo.git().command().arg("-C").arg(path).args(["update-index", "-q", "--refresh"]).output()?;
            let mut times: Vec<f64> = (0..10)
                .map(|_| {
                    let t = Instant::now();
                    cli.status().map(|s| (std::hint::black_box(s), ms(t)).1)
                })
                .collect::<Result<_, _>>()?;
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!("status median={:.2}ms best={:.2}ms", times[5], times[0]);
            budget("status median", times[5], 70.0);
        }
        _ => anyhow::bail!("usage: probe walk|rows|files|diffs|ab|abrefs|status <repo> ... [--check]"),
    }
    Ok(())
}

fn rev(repo: &Repo, r: &str) -> anyhow::Result<CommitId> {
    let out = repo.git().command().arg("--git-dir").arg(repo.git_dir()).args(["rev-parse", &format!("{r}^{{commit}}")]).output()?;
    CommitId::from_hex(String::from_utf8(out.stdout)?.trim()).ok_or_else(|| anyhow::anyhow!("bad rev {r}"))
}
