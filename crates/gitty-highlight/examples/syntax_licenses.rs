//! Prints the licenses of the syntax definitions bundled through two-face (from bat), as
//! Markdown, for THIRD-PARTY-LICENSES.md: `cargo run -q -p gitty-highlight --example syntax_licenses`.
fn main() {
    print!("{}", two_face::acknowledgement::listing().to_md());
}
