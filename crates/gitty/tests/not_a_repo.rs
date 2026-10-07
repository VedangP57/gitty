use std::process::Command;

const EXE: &str = env!("CARGO_BIN_EXE_gitty");

#[test]
fn outside_a_repository_gitty_says_so_in_one_clean_line() {
    let d = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(d.path()).unwrap();
    for args in [&[][..], &["untune"][..]] {
        let out = Command::new(EXE).args(args).current_dir(&dir).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty());
        assert_eq!(String::from_utf8_lossy(&out.stderr), format!("gitty: {} is not a git repository\n", dir.display()), "{args:?}");
    }
}
