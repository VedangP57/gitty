//! The Files tab's listing: one directory at a time, merged from the index and the disk.
mod common;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::files::{DirEntry, EntryKind};

fn list(f: &Fixture, rel: &str) -> Vec<DirEntry> {
    Repo::open(f.path()).unwrap().handle().list_dir(Path::new(rel)).unwrap()
}

fn names(v: &[DirEntry]) -> Vec<String> {
    v.iter().map(|e| e.name.to_string_lossy().into_owned()).collect()
}

fn find<'a>(v: &'a [DirEntry], name: &str) -> &'a DirEntry {
    v.iter().find(|e| e.name == name).unwrap_or_else(|| panic!("{name} not in {:?}", names(v)))
}

#[test]
fn dotfiles_listed_and_git_hidden() {
    let f = Fixture::new();
    f.write(".hidden", "x");
    f.write("a.txt", "a");
    f.commit("one", 1_700_000_000);
    let got = names(&list(&f, ""));
    assert_eq!(got, [".hidden", "a.txt"]);
}

#[test]
fn directories_first_then_case_insensitive() {
    let f = Fixture::new();
    for p in ["b.txt", "A.txt", "C.txt", "Zdir/x", "adir/x", "Bdir/x"] {
        f.write(p, "x");
    }
    f.commit("one", 1_700_000_000);
    let got = list(&f, "");
    assert_eq!(names(&got), ["adir", "Bdir", "Zdir", "A.txt", "b.txt", "C.txt"]);
    assert!(got[0].tracked && matches!(got[0].kind, EntryKind::Dir));
    assert_eq!(find(&got, "b.txt").size, 1);
}

#[test]
fn nested_directory_lists_its_own_children() {
    let f = Fixture::new();
    f.write("src/lib.rs", "fn main() {}\n");
    f.write("src/deep/mod.rs", "");
    f.write("src.txt", "x");
    f.commit("one", 1_700_000_000);
    f.write("src/untracked.rs", "");
    let got = list(&f, "src");
    assert_eq!(names(&got), ["deep", "lib.rs", "untracked.rs"]);
    assert!(find(&got, "lib.rs").tracked);
    assert!(!find(&got, "untracked.rs").tracked);
}

#[test]
fn ignored_files_and_directories_are_flagged() {
    let f = Fixture::new();
    f.write(".gitignore", "*.log\ntarget/\n");
    f.write("keep.txt", "k");
    f.write("a.log", "l");
    f.write("target/out.bin", "o");
    f.commit("one", 1_700_000_000);
    let got = list(&f, "");
    assert!(find(&got, "a.log").ignored);
    assert!(find(&got, "target").ignored);
    assert!(!find(&got, "keep.txt").ignored);
    assert!(!find(&got, ".gitignore").ignored);
    // an ignored directory can still be opened: everything in it is ignored
    let inner = list(&f, "target");
    assert!(inner.iter().all(|e| e.ignored));
}

#[test]
fn a_tracked_file_is_not_ignored_even_when_a_rule_matches() {
    let f = Fixture::new();
    f.write("forced.log", "l");
    f.git(&["add", "-f", "forced.log"]);
    f.write(".gitignore", "*.log\n");
    f.commit("one", 1_700_000_000);
    assert!(!find(&list(&f, ""), "forced.log").ignored);
}

#[test]
fn symlinks_are_leaves_never_followed() {
    let f = Fixture::new();
    f.write("real/inner.txt", "i");
    f.commit("one", 1_700_000_000);
    std::os::unix::fs::symlink("real", f.path().join("link")).unwrap();
    std::os::unix::fs::symlink("nowhere", f.path().join("dangling")).unwrap();
    let got = list(&f, "");
    assert_eq!(find(&got, "link").kind, EntryKind::Symlink { target: "real".into() });
    assert_eq!(find(&got, "dangling").kind, EntryKind::Symlink { target: "nowhere".into() });
    // a symlink sorts with the files, not the directories
    assert_eq!(names(&got), ["real", "dangling", "link"]);
}

#[test]
fn non_utf8_names_keep_their_real_bytes() {
    let f = Fixture::new();
    let name = OsString::from_vec(b"caf\xe9.txt".to_vec());
    // some filesystems (APFS) refuse such names
    if std::fs::write(f.path().join(&name), "x").is_err() {
        return;
    }
    let got = list(&f, "");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, name);
    assert_eq!(got[0].name.to_string_lossy(), "caf\u{fffd}.txt");
}

#[test]
fn nested_repositories_and_gitlinks_are_submodule_leaves() {
    let f = Fixture::new();
    f.write("a.txt", "a");
    f.commit("one", 1_700_000_000);
    // a nested repository (has its own .git)
    std::fs::create_dir_all(f.path().join("nested")).unwrap();
    assert!(std::process::Command::new("git").args(["init", "-q"]).arg(f.path().join("nested")).status().unwrap().success());
    // a gitlink in the index whose directory is plain (not checked out)
    let head = f.git(&["rev-parse", "HEAD"]);
    f.git(&["update-index", "--add", "--cacheinfo", &format!("160000,{head},sub")]);
    f.write("sub/file.txt", "x");
    let got = list(&f, "");
    assert_eq!(find(&got, "nested").kind, EntryKind::Submodule);
    assert_eq!(find(&got, "sub").kind, EntryKind::Submodule);
    assert!(find(&got, "sub").tracked);
    assert_eq!(names(&got)[..2], ["nested", "sub"]);
}

#[test]
fn an_unreadable_directory_is_an_error_not_a_panic() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write("locked/x.txt", "x");
    f.commit("one", 1_700_000_000);
    let dir = f.path().join("locked");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = Repo::open(f.path()).unwrap().handle().list_dir(Path::new("locked"));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // root ignores permission bits
    if std::fs::read_dir(&dir).is_ok() && r.is_ok() {
        return;
    }
    let e = r.unwrap_err();
    assert!(format!("{e:#}").contains("locked"), "{e:#}");
    // the rest of the tree still lists
    assert_eq!(names(&list(&f, "")), ["locked"]);
}

#[test]
fn skip_worktree_paths_absent_on_disk_are_not_listed() {
    let f = Fixture::new();
    f.write("in/a.txt", "a");
    f.write("out/b.txt", "b");
    f.write("top.txt", "t");
    f.commit("one", 1_700_000_000);
    f.git(&["sparse-checkout", "set", "--cone", "in"]);
    let got = names(&list(&f, ""));
    assert!(got.contains(&"in".to_string()) && got.contains(&"top.txt".to_string()), "{got:?}");
    assert!(!got.contains(&"out".to_string()), "{got:?}");
}

#[test]
fn a_linked_worktree_hides_its_git_file() {
    let f = Fixture::new();
    f.write("a.txt", "a");
    f.commit("one", 1_700_000_000);
    let wt = f.path().parent().unwrap().join("linked");
    f.git(&["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
    assert!(wt.join(".git").is_file());
    let got = Repo::open(&wt).unwrap().handle().list_dir(Path::new("")).unwrap();
    assert_eq!(names(&got), ["a.txt"]);
}

#[test]
fn tracked_files_deleted_from_disk_are_not_listed() {
    let f = Fixture::new();
    f.write("gone.txt", "g");
    f.write("here.txt", "h");
    f.commit("one", 1_700_000_000);
    std::fs::remove_file(f.path().join("gone.txt")).unwrap();
    assert_eq!(names(&list(&f, "")), ["here.txt"]);
}

#[test]
fn paths_outside_the_work_tree_are_refused() {
    let f = Fixture::new();
    f.write("a.txt", "a");
    f.commit("one", 1_700_000_000);
    let h = Repo::open(f.path()).unwrap().handle();
    assert!(h.list_dir(Path::new("../")).is_err());
    assert!(h.list_dir(Path::new("/etc")).is_err());
}

#[test]
fn a_big_directory_lists_quickly() {
    let f = Fixture::new();
    f.write(".gitignore", "*.tmp\n");
    for i in 0..4500 {
        std::fs::write(f.path().join(format!("file{i:05}.txt")), "x").unwrap();
    }
    f.commit("many", 1_700_000_000);
    for i in 0..500 {
        std::fs::write(f.path().join(format!("build{i:05}.tmp")), "x").unwrap();
    }
    let h = Repo::open(f.path()).unwrap().handle();
    // the first call pays for opening the index and the exclude stack's files
    h.list_dir(Path::new("")).unwrap();
    let t = std::time::Instant::now();
    let got = h.list_dir(Path::new("")).unwrap();
    let took = t.elapsed();
    assert_eq!(got.len(), 5001);
    assert_eq!(got.iter().filter(|e| e.ignored).count(), 500);
    eprintln!("list_dir of 5000 files: {took:?}");
    // ~60 ms unoptimised on a laptop; the bound leaves room for a loaded CI machine
    assert!(took.as_millis() < 250, "{took:?}");
}

#[test]
fn secret_names() {
    use gitty_core::files::is_secret;
    for yes in [
        ".env", ".ENV", ".env.local", ".env.production", "app/.env", "server.pem", "a/b/Key.PEM", "tls.key", "cert.p12", "cert.pfx", "store.jks", "debug.keystore",
        "id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "id_ed25519.pub", ".ssh/id_rsa", ".netrc", ".npmrc", ".pypirc", "credentials", "credentials.json", "credentials.yml",
        "vault.kdbx", "secrets.yaml", "Secrets.toml", ".git-credentials", "service-account.json", "service-account-prod.json",
    ] {
        assert!(is_secret(Path::new(yes)), "{yes} should be secret");
    }
    for no in [
        ".env.example", ".env.sample", ".env.template", ".env.dist", ".ENV.EXAMPLE", "environment.rs", "env", "main.rs", "README.md", "keyboard.rs", "monkey", "id_rsa_notes.txt",
        "secrets", "my-credentials-guide.md", "service-account.txt", "pem", "key.rs", "Cargo.toml", ".gitignore",
    ] {
        assert!(!is_secret(Path::new(no)), "{no} should not be secret");
    }
}

#[test]
fn a_masked_file_is_not_read_unless_revealed() {
    use gitty_core::files::{FileContent, read_file};
    let f = Fixture::new();
    f.write(".env", "TOKEN=fake-secret-value\n");
    f.write("plain.txt", "hello\n");
    let root = f.path();
    assert_eq!(read_file(&root, Path::new(".env"), false).unwrap(), FileContent::Masked);
    // masked means untouched: it does not even matter whether the file exists
    assert_eq!(read_file(&root, Path::new("missing/.env"), false).unwrap(), FileContent::Masked);
    assert_eq!(read_file(&root, Path::new(".env"), true).unwrap(), FileContent::Text(b"TOKEN=fake-secret-value\n".to_vec()));
    assert_eq!(read_file(&root, Path::new("plain.txt"), false).unwrap(), FileContent::Text(b"hello\n".to_vec()));
}

#[test]
fn read_file_classifies() {
    use gitty_core::files::{FileContent, MAX_VIEW_BYTES, read_file};
    let f = Fixture::new();
    f.write("bin.dat", b"ab\0cd");
    f.write("big.txt", vec![b'a'; MAX_VIEW_BYTES as usize + 1]);
    f.write("lfs.bin", "version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 12345\n");
    f.write("plain.txt", "p");
    std::os::unix::fs::symlink("plain.txt", f.path().join("link")).unwrap();
    let root = f.path();
    assert_eq!(read_file(&root, Path::new("bin.dat"), false).unwrap(), FileContent::Binary { size: 5 });
    assert_eq!(read_file(&root, Path::new("big.txt"), false).unwrap(), FileContent::TooLarge { size: MAX_VIEW_BYTES + 1 });
    assert_eq!(read_file(&root, Path::new("lfs.bin"), false).unwrap(), FileContent::Lfs { size: 12345 });
    assert_eq!(read_file(&root, Path::new("link"), false).unwrap(), FileContent::Symlink { target: "plain.txt".into() });
    assert_eq!(read_file(&root, Path::new("."), false).unwrap(), FileContent::Special);
    assert!(read_file(&root, Path::new("../x"), false).is_err());
    assert!(read_file(&root, Path::new("nope"), false).is_err());
}

#[test]
fn secret_names_by_directory_suffix_and_pattern() {
    use gitty_core::files::is_secret;
    for yes in [
        // inside a secret directory
        ".env/anything.txt", "app/secrets/db.yml", ".secrets/x", "home/.ssh/config", ".aws/config", ".gnupg/pubring.kbx", ".kube/config", ".docker/config.json", "a/.SSH/known_hosts",
        // backups, swap and lock forms of a secret
        ".env~", ".env.bak", ".env.orig", ".env.swp", ".env.swo", ".env.old", ".env.save", ".env.tmp", "#.env#", ".#.env", ".env.", ".env ", "id_rsa.bak", "server.pem~", ".env.bak.bak", "ID_RSA.Pub.OLD",
        // new patterns
        ".envrc", "prod.env", ".env-prod", ".env_local", "id_ed25519_sk", "id_ecdsa_sk.pub", "putty.ppk", ".pgpass", ".htpasswd", "prod.tfvars", "terraform.tfstate", "terraform.tfstate.backup",
        ".vault-token", "backup.gpg", "my-secret-key.asc", "api.token", "db.secret", "db.secrets",
        // the allow-list is for the exact names only
        ".env.example.local", ".env.example.bak", ".env.sample.old", ".env.examples", ".ENV.EXAMPLE.LOCAL",
    ] {
        assert!(is_secret(Path::new(yes)), "{yes} should be secret");
    }
    for no in [
        ".env.example", ".ENV.SAMPLE", "src/.env.template", ".env.dist", ".docker/other.json", "docker/config.json", "foo.asc", "environment.env.rs", "tokenizer.rs", "secretary.txt",
        "my.environment", "README.md", "notes.old", "a.swp", "config.json", "kube/config", "ssh/config",
    ] {
        assert!(!is_secret(Path::new(no)), "{no} should not be secret");
    }
}

#[test]
fn a_directory_swapped_for_a_symlink_is_neither_listed_nor_read() {
    use gitty_core::files::read_file;
    let f = Fixture::new();
    f.write("a.txt", "a");
    f.commit("one", 1_700_000_000);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("leak.txt"), "outside").unwrap();
    std::os::unix::fs::symlink(outside.path(), f.path().join("evil")).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();
    let e = h.list_dir(Path::new("evil")).unwrap_err();
    assert!(format!("{e:#}").contains("symlink"), "{e:#}");
    let e = read_file(&f.path(), Path::new("evil/leak.txt"), false).unwrap_err();
    assert!(format!("{e:#}").contains("symlink"), "{e:#}");
    // a nested link too
    std::fs::create_dir(f.path().join("real")).unwrap();
    std::os::unix::fs::symlink(outside.path(), f.path().join("real/inner")).unwrap();
    assert!(h.list_dir(Path::new("real/inner")).is_err());
    assert!(read_file(&f.path(), Path::new("real/inner/leak.txt"), false).is_err());
    // the link itself is still a leaf in its parent's listing
    assert!(h.list_dir(Path::new("")).unwrap().iter().any(|e| e.name == "evil" && matches!(e.kind, EntryKind::Symlink { .. })));
}

#[test]
fn a_symlink_to_a_secret_shows_only_its_target_name() {
    use gitty_core::files::{FileContent, read_file};
    let f = Fixture::new();
    f.write(".env", "TOKEN=fake-secret-value\n");
    std::os::unix::fs::symlink(".env", f.path().join("notes.txt")).unwrap();
    assert_eq!(read_file(&f.path(), Path::new("notes.txt"), false).unwrap(), FileContent::Symlink { target: ".env".into() });
}

#[test]
fn a_fifo_with_an_ordinary_name_is_not_opened() {
    use gitty_core::files::{FileContent, read_file};
    let f = Fixture::new();
    let fifo = f.path().join("pipe.txt");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    // a hang here is the failure: read it on a thread and wait a bounded time
    let (tx, rx) = std::sync::mpsc::channel();
    let root = f.path();
    std::thread::spawn(move || {
        let _ = tx.send(read_file(&root, Path::new("pipe.txt"), false));
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("read_file hung on a FIFO");
    assert_eq!(got.unwrap(), FileContent::Special);
    // and it lists as a plain entry without being opened
    let listed = Repo::open(f.path()).unwrap().handle().list_dir(Path::new("")).unwrap();
    assert_eq!(listed.len(), 1);
}
