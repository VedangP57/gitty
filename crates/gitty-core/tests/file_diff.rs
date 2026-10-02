mod common;
use common::Fixture;
use gitty_core::diff::classify::FileClass;
use gitty_core::diff::{DiffOptions, FileDiff};
use gitty_core::{CommitId, Repo};

#[test]
fn diff_of_modified_file() {
    let f = Fixture::new();
    f.write("a.rs", "fn main() {\n    println!(\"hi\");\n}\n");
    f.commit("one", 1_700_000_000);
    f.write("a.rs", "fn main() {\n    println!(\"hello\");\n}\n");
    let c = CommitId::from_hex(&f.commit("two", 1_700_000_100)).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();
    let fc = &h.commit_files(c, false).unwrap()[0];
    let d = h.file_diff(fc, DiffOptions::default()).unwrap();
    assert!(matches!(d.class, FileClass::Text));
    assert!(d.is_text());
    assert_eq!((d.added, d.removed), (1, 1));
    assert_eq!(d.changes.len(), 1);
    let hl = d.intraline(0);
    assert_eq!(hl.pair_of_del, vec![Some(0)]);
    let v = d.view();
    assert_eq!(v.row_count(), 4);
    assert_eq!(v.split_row_count(), 3); // pairing applied
}

#[test]
fn eol_and_bidi_flags() {
    let d = FileDiff::from_bytes("x.txt", None, b"a\nb\n".to_vec(), "a\r\nb\u{202E}\r\n".as_bytes().to_vec(), 0o100644, 0o100644, DiffOptions::default());
    assert!(d.eol_change.is_some());
    assert!(d.bidi_warning);
    let plain = FileDiff::from_bytes("x.txt", None, b"a\n".to_vec(), b"b\n".to_vec(), 0o100644, 0o100644, DiffOptions::default());
    assert!(plain.eol_change.is_none() && !plain.bidi_warning);
}

#[test]
fn generated_keeps_text_for_force() {
    let d = FileDiff::from_bytes("Cargo.lock", None, b"a\n".to_vec(), b"b\n".to_vec(), 0o100644, 0o100644, DiffOptions::default());
    assert!(matches!(d.class, FileClass::Generated { .. }));
    assert!(!d.is_text());
    let d = d.force_text();
    assert!(d.is_text());
    assert_eq!((d.added, d.removed), (1, 1));
}

#[test]
fn binary_and_rename_only() {
    let f = Fixture::new();
    f.write("img.bin", [0u8, 1, 2]);
    f.write("old.txt", "same\ncontent\n");
    f.commit("one", 1_700_000_000);
    f.write("img.bin", [0u8, 3, 4, 5]);
    f.git(&["mv", "old.txt", "new.txt"]);
    let c = CommitId::from_hex(&f.commit("two", 1_700_000_100)).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();
    let files = h.commit_files(c, true).unwrap();
    let bin = files.iter().find(|x| x.path == "img.bin").unwrap();
    assert!(matches!(h.file_diff(bin, DiffOptions::default()).unwrap().class, FileClass::Binary { old_size: 3, new_size: 4 }));
    let ren = files.iter().find(|x| x.path == "new.txt").unwrap();
    let d = h.file_diff(ren, DiffOptions::default()).unwrap();
    assert_eq!((d.added, d.removed), (0, 0));
    assert_eq!(d.old_path.as_deref(), Some("old.txt"));
    assert!(d.changes.is_empty());
}

#[test]
fn whitespace_mode_hides_indent_change() {
    let opts = DiffOptions { ws: gitty_core::diff::ops::WsMode::IgnoreAll, ..Default::default() };
    let d = FileDiff::from_bytes("a.rs", None, b"x\n  y\n".to_vec(), b"x\n    y\n".to_vec(), 0o100644, 0o100644, opts);
    assert_eq!((d.added, d.removed), (0, 0));
}
