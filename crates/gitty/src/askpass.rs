//! Askpass trampoline (spec §12.4). git and ssh run the gitty binary itself as `GIT_ASKPASS` /
//! `SSH_ASKPASS` with the prompt as its argument; that helper relays the prompt over a private
//! Unix socket to the running TUI and prints the answer. Answers are [`Secret`]s: never logged,
//! never in `Debug`, zeroed when dropped.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;

/// The environment variable that makes the gitty binary act as the helper.
pub const SOCK_ENV: &str = "GITTY_ASKPASS_SOCK";
const MAX_FRAME: u32 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskKind {
    Text,
    Secret,
    YesNo,
}

/// Username prompts are shown; host-key questions are yes/no; everything else is masked.
pub fn classify(prompt: &str) -> AskKind {
    let p = prompt.to_ascii_lowercase();
    if p.contains("(yes/no") || (p.contains("fingerprint") && p.contains("continue")) {
        AskKind::YesNo
    } else if p.starts_with("username") {
        AskKind::Text
    } else {
        AskKind::Secret
    }
}

/// A prompt answer. `Debug` never shows it; the bytes are zeroed on drop.
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Secret {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: zero bytes are valid UTF-8
        unsafe { self.0.as_bytes_mut() }.fill(0);
    }
}

#[derive(Debug, Clone)]
pub struct Ask {
    pub id: u64,
    pub prompt: String,
    pub kind: AskKind,
}

fn write_frame(w: &mut impl Write, b: &[u8]) -> std::io::Result<()> {
    w.write_all(&(b.len() as u32).to_be_bytes())?;
    w.write_all(b)
}

fn read_frame(r: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut n = [0u8; 4];
    r.read_exact(&mut n)?;
    let n = u32::from_be_bytes(n);
    if n > MAX_FRAME {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut v = vec![0; n as usize];
    r.read_exact(&mut v)?;
    Ok(v)
}

/// Helper mode: relays `prompt`, prints the answer. Exit code 0 with an answer, 1 otherwise.
pub fn helper_main(sock: &Path, prompt: &str) -> i32 {
    let run = || -> std::io::Result<Option<Vec<u8>>> {
        let mut s = UnixStream::connect(sock)?;
        write_frame(&mut s, prompt.as_bytes())?;
        let mut ok = [0u8; 1];
        s.read_exact(&mut ok)?;
        if ok[0] != 1 {
            return Ok(None);
        }
        read_frame(&mut s).map(Some)
    };
    match run() {
        Ok(Some(mut answer)) => {
            let mut out = std::io::stdout().lock();
            let done = out.write_all(&answer).and_then(|_| out.write_all(b"\n")).and_then(|_| out.flush());
            answer.fill(0);
            i32::from(done.is_err())
        }
        _ => 1,
    }
}

struct Inner {
    pending: Mutex<HashMap<u64, UnixStream>>,
    next: AtomicU64,
}

/// Answers prompts from any thread.
#[derive(Clone)]
pub struct AskHandle(Arc<Inner>);

impl AskHandle {
    /// `None` cancels: the helper exits 1 and git fails.
    pub fn answer(&self, id: u64, reply: Option<Secret>) {
        let Some(mut s) = self.0.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) else { return };
        let _ = match &reply {
            Some(r) => s.write_all(&[1]).and_then(|_| write_frame(&mut s, r.expose().as_bytes())),
            None => s.write_all(&[0]),
        };
    }
}

/// The TUI side: a socket in a fresh `0700` directory, removed on drop.
pub struct AskServer {
    dir: PathBuf,
    sock: PathBuf,
    handle: AskHandle,
    stop: Arc<AtomicBool>,
}

impl AskServer {
    /// `on_ask` runs on the accept thread for each prompt; answer it with [`AskServer::handle`].
    pub fn start(on_ask: impl Fn(Ask) + Send + 'static) -> anyhow::Result<AskServer> {
        use std::os::unix::fs::DirBuilderExt;
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!("gitty-ask-{}-{stamp:x}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let sock = dir.join("s");
        let listener = UnixListener::bind(&sock).with_context(|| format!("binding {}", sock.display()))?;
        let inner = Arc::new(Inner { pending: Mutex::new(HashMap::new()), next: AtomicU64::new(1) });
        let stop = Arc::new(AtomicBool::new(false));
        let (inner2, stop2) = (inner.clone(), stop.clone());
        std::thread::Builder::new().name("askpass".into()).spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut s) = conn else { continue };
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let Ok(prompt) = read_frame(&mut s) else { continue };
                let _ = s.set_read_timeout(None);
                let prompt = String::from_utf8_lossy(&prompt).into_owned();
                let id = inner2.next.fetch_add(1, Ordering::Relaxed);
                inner2.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, s);
                on_ask(Ask { id, kind: classify(&prompt), prompt });
            }
        })?;
        Ok(AskServer { dir, sock, handle: AskHandle(inner), stop })
    }

    pub fn socket(&self) -> &Path {
        &self.sock
    }

    pub fn handle(&self) -> AskHandle {
        self.handle.clone()
    }
}

impl Drop for AskServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the accept loop so it sees `stop`
        let _ = UnixStream::connect(&self.sock);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_kinds() {
        assert_eq!(classify("Username for 'https://github.com': "), AskKind::Text);
        assert_eq!(classify("Password for 'https://ann@github.com': "), AskKind::Secret);
        assert_eq!(classify("Enter passphrase for key '/Users/a/.ssh/id_ed25519': "), AskKind::Secret);
        assert_eq!(classify("Enter passphrase for \"/Users/a/.ssh/id\":"), AskKind::Secret);
        assert_eq!(classify("Are you sure you want to continue connecting (yes/no/[fingerprint])? "), AskKind::YesNo);
        assert_eq!(classify("Enter PIN for authenticator: "), AskKind::Secret);
        assert_eq!(classify("something new: "), AskKind::Secret, "unknown prompts are masked");
    }
}
