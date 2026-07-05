use std::io::{self, Write};
use std::os::unix::io::{BorrowedFd, RawFd};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::time::Duration;

use mio::{Events, Interest, Poll, Token};
use mio::unix::SourceFd;
use nix::sys::termios;

use loom_core::log::{Logger, self as log_core};
use loom_ipc::message::Message;
use loom_ipc::peer::Peer;
use loom_server::server::{Server, ServerConfig};

const STDIN_TOKEN: Token = Token(0);
const PEER_TOKEN: Token = Token(1);

fn default_socket_path() -> String {
    format!(
        "{}/.loom/default.sock",
        std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())
    )
}

fn main() -> io::Result<()> {
    log_core::init();
    let log = Logger::new("client");

    let args: Vec<String> = std::env::args().collect();
    let socket_path = default_socket_path();

    loom_core::log_info!(log, "main", "starting loom, args={:?}", args);
    loom_core::log_debug!(log, "main", "socket_path={}", socket_path);

    if let Some(parent) = std::path::Path::new(&socket_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if args.get(1).map(|s| s.as_str()) == Some("start-server") {
        loom_core::log_info!(log, "main", "running as server");
        return start_server(&socket_path);
    }

    for attempt in 0..60 {
        match connect_and_run(&socket_path, &args[1..]) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused
                || e.kind() == io::ErrorKind::NotFound =>
            {
                loom_core::log_debug!(log, "connect", "attempt {} failed: {:?}", attempt, e.kind());
                if attempt == 0 {
                    loom_core::log_info!(log, "connect", "no server running, spawning one");
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(&exe)
                            .arg("start-server")
                            .spawn();
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                loom_core::log_error!(log, "connect", "unexpected error: {}", e);
                return Err(e);
            }
        }
    }
    loom_core::log_error!(log, "connect", "server not available after 60 retries");
    eprintln!("error: no server running on {}", socket_path);
    std::process::exit(1);
}

fn start_server(socket_path: &str) -> io::Result<()> {
    let config = ServerConfig {
        socket_path: socket_path.to_string(),
        socket_mode: 0o600,
    };
    let mut server = Server::new(config)?;
    server.create_socket()?;
    server.run()?;
    Ok(())
}

fn send_msg(peer: &mut Peer, msg: &Message) -> io::Result<()> {
    peer.send(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("send: {}", e)))?;
    Ok(())
}

fn connect_and_run(socket_path: &str, cmd_args: &[String]) -> io::Result<()> {
    let log = Logger::new("client");

    loom_core::log_debug!(log, "connect", "connecting to {}", socket_path);
    let std_stream = match StdUnixStream::connect(socket_path) {
        Ok(s) => { loom_core::log_debug!(log, "connect", "connected successfully"); s }
        Err(e) => {
            loom_core::log_debug!(log, "connect", "connect failed: {}", e);
            return Err(e);
        }
    };

    std_stream.set_nonblocking(true)?;
    let stream = mio::net::UnixStream::from_std(std_stream);
    let mut peer = Peer::new(stream);

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".to_string());
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());

    loom_core::log_debug!(log, "identify", "sending identify ({}, cwd={})", term, cwd);
    send_msg(&mut peer, &Message::IdentifyFlags(0))?;
    send_msg(&mut peer, &Message::IdentifyLongFlags(0))?;
    send_msg(&mut peer, &Message::IdentifyTerm(term))?;
    send_msg(&mut peer, &Message::IdentifyTtyName(String::new()))?;
    send_msg(&mut peer, &Message::IdentifyCwd(cwd))?;
    send_msg(&mut peer, &Message::IdentifyClientPid(std::process::id()))?;
    send_msg(&mut peer, &Message::IdentifyEnviron(std::env::vars().collect()))?;
    send_msg(&mut peer, &Message::IdentifyDone)?;
    peer.flush()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("flush: {}", e)))?;
    loom_core::log_debug!(log, "identify", "identify sent, waiting for Ready");

    loop {
        match peer.recv()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("recv: {}", e)))?
        {
            Some(Message::Ready) => {
                loom_core::log_debug!(log, "identify", "got Ready");
                break;
            }
            Some(m) => {
                loom_core::log_debug!(log, "identify", "got unexpected message: {:?}", m);
                continue;
            }
            None => { std::thread::sleep(Duration::from_millis(10)); continue; }
        }
    }

    let is_attach = if cmd_args.is_empty() || cmd_args[0] == "attach" || cmd_args[0] == "attach-session" {
        loom_core::log_info!(log, "cmd", "auto: new-session (attach)");
        send_msg(&mut peer, &Message::Command {
            argc: 1, argv: vec!["new-session".into()],
        })?;
        true
    } else {
        loom_core::log_info!(log, "cmd", "forwarding: {:?}", cmd_args);
        send_msg(&mut peer, &Message::Command {
            argc: cmd_args.len() as u32,
            argv: cmd_args.to_vec(),
        })?;
        cmd_args[0] == "new-session" || cmd_args[0] == "new"
    };
    peer.flush()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("flush: {}", e)))?;

    if is_attach {
        loom_core::log_debug!(log, "attach", "sending AttachSession");
        send_msg(&mut peer, &Message::AttachSession)?;
        peer.flush()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("flush: {}", e)))?;
        loom_core::log_info!(log, "attach", "entering attach mode");
        run_attached(&mut peer, log)
    } else {
        for _ in 0..100 {
            match peer.recv()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("recv: {}", e)))?
            {
                Some(Message::Exit) => {
                    loom_core::log_debug!(log, "response", "got Exit");
                    break;
                }
                Some(Message::Command { argv, .. }) => {
                    if argv.len() >= 2 && argv[0] == ";" {
                        loom_core::log_debug!(log, "response", "got response: {}", argv[1]);
                        print!("{}", argv[1]);
                        let _ = io::stdout().flush();
                        break;
                    }
                }
                Some(m) => {
                    loom_core::log_debug!(log, "response", "got unexpected: {:?}", m);
                    break;
                }
                None => { std::thread::sleep(Duration::from_millis(20)); }
            }
        }
        Ok(())
    }
}

fn get_terminal_size() -> (u32, u32) {
    let mut ws = nix::libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe { nix::libc::ioctl(0, nix::libc::TIOCGWINSZ, &mut ws); }
    (ws.ws_col as u32, ws.ws_row as u32)
}

fn run_attached(peer: &mut Peer, log: Option<Logger>) -> io::Result<()> {
    let bfd = |fd: i32| unsafe { BorrowedFd::borrow_raw(fd) };

    loom_core::log_debug!(log, "attach", "setting raw mode");
    let orig_tio = termios::tcgetattr(bfd(0))
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("tcgetattr: {}", e)))?;

    let mut raw = orig_tio.clone();
    raw.input_flags &= !(termios::InputFlags::IXON | termios::InputFlags::ICRNL
        | termios::InputFlags::INLCR | termios::InputFlags::IGNCR
        | termios::InputFlags::ISTRIP);
    raw.output_flags &= !(termios::OutputFlags::OPOST);
    raw.local_flags &= !(termios::LocalFlags::ICANON | termios::LocalFlags::ECHO
        | termios::LocalFlags::ECHOE | termios::LocalFlags::ECHONL
        | termios::LocalFlags::ISIG | termios::LocalFlags::IEXTEN);
    raw.control_chars[termios::SpecialCharacterIndices::VMIN as usize] = 1;
    raw.control_chars[termios::SpecialCharacterIndices::VTIME as usize] = 0;
    termios::tcsetattr(bfd(0), termios::SetArg::TCSANOW, &raw)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("tcsetattr: {}", e)))?;

    struct RawModeGuard(RawFd, termios::Termios);
    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            let _ = termios::tcsetattr(
                unsafe { BorrowedFd::borrow_raw(self.0) },
                termios::SetArg::TCSANOW,
                &self.1,
            );
        }
    }
    let _guard = RawModeGuard(0, orig_tio);

    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(1024);

    let mut stdin_source = SourceFd(&0);
    poll.registry().register(&mut stdin_source, STDIN_TOKEN, Interest::READABLE)?;

    peer.register(poll.registry(), PEER_TOKEN, Interest::READABLE | Interest::WRITABLE)?;

    let (sx, sy) = get_terminal_size();
    loom_core::log_debug!(log, "attach", "initial size: {}x{}", sx, sy);
    let _ = send_msg(peer, &Message::Resize { sx, sy });
    let _ = peer.flush();

    let _ = io::stdout().write_all(b"\x1b[2J\x1b[H");
    let _ = io::stdout().flush();

    let mut last_size = (sx, sy);

    loop {
        match poll.poll(&mut events, Some(Duration::from_millis(200))) {
            Ok(_) => {}
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }

        let current_size = get_terminal_size();
        if current_size != last_size {
            last_size = current_size;
            loom_core::log_debug!(log, "resize", "new size: {}x{}", current_size.0, current_size.1);
            let _ = send_msg(peer, &Message::Resize {
                sx: current_size.0,
                sy: current_size.1,
            });
            let _ = peer.flush();
        }

        for event in &events {
            match event.token() {
                STDIN_TOKEN => {
                    let mut buf = [0u8; 256];
                    match nix::unistd::read(0, &mut buf) {
                        Ok(0) => {
                            loom_core::log_debug!(log, "stdin", "EOF");
                            return Ok(());
                        }
                        Ok(n) => {
                            let keys = buf[..n].to_vec();
                            if keys == [0x03] || keys == [0x04] {
                                loom_core::log_debug!(log, "stdin", "Ctrl-C/D, detaching");
                                let _ = send_msg(peer, &Message::Detach);
                                let _ = peer.flush();
                                return Ok(());
                            }
                            let _ = send_msg(peer, &Message::KeyPress { key: keys });
                            let _ = peer.flush();
                        }
                        Err(nix::errno::Errno::EAGAIN) => {}
                        Err(e) => {
                            loom_core::log_error!(log, "stdin", "read error: {}", e);
                            return Ok(());
                        }
                    }
                }
                PEER_TOKEN => {
                    if event.is_error() || event.is_read_closed() || event.is_write_closed() {
                        loom_core::log_debug!(log, "peer", "connection closed");
                        return Ok(());
                    }
                    if event.is_readable() {
                        match peer.recv()
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("recv: {}", e)))?
                        {
                            Some(Message::ScreenUpdate { data }) => {
                                loom_core::log_debug!(log, "screen", "got ScreenUpdate ({} bytes)", data.len());
                                let _ = io::stdout().write_all(&data);
                                let _ = io::stdout().flush();
                            }
                            Some(Message::Exit) | Some(Message::Exited) => {
                                loom_core::log_debug!(log, "peer", "got Exit/Exited");
                                return Ok(());
                            }
                            Some(m) => {
                                loom_core::log_debug!(log, "peer", "unexpected msg: {:?}", m);
                            }
                            None => {}
                        }
                    }
                    if event.is_writable() {
                        if peer.has_pending_writes() {
                            let _ = peer.flush();
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
