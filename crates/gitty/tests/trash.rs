//! Discards when the Trash cannot be written (macOS refuses ~/.Trash without Full Disk Access).
//! Its own binary: it sets process-wide variables the other tests must not see.

#[path = "../../gitty-core/tests/common/mod.rs"]
mod common;

use std::os::unix::fs::PermissionsExt;

use common::Fixture;
use gitty::msg::WriteOp;
use gitty_core::Repo;
use gitty_core::commit_files::BlobId;

#[test]
fn an_unwritable_trash_falls_back_to_the_state_dir() {
    let trash = tempfile::tempdir().unwrap();
    std::fs::set_permissions(trash.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let state = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary
    unsafe {
        std::env::set_var("GITTY_TRASH_DIR", trash.path());
        std::env::set_var("XDG_STATE_HOME", state.path());
    }
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "edited\n");
    let h = Repo::open(f.path()).unwrap().handle();
    let op = WriteOp::WriteFile { path: "a.txt".into(), bytes: b"a\n".to_vec(), expect: BlobId::hash_of(b"edited\n"), head_path: "a.txt".into(), head: Some(BlobId::hash_of(b"a\n")) };
    let note = gitty::write::run(&h, &op, &mut |_| {}).unwrap().expect("says where the copy went");
    let fallback = state.path().join("gitty/trash");
    assert!(note.contains(&fallback.display().to_string()), "{note}");
    let saved: Vec<String> = std::fs::read_dir(&fallback).unwrap().map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap()).collect();
    assert_eq!(saved, ["edited\n"]);
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "a\n");
    std::fs::set_permissions(trash.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}
