use std::io::{self, Write};
use std::os::unix::io::{BorrowedFd, RawFd};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::time::Duration;

use mio::{Events, Interest, Poll, Token};
use mio::unix::SourceFd;
use nix::sys::termios;

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
    let args: Vec<String> = std::env::args().collect();
    let socket_path = default_socket_path();

    if let Some(parent) = std::path::Path::new(&socket_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if args.get(1).map(|s| s.as_str()) == Some("start-server") {
        return start_server(&socket_path);
    }

    // Try to connect. If no server, spawn one.
    for attempt in 0..60 {
        match connect_and_run(&socket_path, &args[1..]) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused
                || e.kind() == io::ErrorKind::NotFound =>
            {
                if attempt == 0 {
                    // Spawn server as a subprocess on first failure
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(&exe)
                            .arg("start-server")
                            .spawn();
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e),
        }
    }
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
    let std_stream = StdUnixStream::connect(socket_path)?;
    std_stream.set_nonblocking(true)?;
    let stream = mio::net::UnixStream::from_std(std_stream);
    let mut peer = Peer::new(stream);

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".to_string());
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());

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

    loop {
        match peer.recv()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("recv: {}", e)))?
        {
            Some(Message::Ready) => break,
            Some(_) => continue,
            None => { std::thread::sleep(Duration::from_millis(10)); continue; }
        }
    }

    let is_attach = if cmd_args.is_empty() || cmd_args[0] == "attach" || cmd_args[0] == "attach-session" {
        send_msg(&mut peer, &Message::Command {
            argc: 1, argv: vec!["new-session".into()],
        })?;
        true
    } else {
        send_msg(&mut peer, &Message::Command {
            argc: cmd_args.len() as u32,
            argv: cmd_args.to_vec(),
        })?;
        cmd_args[0] == "new-session" || cmd_args[0] == "new"
    };
    peer.flush()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("flush: {}", e)))?;

    if is_attach {
        send_msg(&mut peer, &Message::AttachSession)?;
        peer.flush()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("flush: {}", e)))?;
        run_attached(&mut peer)
    } else {
        // Read responses; break on first meaningful response or Exit
        for _ in 0..100 {
            match peer.recv()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("recv: {}", e)))?
            {
                Some(Message::Exit) => break,
                Some(Message::Command { argv, .. }) => {
                    if argv.len() >= 2 && argv[0] == ";" {
                        print!("{}", argv[1]);
                        let _ = io::stdout().flush();
                        break;
                    }
                }
                Some(Message::Ready) => {}
                Some(_) => break,
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

fn run_attached(peer: &mut Peer) -> io::Result<()> {
    let bfd = |fd: i32| unsafe { BorrowedFd::borrow_raw(fd) };

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

    // Send initial terminal size
    let (sx, sy) = get_terminal_size();
    let _ = send_msg(peer, &Message::Resize { sx, sy });
    let _ = peer.flush();

    // Write initial clear
    let _ = io::stdout().write_all(b"\x1b[2J\x1b[H");
    let _ = io::stdout().flush();

    let mut last_size = (sx, sy);

    loop {
        // Poll for events with a short timeout to check terminal size
        match poll.poll(&mut events, Some(Duration::from_millis(200))) {
            Ok(_) => {}
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }

        // Check for terminal resize periodically
        let current_size = get_terminal_size();
        if current_size != last_size {
            last_size = current_size;
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
                        Ok(0) => return Ok(()),
                        Ok(n) => {
                            let keys = buf[..n].to_vec();
                            // Ctrl-C (0x03) or Ctrl-D (0x04) exits attach mode
                            if keys == [0x03] || keys == [0x04] {
                                let _ = send_msg(peer, &Message::Detach);
                                let _ = peer.flush();
                                return Ok(());
                            }
                            let _ = send_msg(peer, &Message::KeyPress { key: keys });
                            let _ = peer.flush();
                        }
                        Err(nix::errno::Errno::EAGAIN) => {}
                        Err(_) => return Ok(()),
                    }
                }
                PEER_TOKEN => {
                    if event.is_error() || event.is_read_closed() || event.is_write_closed() {
                        return Ok(());
                    }
                    if event.is_readable() {
                        match peer.recv()
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("recv: {}", e)))?
                        {
                            Some(Message::ScreenUpdate { data }) => {
                                let _ = io::stdout().write_all(&data);
                                let _ = io::stdout().flush();
                            }
                            Some(Message::Exit) | Some(Message::Exited) => return Ok(()),
                            Some(_) => {}
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
