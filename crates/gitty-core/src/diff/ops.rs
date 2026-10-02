//! Line diff ops over complete texts (imara-diff via gix, Myers + indent heuristic by default).

use std::ops::Range;

use gix::diff::blob::{Algorithm, Diff, InternedInput};

use super::text::Text;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum DiffAlgorithm {
    #[default]
    Myers,
    Histogram,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum WsMode {
    #[default]
    Show,
    /// `git diff -w`: ignore all whitespace.
    IgnoreAll,
    /// `git diff -b`: ignore changes in amount of whitespace and trailing whitespace.
    IgnoreAmount,
}

/// A run of the diff. Ops cover both texts completely, in order, alternating kinds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Op {
    Equal { old: u32, new: u32, len: u32 },
    Change { old: Range<u32>, new: Range<u32> },
}

pub fn compute_ops(old: &Text, new: &Text, alg: DiffAlgorithm, ws: WsMode) -> Vec<Op> {
    let (no, nn) = (old.len(), new.len());
    if no == nn && old.bytes() == new.bytes() {
        return if no == 0 { vec![] } else { vec![Op::Equal { old: 0, new: 0, len: no }] };
    }
    // No manual prefix/suffix trim: imara strips common ends itself after interning, and a trim
    // window would stop the indent heuristic from sliding hunks the way git does.
    let (p, om, nm) = (0u32, 0..no, 0..nn);
    let hunks: Vec<(Range<u32>, Range<u32>)> = match ws {
        WsMode::Show => {
            let o: Vec<&[u8]> = om.clone().map(|i| old.raw_line(i)).collect();
            let n: Vec<&[u8]> = nm.clone().map(|i| new.raw_line(i)).collect();
            diff_tokens(&o, &n, alg)
        }
        WsMode::IgnoreAll | WsMode::IgnoreAmount => {
            let norm = |t: &Text, i: u32| normalize(t.line(i), ws);
            let o: Vec<Vec<u8>> = om.clone().map(|i| norm(old, i)).collect();
            let n: Vec<Vec<u8>> = nm.clone().map(|i| norm(new, i)).collect();
            let o: Vec<&[u8]> = o.iter().map(Vec::as_slice).collect();
            let n: Vec<&[u8]> = n.iter().map(Vec::as_slice).collect();
            diff_tokens(&o, &n, alg)
        }
    };

    let mut ops = Vec::with_capacity(hunks.len() * 2 + 1);
    let (mut o, mut n) = (0u32, 0u32);
    for (b, a) in hunks {
        let (bs, as_) = (b.start + p, a.start + p);
        if bs > o {
            push_equal(&mut ops, o, n, bs - o);
        }
        ops.push(Op::Change { old: bs..b.end + p, new: as_..a.end + p });
        o = b.end + p;
        n = a.end + p;
    }
    if o < no {
        debug_assert_eq!(no - o, nn - n);
        push_equal(&mut ops, o, n, no - o);
    }
    ops
}

fn push_equal(ops: &mut Vec<Op>, old: u32, new: u32, len: u32) {
    if let Some(Op::Equal { len: l, .. }) = ops.last_mut() {
        *l += len;
    } else {
        ops.push(Op::Equal { old, new, len });
    }
}

fn diff_tokens(o: &[&[u8]], n: &[&[u8]], alg: DiffAlgorithm) -> Vec<(Range<u32>, Range<u32>)> {
    let mut input: InternedInput<&[u8]> = InternedInput::default();
    input.update_before(o.iter().copied());
    input.update_after(n.iter().copied());
    let alg = match alg {
        DiffAlgorithm::Myers => Algorithm::Myers,
        DiffAlgorithm::Histogram => Algorithm::Histogram,
    };
    let mut diff = Diff::default();
    diff.compute_with(alg, &input.before, &input.after, input.interner.num_tokens());
    diff.postprocess_lines(&input);
    diff.hunks().map(|h| (h.before, h.after)).collect()
}

fn normalize(line: &[u8], ws: WsMode) -> Vec<u8> {
    match ws {
        WsMode::IgnoreAll => line.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect(),
        _ => {
            let mut out = Vec::with_capacity(line.len());
            let mut in_ws = false;
            for &b in line {
                if b.is_ascii_whitespace() {
                    in_ws = true;
                } else {
                    if in_ws && !out.is_empty() {
                        out.push(b' ');
                    } else if in_ws {
                        // leading whitespace still counts as "some whitespace" for -b
                        out.push(b' ');
                    }
                    in_ws = false;
                    out.push(b);
                }
            }
            out
        }
    }
}

/// (added, removed) line counts.
pub fn change_counts(ops: &[Op]) -> (u32, u32) {
    ops.iter().fold((0, 0), |(a, r), op| match op {
        Op::Change { old, new } => (a + new.len() as u32, r + old.len() as u32),
        Op::Equal { .. } => (a, r),
    })
}
