use std::collections::HashMap;
use std::io;
use std::os::unix::io::{FromRawFd, RawFd};
use std::time::Duration;

use mio::net::UnixStream;
use mio::{event::Event, Events, Interest, Poll, Registry, Token, Waker};

use loom_core::session::{Session, SessionId, Window, WindowId};
use loom_ipc::message::Message;
use loom_ipc::peer::Peer;

/// Token for the accept listener.
const ACCEPT_TOKEN: Token = Token(0);
/// First token for client peers.
const CLIENT_BASE: usize = 256;
/// Signal notification token.
const SIGNAL_TOKEN: Token = Token(1);

/// Server configuration.
#[derive(Clone)]
pub struct ServerConfig {
    pub socket_path: String,
    pub socket_mode: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            socket_path: format!(
                "{}/.loom/default.sock",
                std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())
            ),
            socket_mode: 0o600,
        }
    }
}

/// Connected client state.
pub struct ClientState {
    pub peer: Peer,
    pub flags: u64,
    pub session_id: Option<SessionId>,
    pub identified: bool,
    pub term_name: String,
    pub tty_name: String,
    pub cwd: String,
    pub pid: u32,
}

/// The server manages sessions, windows, clients and the event loop.
pub struct Server {
    config: ServerConfig,
    poll: Poll,
    #[allow(dead_code)]
    waker: Waker,
    clients: HashMap<Token, ClientState>,
    next_client_token: usize,
    sessions: HashMap<SessionId, Session>,
    windows: HashMap<WindowId, Window>,
    listen_fd: Option<RawFd>,
    pub exit: bool,
}

impl Server {
    pub fn new(config: ServerConfig) -> io::Result<Self> {
        let poll = Poll::new()?;
        let waker = Waker::new(poll.registry(), Token(2))?;
        Ok(Self {
            config,
            poll,
            waker,
            clients: HashMap::new(),
            next_client_token: 0,
            sessions: HashMap::new(),
            windows: HashMap::new(),
            listen_fd: None,
            exit: false,
        })
    }

    pub fn registry(&self) -> &Registry {
        self.poll.registry()
    }

    /// Create and bind the Unix domain socket.
    pub fn create_socket(&mut self) -> io::Result<()> {
        let path = &self.config.socket_path;

        // Remove existing socket file
        let _ = std::fs::remove_file(path);

        let stream = UnixStream::connect(path);
        match stream {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("socket already in use: {}", path),
                ));
            }
            Err(ref e) if e.kind() != io::ErrorKind::ConnectionRefused
                && e.kind() != io::ErrorKind::NotFound =>
            {
                return Err(io::Error::new(
                    e.kind(),
                    format!("error checking socket: {}", e),
                ));
            }
            _ => {}
        }

        // Create and bind a listener socket manually
        let fd = unsafe {
            let fd = nix::libc::socket(
                nix::libc::AF_UNIX,
                nix::libc::SOCK_STREAM | nix::libc::SOCK_CLOEXEC,
                0,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut addr = std::mem::zeroed::<nix::libc::sockaddr_un>();
            addr.sun_family = nix::libc::AF_UNIX as u16;
            let path_bytes = path.as_bytes();
            let max_len = std::mem::size_of_val(&addr.sun_path) - 1;
            let len = path_bytes.len().min(max_len);
            std::ptr::copy_nonoverlapping(
                path_bytes.as_ptr(),
                addr.sun_path.as_mut_ptr() as *mut u8,
                len,
            );
            let addrlen = std::mem::size_of::<nix::libc::sa_family_t>() + 2 + len;
            let ret = nix::libc::bind(
                fd,
                &addr as *const _ as *const nix::libc::sockaddr,
                addrlen as u32,
            );
            if ret < 0 {
                nix::libc::close(fd);
                return Err(io::Error::last_os_error());
            }
            let ret = nix::libc::listen(fd, 128);
            if ret < 0 {
                nix::libc::close(fd);
                return Err(io::Error::last_os_error());
            }
            fd
        };

        // Set non-blocking
        nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK))
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("fcntl: {}", e)))?;

        self.listen_fd = Some(fd);

        // Register with mio
        use mio::unix::SourceFd;
        let mut source = SourceFd(&fd);
        self.poll.registry().register(&mut source, ACCEPT_TOKEN, Interest::READABLE)?;

        Ok(())
    }

    /// Start the server event loop.
    pub fn run(&mut self) -> io::Result<()> {
        let mut events = Events::with_capacity(1024);

        while !self.exit {
            match self.poll.poll(&mut events, Some(Duration::from_millis(100))) {
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }

            for event in &events {
                match event.token() {
                    ACCEPT_TOKEN => {
                        self.handle_accept()?;
                    }
                    SIGNAL_TOKEN => {
                        // Signal received
                    }
                    token => {
                        self.handle_client_event(token, event)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_accept(&mut self) -> io::Result<()> {
        let fd = match self.listen_fd {
            Some(fd) => fd,
            None => return Ok(()),
        };

        loop {
            let ret = unsafe {
                nix::libc::accept4(
                    fd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    nix::libc::SOCK_CLOEXEC | nix::libc::SOCK_NONBLOCK,
                )
            };
            if ret < 0 {
                let err = io::Error::last_os_error();
                match err.kind() {
                    io::ErrorKind::WouldBlock => break,
                    io::ErrorKind::Interrupted => continue,
                    _ => return Err(err),
                }
            }
            let client_fd = ret;

            let std_stream = unsafe {
                std::os::unix::net::UnixStream::from_raw_fd(client_fd)
            };
            let stream = mio::net::UnixStream::from_std(std_stream);
            let peer = Peer::new(stream);
            let token = Token(CLIENT_BASE + self.next_client_token);
            self.next_client_token += 1;

            let mut client = ClientState {
                peer,
                flags: 0,
                session_id: None,
                identified: false,
                term_name: String::new(),
                tty_name: String::new(),
                cwd: String::new(),
                pid: 0,
            };

            client.peer.register(
                self.poll.registry(),
                token,
                Interest::READABLE | Interest::WRITABLE,
            )?;

            self.clients.insert(token, client);
        }

        Ok(())
    }

    fn handle_client_event(&mut self, token: Token, event: &Event) -> io::Result<()> {
        if event.is_error() || event.is_read_closed() || event.is_write_closed() {
            self.clients.remove(&token);
            return Ok(());
        }

        if event.is_readable() {
            let msg = {
                let client = match self.clients.get_mut(&token) {
                    Some(c) => c,
                    None => return Ok(()),
                };
                client.peer.recv()?
            };

            if let Some(msg) = msg {
                self.dispatch_message(token, msg)?;
            }
        }

        if event.is_writable() {
            if let Some(client) = self.clients.get_mut(&token) {
                if client.peer.has_pending_writes() {
                    client.peer.flush()?;
                }
            }
        }

        Ok(())
    }

    fn dispatch_message(&mut self, token: Token, msg: Message) -> io::Result<()> {
        match msg {
            Message::IdentifyFlags(flags) => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.flags = flags;
                }
            }
            Message::IdentifyLongFlags(flags) => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.flags = flags;
                }
            }
            Message::IdentifyTerm(term) => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.term_name = term;
                }
            }
            Message::IdentifyTtyName(tty) => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.tty_name = tty;
                }
            }
            Message::IdentifyCwd(cwd) => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.cwd = cwd;
                }
            }
            Message::IdentifyClientPid(pid) => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.pid = pid;
                }
            }
            Message::IdentifyDone => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.identified = true;
                }
                self.send_to(token, &Message::Ready)?;
            }
            Message::Command { argc: _, argv } => {
                // Execute the command (basic stub - will be expanded with cmd system)
                if argv.len() >= 1 {
                    match argv[0].as_str() {
                        "new-session" | "new" => {
                            let cwd = self.clients.get(&token)
                                .map(|c| c.cwd.clone())
                                .unwrap_or_else(|| "/tmp".to_string());
                            let mut session = Session::new(None, &cwd);
                            let mut window = Window::new(80, 24);
                            let _pid = window.create_pane(80, 24);
                            let wid = window.id;
                            session.attach_window(0, wid);

                            let sid = session.id;
                            self.windows.insert(wid, window);
                            self.sessions.insert(sid, session);

                            if let Some(client) = self.clients.get_mut(&token) {
                                client.session_id = Some(sid);
                            }
                        }
                        "kill-session" => {
                            if let Some(client) = self.clients.get(&token) {
                                if let Some(sid) = client.session_id {
                                    let mut to_remove = Vec::new();
                                    if let Some(session) = self.sessions.get(&sid) {
                                        // Collect window IDs to remove
                                        for (_, wl) in &session.windows {
                                            to_remove.push(wl.window_id);
                                        }
                                    }
                                    self.sessions.remove(&sid);
                                    for wid in to_remove {
                                        self.windows.remove(&wid);
                                    }
                                }
                            }
                        }
                        "list-sessions" | "ls" => {
                            let names: Vec<String> = self.sessions.values()
                                .map(|s| format!("{}: {} windows", s.name, s.windows.len()))
                                .collect();
                            let response = names.join("\n");
                            self.send_to(token, &Message::Command {
                                argc: 0,
                                argv: vec![";".into(), response],
                            })?;
                        }
                        _ => {}
                    }
                }
            }
            Message::Detach => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.session_id = None;
                }
                self.send_to(token, &Message::Exit)?;
            }
            Message::Resize { sx, sy } => {
                if let Some(client) = self.clients.get(&token) {
                    if let Some(sid) = client.session_id {
                        if let Some(session) = self.sessions.get(&sid) {
                            if let Some(wl) = session.current_winlink() {
                                if let Some(window) = self.windows.get_mut(&wl.window_id) {
                                    window.sx = sx;
                                    window.sy = sy;
                                    for (_, pane) in &mut window.panes {
                                        pane.sx = sx;
                                        pane.sy = sy;
                                        pane.screen.resize(sx, sy);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Message::Exit => {
                let token_copy = token;
                if let Some(client) = self.clients.get(&token_copy) {
                    if let Some(sid) = client.session_id {
                        self.sessions.remove(&sid);
                    }
                }
                self.clients.remove(&token_copy);
            }
            _ => {}
        }
        Ok(())
    }

    fn send_to(&mut self, token: Token, msg: &Message) -> io::Result<()> {
        if let Some(client) = self.clients.get_mut(&token) {
            client.peer.send(msg)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("send: {}", e)))?;
            client.peer.flush()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("flush: {}", e)))?;
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(fd) = self.listen_fd {
            unsafe { nix::libc::close(fd); }
        }
        // Clean up socket file
        let _ = std::fs::remove_file(&self.config.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn test_server_create() {
        let config = ServerConfig {
            socket_path: format!("/tmp/loom-test-{}.sock", std::process::id()),
            socket_mode: 0o600,
        };
        let server = Server::new(config).unwrap();
        assert_eq!(server.clients.len(), 0);
    }
}
