//! Prints the licenses of the syntax definitions bundled through two-face (from bat), as
//! Markdown, for THIRD-PARTY-LICENSES.md: `cargo run -q -p gitty-highlight --example syntax_licenses`.
//! Only the syntaxes: gitty does not ship two-face's themes.
fn main() {
    let mut md = String::from("Most of the syntax definitions and their curation come from the [bat](https://github.com/sharkdp/bat) project.\n\n");
    for license in two_face::acknowledgement::listing().for_syntaxes() {
        license.write_md(&mut md);
    }
    print!("{md}");
}
