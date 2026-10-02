//! Word-level highlights inside changed lines.

use std::ops::Range;

use gix::diff::blob::{Algorithm, Diff, InternedInput};
use smallvec::SmallVec;

pub const MAX_LINE: usize = 1024;
pub const MAX_PRODUCT: usize = 4096;
pub const MAX_CANDIDATES: usize = 32;
pub const MAX_DISTANCE: f32 = 0.6;

pub type Ranges = SmallVec<[Range<u32>; 2]>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockHighlights {
    pub pair_of_del: Vec<Option<u32>>,
    pub pair_of_add: Vec<Option<u32>>,
    pub del_emph: Vec<Ranges>,
    pub add_emph: Vec<Ranges>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class(b: u8) -> Class {
    if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 {
        Class::Word
    } else if b.is_ascii_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

/// Word runs (`[A-Za-z0-9_]` or non-ASCII), whitespace runs, and single punctuation bytes.
pub fn tokenize(line: &[u8]) -> SmallVec<[Range<u32>; 16]> {
    let mut out = SmallVec::new();
    let mut i = 0;
    while i < line.len() {
        let c = class(line[i]);
        let mut j = i + 1;
        if c != Class::Other {
            while j < line.len() && class(line[j]) == c {
                j += 1;
            }
        }
        out.push(i as u32..j as u32);
        i = j;
    }
    out
}

fn chars(line: &[u8]) -> usize {
    line.iter().filter(|&&b| b & 0xC0 != 0x80).count()
}

fn trimmed_width(t: &[u8]) -> usize {
    t.iter().filter(|b| !b.is_ascii_whitespace()).count()
}

/// Token diff of one line pair: distance plus removed/added token flags.
struct PairDiff {
    distance: f32,
    removed: Vec<bool>,
    added: Vec<bool>,
}

struct Scratch<'a> {
    input: InternedInput<&'a [u8]>,
    diff: Diff,
}

impl<'a> Scratch<'a> {
    fn new() -> Self {
        Scratch { input: InternedInput::default(), diff: Diff::default() }
    }

    fn diff(&mut self, a: &'a [u8], ta: &[Range<u32>], b: &'a [u8], tb: &[Range<u32>]) -> PairDiff {
        self.input.clear();
        self.input.update_before(ta.iter().map(|r| &a[r.start as usize..r.end as usize]));
        self.input.update_after(tb.iter().map(|r| &b[r.start as usize..r.end as usize]));
        self.diff.compute_with(Algorithm::Myers, &self.input.before, &self.input.after, self.input.interner.num_tokens());
        let removed: Vec<bool> = (0..ta.len() as u32).map(|i| self.diff.is_removed(i)).collect();
        let added: Vec<bool> = (0..tb.len() as u32).map(|i| self.diff.is_added(i)).collect();
        let (mut changed, mut equal) = (0usize, 0usize);
        for (r, &rm) in ta.iter().zip(&removed) {
            let w = trimmed_width(&a[r.start as usize..r.end as usize]);
            if rm { changed += w } else { equal += w }
        }
        for (r, &ad) in tb.iter().zip(&added) {
            if ad {
                changed += trimmed_width(&b[r.start as usize..r.end as usize]);
            }
        }
        let den = changed + 2 * equal;
        let distance = if den == 0 { 0.0 } else { changed as f32 / den as f32 };
        PairDiff { distance, removed, added }
    }
}

/// Emphasis ranges from changed token flags: merge across whitespace-only equal gaps, then drop
/// ranges that are only leading/trailing whitespace.
fn emphasis(line: &[u8], toks: &[Range<u32>], changed: &[bool]) -> Ranges {
    let mut out: Ranges = SmallVec::new();
    let mut pending_ws: Option<u32> = None; // end of a whitespace run after the last range
    for (r, &c) in toks.iter().zip(changed) {
        let is_ws = line[r.start as usize..r.end as usize].iter().all(u8::is_ascii_whitespace);
        if c {
            match out.last_mut() {
                Some(last) if last.end == r.start || pending_ws == Some(r.start) => last.end = r.end,
                _ => out.push(r.clone()),
            }
            pending_ws = None;
        } else if is_ws && out.last().is_some_and(|l| l.end == r.start) {
            pending_ws = Some(r.end);
        } else {
            pending_ws = None;
        }
    }
    let len = line.len() as u32;
    out.retain(|r| {
        let ws = line[r.start as usize..r.end as usize].iter().all(u8::is_ascii_whitespace);
        !(ws && (r.start == 0 || r.end == len))
    });
    out
}

pub fn block_highlights(dels: &[&[u8]], adds: &[&[u8]]) -> BlockHighlights {
    let (d, a) = (dels.len(), adds.len());
    let mut h = BlockHighlights {
        pair_of_del: vec![None; d],
        pair_of_add: vec![None; a],
        del_emph: vec![SmallVec::new(); d],
        add_emph: vec![SmallVec::new(); a],
    };
    if d == 0 || a == 0 {
        return h;
    }
    let ok = |l: &[u8]| chars(l) < MAX_LINE;
    let dt: Vec<_> = dels.iter().map(|l| if ok(l) { tokenize(l) } else { SmallVec::new() }).collect();
    let at: Vec<_> = adds.iter().map(|l| if ok(l) { tokenize(l) } else { SmallVec::new() }).collect();
    let similar_len = |x: &[u8], y: &[u8]| {
        let (lo, hi) = (x.len().min(y.len()), x.len().max(y.len()));
        hi == 0 || lo as f32 / hi as f32 >= 0.2
    };
    let mut scratch = Scratch::new();
    let mut accept = |h: &mut BlockHighlights, i: usize, j: usize, pd: PairDiff| {
        h.pair_of_del[i] = Some(j as u32);
        h.pair_of_add[j] = Some(i as u32);
        h.del_emph[i] = emphasis(dels[i], &dt[i], &pd.removed);
        h.add_emph[j] = emphasis(adds[j], &at[j], &pd.added);
    };
    if d * a <= MAX_PRODUCT {
        let mut next_free = 0usize;
        for i in 0..d {
            if !ok(dels[i]) {
                continue;
            }
            // closest of the next MAX_CANDIDATES unpaired adds (ties → earliest); pairing stays monotone
            let mut tried = 0;
            let mut best: Option<(usize, PairDiff)> = None;
            let mut j = next_free;
            while j < a && tried < MAX_CANDIDATES {
                if h.pair_of_add[j].is_none() && ok(adds[j]) && similar_len(dels[i], adds[j]) {
                    tried += 1;
                    let pd = scratch.diff(dels[i], &dt[i], adds[j], &at[j]);
                    let better = best.as_ref().is_none_or(|(_, b)| pd.distance < b.distance);
                    if pd.distance <= MAX_DISTANCE && better {
                        let exact = pd.distance == 0.0;
                        best = Some((j, pd));
                        if exact {
                            break;
                        }
                    }
                }
                j += 1;
            }
            if let Some((j, pd)) = best {
                accept(&mut h, i, j, pd);
                next_free = j + 1;
            }
        }
    } else if d == a {
        for i in 0..d {
            if ok(dels[i]) && ok(adds[i]) && similar_len(dels[i], adds[i]) {
                let pd = scratch.diff(dels[i], &dt[i], adds[i], &at[i]);
                if pd.distance <= MAX_DISTANCE {
                    accept(&mut h, i, i, pd);
                }
            }
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s<'a>(r: &Range<u32>, l: &'a str) -> &'a str {
        &l[r.start as usize..r.end as usize]
    }
    #[test]
    fn tokens() {
        let l = "let x_1 = foo(bar);";
        let t: Vec<&str> = tokenize(l.as_bytes()).iter().map(|r| s(r, l)).collect();
        assert_eq!(t, vec!["let", " ", "x_1", " ", "=", " ", "foo", "(", "bar", ")", ";"]);
        let u = "héllo  wörld";
        let t: Vec<&str> = tokenize(u.as_bytes()).iter().map(|r| s(r, u)).collect();
        assert_eq!(t, vec!["héllo", "  ", "wörld"]);
    }
    #[test]
    fn pairs_similar_and_emphasizes_changed_word() {
        let d = "const total = price * qty;";
        let a = "const total = price * quantity;";
        let h = block_highlights(&[d.as_bytes()], &[a.as_bytes()]);
        assert_eq!(h.pair_of_del, vec![Some(0)]);
        assert_eq!(h.pair_of_add, vec![Some(0)]);
        assert_eq!(h.del_emph[0].iter().map(|r| s(r, d)).collect::<Vec<_>>(), vec!["qty"]);
        assert_eq!(h.add_emph[0].iter().map(|r| s(r, a)).collect::<Vec<_>>(), vec!["quantity"]);
    }
    #[test]
    fn adjacent_changes_merge_across_spaces() {
        let d = "a = old value here;";
        let a = "a = new thing here;";
        let h = block_highlights(&[d.as_bytes()], &[a.as_bytes()]);
        assert_eq!(h.add_emph[0].iter().map(|r| s(r, a)).collect::<Vec<_>>(), vec!["new thing"]);
    }
    #[test]
    fn unequal_counts_still_pair() {
        let dels = ["foo(a, b);", "bar();"];
        let adds = ["// new comment", "foo(a, b, c);", "baz();", "bar(1);"];
        let h = block_highlights(&dels.map(str::as_bytes), &adds.map(str::as_bytes));
        assert_eq!(h.pair_of_del, vec![Some(1), Some(3)]);
        assert_eq!(h.pair_of_add, vec![None, Some(0), None, Some(1)]);
    }
    #[test]
    fn dissimilar_not_paired() {
        let h = block_highlights(&[b"alpha beta gamma".as_slice()], &[b"}".as_slice()]);
        assert_eq!(h.pair_of_del, vec![None]);
        assert!(h.del_emph[0].is_empty());
    }
    #[test]
    fn whitespace_only_change_has_no_edge_emphasis() {
        let h = block_highlights(&[b"  x = 1;".as_slice()], &[b"    x = 1;".as_slice()]);
        assert_eq!(h.pair_of_del, vec![Some(0)]);
        assert!(h.add_emph[0].is_empty(), "{:?}", h.add_emph);
    }
    #[test]
    fn emph_ranges_on_char_boundaries() {
        let d = "naïve café ok";
        let a = "naïve cafés ok";
        let h = block_highlights(&[d.as_bytes()], &[a.as_bytes()]);
        assert!(!h.add_emph[0].is_empty());
        for r in h.add_emph[0].iter() {
            assert!(a.is_char_boundary(r.start as usize) && a.is_char_boundary(r.end as usize));
        }
    }
    #[test]
    fn long_lines_skipped() {
        let d = "x".repeat(2000);
        let a = format!("{d}y");
        let h = block_highlights(&[d.as_bytes()], &[a.as_bytes()]);
        assert_eq!(h.pair_of_del, vec![None]);
    }
    #[test]
    fn huge_block_is_bounded() {
        let dels: Vec<String> = (0..3000).map(|i| format!("old line number {i} with text")).collect();
        let adds: Vec<String> = (0..3500).map(|i| format!("new line number {i} with other text")).collect();
        let d: Vec<&[u8]> = dels.iter().map(|x| x.as_bytes()).collect();
        let a: Vec<&[u8]> = adds.iter().map(|x| x.as_bytes()).collect();
        let t = std::time::Instant::now();
        let h = block_highlights(&d, &a);
        assert_eq!(h.pair_of_del.len(), 3000);
        assert!(t.elapsed().as_millis() < 500, "took {:?}", t.elapsed());
        let eq: Vec<&[u8]> = adds[..3000].iter().map(|x| x.as_bytes()).collect();
        let t = std::time::Instant::now();
        let h = block_highlights(&d, &eq);
        assert!(h.pair_of_del.iter().all(Option::is_some));
        assert!(t.elapsed().as_millis() < 2000, "positional took {:?}", t.elapsed());
    }
}
