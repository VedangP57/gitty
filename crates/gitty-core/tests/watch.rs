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

/// inotify (Linux) also reports opens and reads; gitty's own status run reads the index and
/// .gitignore, so a read must never count as a change (it would refresh in a loop).
#[test]
fn reading_files_is_not_a_change() {
    let f = Fixture::new();
    f.write(".gitignore", "*.log\n");
    f.write("src/a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    let repo = Repo::open(f.path()).unwrap();
    let (tx, rx) = mpsc::channel();
    let _w = Watcher::spawn(&repo, move |c| {
        let _ = tx.send(c);
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    collect(&rx, Duration::from_millis(200));
    for p in ["src/a.txt", ".gitignore", ".git/index", ".git/HEAD", ".git/config"] {
        std::fs::read(f.path().join(p)).unwrap();
    }
    f.git(&["--no-optional-locks", "status", "--porcelain"]);
    let m = collect(&rx, Duration::from_millis(800));
    assert!(m.is_empty(), "reads triggered {m:?}");
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

#[test]
fn a_linked_worktree_sees_refs_made_from_the_main_one() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    let wt = f.path().parent().unwrap().join("linked");
    f.git(&["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
    let repo = Repo::open(&wt).unwrap();
    let (tx, rx) = mpsc::channel();
    let _w = Watcher::spawn(&repo, move |c| {
        let _ = tx.send(c);
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    collect(&rx, Duration::from_millis(200));

    // branches live in the common git dir, outside this worktree's own
    f.git(&["branch", "other"]);
    let m = collect(&rx, Duration::from_millis(800));
    assert!(m.contains(Changed::REFS), "{m:?}");

    // the main worktree's index and HEAD are not this worktree's
    f.write("a.txt", "a\nmain\n");
    f.git(&["add", "a.txt"]);
    let m = collect(&rx, Duration::from_millis(800));
    assert!(!m.intersects(Changed::INDEX | Changed::WORKTREE), "{m:?}");
}
