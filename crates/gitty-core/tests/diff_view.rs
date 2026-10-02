use gitty_core::diff::ops::{compute_ops, DiffAlgorithm, WsMode};
use gitty_core::diff::text::Text;
use gitty_core::diff::view::{DiffView, Expand, Row};
use proptest::prelude::*;

fn mk(old: &str, new: &str) -> (Text, Text, DiffView) {
    let (o, n) = (Text::new(old.as_bytes().to_vec()), Text::new(new.as_bytes().to_vec()));
    let ops = compute_ops(&o, &n, DiffAlgorithm::Myers, WsMode::Show);
    let v = DiffView::new(&ops, &o, &n);
    (o, n, v)
}

fn numbered(n: u32) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

#[test]
fn default_context_and_gaps() {
    let old = numbered(100);
    let new = old.replace("line 50\n", "line fifty\n");
    let (_, _, v) = mk(&old, &new);
    let rows = v.rows(0..v.row_count());
    assert_eq!(rows.len(), 1 + 3 + 2 + 3 + 1, "{rows:#?}");
    assert!(matches!(rows[0], Row::Gap { hidden: 46, can_up: true, can_down: false, .. }), "{:?}", rows[0]);
    assert!(matches!(rows[1], Row::Context { old: 46, new: 46 }));
    assert!(matches!(rows[4], Row::Del { old: 49, change: 0 }));
    assert!(matches!(rows[5], Row::Add { new: 49, change: 0 }));
    assert!(matches!(rows[9], Row::Gap { hidden: 47, can_up: false, can_down: true, .. }));
    match &rows[0] {
        Row::Gap { header, .. } => assert!(header.starts_with("@@ -47,7 +47,7 @@"), "{header}"),
        _ => unreachable!(),
    }
    assert_eq!(v.gap_rows(), vec![0, 9]);
    assert_eq!(v.hunk_starts(), vec![4]);
}

#[test]
fn expand_up_down_all_whole() {
    let old = numbered(100);
    let new = old.replace("line 50\n", "line fifty\n");
    let (_, _, mut v) = mk(&old, &new);
    v.expand(Expand::Up(0));
    assert!(matches!(v.row(0), Row::Gap { hidden: 26, .. }), "{:?}", v.row(0));
    v.expand(Expand::Up(0));
    assert!(matches!(v.row(0), Row::Gap { hidden: 6, .. }));
    v.expand(Expand::Up(0));
    assert!(matches!(v.row(0), Row::Context { old: 0, new: 0 }));
    v.expand(Expand::Down(1));
    let last = v.row(v.row_count() - 1);
    assert!(matches!(last, Row::Gap { hidden: 27, .. }), "{last:?}");
    v.expand(Expand::WholeFile);
    assert_eq!(v.row_count(), 101);
    v.expand(Expand::Collapse);
    assert_eq!(v.row_count(), 10);
    v.expand(Expand::All(1));
    assert_eq!(v.row_count(), 10 - 1 + 47);
}

#[test]
fn small_gap_between_changes_has_no_gap_row() {
    let old = numbered(20);
    let new = old.replace("line 5\n", "five\n").replace("line 12\n", "twelve\n");
    let (_, _, v) = mk(&old, &new);
    let gaps = v.rows(0..v.row_count()).iter().filter(|r| matches!(r, Row::Gap { .. })).count();
    assert_eq!(gaps, 2);
}

#[test]
fn added_file_and_identical() {
    let (_, _, v) = mk("", "a\nb\n");
    assert_eq!(v.rows(0..v.row_count()).len(), 2);
    let (_, _, v) = mk("same\n", "same\n");
    assert_eq!(v.row_count(), 1);
    assert!(matches!(v.row(0), Row::Gap { hidden: 1, can_up: false, can_down: false, .. }));
    let (_, _, v) = mk("", "");
    assert_eq!(v.row_count(), 0);
}

#[test]
fn funcname_header() {
    let old = "fn alpha() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    let d = 4;\n    let e = 5;\n}\n";
    let new = old.replace("let e = 5;", "let e = 50;");
    let (_, _, v) = mk(old, &new);
    match v.row(0) {
        Row::Gap { header, .. } => assert!(header.ends_with("fn alpha() {"), "{header}"),
        r => panic!("{r:?}"),
    }
}

proptest! {
    #[test]
    fn expansion_invariants(seed in prop::collection::vec(0u8..8, 0..12), cut in 1u32..60) {
        let old = numbered(80);
        let new = old.replace(&format!("line {cut}\n"), "changed\n").replace("line 70\n", "");
        let (o, n, mut v) = mk(&old, &new);
        for s in seed {
            let gaps = v.gap_rows();
            if gaps.is_empty() { break; }
            let g = match v.row(gaps[s as usize % gaps.len()]) { Row::Gap { gap, .. } => gap, _ => unreachable!() };
            v.expand(match s % 5 { 0 => Expand::Up(g), 1 => Expand::Down(g), 2 => Expand::All(g), 3 => Expand::Up(g), _ => Expand::Down(g) });
        }
        let (mut lo, mut ln) = (None::<u32>, None::<u32>);
        let (mut seen_old, mut seen_new, mut hidden) = (0u32, 0u32, 0u32);
        for r in v.rows(0..v.row_count()) {
            match r {
                Row::Context { old, new } => {
                    prop_assert!(lo.is_none_or(|p| old == p + 1) || lo.is_none(), "old gap before {}", old);
                    prop_assert!(ln.is_none_or(|p| new > p));
                    lo = Some(old); ln = Some(new); seen_old += 1; seen_new += 1;
                }
                Row::Del { old, .. } => { prop_assert!(lo.is_none_or(|p| old > p)); lo = Some(old); seen_old += 1; }
                Row::Add { new, .. } => { prop_assert!(ln.is_none_or(|p| new > p)); ln = Some(new); seen_new += 1; }
                Row::Gap { hidden: h, .. } => {
                    hidden += h;
                    lo = lo.map(|p| p + h).or(Some(h - 1));
                    ln = ln.map(|p| p + h).or(Some(h - 1));
                }
            }
        }
        prop_assert_eq!(seen_old + hidden, o.len());
        prop_assert_eq!(seen_new + hidden, n.len());
    }
}
