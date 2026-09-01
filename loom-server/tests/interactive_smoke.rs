//! End-to-end interactive smoke test.
//!
//! Spawns a real `Server` (driven via `process_once` in a background thread),
//! connects a real client over a Unix socket, creates a session with a real
//! PTY shell, attaches, and verifies that typed commands round-trip into
//! `ScreenUpdate` data. Also verifies that the session survives a client
//! disconnect (P0-6) and can be re-listed by a fresh client.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use loom_ipc::message::Message;
use loom_ipc::peer::Peer;
use loom_server::server::{Server, ServerConfig};
use mio::net::UnixStream as MioUnixStream;

fn socket_path() -> String {
    format!("/tmp/loom-e2e-{}.sock", std::process::id())
}

/// Connect, identify, and wait for `Ready`.
fn connect_identified(path: &str) -> Peer {
    let std_stream = UnixStream::connect(path).expect("connect to server socket");
    std_stream.set_nonblocking(true).expect("set non-blocking");
    let mut peer = Peer::new(MioUnixStream::from_std(std_stream));

    peer.send(&Message::IdentifyFlags(0)).expect("send");
    peer.send(&Message::IdentifyLongFlags(0)).expect("send");
    peer.send(&Message::IdentifyTerm("xterm-256color".into())).expect("send");
    peer.send(&Message::IdentifyTtyName(String::new())).expect("send");
    peer.send(&Message::IdentifyCwd("/tmp".into())).expect("send");
    peer.send(&Message::IdentifyClientPid(4242)).expect("send");
    peer.send(&Message::IdentifyDone).expect("send");
    peer.flush().expect("flush");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match peer.recv().expect("recv") {
            Some(Message::Ready) => return peer,
            Some(_) => {}
            None => {
                assert!(Instant::now() < deadline, "server never became Ready");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Accumulate `ScreenUpdate` payloads until `needle` appears (or timeout).
fn wait_for_content(peer: &mut Peer, needle: &[u8], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut acc = Vec::new();
    while Instant::now() < deadline {
        match peer.recv() {
            Ok(Some(Message::ScreenUpdate { data })) => {
                acc.extend_from_slice(&data);
                if acc.windows(needle.len()).any(|w| w == needle) {
                    return true;
                }
            }
            Ok(Some(Message::Exit | Message::Exited)) => {
                eprintln!("server closed the connection early");
                std::fs::write("/tmp/loom_smoke_debug.txt", &acc).ok();
                return false;
            }
            Ok(Some(m)) => {
                eprintln!("non-screen message: {:?}", m);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                eprintln!("recv error: {}", e);
                return false;
            }
        }
    }
    // On failure, dump everything we accumulated for diagnosis.
    let plain = String::from_utf8_lossy(&acc);
    std::fs::write("/tmp/loom_smoke_debug.txt", format!("=== {} bytes total ===\n{}", acc.len(), plain)).ok();
    eprintln!("TIMED OUT waiting for {:?}; got {} bytes of screen data", needle, acc.len());
    false
}

fn run_scenario(path: &str) {
    let mut peer = connect_identified(path);

    peer.send(&Message::Resize { sx: 80, sy: 24 }).unwrap();
    peer.flush().unwrap();

    peer.send(&Message::Command {
        argc: 1,
        argv: vec!["new-session".into()],
    })
    .unwrap();
    peer.send(&Message::AttachSession).unwrap();
    peer.flush().unwrap();

    // Let the shell start and paint its prompt.
    std::thread::sleep(Duration::from_millis(500));

    // Type `echo MARKER_12345` and expect it in the screen updates.
    peer.send(&Message::KeyPress {
        key: b"echo MARKER_12345\r".to_vec(),
    })
    .unwrap();
    peer.flush().unwrap();
    assert!(
        wait_for_content(&mut peer, b"MARKER_12345", Duration::from_secs(10)),
        "typed command output never appeared in screen updates"
    );

    // Second round-trip to prove parser state persists across reads.
    peer.send(&Message::KeyPress {
        key: b"echo round-trip-ok\r".to_vec(),
    })
    .unwrap();
    peer.flush().unwrap();
    assert!(
        wait_for_content(&mut peer, b"round-trip-ok", Duration::from_secs(10)),
        "second round-trip failed"
    );

    // Detach: close this client. The session must survive (P0-6).
    drop(peer);
    std::thread::sleep(Duration::from_millis(500));

    // A fresh client should still see the session listed.
    let mut peer = connect_identified(path);
    peer.send(&Message::Command {
        argc: 1,
        argv: vec!["list-sessions".into()],
    })
    .unwrap();
    peer.flush().unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut found_session = false;
    while Instant::now() < deadline {
        match peer.recv() {
            Ok(Some(Message::Command { argv, .. }))
                if argv.len() >= 2 && argv[0] == ";" =>
            {
                if argv[1].contains("windows") {
                    found_session = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(found_session, "session did not survive client disconnect");
}

#[test]
fn interactive_smoke() {
    // Skip in sandboxes where pty allocation is unavailable (posix_openpt
    // is denied); this test needs a real PTY + shell.
    let master = unsafe { nix::libc::posix_openpt(nix::libc::O_RDWR) };
    if master < 0 {
        eprintln!(
            "SKIP interactive_smoke: pty allocation unavailable ({:?})",
            std::io::Error::last_os_error()
        );
        return;
    }
    unsafe { nix::libc::close(master); }

    // Deterministic shell + logging for the spawned pane.
    unsafe {
        std::env::set_var("SHELL", "/bin/bash");
        std::env::set_var("HOME", "/tmp/loom_smoke_home");
        std::env::set_var("LOOM_LOG", "1");
    }
    std::fs::create_dir_all("/tmp/loom_smoke_home").ok();
    loom_core::log::init();

    let path = socket_path();
    let _ = std::fs::remove_file(&path);

    let server = Server::new(ServerConfig {
        socket_path: path.clone(),
        socket_mode: 0o600,
    })
    .expect("Server::new");

    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let server_thread = std::thread::spawn(move || {
        let mut s = server;
        s.create_socket().expect("create_socket");
        while !s.exit {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            match s.process_once() {
                Ok(false) => break,
                _ => {}
            }
        }
        // Dropping the server kills pane process groups, so the background
        // shell does not outlive the test.
    });

    // Wait for the socket to appear.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::path::Path::new(&path).exists() {
        assert!(Instant::now() < deadline, "server socket did not appear");
        std::thread::sleep(Duration::from_millis(20));
    }

    if let Err(payload) = std::panic::catch_unwind(|| run_scenario(&path)) {
        // Stop the server, then re-raise so the harness reports the failure.
        let _ = stop_tx.send(());
        let _ = server_thread.join();
        let _ = std::fs::remove_file(&path);
        std::panic::resume_unwind(payload);
    }

    // Clean shutdown.
    let _ = stop_tx.send(());
    let _ = server_thread.join();
    let _ = std::fs::remove_file(&path);
}
