//! Line-level diffing over blobs (imara-diff via gix). Grows into the full diff engine in M2.

use gix::diff::blob::{Algorithm, Diff, InternedInput};

/// (added, removed) line counts with Myers + indent heuristic (git default).
pub fn count(old: &[u8], new: &[u8]) -> (u32, u32) {
    let input = InternedInput::new(old, new);
    let mut d = Diff::compute(Algorithm::Myers, &input);
    d.postprocess_lines(&input);
    (d.count_additions(), d.count_removals())
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts() {
        assert_eq!(super::count(b"a\nb\nc\n", b"a\nB\nc\nd\n"), (2, 1));
        assert_eq!(super::count(b"", b"x\n"), (1, 0));
        assert_eq!(super::count(b"x\n", b"x\n"), (0, 0));
    }
}
