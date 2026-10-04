mod common;

use std::sync::mpsc;
use std::time::Duration;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::watch::{Changed, Watcher};

fn collect(rx: &mpsc::Receiver<Changed>, wait: Duration) -> Changed {
    let mut m = Changed::NONE;
    let end = std::time::Instant::now() + wait;
    while let Ok(c) = rx.recv_timeout(end.saturating_duration_since(std::time::Instant::now())) {
        m |= c;
    }
    m
}

#[test]
fn worktree_index_and_ignored_paths() {
    let f = Fixture::new();
    f.write(".gitignore", "target/\n*.log\n");
    f.write("src/a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    std::fs::create_dir_all(f.path().join("target/debug")).unwrap();
    let repo = Repo::open(f.path()).unwrap();
    let (tx, rx) = mpsc::channel();
    let w = Watcher::spawn(&repo, move |c| {
        let _ = tx.send(c);
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    collect(&rx, Duration::from_millis(200));

    // a build in an ignored directory and ignored files: silence
    for i in 0..200 {
        f.write(&format!("target/debug/obj{i}.o"), "x");
    }
    f.write("build.log", "noise");
    let m = collect(&rx, Duration::from_millis(800));
    assert!(!m.intersects(Changed::WORKTREE), "ignored paths triggered {m:?}");

    f.write("src/a.txt", "a\nb\n");
    let m = collect(&rx, Duration::from_millis(800));
    assert!(m.contains(Changed::WORKTREE), "{m:?}");

    f.git(&["add", "src/a.txt"]);
    let m = collect(&rx, Duration::from_millis(800));
    assert!(m.contains(Changed::INDEX), "{m:?}");

    f.git(&["commit", "-q", "-m", "next"]);
    let m = collect(&rx, Duration::from_millis(800));
    assert!(m.contains(Changed::REFS), "{m:?}");

    // an index state gitty's own status run has already seen does not echo back
    f.write("src/b.txt", "b\n");
    collect(&rx, Duration::from_millis(400));
    f.git(&["add", "src/b.txt"]);
    w.index_mark().note();
    let m = collect(&rx, Duration::from_millis(600));
    assert!(!m.contains(Changed::INDEX), "{m:?}");
    // a later change to it does
    f.git(&["rm", "-q", "--cached", "src/b.txt"]);
    let m = collect(&rx, Duration::from_millis(800));
    assert!(m.contains(Changed::INDEX), "{m:?}");
}
