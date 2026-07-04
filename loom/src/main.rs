use std::io;
use std::os::unix::net::UnixStream as StdUnixStream;

use loom_ipc::message::Message;
use loom_ipc::peer::Peer;
use loom_server::server::{Server, ServerConfig};

/// Default socket path.
fn default_socket_path() -> String {
    format!(
        "{}/.loom/default.sock",
        std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())
    )
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let socket_path = default_socket_path();

    // Ensure socket directory exists
    if let Some(parent) = std::path::Path::new(&socket_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if args.get(1).map(|s| s.as_str()) == Some("start-server") {
        return start_server(&socket_path);
    }

    // Try to connect to server
    match connect_and_run(&socket_path, &args[1..]) {
        Ok(_) => {}
        Err(e) => {
            // If connection refused, start server and retry
            if e.kind() == io::ErrorKind::ConnectionRefused
                || e.kind() == io::ErrorKind::NotFound
            {
                // Fork and start server
                match unsafe { nix::libc::fork() } {
                    -1 => return Err(io::Error::last_os_error()),
                    0 => {
                        // Child: start server
                        return start_server(&socket_path);
                    }
                    pid => {
                        // Parent: wait briefly then connect
                        let _ = nix::sys::wait::waitpid(
                            nix::unistd::Pid::from_raw(pid),
                            None,
                        );
                        connect_and_run(&socket_path, &args[1..])?;
                    }
                }
            } else {
                return Err(e);
            }
        }
    }
    Ok(())
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

    // Send identify messages
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

    // Wait for Ready
    loop {
        match peer.recv()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("recv: {}", e)))?
        {
            Some(Message::Ready) => break,
            Some(_) => continue,
            None => {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
        }
    }

    // Send command
    if cmd_args.is_empty() {
        send_msg(&mut peer, &Message::Command {
            argc: 1,
            argv: vec!["new-session".into()],
        })?;
    } else {
        send_msg(&mut peer, &Message::Command {
            argc: cmd_args.len() as u32,
            argv: cmd_args.to_vec(),
        })?;
    }
    peer.flush()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("flush: {}", e)))?;

    // Read response
    loop {
        match peer.recv()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("recv: {}", e)))?
        {
            Some(Message::Exit) => break,
            Some(msg) => {
                if let Message::Command { argv, .. } = &msg {
                    if argv.len() >= 2 && argv[0] == ";" {
                        print!("{}", argv[1]);
                    }
                }
            }
            None => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    Ok(())
}
