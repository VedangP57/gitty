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
    let mut v = d.view();
    assert_eq!(v.row_count(), 4);
    d.apply_pairing(&mut v, 0..1);
    assert_eq!(v.split_row_count(), 3); // paired del+add share a row
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

/// Review finding: building split pairing for every change block was O(blocks²) and eager.
#[test]
fn pairing_many_blocks_is_linear_and_lazy() {
    let mut old = String::new();
    let mut new = String::new();
    for i in 0..30_000 {
        old.push_str(&format!("line {i}\n"));
        new.push_str(&if i % 3 == 1 { format!("line {i} changed\n") } else { format!("line {i}\n") });
    }
    let d = FileDiff::from_bytes("big.txt", None, old.into_bytes(), new.into_bytes(), 0o100644, 0o100644, DiffOptions::default());
    assert!(d.is_text(), "{:?}", d.class);
    assert_eq!(d.changes.len(), 10_000);
    let t = std::time::Instant::now();
    let mut v = d.view();
    assert!(t.elapsed().as_millis() < 200, "view() took {:?}", t.elapsed());
    assert!(!d.is_paired(0), "view() must not compute intraline eagerly");
    let t = std::time::Instant::now();
    d.apply_pairing(&mut v, 0..d.changes.len());
    assert!(t.elapsed().as_millis() < 3_000, "apply_pairing over all blocks took {:?}", t.elapsed());
    assert!(d.is_paired(9_999));
    assert_eq!(v.split_row_count(), v.row_count() - 10_000);
}

#[test]
fn intraline_ready_is_lazy() {
    let d = FileDiff::from_bytes("a.txt", None, b"one\ntwo\n".to_vec(), b"one\nthree\n".to_vec(), 0o100644, 0o100644, DiffOptions::default());
    assert!(d.intraline_ready(0).is_none());
    let h = d.intraline(0).clone();
    assert_eq!(d.intraline_ready(0), Some(&h));
    assert!(d.intraline_ready(99).is_none());
}
