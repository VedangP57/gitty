//! Measures the core read path on a real repository. See bench/run.sh.

use std::time::Instant;

use gitty_core::refs::HistoryScope;
use gitty_core::{CommitId, Repo};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
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
        }
        "ab" => {
            let (Some(l), Some((name, u))) = (refs.head_id(), refs.upstream.clone()) else { anyhow::bail!("no upstream") };
            let t = Instant::now();
            let ab = h.ahead_behind(l, u)?;
            println!("ab vs {name}: ahead={} behind={} {:.2}ms", ab.ahead.len(), ab.behind.len(), ms(t));
        }
        "abrefs" => {
            let l = rev(&repo, &a[3])?;
            let u = rev(&repo, &a[4])?;
            let t = Instant::now();
            let ab = h.ahead_behind(l, u)?;
            println!("ab {}...{}: ahead={} behind={} {:.2}ms", a[3], a[4], ab.ahead.len(), ab.behind.len(), ms(t));
        }
        _ => anyhow::bail!("usage: probe walk|rows|files|ab|abrefs <repo> ..."),
    }
    Ok(())
}

fn rev(repo: &Repo, r: &str) -> anyhow::Result<CommitId> {
    let out = repo.git().command().arg("--git-dir").arg(repo.git_dir()).args(["rev-parse", &format!("{r}^{{commit}}")]).output()?;
    CommitId::from_hex(String::from_utf8(out.stdout)?.trim()).ok_or_else(|| anyhow::anyhow!("bad rev {r}"))
}
