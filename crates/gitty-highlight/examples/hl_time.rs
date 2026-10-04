//! Times whole-file highlighting: `cargo run --release -p gitty-highlight --example hl_time -- FILE...`
//! Prints the first run (includes compiling the language's queries) and the best of 5 warm runs.

use std::time::Instant;

use gitty_highlight::{Highlighter, detect};

fn main() {
    let mut h = Highlighter::new();
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).expect("read");
        let first = bytes.split(|&b| b == b'\n').next().unwrap_or(&[]);
        let lang = detect(&path, first).map_or("none".into(), |l| format!("{} ({:?})", l.name(), l.engine()));
        let lines = bytes.iter().filter(|&&b| b == b'\n').count();
        let t = Instant::now();
        let spans = h.highlight(&path, &bytes, &|| false);
        let cold = t.elapsed();
        let warm = (0..5)
            .map(|_| {
                let t = Instant::now();
                h.highlight(&path, &bytes, &|| false);
                t.elapsed()
            })
            .min()
            .unwrap();
        println!("{path}: {lang}, {lines} lines, {} KiB, spans: {}, first {cold:.1?}, warm {warm:.1?}", bytes.len() / 1024, spans.is_some());
    }
}
