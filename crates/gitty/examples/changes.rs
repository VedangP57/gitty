//! Changes-tab latencies on a throwaway `--shared` clone of a repository:
//! `cargo run --release -p gitty-cli --example changes -- PATH [FILE]`.
//!
//! - status: `git status --porcelain=v2`, parsed (best and median of 10), before and after the
//!   index refresh gitty runs when status is slow
//! - save → status: a worktree write until the watcher fires, plus the status run it triggers
//! - stage a line: change diff + staged-line derivation + patch + `git apply --cached`

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gitty::exec::exec;
use gitty::msg::{Gens, Msg, Request, WriteOp};
use gitty_core::Repo;
use gitty_core::diff::DiffOptions;
use gitty_core::git_cli::GitCli;
use gitty_core::watch::{Changed, Watcher};

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn stats(mut v: Vec<f64>) -> String {
    v.sort_by(f64::total_cmp);
    format!("best {:>7.1} ms  median {:>7.1} ms", v[0], v[v.len() / 2])
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git").current_dir(dir).args(args).status().unwrap().success();
    assert!(ok, "git {args:?}");
}

fn main() {
    let src = std::env::args().nth(1).expect("PATH");
    let file = std::env::args().nth(2).unwrap_or_else(|| "README.md".into());
    let tmp = tempfile_dir();
    let dir = tmp.join("clone");
    let t = Instant::now();
    git(&tmp, &["clone", "-q", "--shared", &src, "clone"]);
    println!("clone --shared + checkout {:>8.1} ms", ms(t.elapsed()));
    let repo = Repo::open(&dir).unwrap();
    let cli = GitCli::new(&repo);
    let n = cli.status().unwrap().entries.len();
    let files = std::process::Command::new("git").current_dir(&dir).args(["ls-files"]).output().unwrap().stdout.iter().filter(|b| **b == b'\n').count();
    println!("{files} tracked files, {n} changed");

    let runs: Vec<f64> = (0..10).map(|_| {
        let t = Instant::now();
        cli.status().unwrap();
        ms(t.elapsed())
    }).collect();
    println!("status (fresh checkout)   {}", stats(runs));
    let h = repo.handle();
    let t = Instant::now();
    gitty::write::run(&h, &WriteOp::RefreshIndex, &mut |_| {}).unwrap();
    println!("index refresh             {:>8.1} ms", ms(t.elapsed()));
    let runs: Vec<f64> = (0..10).map(|_| {
        let t = Instant::now();
        cli.status().unwrap();
        ms(t.elapsed())
    }).collect();
    println!("status (after refresh)    {}", stats(runs));

    // save → watcher → status
    let (tx, rx) = mpsc::channel();
    let _w = Watcher::spawn(&repo, move |c| {
        if c.intersects(Changed::WORKTREE) {
            let _ = tx.send(Instant::now());
        }
    }).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    while rx.try_recv().is_ok() {}
    let path = dir.join(&file);
    let original = std::fs::read(&path).unwrap();
    let mut fire = Vec::new();
    let mut total = Vec::new();
    for i in 0..10 {
        let mut b = original.clone();
        b.extend_from_slice(format!("edit {i}\n").as_bytes());
        let t = Instant::now();
        std::fs::write(&path, &b).unwrap();
        let fired = rx.recv_timeout(Duration::from_secs(5)).expect("watcher fired");
        cli.status().unwrap();
        fire.push(ms(fired - t));
        total.push(ms(t.elapsed()));
        std::thread::sleep(Duration::from_millis(400));
        while rx.try_recv().is_ok() {}
    }
    println!("save → watcher fired      {}", stats(fire));
    println!("save → status refreshed   {}", stats(total));

    // stage one line, the way Space does: diff load, then the write
    let gens = Gens::default();
    let mut body = original.clone();
    body.extend_from_slice(b"first added line\nsecond added line\n");
    std::fs::write(&path, &body).unwrap();
    let mut load = Vec::new();
    let mut write = Vec::new();
    for _ in 0..10 {
        git(&dir, &["reset", "-q"]);
        let entry = cli.status().unwrap().entries.into_iter().find(|e| e.path == file).unwrap();
        let t = Instant::now();
        let mut got = None;
        exec(&h, Request::ChangeDiff { generation: 0, entry: entry.clone(), opts: DiffOptions::default(), force_text: false }, &mut |m| {
            if let Msg::ChangeDiff { diff, texts, staged, .. } = m {
                got = Some((diff, texts, staged.unwrap()));
            }
        }, &gens);
        load.push(ms(t.elapsed()));
        let (diff, texts, mut flags) = got.unwrap();
        *flags.last_mut().unwrap() = true;
        let t = Instant::now();
        gitty::write::run(&h, &WriteOp::SetStaged { entry, texts, diff, flags }, &mut |_| {}).unwrap();
        write.push(ms(t.elapsed()));
    }
    println!("change diff load          {}", stats(load));
    println!("stage a line (write)      {}", stats(write));
    let _ = std::fs::remove_dir_all(&tmp);
}

fn tempfile_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("gitty-changes-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}
