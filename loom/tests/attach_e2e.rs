//! Host-only end-to-end attach test: real PTY, real server, real shell.
//!
//! This is the automated form of "type `eza -l` in loom and it freezes" and
//! of "loom's colours don't match my shell". It drives the actual `loom`
//! client binary through a pseudo-terminal (so `run_attached` takes exactly
//! the code path a user's terminal does), runs a real shell in a real pane
//! PTY, types a command, requires its real output back, and then asserts the
//! client is still alive by requiring a follow-up `echo` marker to round-trip.
//!
//! A frozen client produces a deterministic failure and the raw terminal
//! stream is dumped to `/tmp/loom-e2e-*.raw` for offline replay/diffing.
//!
//! # Environment switch
//!
//! The pane shell inherits the *server* process environment (the server runs
//! in-process under this test), while the client gets an isolated `HOME` so it
//! never touches the real loom socket. That makes the pane environment a knob:
//!
//! | Variable | Effect |
//! |---|---|
//! | *(unset)* | Hermetic: pane `HOME`/`ZDOTDIR` point at a temp dir, shell `/bin/zsh`, no user rc. Deterministic, CI-safe. |
//! | `LOOM_E2E_REAL_ENV=1` | **Real-environment reproduction**: inherit `HOME`, `ZDOTDIR`, `LS_COLORS`, `EZA_COLORS`, `SHELL`, … from the invoking shell, so the user's prompt (p10k), eza aliases/colours and shell config all load. The client socket stays isolated. |
//! | `LOOM_E2E_HOME=<path>` | Explicit pane `HOME` (still sanitized). |
//! | `LOOM_E2E_SHELL=<path>` | Shell for the pane (default `/bin/zsh`). |
//! | `LOOM_E2E_CMD=<line>` | Command to type (default `eza -l --color=always`). |
//! | `LOOM_E2E_EXPECT=<text>` | Required output substring (default `AUDIT`; empty disables). |
//! | `LOOM_E2E_REQUIRE_SGR=<sgr>` | SGR substring the rendering must contain (default `38;5;8` for the default command; empty disables). |
//!
//! Note: bare `eza -l` emits **no** escapes even on a real PTY (`--color` is
//! `auto` and this eza build stays plain), so a colour probe must force colour.
//!
//! Reproduce the user's own environment:
//!
//! ```sh
//! LOOM_E2E_REAL_ENV=1 cargo test -p loom --test attach_e2e -- --nocapture
//! ```
//!
//! The test self-skips when PTY allocation is unavailable (the agent sandbox
//! denies `openpty`), so it is safe to include in the default suite; it is
//! meaningful on a host.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use loom_server::server::{Server, ServerConfig};

/// True when the platform lets us allocate a PTY.
fn pty_available() -> bool {
    nix::pty::openpty(None::<&nix::pty::Winsize>, None::<&nix::sys::termios::Termios>).is_ok()
}

/// Sets process environment variables for the duration of a scope and restores
/// them on drop — including when an assertion panics. The pane shell is forked
/// by the in-process server, so this is how its environment is controlled.
struct EnvScope {
    saved: Vec<(String, Option<OsString>)>,
}

impl EnvScope {
    fn apply(vars: &[(&str, Option<OsString>)]) -> Self {
        let mut saved = Vec::with_capacity(vars.len());
        for (key, value) in vars {
            saved.push(((*key).to_string(), std::env::var_os(key)));
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        Self { saved }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// The client's terminal, seen from the test side of the PTY master.
struct Terminal {
    master: std::fs::File,
    buf: Vec<u8>,
    last_data: Instant,
    dump_path: PathBuf,
}

impl Terminal {
    fn new(master: std::fs::File, dump_path: PathBuf) -> Self {
        Self { master, buf: Vec::new(), last_data: Instant::now(), dump_path }
    }

    fn set_nonblocking(&self) {
        nix::fcntl::fcntl(
            self.master.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .expect("set master nonblocking");
    }

    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).expect("write to client PTY");
        self.master.flush().ok();
    }

    /// Drain whatever is available without blocking.
    fn drain(&mut self) {
        let mut chunk = [0u8; 65536];
        loop {
            match self.master.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    self.last_data = Instant::now();
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    /// Drain until `needle` appears in the stream or the deadline passes.
    fn wait_for(&mut self, needle: &[u8], timeout: Duration) -> bool {
        if needle.is_empty() {
            return true;
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.drain();
            if self.contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.drain();
        self.contains(needle)
    }

    fn contains(&self, needle: &[u8]) -> bool {
        self.buf.windows(needle.len()).any(|w| w == needle)
    }

    fn quiet_for(&self) -> Duration {
        self.last_data.elapsed()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Always leave the raw stream behind for offline replay/diffing —
        // including when an assertion panics.
        let _ = std::fs::write(&self.dump_path, &self.buf);
    }
}

/// Owns every resource the test must release even if an assertion panics.
struct Cleanup {
    child: Child,
    stop: Sender<()>,
    server_thread: Option<JoinHandle<()>>,
    socket: PathBuf,
    home: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.stop.send(());
        if let Some(t) = self.server_thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
fn typing_eza_does_not_freeze_the_client() {
    if !pty_available() {
        eprintln!("SKIP attach_e2e: pty allocation unavailable");
        return;
    }

    // ── Reproduction knobs ──────────────────────────────────────────────
    let real_env = std::env::var_os("LOOM_E2E_REAL_ENV").is_some_and(|v| !v.is_empty());
    let shell = std::env::var("LOOM_E2E_SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let command = std::env::var("LOOM_E2E_CMD").unwrap_or_else(|_| "eza -l --color=always".to_string());
    let expect = match std::env::var("LOOM_E2E_EXPECT") {
        Ok(v) => v, // empty string disables the output assertion
        Err(_) => "AUDIT".to_string(),
    };
    let default_cmd = std::env::var_os("LOOM_E2E_CMD").is_none();
    // Bright-black (palette 8) is what eza's `\x1b[90m` permission dashes must
    // render as. Before the colour fix loom collapsed that to the default fg,
    // so requiring it turns this into a live colour regression check.
    let require_sgr = std::env::var("LOOM_E2E_REQUIRE_SGR").unwrap_or_else(|_| {
        if default_cmd {
            "38;5;8".to_string()
        } else {
            String::new()
        }
    });

    // Isolated HOME for the *client* so the real loom socket is never touched,
    // regardless of mode.
    let home = PathBuf::from(format!("/tmp/loom-e2e-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".loom")).expect("create home");

    // The pane shell inherits the server process env. In hermetic mode we pin
    // HOME/ZDOTDIR/SHELL to keep CI deterministic; with LOOM_E2E_REAL_ENV we
    // deliberately leave the invoking shell's environment intact so the user's
    // prompt and tool config are reproduced.
    let pane_env = if real_env {
        eprintln!("attach_e2e: REAL ENV mode (inheriting HOME/SHELL/ZDOTDIR/colour vars)");
        Vec::new()
    } else {
        let pane_home = std::env::var_os("LOOM_E2E_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.clone());
        eprintln!(
            "attach_e2e: hermetic mode (pane HOME={}, shell={})",
            pane_home.display(),
            shell
        );
        vec![
            ("HOME", Some(pane_home.clone().into_os_string())),
            // ZDOTDIR pointed at the empty HOME stops zsh loading ~/.zshrc.
            ("ZDOTDIR", Some(pane_home.into_os_string())),
            ("SHELL", Some(OsString::from(shell.clone()))),
        ]
    };
    let _env = EnvScope::apply(&pane_env);
    // TERM always matches what loom advertises to panes.
    let _term = EnvScope::apply(&[("TERM", Some(OsString::from("xterm-256color")))]);

    // In-process server: the test owns its lifecycle, so no detached server
    // survives the run (pane process groups are killed when it drops).
    let socket_path = home.join(".loom/default.sock");
    let server = Server::new(ServerConfig {
        socket_path: socket_path.to_string_lossy().into_owned(),
        socket_mode: 0o600,
    })
    .expect("Server::new");
    let (stop_tx, stop_rx): (Sender<()>, Receiver<()>) = mpsc::channel();
    let server_thread = std::thread::spawn(move || {
        let mut s = server;
        s.create_socket().expect("create_socket");
        while !s.exit {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            let _ = s.process_once();
        }
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(Instant::now() < deadline, "server socket did not appear");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A PTY plays the role of the user's terminal.
    let ws = nix::pty::Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    let open = nix::pty::openpty(Some(&ws), None).expect("openpty");

    let child = Command::new(env!("CARGO_BIN_EXE_loom"))
        // Isolated HOME: the client's socket lives under the temp dir even in
        // real-env mode.
        .env("HOME", &home)
        .env("SHELL", &shell)
        .env("TERM", "xterm-256color")
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("LOOM_LOG", "1")
        // List a directory with known entries so we can assert real output.
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("workspace root"),
        )
        .stdin(Stdio::from(open.slave.try_clone().expect("dup slave")))
        .stdout(Stdio::from(open.slave.try_clone().expect("dup slave")))
        .stderr(Stdio::from(open.slave))
        .spawn()
        .expect("spawn loom client");

    // From here on, panics still kill the client/server and dump the stream.
    let cleanup = Cleanup {
        child,
        stop: stop_tx,
        server_thread: Some(server_thread),
        socket: socket_path,
        home,
    };

    let dump_path = PathBuf::from(format!(
        "/tmp/loom-e2e-{}-{}.raw",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    eprintln!("attach_e2e raw stream: {}", dump_path.display());
    let mut term = Terminal::new(std::fs::File::from(open.master), dump_path.clone());
    term.set_nonblocking();

    // 1. The client must come up at all (attach + first paint). In real-env
    //    mode a slow/heavy prompt can take longer, so allow more slack.
    let paint_timeout = if real_env { 20 } else { 10 };
    assert!(
        term.wait_for(b"\x1b[", Duration::from_secs(paint_timeout)),
        "client never painted anything; got {} bytes",
        term.buf.len()
    );

    // 2. Type the command and require real output, not merely the typed echo.
    term.send(format!("{command}\r").as_bytes());
    assert!(
        term.wait_for(expect.as_bytes(), Duration::from_secs(20)),
        "`{command}` produced no expected output {expect:?}; got {} bytes (dump: {})",
        term.buf.len(),
        dump_path.display()
    );

    // 3. Let the command finish, then wait for the stream to go quiet.
    let quiet_deadline = Instant::now() + Duration::from_secs(20);
    while term.quiet_for() < Duration::from_secs(1) && Instant::now() < quiet_deadline {
        term.drain();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!term.buf.is_empty(), "command produced no client output at all");

    // 3b. Colour regression check: the rendering must actually carry the SGR
    //     the command asked for (see `LOOM_E2E_REQUIRE_SGR`).
    if !require_sgr.is_empty() {
        assert!(
            term.contains(require_sgr.as_bytes()),
            "`{command}` rendered without required SGR {require_sgr:?} — colour was dropped \
             ({} bytes, dump: {})",
            term.buf.len(),
            dump_path.display()
        );
    }

    // 4. Liveness probe: a follow-up command must round-trip. This is the
    //    actual freeze assertion — a hung client never echoes the marker.
    const MARKER: &[u8] = b"LOOM_ALIVE_9f3c";
    term.send(b"echo LOOM_ALIVE_9f3c\r");
    let alive = term.wait_for(MARKER, Duration::from_secs(10));

    // 5. Large burst: mio is edge-triggered, so the client must drain every
    //    buffered message. If it reads only one per event, the server's socket
    //    buffer fills, no new edge arrives, and the session deadlocks (the
    //    reported `eza -l` freeze). `seq` gives a large, deterministic burst.
    let before_burst = term.buf.len();
    term.send(b"seq 1 200000\r");
    let burst_deadline = Instant::now() + Duration::from_secs(60);
    let mut burst_bytes = 0usize;
    let mut last_len = before_burst;
    let mut stable_since = Instant::now();
    while Instant::now() < burst_deadline {
        term.drain();
        if term.buf.len() != last_len {
            last_len = term.buf.len();
            stable_since = Instant::now();
        }
        burst_bytes = term.buf.len().saturating_sub(before_burst);
        // Enough volume proves the whole burst was drained, not just the first
        // socket buffer; two quiet seconds prove the drain finished.
        if burst_bytes > 512 * 1024 && stable_since.elapsed() > Duration::from_secs(2) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let burst_done = burst_bytes > 512 * 1024;
    term.send(b"echo LOOM_ALIVE_BURST\r");
    let alive_after_burst = term.wait_for(b"LOOM_ALIVE_BURST", Duration::from_secs(60));

    // Explicit teardown before the assert decision (also happens on panic).
    drop(term);
    drop(cleanup);

    assert!(
        alive,
        "client froze after `{command}`: follow-up marker never returned \
         ({} bytes). Raw stream: {}",
        term_len(&dump_path),
        dump_path.display()
    );
    assert!(
        burst_done,
        "large output burst was not fully drained (only {burst_bytes} bytes) — \
         the client stalled. Raw stream: {}",
        dump_path.display()
    );
    assert!(
        alive_after_burst,
        "client froze after a large burst: liveness marker never returned \
         ({} bytes). Raw stream: {}",
        term_len(&dump_path),
        dump_path.display()
    );

    eprintln!(
        "attach_e2e: OK — `{command}` produced {expect:?} and the client stayed responsive. Raw stream: {}",
        dump_path.display()
    );
}

fn term_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
