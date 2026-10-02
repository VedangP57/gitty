use gitty_core::diff::ops::{compute_ops, DiffAlgorithm, Op, WsMode};
use gitty_core::diff::text::Text;
use proptest::prelude::*;

fn apply(old: &Text, new: &Text, ops: &[Op]) -> Vec<Vec<u8>> {
    let mut out = vec![];
    let (mut o, mut n) = (0u32, 0u32);
    for op in ops {
        match op {
            Op::Equal { old: os, new: ns, len } => {
                assert_eq!((*os, *ns), (o, n), "ops must be contiguous");
                assert!(*len > 0);
                for i in 0..*len {
                    out.push(new.line(ns + i).to_vec());
                }
                o += len;
                n += len;
            }
            Op::Change { old: or, new: nr } => {
                assert_eq!((or.start, nr.start), (o, n));
                assert!(!or.is_empty() || !nr.is_empty());
                for i in nr.clone() {
                    out.push(new.line(i).to_vec());
                }
                o = or.end;
                n = nr.end;
            }
        }
    }
    assert_eq!((o, n), (old.len(), new.len()));
    out
}

fn lines_strat() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(prop::sample::select(vec!["a", "b", "c", "  a", "fn x() {", "}", ""]), 0..40).prop_map(|v| {
        let mut s = v.join("\n");
        if !s.is_empty() {
            s.push('\n');
        }
        s.into_bytes()
    })
}

proptest! {
    #[test]
    fn ops_reconstruct_new(a in lines_strat(), b in lines_strat(), hist in any::<bool>()) {
        let (ta, tb) = (Text::new(a), Text::new(b));
        let alg = if hist { DiffAlgorithm::Histogram } else { DiffAlgorithm::Myers };
        let ops = compute_ops(&ta, &tb, alg, WsMode::Show);
        let got = apply(&ta, &tb, &ops);
        let want: Vec<Vec<u8>> = (0..tb.len()).map(|i| tb.line(i).to_vec()).collect();
        prop_assert_eq!(got, want);
        for op in &ops {
            if let Op::Equal { old, new, len } = op {
                for i in 0..*len { prop_assert_eq!(ta.raw_line(old + i), tb.raw_line(new + i)); }
            }
        }
        for w in ops.windows(2) {
            let same_kind = matches!((&w[0], &w[1]), (Op::Equal { .. }, Op::Equal { .. }) | (Op::Change { .. }, Op::Change { .. }));
            prop_assert!(!same_kind, "adjacent ops of the same kind: {:?}", w);
        }
    }
}

#[test]
fn identical_is_single_equal() {
    let t = Text::new(b"a\nb\n".to_vec());
    assert_eq!(compute_ops(&t, &t, DiffAlgorithm::Myers, WsMode::Show), vec![Op::Equal { old: 0, new: 0, len: 2 }]);
    let e = Text::new(vec![]);
    assert!(compute_ops(&e, &e, DiffAlgorithm::Myers, WsMode::Show).is_empty());
}

#[test]
fn add_and_delete_whole_file() {
    let e = Text::new(vec![]);
    let t = Text::new(b"x\ny\n".to_vec());
    assert_eq!(compute_ops(&e, &t, DiffAlgorithm::Myers, WsMode::Show), vec![Op::Change { old: 0..0, new: 0..2 }]);
    assert_eq!(compute_ops(&t, &e, DiffAlgorithm::Myers, WsMode::Show), vec![Op::Change { old: 0..2, new: 0..0 }]);
}

#[test]
fn missing_final_newline_is_a_change() {
    let a = Text::new(b"x\ny".to_vec());
    let b = Text::new(b"x\ny\n".to_vec());
    assert_eq!(
        compute_ops(&a, &b, DiffAlgorithm::Myers, WsMode::Show),
        vec![Op::Equal { old: 0, new: 0, len: 1 }, Op::Change { old: 1..2, new: 1..2 }]
    );
}

#[test]
fn whitespace_modes() {
    let a = Text::new(b"if x {\n  y();\n}\n".to_vec());
    let b = Text::new(b"if x {\n    y();  \n}\n".to_vec());
    assert_eq!(compute_ops(&a, &b, DiffAlgorithm::Myers, WsMode::Show).len(), 3);
    assert_eq!(compute_ops(&a, &b, DiffAlgorithm::Myers, WsMode::IgnoreAll), vec![Op::Equal { old: 0, new: 0, len: 3 }]);
    assert_eq!(compute_ops(&a, &b, DiffAlgorithm::Myers, WsMode::IgnoreAmount), vec![Op::Equal { old: 0, new: 0, len: 3 }]);
    let c = Text::new(b"if x {\n  y ();\n}\n".to_vec());
    assert_eq!(compute_ops(&a, &c, DiffAlgorithm::Myers, WsMode::IgnoreAmount).len(), 3);
    assert_eq!(compute_ops(&a, &c, DiffAlgorithm::Myers, WsMode::IgnoreAll), vec![Op::Equal { old: 0, new: 0, len: 3 }]);
}

fn git_hunks(old: &[u8], new: &[u8]) -> (Vec<(u32, u32, u32, u32)>, String) {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("o"), old).unwrap();
    std::fs::write(d.path().join("n"), new).unwrap();
    let out = std::process::Command::new("git")
        .current_dir(d.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "diff.indentHeuristic=true", "diff", "--no-index", "--no-color", "-U0", "o", "n"])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let hunks = text
        .lines()
        .filter(|l| l.starts_with("@@"))
        .map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            let parse = |s: &str| {
                let mut it = s[1..].split(',');
                let a: u32 = it.next().unwrap().parse().unwrap();
                let b: u32 = it.next().map(|x| x.parse().unwrap()).unwrap_or(1);
                (a, b)
            };
            let (os, ol) = parse(p[1]);
            let (ns, nl) = parse(p[2]);
            (if ol == 0 { os } else { os - 1 }, ol, if nl == 0 { ns } else { ns - 1 }, nl)
        })
        .collect();
    (hunks, text)
}

fn our_hunks(old: &[u8], new: &[u8]) -> Vec<(u32, u32, u32, u32)> {
    compute_ops(&Text::new(old.to_vec()), &Text::new(new.to_vec()), DiffAlgorithm::Myers, WsMode::Show)
        .iter()
        .filter_map(|o| match o {
            Op::Change { old, new } => Some((old.start, old.len() as u32, new.start, new.len() as u32)),
            _ => None,
        })
        .collect()
}

#[test]
fn matches_git_hunks_with_indent_heuristic() {
    let old = b"int a() {\n\treturn 1;\n}\n\nint c() {\n\treturn 3;\n}\n".to_vec();
    let new = b"int a() {\n\treturn 1;\n}\n\nint b() {\n\treturn 2;\n}\n\nint c() {\n\treturn 3;\n}\n".to_vec();
    let (git, text) = git_hunks(&old, &new);
    assert_eq!(our_hunks(&old, &new), git, "git said:\n{text}");
}

#[test]
fn matches_git_on_long_shared_prefix_and_suffix() {
    let mut old = String::new();
    for i in 0..300 { old.push_str(&format!("    line {i}\n")); }
    let new = old.replace("    line 150\n", "    line 150 changed\n    inserted\n").replace("    line 10\n", "");
    let (git, text) = git_hunks(old.as_bytes(), new.as_bytes());
    assert_eq!(our_hunks(old.as_bytes(), new.as_bytes()), git, "git said:\n{text}");
}
