use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use gitty::askpass::{Ask, AskKind, AskServer, Secret};

const EXE: &str = env!("CARGO_BIN_EXE_gitty");

/// Answers every prompt by kind: Text → "ann", Secret → "hunter2", YesNo → "yes"; or cancels all.
fn server(cancel: bool) -> (AskServer, mpsc::Receiver<(String, AskKind)>) {
    let (seen_tx, seen_rx) = mpsc::channel();
    let (ask_tx, ask_rx) = mpsc::channel::<Ask>();
    let server = AskServer::start(move |a| {
        let _ = ask_tx.send(a);
    })
    .unwrap();
    let handle = server.handle();
    std::thread::spawn(move || {
        for a in ask_rx {
            let _ = seen_tx.send((a.prompt.clone(), a.kind));
            let reply = (!cancel).then(|| {
                Secret::new(match a.kind {
                    AskKind::Text => "ann",
                    AskKind::Secret => "hunter2",
                    AskKind::YesNo => "yes",
                })
            });
            handle.answer(a.id, reply);
        }
    });
    (server, seen_rx)
}

fn helper(server: &AskServer, prompt: &str) -> std::process::Output {
    Command::new(EXE).arg(prompt).env("GITTY_ASKPASS_SOCK", server.socket()).output().unwrap()
}

#[test]
fn helper_round_trip_through_the_binary() {
    let (s, seen) = server(false);
    let out = helper(&s, "Password for 'https://ann@example.com': ");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hunter2\n");
    assert_eq!(seen.recv().unwrap(), ("Password for 'https://ann@example.com': ".to_string(), AskKind::Secret));
}

#[test]
fn cancel_makes_the_helper_fail() {
    let (s, _) = server(true);
    let out = helper(&s, "Username for 'https://example.com': ");
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
}

fn credential_fill(s: &AskServer) -> std::process::Output {
    let mut c = Command::new("git")
        .args(["-c", "credential.helper=", "credential", "fill"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", EXE)
        .env("GITTY_ASKPASS_SOCK", s.socket())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(b"protocol=https\nhost=example.com\n\n").unwrap();
    c.wait_with_output().unwrap()
}

#[test]
fn real_git_asks_through_the_trampoline() {
    let (s, seen) = server(false);
    let out = credential_fill(&s);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("username=ann") && text.contains("password=hunter2"), "{text}");
    let kinds: Vec<AskKind> = seen.try_iter().map(|(_, k)| k).collect();
    assert_eq!(kinds, [AskKind::Text, AskKind::Secret]);
}

#[test]
fn real_git_fails_cleanly_on_cancel() {
    let (s, _) = server(true);
    let out = credential_fill(&s);
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("password="));
}

#[test]
fn socket_is_private_and_removed_on_drop() {
    use std::os::unix::fs::PermissionsExt;
    let (s, _) = server(false);
    let dir = s.socket().parent().unwrap().to_path_buf();
    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    drop(s);
    assert!(!dir.exists());
}

#[test]
fn secrets_never_print() {
    let s = Secret::new("hunter2");
    assert!(!format!("{s:?}").contains("hunter2"));
    assert_eq!(s.expose(), "hunter2");
}
