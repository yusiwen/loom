use std::collections::HashMap;
use std::io;
use std::os::unix::io::{AsFd, BorrowedFd, FromRawFd, RawFd};
use std::time::Duration;

use mio::net::UnixStream;
use mio::{event::Event, Events, Interest, Poll, Registry, Token, Waker};
use mio::unix::SourceFd;

use loom_core::log::Logger;
use loom_core::session::{PaneId, Session, SessionId, Window, WindowId};
use loom_ipc::message::Message;
use loom_ipc::peer::Peer;
use loom_input::input::InputCtx;

use crate::redraw;
use crate::spawn as spawner;

/// Token for the accept listener.
const ACCEPT_TOKEN: Token = Token(0);
/// Signal notification token.
const SIGNAL_TOKEN: Token = Token(1);
/// Waker token.
const WAKER_TOKEN: Token = Token(2);
/// First token for client peers.
const CLIENT_BASE: usize = 256;
/// First token for PTY fds.
const PTY_BASE: usize = 512;

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
    pub attached: bool,
    pub pending_size: Option<(u32, u32)>,
}

/// The server manages sessions, windows, clients and the event loop.
pub struct Server {
    config: ServerConfig,
    poll: Poll,
    #[allow(dead_code)]
    waker: Waker,
    log: Option<Logger>,
    clients: HashMap<Token, ClientState>,
    next_client_token: usize,
    sessions: HashMap<SessionId, Session>,
    windows: HashMap<WindowId, Window>,
    listen_fd: Option<RawFd>,
    /// Map PTY master fd → (client_token, pane_id)
    attached_panes: HashMap<RawFd, (Token, PaneId)>,
    next_pty_token: usize,
    pub exit: bool,
}

impl Server {
    pub fn new(config: ServerConfig) -> io::Result<Self> {
        loom_core::log::init();
        let poll = Poll::new()?;
        let waker = Waker::new(poll.registry(), WAKER_TOKEN)?;
        let log = Logger::new("server");
        loom_core::log_info!(log, "start", "server created, socket={}", config.socket_path);
        Ok(Self {
            config,
            poll,
            waker,
            log,
            clients: HashMap::new(),
            next_client_token: 0,
            sessions: HashMap::new(),
            windows: HashMap::new(),
            listen_fd: None,
            attached_panes: HashMap::new(),
            next_pty_token: 0,
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

    /// Process one event loop iteration. Returns `false` if server should exit.
    pub fn process_once(&mut self) -> io::Result<bool> {
        if self.exit {
            return Ok(false);
        }
        let mut events = Events::with_capacity(1024);
        match self.poll.poll(&mut events, Some(Duration::from_millis(10))) {
            Ok(_) => {}
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => return Ok(true),
            Err(e) => return Err(e),
        }

        for event in &events {
            let token = event.token();
            if token == ACCEPT_TOKEN {
                self.handle_accept()?;
            } else if token.0 >= PTY_BASE as usize {
                self.handle_pty_event(event)?;
            } else {
                self.handle_client_event(token, event)?;
            }
        }
        self.poll_ptys()?;
        Ok(true)
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
                let token = event.token();
                if token == ACCEPT_TOKEN {
                    self.handle_accept()?;
                } else if token.0 >= PTY_BASE as usize {
                    self.handle_pty_event(event)?;
                } else {
                    self.handle_client_event(token, event)?;
                }
            }

            // Poll PTY fds directly (workaround for mio SourceFd signal fd issue)
            self.poll_ptys()?;
        }
        Ok(())
    }

    fn poll_ptys(&mut self) -> io::Result<()> {
        let snapshot: Vec<(RawFd, Token, PaneId)> = self.attached_panes.iter()
            .map(|(&fd, &(token, pid))| (fd, token, pid))
            .collect();

        for (fd, client_token, pane_id) in snapshot {
            if !self.attached_panes.contains_key(&fd) {
                continue;
            }
            if !self.clients.contains_key(&client_token) {
                loom_core::log_debug!(self.log, "pty_poll", "client gone, cleanup fd={}", fd);
                self.cleanup_pty(fd);
                continue;
            }

            let mut buf = [0u8; 65536];
            match nix::unistd::read(fd, &mut buf) {
                Ok(0) => {
                    loom_core::log_debug!(self.log, "pty_poll", "EOF on fd={}", fd);
                    let _ = self.send_to(client_token, &Message::Exited);
                    self.cleanup_pty(fd);
                }
                Ok(n) => {
                    loom_core::log_debug!(self.log, "pty_poll", "read {} bytes from fd={}", n, fd);
                    self.process_pty_data(client_token, pane_id, &buf[..n])?;
                }
                Err(nix::errno::Errno::EAGAIN) => {
                    loom_core::log_debug!(self.log, "pty_poll", "EAGAIN on fd={}", fd);
                }
                Err(nix::errno::Errno::EINTR) => {}
                Err(e) => {
                    loom_core::log_error!(self.log, "pty_poll", "read error on fd={}: {}", fd, e);
                    self.cleanup_pty(fd);
                }
            }
        }
        Ok(())
    }

    fn process_pty_data(&mut self, client_token: Token, pane_id: PaneId, data: &[u8]) -> io::Result<()> {
        loom_core::log_debug!(self.log, "pty_data", "processing {} bytes for pane={}", data.len(), pane_id);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.process_pty_data_inner(client_token, pane_id, data)
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                loom_core::log_error!(self.log, "pty_data", "error: {} (pane={})", e, pane_id);
            }
            Err(panic) => {
                let msg = if let Some(s) = panic.downcast_ref::<&str>() { s.to_string() }
                          else if let Some(s) = panic.downcast_ref::<String>() { s.clone() }
                          else { format!("{:?}", panic) };
                loom_core::log_error!(self.log, "pty_data", "PANIC: {} (pane={}, bytes={})", msg, pane_id, data.len());
            }
        }
        Ok(())
    }

    fn process_pty_data_inner(&mut self, client_token: Token, pane_id: PaneId, data: &[u8]) -> io::Result<()> {
        let wid = match self.windows.iter()
            .find(|(_, w)| w.panes.contains_key(&pane_id))
            .map(|(&id, _)| id)
        {
            Some(wid) => wid,
            None => {
                loom_core::log_debug!(self.log, "pty_data", "no window for pane={}", pane_id);
                return Ok(());
            }
        };

        // DEBUG: log raw PTY data first 200 bytes
        let preview = &data[..data.len().min(200)];
        loom_core::log_debug!(self.log, "pty_raw", "{} bytes, preview={:?}", data.len(), preview);

        if let Some(window) = self.windows.get_mut(&wid) {
            if let Some(pane) = window.panes.get_mut(&pane_id) {
                loom_core::log_debug!(self.log, "pty_data", "parsing {} bytes through InputCtx", data.len());
                let mut ctx = InputCtx::new(&mut pane.screen);
                ctx.parse_buf(data);

                // DEBUG: log InputCtx state after parse (via ctx, not pane)
                let (cx, cy) = (ctx.screen.cx, ctx.screen.cy);
                let (fg, bg, attr) = (ctx.cell.fg, ctx.cell.bg, ctx.cell.attr);
                loom_core::log_debug!(self.log, "pty_state",
                    "cx={}, cy={}, fg={:#010x}, bg={:#010x}, attr={:#06x}",
                    cx, cy, fg, bg, attr);
            }
        }

        if let Some(window) = self.windows.get(&wid) {
            let mut redraw_buf = Vec::new();
            if redraw::redraw_window(window, &mut redraw_buf).is_ok() {
                loom_core::log_debug!(self.log, "pty_data", "sending ScreenUpdate ({} bytes)", redraw_buf.len());
                // DEBUG: log ScreenUpdate first 200 bytes
                let preview = &redraw_buf[..redraw_buf.len().min(200)];
                loom_core::log_debug!(self.log, "redraw", "preview={:?}", preview);
                let _ = self.send_to(client_token, &Message::ScreenUpdate { data: redraw_buf });
            } else {
                loom_core::log_error!(self.log, "pty_data", "redraw failed");
            }
        }
        Ok(())
    }

    /// Create a pane with a spawned shell process.
    fn spawn_pane(&mut self, wid: WindowId, sx: u32, sy: u32, cwd: &str) -> Option<PaneId> {
        let window = self.windows.get_mut(&wid)?;
        let pid = window.create_pane(sx, sy);
        loom_core::log_debug!(self.log, "spawn", "pane_id={}, wid={}, cwd={}", pid, wid, cwd);

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        loom_core::log_debug!(self.log, "spawn", "calling spawn_pty(shell={}, cwd={})", shell, cwd);
        match spawner::spawn_pty(&[shell.clone()], cwd, sx, sy) {
            Ok((child_pid, master_fd)) => {
                loom_core::log_info!(self.log, "spawn", "spawn_pty ok: pid={}, fd={}", child_pid, master_fd);
                if let Some(pane) = window.panes.get_mut(&pid) {
                    pane.fd = Some(master_fd);
                    pane.pid = Some(child_pid.as_raw() as u32);
                    pane.shell = shell;
                    pane.cwd = cwd.to_string();
                }
            }
            Err(e) => {
                loom_core::log_error!(self.log, "spawn", "spawn_pty FAILED: {}", e);
            }
        }
        Some(pid)
    }

    /// Add a pre-established client stream (for testing).
    pub fn add_client_stream(&mut self, std_stream: std::os::unix::net::UnixStream) -> io::Result<Token> {
        std_stream.set_nonblocking(true)?;
        let stream = mio::net::UnixStream::from_std(std_stream);
        let mut peer = Peer::new(stream);
        let token = Token(CLIENT_BASE + self.next_client_token);
        self.next_client_token += 1;

        peer.register(self.poll.registry(), token, Interest::READABLE | Interest::WRITABLE)?;

        let client = ClientState {
            peer,
            flags: 0,
            session_id: None,
            identified: false,
            term_name: String::new(),
            tty_name: String::new(),
            cwd: String::new(),
            pid: 0,
            attached: false,
            pending_size: None,
        };

        self.clients.insert(token, client);
        Ok(token)
    }

    fn handle_accept(&mut self) -> io::Result<()> {
        loom_core::log_debug!(self.log, "accept", "handling accept");
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
                attached: false,
                pending_size: None,
            };

            client.peer.register(
                self.poll.registry(),
                token,
                Interest::READABLE | Interest::WRITABLE,
            )?;

            self.clients.insert(token, client);
            loom_core::log_debug!(self.log, "accept", "accepted client token={:?}", token);
        }

        Ok(())
    }

    fn handle_pty_event(&mut self, event: &Event) -> io::Result<()> {
        let token = event.token();

        // Find the fd for this token
        let fd = match self.attached_panes.iter()
            .find(|(_, (t, _))| *t == token)
            .map(|(&fd, _)| fd)
        {
            Some(fd) => fd,
            None => return Ok(()),
        };

        if event.is_error() || event.is_read_closed() {
            self.cleanup_pty(fd);
            return Ok(());
        }

        if event.is_readable() {
            let mut buf = vec![0u8; 65536];
            match nix::unistd::read(fd, &mut buf) {
                Ok(0) => {
                    loom_core::log_debug!(self.log, "pty", "EOF on fd={}", fd);
                    let notify_client = self.attached_panes.get(&fd).map(|&(ct, _)| ct);
                    self.cleanup_pty(fd);
                    if let Some(ct) = notify_client {
                        let _ = self.send_to(ct, &Message::Exited);
                    }
                    return Ok(());
                }
                Ok(n) => {
                    buf.truncate(n);
                    loom_core::log_debug!(self.log, "pty", "read {} bytes from fd={}", n, fd);

                    let (client_token, pane_id) = match self.attached_panes.get(&fd) {
                        Some(&(ct, pid)) => (ct, pid),
                        None => return Ok(()),
                    };

                    // Check if client is still connected
                    if !self.clients.contains_key(&client_token) {
                        self.cleanup_pty(fd);
                        return Ok(());
                    }

                    let wid = self.windows.iter()
                        .find(|(_, w)| w.panes.contains_key(&pane_id))
                        .map(|(&id, _)| id);

                    if let Some(wid) = wid {
                        if let Some(window) = self.windows.get_mut(&wid) {
                            if let Some(pane) = window.panes.get_mut(&pane_id) {
                                let screen = &mut pane.screen;
                                let mut ctx = InputCtx::new(screen);
                                ctx.parse_buf(&buf);
                            }
                        }

                        if let Some(window) = self.windows.get(&wid) {
                            let mut redraw_buf = Vec::new();
                            if let Err(e) = redraw::redraw_window(window, &mut redraw_buf) {
                                eprintln!("redraw error: {}", e);
                            } else {
                                // Ignore errors - client may have disconnected
                                let _ = self.send_to(client_token, &Message::ScreenUpdate {
                                    data: redraw_buf,
                                });
                            }
                        }
                    }
                }
                Err(nix::errno::Errno::EAGAIN) => {}
                Err(e) => {
                    loom_core::log_error!(self.log, "pty", "read error on fd={}: {}", fd, e);
                    self.cleanup_pty(fd);
                }
            }
        }

        Ok(())
    }

    fn cleanup_pty(&mut self, fd: RawFd) {
        if let Some(&(_, _)) = self.attached_panes.get(&fd) {
            // Deregister from event loop
            let mut source = SourceFd(&fd);
            let _ = self.poll.registry().deregister(&mut source);
            // Close PTY fd
            unsafe { nix::libc::close(fd); }
            self.attached_panes.remove(&fd);
        }
    }

    fn handle_client_event(&mut self, token: Token, event: &Event) -> io::Result<()> {
        if event.is_error() || event.is_read_closed() || event.is_write_closed() {
            self.clients.remove(&token);
            return Ok(());
        }

        if event.is_readable() {
            loop {
                let msg = {
                    let client = match self.clients.get_mut(&token) {
                        Some(c) => c,
                        None => return Ok(()),
                    };
                    client.peer.recv()?
                };
                match msg {
                    Some(msg) => {
                        self.dispatch_message(token, msg)?;
                    }
                    None => break,
                }
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
        loom_core::log_debug!(self.log, "dispatch", "got msg from token={:?}", token);
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
                loom_core::log_debug!(self.log, "dispatch", "IdentifyDone from token={:?}", token);
                if let Some(client) = self.clients.get_mut(&token) {
                    client.identified = true;
                }
                loom_core::log_debug!(self.log, "dispatch", "sending Ready");
                self.send_to(token, &Message::Ready)?;
            }
            Message::Command { argc: _, argv } => {
                loom_core::log_info!(self.log, "dispatch", "Command: {:?}", argv);
                if argv.len() >= 1 {
                    match argv[0].as_str() {
                        "new-session" | "new" => {
                            loom_core::log_info!(self.log, "dispatch", "creating new session");
                            let cwd = self.clients.get(&token)
                                .map(|c| c.cwd.clone())
                                .unwrap_or_else(|| "/tmp".to_string());
                            // Use pending terminal size if available
                            let (sx, sy) = self.clients.get(&token)
                                .and_then(|c| c.pending_size)
                                .unwrap_or((80, 24));
                            loom_core::log_debug!(self.log, "dispatch", "window size: {}x{}", sx, sy);
                            let mut session = Session::new(None, &cwd);
                            let mut window = Window::new(sx, sy);
                            let wid = window.id;
                            let sid = session.id;

                            // Insert window first so spawn_pane can find it
                            self.windows.insert(wid, window);
                            let _pane_id = self.spawn_pane(wid, sx, sy, &cwd);

                            session.attach_window(0, wid);
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
                if let Some(client) = self.clients.get_mut(&token) {
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
                    } else {
                        // No session yet — store size for when window is created
                        client.pending_size = Some((sx, sy));
                    }
                }
            }
            Message::AttachSession => {
                loom_core::log_debug!(self.log, "dispatch", "AttachSession from token={:?}", token);
                if let Some(client) = self.clients.get(&token) {
                    if let Some(sid) = client.session_id {
                        if let Some(session) = self.sessions.get(&sid) {
                            if let Some(wl) = session.current_winlink() {
                                if let Some(window) = self.windows.get(&wl.window_id) {
                                    if let Some(active_pane_id) = window.active_pane_id {
                                        if let Some(pane) = window.panes.get(&active_pane_id) {
                                            if let Some(pfd) = pane.fd {
                                                loom_core::log_debug!(self.log, "dispatch", "attaching PTY fd={}", pfd);
                                                // Set PTY fd to non-blocking for poll_ptys
                                                let flags = nix::fcntl::fcntl(pfd, nix::fcntl::FcntlArg::F_GETFL)
                                                    .unwrap_or(0);
                                                let _ = nix::fcntl::fcntl(pfd, nix::fcntl::FcntlArg::F_SETFL(
                                                    nix::fcntl::OFlag::from_bits_truncate(flags | nix::libc::O_NONBLOCK as i32)
                                                ));
                                                let pty_token = Token(PTY_BASE + self.next_pty_token);
                                                self.next_pty_token += 1;
                                                let mut source = SourceFd(&pfd);
                                                if self.poll.registry().register(
                                                    &mut source,
                                                    pty_token,
                                                    Interest::READABLE,
                                                ).is_ok() {
                                                    self.attached_panes.insert(pfd, (token, active_pane_id));
                                                    loom_core::log_info!(self.log, "dispatch", "PTY registered token={:?}", pty_token);
                                                } else {
                                                    loom_core::log_error!(self.log, "dispatch", "failed to register PTY fd");
                                                }
                                            } else {
                                                loom_core::log_error!(self.log, "dispatch", "AttachSession: pane.fd is None (spawn failed?)");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Mark client as attached
                if let Some(client) = self.clients.get_mut(&token) {
                    client.attached = true;
                }
            }
            Message::KeyPress { key } => {
                loom_core::log_debug!(self.log, "dispatch", "KeyPress ({} bytes)", key.len());
                if let Some(client) = self.clients.get(&token) {
                    if let Some(sid) = client.session_id {
                        if let Some(session) = self.sessions.get(&sid) {
                            if let Some(wl) = session.current_winlink() {
                                if let Some(window) = self.windows.get(&wl.window_id) {
                                    if let Some(active_pane_id) = window.active_pane_id {
                                        if let Some(pane) = window.panes.get(&active_pane_id) {
                    if let Some(pfd) = pane.fd {
                                            let bfd = unsafe { BorrowedFd::borrow_raw(pfd) };
                                            let _ = nix::unistd::write(&bfd, &key);
                                            }
                                        }
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
        let client = match self.clients.get_mut(&token) {
            Some(c) => c,
            None => return Ok(()),
        };
        if let Err(e) = client.peer.send(msg) {
            loom_core::log_error!(self.log, "send", "send to token={:?} failed: {}", token, e);
            return Ok(());
        }
        if let Err(e) = client.peer.flush() {
            loom_core::log_error!(self.log, "send", "flush to token={:?} failed: {}", token, e);
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
