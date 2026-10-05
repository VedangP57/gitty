use std::path::Path;

use gitty::external::{difftool_argv, editor_argv, split_command, temp_pair};

fn v(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn split_command_follows_shell_quoting_without_a_shell() {
    let cases: &[(&str, &[&str])] = &[
        ("vim", &["vim"]),
        ("  code   --wait ", &["code", "--wait"]),
        ("'my editor' -f", &["my editor", "-f"]),
        (r#""C:\x" a"#, &[r"C:\x", "a"]),
        (r#""say \"hi\"" \$HOME"#, &[r#"say "hi""#, "$HOME"]),
        (r"my\ editor --flag", &["my editor", "--flag"]),
        ("emacs -nw $(rm -rf ~) `id` ; ls | x && y", &["emacs", "-nw", "$(rm", "-rf", "~)", "`id`", ";", "ls", "|", "x", "&&", "y"]),
        ("a''b \"\"", &["ab", ""]),
    ];
    for (input, want) in cases {
        assert_eq!(split_command(input).unwrap(), v(want), "{input}");
    }
    assert!(split_command("vim 'unterminated").is_err());
    assert!(split_command("   ").is_err(), "an empty command is an error");
}

#[test]
fn editor_argv_puts_the_line_and_the_path_as_separate_arguments() {
    let p = Path::new("/repo/dir with space/it's \"q\".rs");
    let path = p.to_string_lossy().to_string();
    assert_eq!(editor_argv("nvim -p", p, Some(12)).unwrap(), v(&["nvim", "-p", "+12", &path]));
    assert_eq!(editor_argv("vim", p, None).unwrap(), v(&["vim", &path]));
    assert_eq!(editor_argv("code --wait", p, Some(3)).unwrap(), v(&["code", "--wait", "--goto", &format!("{path}:3")]));
    assert_eq!(editor_argv("/opt/bin/hx", p, Some(3)).unwrap(), v(&["/opt/bin/hx", &format!("{path}:3")]));
}

#[test]
fn difftool_gets_two_temp_files_that_are_removed_afterwards() {
    let pair = temp_pair("src/dir with space/main.rs", b"old\n", b"new\n").unwrap();
    assert_eq!(std::fs::read(&pair.old).unwrap(), b"old\n");
    assert_eq!(std::fs::read(&pair.new).unwrap(), b"new\n");
    assert!(pair.old.ends_with("main.rs") && pair.new.ends_with("main.rs"), "named after the file, for the tool's syntax colours");
    assert!(pair.old.starts_with(std::env::temp_dir()), "never inside the repository");
    let argv = difftool_argv("delta --side-by-side", &pair.old, &pair.new).unwrap();
    assert_eq!(argv, v(&["delta", "--side-by-side", &pair.old.to_string_lossy(), &pair.new.to_string_lossy()]));
    let dir = pair.old.parent().unwrap().parent().unwrap().to_path_buf();
    drop(pair);
    assert!(!dir.exists(), "temp files are removed");
}
