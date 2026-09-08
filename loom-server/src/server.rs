use std::collections::HashMap;
use std::io;
use std::os::unix::io::{AsRawFd, BorrowedFd, RawFd};
use std::sync::OnceLock;
use std::time::Duration;

use mio::{event::Event, Events, Interest, Poll, Registry, Token, Waker};
use mio::unix::SourceFd;

use loom_core::log::Logger;
use loom_core::grid_cell::Grid;
use loom_core::options::{Options, Scope};
use loom_core::session::{
    CopyMode, PaneId, Session, SessionId, Window, WindowId, WindowPane, WINLINK_ALERTFLAGS,
    WINLINK_BELL, WINDOW_ACTIVITY, WINDOW_BELL,
};
use loom_ipc::message::Message;
use loom_ipc::peer::Peer;
use loom_input::input::Parser;
use loom_tty::tty::Tty;

use crate::layout;
use crate::redraw;
use crate::spawn as spawner;

/// Token for the accept listener.
const ACCEPT_TOKEN: Token = Token(0);
/// Signal notification token.
#[allow(dead_code)]
const SIGNAL_TOKEN: Token = Token(1);
/// Waker token.
const WAKER_TOKEN: Token = Token(2);
/// First token for client peers.
const CLIENT_BASE: usize = 256;
/// First token for PTY fds.
const PTY_BASE: usize = 512;

/// Rows reserved for the status line at the bottom of the client terminal.
/// Window/PTY content is sized to `terminal - STATUS_ROWS` rows (B3).
const STATUS_ROWS: u32 = 1;

/// Window/PTY height derived from a client terminal height `sy`: the status
/// line reserves the bottom row(s); a terminal too short to hold a status
/// line keeps its full height.
fn content_sy(sy: u32) -> u32 {
    if sy > STATUS_ROWS {
        sy - STATUS_ROWS
    } else {
        sy
    }
}

/// A server command handler: invoked with the command arguments (argv with
/// the command name removed) for the requesting client's token.
type CommandHandler = fn(&mut Server, Token, &[String]) -> io::Result<()>;

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
    pub tty: Option<Tty>,
    pub tty_initialized: bool,
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
    listener: Option<std::os::unix::net::UnixListener>,
    /// Map PTY token → (master_fd, pane_id). One reader per PTY (mio event).
    pty_fds: HashMap<Token, (RawFd, PaneId)>,
    next_pty_token: usize,
    /// Persistent per-pane input parsers (state survives across reads).
    parsers: HashMap<PaneId, Parser>,
    /// Global paste buffer (last yank from copy-mode).
    paste_buffer: String,
    /// Server-wide (global) options (B8). Sessions/windows/panes are child
    /// options containers that inherit from these defaults.
    global_options: Options,
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
            listener: None,
            pty_fds: HashMap::new(),
            next_pty_token: 0,
            parsers: HashMap::new(),
            paste_buffer: String::new(),
            global_options: Options::with_defaults(),
            exit: false,
        })
    }

    pub fn registry(&self) -> &Registry {
        self.poll.registry()
    }

    /// Create and bind the Unix domain socket.
    pub fn create_socket(&mut self) -> io::Result<()> {
        let path = &self.config.socket_path;

        // If we can connect, another server is already running.
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("socket already in use: {}", path),
                ));
            }
            Err(_) => {} // not running / no socket file yet — proceed to bind
        }

        // Remove stale socket file, then bind+listen via std (portable to
        // macOS and Linux, unlike raw accept4 which is Linux-only).
        let _ = std::fs::remove_file(path);
        let listener = std::os::unix::net::UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        let _ = std::fs::set_permissions(
            path,
            std::os::unix::fs::PermissionsExt::from_mode(self.config.socket_mode),
        );

        let fd = listener.as_raw_fd();
        let mut source = SourceFd(&fd);
        self.poll.registry().register(&mut source, ACCEPT_TOKEN, Interest::READABLE)?;

        self.listener = Some(listener);

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
            } else if token.0 >= PTY_BASE {
                self.handle_pty_event(event)?;
            } else {
                self.handle_client_event(token, event)?;
            }
        }
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
                } else if token.0 >= PTY_BASE {
                    self.handle_pty_event(event)?;
                } else {
                    self.handle_client_event(token, event)?;
                }
            }
        }
        Ok(())
    }

    /// Find the session that owns a given window.
    fn session_of_window(&self, wid: WindowId) -> Option<SessionId> {
        for s in self.sessions.values() {
            if s.windows.values().any(|wl| wl.window_id == wid) {
                return Some(s.id);
            }
        }
        None
    }

    /// Handle a mio event for a PTY master fd. This is the ONLY place PTY
    /// data is read (P0-1 fix: previously `poll_ptys` also read the same fd).
    fn handle_pty_event(&mut self, event: &Event) -> io::Result<()> {
        let token = event.token();

        let (fd, pane_id) = match self.pty_fds.get(&token).copied() {
            Some(v) => v,
            None => return Ok(()),
        };

        if event.is_error() || event.is_read_closed() {
            self.on_pane_gone(pane_id, fd);
            return Ok(());
        }

        if event.is_readable() {
            let mut buf = [0u8; 65536];
            loop {
                match nix::unistd::read(fd, &mut buf) {
                    Ok(0) => {
                        // EOF: the shell exited
                        loom_core::log_debug!(self.log, "pty", "EOF on pane={} fd={}", pane_id, fd);
                        self.on_pane_gone(pane_id, fd);
                        break;
                    }
                    Ok(n) => {
                        self.process_pty_data(pane_id, fd, &buf[..n]);
                    }
                    Err(nix::errno::Errno::EAGAIN) => break,
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(e) => {
                        loom_core::log_error!(self.log, "pty", "read error pane={} fd={}: {}", pane_id, fd, e);
                        self.on_pane_gone(pane_id, fd);
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// A PTY is gone (shell exited, error). Clean up the fd, parser and
    /// notify attached clients. The session itself stays alive (P0-6).
    fn on_pane_gone(&mut self, pane_id: PaneId, fd: RawFd) {
        // Find the window to know which session to notify.
        let wid = self
            .windows
            .iter()
            .find(|(_, w)| w.panes.contains_key(&pane_id))
            .map(|(&id, _)| id);

        // Remove the pty registration.
        let token = self.pty_fds.iter().find(|(_, (_, p))| *p == pane_id).map(|(t, _)| *t);
        if let Some(t) = token {
            let mut source = SourceFd(&fd);
            let _ = self.poll.registry().deregister(&mut source);
            self.pty_fds.remove(&t);
        }
        unsafe {
            nix::libc::close(fd);
        }
        self.parsers.remove(&pane_id);

        // Mark the pane as dead.
        if let Some(wid) = wid {
            if let Some(window) = self.windows.get_mut(&wid) {
                if let Some(pane) = window.panes.get_mut(&pane_id) {
                    pane.fd = None;
                    pane.pid = None;
                }
            }
        }

        // Notify attached clients of this session that the pane exited.
        if let Some(wid) = wid {
            if let Some(sid) = self.session_of_window(wid) {
                let tokens: Vec<Token> = self
                    .clients
                    .iter()
                    .filter(|(_, c)| c.session_id == Some(sid) && c.attached)
                    .map(|(t, _)| *t)
                    .collect();
                for t in tokens {
                    let _ = self.send_to(t, &Message::Exited);
                }
            }
        }
    }

    /// Parse PTY output with the pane's persistent parser, write DSR/DA
    /// responses back to the shell, and broadcast a redraw to attached clients.
    fn process_pty_data(&mut self, pane_id: PaneId, fd: RawFd, data: &[u8]) {
        let wid = match self
            .windows
            .iter()
            .find(|(_, w)| w.panes.contains_key(&pane_id))
            .map(|(&id, _)| id)
        {
            Some(id) => id,
            None => return,
        };
        let sid = self.session_of_window(wid);

        loom_core::log_debug!(
            self.log,
            "pty_data",
            "processing {} bytes for pane={}",
            data.len(),
            pane_id
        );

        // Parse into the pane's screen using the persistent parser (P0-2, P0-3).
        // Also surface OSC title (B7), BEL (B6) and the dirty flag from the
        // parsed data.
        let (response, title, bell, dirty) = {
            let mut resp = Vec::new();
            let mut title = String::new();
            let mut bell = false;
            let mut dirty = false;
            if let Some(window) = self.windows.get_mut(&wid) {
                if let Some(pane) = window.panes.get_mut(&pane_id) {
                    if let Some(parser) = self.parsers.get_mut(&pane_id) {
                        parser.parse_buf(&mut pane.screen, data);
                        resp = parser.take_response();
                        bell = parser.take_bell();
                        dirty = parser.take_dirty();
                    }
                    if !pane.screen.title.is_empty() {
                        title = pane.screen.title.clone();
                    }
                }
            }
            (resp, title, bell, dirty)
        };

        // Write DSR/DA responses back to the PTY master (P0-5).
        if !response.is_empty() {
            let bfd = unsafe { BorrowedFd::borrow_raw(fd) };
            let _ = nix::unistd::write(&bfd, &response);
        }

        // (B7) OSC 0/2 title: surface on the window so the status line shows it.
        let title_present = !title.is_empty();
        if title_present {
            if let Some(w) = self.windows.get_mut(&wid) {
                w.name = title;
            }
        }

        // (B6) BEL: flag the window so the status line can mark it as alerted.
        if bell {
            if let Some(w) = self.windows.get_mut(&wid) {
                w.flags |= WINDOW_BELL;
            }
            if let Some(sid) = sid {
                if let Some(s) = self.sessions.get_mut(&sid) {
                    for wl in s.windows.values_mut() {
                        if wl.window_id == wid {
                            wl.flags |= WINLINK_BELL;
                        }
                    }
                }
            }
        }

        // Redraw for every attached client of this session. Skip when nothing
        // visible changed (query-only sequences like DSR/DA produce no screen
        // output, and the status line is only affected by title/bell changes).
        if dirty || bell || title_present {
            if let Some(sid) = sid {
                self.broadcast_redraw(sid, wid, false);
            }
        }
    }

    /// Send a redraw to every client attached to `sid`.
    /// `full=true` forces a complete screen clear (used on attach/resize).
    fn broadcast_redraw(&mut self, sid: SessionId, wid: WindowId, full: bool) {
        let tokens: Vec<Token> = self
            .clients
            .iter()
            .filter(|(_, c)| c.session_id == Some(sid) && c.attached)
            .map(|(t, _)| *t)
            .collect();
        for token in tokens {
            self.redraw_for_client(token, wid, full);
        }
    }

    fn redraw_for_client(&mut self, token: Token, wid: WindowId, full: bool) {
        // Ensure the client has a Tty of the right size; optionally reset it.
        {
            let client = match self.clients.get_mut(&token) {
                Some(c) => c,
                None => return,
            };
            let (sx, sy) = client.pending_size.unwrap_or((80, 24));
            if client.tty.is_none() {
                client.tty = Some(Tty::new(sx, sy));
            }
            if full {
                if let Some(tty) = client.tty.as_mut() {
                    tty.invalidate();
                }
            }
        }

        let data = {
            let window = match self.windows.get(&wid) {
                Some(w) => w,
                None => return,
            };
            let client = match self.clients.get_mut(&token) {
                Some(c) => c,
                None => return,
            };
            let mut out = Vec::new();
            if let Some(tty) = client.tty.as_mut() {
                if full {
                    redraw::redraw_window(tty, window);
                } else {
                    redraw::redraw_update(tty, window);
                }
                // (B3) Status line on the bottom row. Drawn before the final
                // cursor positioning so the hardware cursor lands in the
                // content area.
                let sid = client.session_id.unwrap_or(0);
                let segments = status_segments(&self.sessions, &self.windows, sid, wid);
                redraw::draw_status_line(tty, window, &segments);
                redraw::position_cursor(tty, window);
                out = tty.take_output();
            }
            out
        };

        if !data.is_empty() {
            let _ = self.send_to(token, &Message::ScreenUpdate { data });
        }
    }

    /// Create a pane with a spawned shell process and register its PTY with
    /// the event loop (P0-8: also used by split-window / new-window).
    fn spawn_pane(&mut self, wid: WindowId, sx: u32, sy: u32, cwd: &str) -> Option<PaneId> {
        let pane_id = {
            let window = self.windows.get_mut(&wid)?;
            window.create_pane(sx, sy)
        };
        loom_core::log_debug!(
            self.log,
            "spawn",
            "pane_id={}, wid={}, cwd={}",
            pane_id, wid, cwd
        );

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        match spawner::spawn_pty(&[shell.clone()], cwd, sx, sy) {
            Ok((child_pid, master_fd)) => {
                let pid = child_pid.as_raw() as u32;
                loom_core::log_info!(
                    self.log,
                    "spawn",
                    "spawn_pty ok: pid={}, fd={}",
                    child_pid, master_fd
                );

                // Make the master fd non-blocking for the mio event loop.
                let _ = nix::fcntl::fcntl(
                    master_fd,
                    nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
                );

                // Register with the event loop — the single reader path.
                let pty_token = Token(PTY_BASE + self.next_pty_token);
                self.next_pty_token += 1;
                let mut source = SourceFd(&master_fd);
                if self
                    .poll
                    .registry()
                    .register(&mut source, pty_token, Interest::READABLE)
                    .is_ok()
                {
                    self.pty_fds.insert(pty_token, (master_fd, pane_id));
                    loom_core::log_info!(
                        self.log,
                        "spawn",
                        "PTY registered token={:?}",
                        pty_token
                    );
                } else {
                    loom_core::log_error!(self.log, "spawn", "failed to register PTY fd");
                }

                // Persistent parser for this pane (P0-2).
                self.parsers.insert(pane_id, Parser::new());

                // Record process info on the pane.
                if let Some(window) = self.windows.get_mut(&wid) {
                    let window_opts = window.options.clone();
                    if let Some(pane) = window.panes.get_mut(&pane_id) {
                        pane.fd = Some(master_fd);
                        pane.pid = Some(pid);
                        pane.shell = shell;
                        pane.cwd = cwd.to_string();
                        pane.options.set_parent(window_opts);
                    }
                }
            }
            Err(e) => {
                loom_core::log_error!(self.log, "spawn", "spawn_pty FAILED: {}", e);
            }
        }
        Some(pane_id)
    }

    /// Kill a pane's process, close its PTY, and deregister it.
    fn close_pane_process(&mut self, pane_id: PaneId, pid: Option<u32>, fd: Option<RawFd>) {
        if let Some(pid) = pid {
            kill_process_group(pid);
        }
        if let Some(fd) = fd {
            let token = self
                .pty_fds
                .iter()
                .find(|(_, (f, p))| *f == fd && *p == pane_id)
                .map(|(t, _)| *t);
            if let Some(t) = token {
                let mut source = SourceFd(&fd);
                let _ = self.poll.registry().deregister(&mut source);
                self.pty_fds.remove(&t);
            }
            unsafe {
                nix::libc::close(fd);
            }
        }
        self.parsers.remove(&pane_id);
    }

    /// Kill a whole window: every pane's process + PTY.
    fn kill_window(&mut self, wid: WindowId) {
        let panes: Vec<(PaneId, Option<u32>, Option<RawFd>)> =
            self.windows
                .get(&wid)
                .map(|w| w.panes.values().map(|p| (p.id, p.pid, p.fd)).collect())
                .unwrap_or_default();
        for (pane_id, pid, fd) in panes {
            self.close_pane_process(pane_id, pid, fd);
        }
        self.windows.remove(&wid);
    }

    /// Kill a whole session: every window's panes, then the session.
    fn kill_session(&mut self, sid: SessionId) {
        let window_ids: Vec<WindowId> = self
            .sessions
            .get(&sid)
            .map(|s| s.windows.values().map(|wl| wl.window_id).collect())
            .unwrap_or_default();
        for wid in window_ids {
            self.kill_window(wid);
        }
        self.sessions.remove(&sid);
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
            tty: None,
            tty_initialized: false,
        };

        self.clients.insert(token, client);
        Ok(token)
    }

    fn handle_accept(&mut self) -> io::Result<()> {
        loom_core::log_debug!(self.log, "accept", "handling accept");
        let listener = match self.listener.as_ref() {
            Some(l) => l,
            None => return Ok(()),
        };

        loop {
            let std_stream = match listener.accept() {
                Ok((stream, _addr)) => stream,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            std_stream.set_nonblocking(true).map_err(|e| {
                io::Error::new(io::ErrorKind::Other, format!("set_nonblocking: {}", e))
            })?;
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
                tty: None,
                tty_initialized: false,
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

    fn handle_client_event(&mut self, token: Token, event: &Event) -> io::Result<()> {
        if event.is_error() || event.is_read_closed() || event.is_write_closed() {
            // Client socket went away. The session (and its PTYs) stay alive.
            loom_core::log_debug!(self.log, "client", "client {} disconnected", token.0);
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
                self.handle_command(token, &argv)?;
            }
            Message::Detach => {
                // Detach: the client leaves, but the session keeps running (P0-6).
                loom_core::log_debug!(self.log, "dispatch", "Detach from token={:?}", token);
                if let Some(client) = self.clients.get_mut(&token) {
                    client.session_id = None;
                    client.attached = false;
                }
                self.send_to(token, &Message::Exit)?;
            }
            Message::Resize { sx, sy } => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.pending_size = Some((sx, sy));
                }
                if let Some(sid) = self.clients.get(&token).and_then(|c| c.session_id) {
                    if let Some(session) = self.sessions.get(&sid) {
                        if let Some(wl) = session.current_winlink() {
                            let wid = wl.window_id;
                            if let Some(window) = self.windows.get_mut(&wid) {
                                // (B3) the status row is reserved: content height
                                // is the terminal height minus STATUS_ROWS.
                                layout::layout_resize(window, sx, content_sy(sy));
                            }
                            // Notify every PTY in the window of the new size.
                            let sizes: Vec<(RawFd, u32, u32)> = self
                                .windows
                                .get(&wid)
                                .map(|w| {
                                    w.panes
                                        .values()
                                        .filter_map(|p| p.fd.map(|fd| (fd, p.sx, p.sy)))
                                        .collect()
                                })
                                .unwrap_or_default();
                            for (fd, px, py) in sizes {
                                set_pty_size(fd, px, py);
                            }
                            self.broadcast_redraw(sid, wid, true);
                        }
                    }
                }
            }
            Message::AttachSession => {
                loom_core::log_debug!(self.log, "dispatch", "AttachSession from token={:?}", token);
                // The PTY was registered at spawn time; here we only set up
                // the client's rendering state and push an initial full redraw.
                let sid = self.clients.get(&token).and_then(|c| c.session_id);
                let wid = sid
                    .and_then(|sid| self.sessions.get(&sid))
                    .and_then(|s| s.current_winlink())
                    .map(|wl| wl.window_id);

                if let Some(_sid) = sid {
                    if let Some(client) = self.clients.get_mut(&token) {
                        let (sx, sy) = client.pending_size.unwrap_or((80, 24));
                        if client.tty.is_none() {
                            client.tty = Some(Tty::new(sx, sy));
                        }
                        client.attached = true;
                        client.tty_initialized = true;
                    }
                    // Initial full-screen render.
                    if let Some(wid) = wid {
                        self.redraw_for_client(token, wid, true);
                    }
                }
            }
            Message::KeyPress { key } => {
                loom_core::log_debug!(self.log, "dispatch", "KeyPress ({} bytes)", key.len());
                if let Some(client) = self.clients.get(&token) {
                    if let Some(sid) = client.session_id {
                        // Resolve target pane ids before taking a mutable
                        // borrow; copy-mode routing decides whether the key
                        // reaches the PTY at all.
                        let target: Option<(WindowId, PaneId, bool)> =
                            self.sessions.get(&sid).and_then(|session| {
                                session
                                    .current_winlink()
                                    .and_then(|wl| {
                                        let wid = wl.window_id;
                                        self.windows.get(&wid).and_then(|window| {
                                            window
                                                .active_pane_id
                                                .and_then(|pid| {
                                                    window
                                                        .panes
                                                        .get(&pid)
                                                        .map(|p| (wid, pid, p.copy.active))
                                                })
                                        })
                                    })
                            });
                        match target {
                            Some((wid, pid, in_copy_mode)) => {
                                if in_copy_mode {
                                    self.copy_mode_key(sid, wid, pid, &key);
                                } else if let Some(pane) =
                                    self.windows.get_mut(&wid).and_then(|w| w.panes.get_mut(&pid))
                                {
                                    if let Some(pfd) = pane.fd {
                                        let bfd =
                                            unsafe { BorrowedFd::borrow_raw(pfd) };
                                        let _ = nix::unistd::write(&bfd, &key);
                                    }
                                }
                            }
                            None => {}
                        }
                    }
                }
            }
            Message::Mouse { button, sx, sy, release } => {
                self.handle_mouse_event(token, button, sx, sy, release);
            }
            Message::Exit => {
                // Client is going away. The session persists (P0-6).
                loom_core::log_debug!(self.log, "dispatch", "Exit from token={:?}", token);
                self.clients.remove(&token);
            }
            _ => {}
        }
        Ok(())
    }

    /// Run an interactive command (new-session, split-window, ...).
    ///
    /// Dispatches through a static command registry instead of a monolithic
    /// match (B2). Adding a command means adding one `cmd_*` method and one
    /// registry entry — no edits to this method.
    fn handle_command(&mut self, token: Token, argv: &[String]) -> io::Result<()> {
        if argv.is_empty() {
            return Ok(());
        }
        let name = argv[0].as_str();
        let args = &argv[1..];
        match Self::command_registry().get(name) {
            Some(&handler) => handler(self, token, args),
            None => {
                loom_core::log_info!(
                    self.log,
                    "cmd",
                    "unknown command: {}",
                    name
                );
                Ok(())
            }
        }
    }

    /// Command dispatch table: maps command names and aliases to handlers.
    /// Built once, lazily, via `OnceLock`.
    fn command_registry() -> &'static HashMap<&'static str, CommandHandler> {
        static REGISTRY: OnceLock<HashMap<&'static str, CommandHandler>> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            let mut m: HashMap<&'static str, CommandHandler> = HashMap::new();
            // Sessions
            m.insert("new-session", Self::cmd_new_session);
            m.insert("new", Self::cmd_new_session);
            m.insert("kill-session", Self::cmd_kill_session);
            m.insert("list-sessions", Self::cmd_list_sessions);
            m.insert("ls", Self::cmd_list_sessions);
            m.insert("select-session", Self::cmd_select_session);
            m.insert("attach-session", Self::cmd_select_session);
            m.insert("attach", Self::cmd_select_session);
            // Windows
            m.insert("new-window", Self::cmd_new_window);
            m.insert("neww", Self::cmd_new_window);
            m.insert("kill-window", Self::cmd_kill_window);
            m.insert("select-window", Self::cmd_select_window);
            m.insert("list-windows", Self::cmd_list_windows);
            m.insert("lsw", Self::cmd_list_windows);
            // Panes
            m.insert("split-window", Self::cmd_split_window);
            m.insert("split", Self::cmd_split_window);
            m.insert("select-pane", Self::cmd_select_pane);
            m.insert("resize-pane", Self::cmd_resize_pane);
            m.insert("select-layout", Self::cmd_select_layout);
            m.insert("kill-pane", Self::cmd_kill_pane);
            m.insert("swap-pane", Self::cmd_swap_pane);
            m.insert("list-panes", Self::cmd_list_panes);
            m.insert("lsp", Self::cmd_list_panes);
            // Client / introspection
            m.insert("list-clients", Self::cmd_list_clients);
            m.insert("show-options", Self::cmd_show_options);
            m.insert("set-option", Self::cmd_set_option);
            m.insert("set", Self::cmd_set_option);
            m.insert("run-shell", Self::cmd_run_shell);
            m.insert("copy-mode", Self::cmd_copy_mode);
            m.insert("paste-buffer", Self::cmd_paste_buffer);
            m
        })
    }

    // ── Command handlers ──────────────────────────────────────────────
    // Each handler is a plain method; the registry in `command_registry`
    // routes by name. Args are the argv with the command name removed.

    fn cmd_new_session(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        loom_core::log_info!(self.log, "dispatch", "creating new session");
        let cwd = self
            .clients
            .get(&token)
            .map(|c| c.cwd.clone())
            .unwrap_or_else(|| "/tmp".to_string());
        let (sx, sy) = self
            .clients
            .get(&token)
            .and_then(|c| c.pending_size)
            .unwrap_or((80, 24));
        loom_core::log_debug!(self.log, "dispatch", "window size: {}x{}", sx, sy);

        let csy = content_sy(sy); // reserve the status row (B3)
        let global = self.global_options.clone();
        let mut session = Session::new(None, &cwd);
        session.options.set_parent(global.clone());
        let mut window = Window::new(sx, csy);
        window.options.set_parent(session.options.clone());
        let wid = window.id;
        let sid = session.id;

        self.windows.insert(wid, window);
        let _pane_id = self.spawn_pane(wid, sx, csy, &cwd);

        session.attach_window(0, wid);
        self.sessions.insert(sid, session);

        if let Some(client) = self.clients.get_mut(&token) {
            client.session_id = Some(sid);
        }
        Ok(())
    }

    fn cmd_kill_session(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                self.kill_session(sid);
                if let Some(c) = self.clients.get_mut(&token) {
                    c.session_id = None;
                }
            }
        }
        Ok(())
    }

    fn cmd_list_sessions(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        let names: Vec<String> = self
            .sessions
            .values()
            .map(|s| format!("{}: {} windows", s.name, s.windows.len()))
            .collect();
        let response = names.join("\n");
        self.send_to(
            token,
            &Message::Command {
                argc: 0,
                argv: vec![";".into(), response],
            },
        )?;
        Ok(())
    }

    fn cmd_select_session(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        // Point the client at a session so it can attach: explicit name (or
        // id), else the most recent session. Replies `OK` or `no-session`
        // so the client can fall back to `new-session`.
        let target = args.first().map(|s| s.as_str());
        let sid = match target {
            Some(t) => self
                .sessions
                .values()
                .find(|s| s.name == t || s.id.to_string() == t)
                .map(|s| s.id),
            None => self.sessions.keys().max().copied(),
        };
        match sid {
            Some(sid) => {
                if let Some(client) = self.clients.get_mut(&token) {
                    client.session_id = Some(sid);
                }
                self.send_to(
                    token,
                    &Message::Command {
                        argc: 0,
                        argv: vec![";".into(), "OK".into()],
                    },
                )?;
            }
            None => {
                self.send_to(
                    token,
                    &Message::Command {
                        argc: 0,
                        argv: vec![";".into(), "no-session".into()],
                    },
                )?;
            }
        }
        Ok(())
    }

    fn cmd_new_window(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        self.do_new_window(token)
    }

    fn cmd_split_window(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        let vertical = args.iter().any(|a| a == "-v");
        self.do_split_window(token, vertical)
    }

    fn cmd_kill_window(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(session) = self.sessions.get(&sid) {
                    if let Some(wl) = session.current_winlink() {
                        let wid = wl.window_id;
                        self.kill_window(wid);
                        if let Some(s) = self.sessions.get_mut(&sid) {
                            let idx = s
                                .windows
                                .iter()
                                .find(|(_, w)| w.window_id == wid)
                                .map(|(i, _)| *i);
                            if let Some(i) = idx {
                                s.detach_window(i);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn cmd_select_window(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        // Targets: "N", "-n" (next), "-p" (previous).
        let target = if args.iter().any(|a| a == "-n") {
            "next"
        } else if args.iter().any(|a| a == "-p") {
            "prev"
        } else {
            "index"
        };
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(session) = self.sessions.get(&sid) {
                    let count = i32::try_from(session.windows.len()).unwrap_or(0);
                    if count > 0 {
                        let cur = session.curw_idx.unwrap_or(0);
                        let idx = match target {
                            "next" => (cur + 1).rem_euclid(count),
                            "prev" => (cur - 1).rem_euclid(count),
                            _ => args.iter().find_map(|a| a.parse::<i32>().ok()).unwrap_or(cur),
                        };
                        let valid = session.windows.contains_key(&idx);
                        if valid {
                            if let Some(session) = self.sessions.get_mut(&sid) {
                                session.set_current_window(idx);
                                // Visiting a window clears its alert flags.
                                for wl in session.windows.values_mut() {
                                    wl.flags &= !WINLINK_ALERTFLAGS;
                                }
                            }
                            if let Some(wl) = self
                                .sessions
                                .get(&sid)
                                .and_then(|s| s.current_winlink())
                            {
                                let wid = wl.window_id;
                                if let Some(w) = self.windows.get_mut(&wid) {
                                    w.flags &= !(WINDOW_BELL | WINDOW_ACTIVITY);
                                }
                                self.broadcast_redraw(sid, wid, true);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn cmd_select_pane(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        // Targets: a pane index, or a direction -L/-R/-U/-D.
        let dir = match args.first().map(|s| s.as_str()) {
            Some("-L") => Some("L"),
            Some("-R") => Some("R"),
            Some("-U") => Some("U"),
            Some("-D") => Some("D"),
            _ => None,
        };
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(session) = self.sessions.get(&sid) {
                    if let Some(wl) = session.current_winlink() {
                        let wid = wl.window_id;
                        if let Some(window) = self.windows.get_mut(&wid) {
                            let target_pid = match dir {
                                Some(d) => pane_in_direction(window, d),
                                None => args
                                    .first()
                                    .and_then(|s| s.parse::<usize>().ok())
                                    .and_then(|n| window.pane_order.get(n).copied()),
                            };
                            if let Some(pid) = target_pid {
                                window.set_active_pane(pid);
                            }
                        }
                        self.broadcast_redraw(sid, wid, false);
                    }
                }
            }
        }
        Ok(())
    }

    fn cmd_resize_pane(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        // -Z toggles zoom (B1): the active pane fills the whole window.
        if !args.iter().any(|a| a == "-Z") {
            return Ok(());
        }
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(session) = self.sessions.get(&sid) {
                    if let Some(wl) = session.current_winlink() {
                        let wid = wl.window_id;
                        let pid = self.windows.get(&wid).and_then(|w| w.active_pane_id);
                        if let Some(pid) = pid {
                            let zoomed = {
                                let mut ok = false;
                                if let Some(w) = self.windows.get_mut(&wid) {
                                    ok = layout::layout_zoom(w, pid);
                                }
                                ok
                            };
                            if zoomed {
                                // Reflow the PTY to the new pane size.
                                if let Some((px, py, pfd)) = self
                                    .windows
                                    .get(&wid)
                                    .and_then(|w| w.panes.get(&pid))
                                    .map(|p| (p.sx, p.sy, p.fd))
                                {
                                    if let Some(fd) = pfd {
                                        set_pty_size(fd, px, py);
                                    }
                                }
                                self.broadcast_redraw(sid, wid, true);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// select-layout <name> — apply a named layout preset to the active
    /// window (tmux: even-horizontal, even-vertical, main-horizontal,
    /// main-vertical, tiled).
    fn cmd_select_layout(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        let name = match args.first() {
            Some(n) => n.as_str(),
            None => {
                self.send_to(
                    token,
                    &Message::Command {
                        argc: 0,
                        argv: vec![";".into(), "usage: select-layout <name>".into()],
                    },
                )?;
                return Ok(());
            }
        };
        let preset = match layout::LayoutPreset::parse(name) {
            Some(p) => p,
            None => {
                self.send_to(
                    token,
                    &Message::Command {
                        argc: 0,
                        argv: vec![";".into(), format!("unknown layout: {}", name)],
                    },
                )?;
                return Ok(());
            }
        };
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(wl) = self.sessions.get(&sid).and_then(|s| s.current_winlink()) {
                    let wid = wl.window_id;
                    // Snapshot pane sizes + fds to reflow PTYs after the layout.
                    let sizes: Vec<(RawFd, u32, u32)> = {
                        let mut out = Vec::new();
                        if let Some(w) = self.windows.get(&wid) {
                            for p in w.panes.values() {
                                if let Some(fd) = p.fd {
                                    out.push((fd, p.sx, p.sy));
                                }
                            }
                        }
                        out
                    };
                    let ok = if let Some(w) = self.windows.get_mut(&wid) {
                        layout::layout_preset(w, preset)
                    } else {
                        false
                    };
                    if ok {
                        // Reflow each PTY to its new pane size.
                        for (fd, px, py) in self
                            .windows
                            .get(&wid)
                            .map(|w| {
                                w.panes
                                    .values()
                                    .filter_map(|p| p.fd.map(|fd| (fd, p.sx, p.sy)))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or(sizes)
                        {
                            set_pty_size(fd, px, py);
                        }
                        self.broadcast_redraw(sid, wid, true);
                    }
                }
            }
        }
        Ok(())
    }

    fn cmd_kill_pane(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(session) = self.sessions.get(&sid) {
                    if let Some(wl) = session.current_winlink() {
                        let wid = wl.window_id;
                        if let Some(window) = self.windows.get(&wid) {
                            if let Some(pid) = window.active_pane_id {
                                let (ppid, pfd) = match window.panes.get(&pid) {
                                    Some(p) => (p.pid, p.fd),
                                    None => (None, None),
                                };
                                self.close_pane_process(pid, ppid, pfd);
                                if let Some(w) = self.windows.get_mut(&wid) {
                                    w.remove_pane(pid);
                                }
                                self.broadcast_redraw(sid, wid, false);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn cmd_list_windows(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(session) = self.sessions.get(&sid) {
                    let lines: Vec<String> = session
                        .windows
                        .iter()
                        .map(|(idx, wl)| {
                            let active = session.curw_idx == Some(*idx);
                            let marker = if active { "*" } else { " " };
                            let name = self
                                .windows
                                .get(&wl.window_id)
                                .map(|w| w.name.as_str())
                                .unwrap_or("unnamed");
                            format!("{}{}: {}", marker, idx, name)
                        })
                        .collect();
                    let response = lines.join("\n");
                    self.send_to(
                        token,
                        &Message::Command {
                            argc: 0,
                            argv: vec![";".into(), response],
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    fn cmd_list_panes(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(session) = self.sessions.get(&sid) {
                    if let Some(wl) = session.current_winlink() {
                        let wid = wl.window_id;
                        if let Some(window) = self.windows.get(&wid) {
                            let lines: Vec<String> = window
                                .pane_order
                                .iter()
                                .map(|pid| {
                                    let active = window.active_pane_id == Some(*pid);
                                    let marker = if active { "*" } else { " " };
                                    let pane = window.panes.get(pid);
                                    format!(
                                        "{}%{}: {} ({}x{})",
                                        marker,
                                        pid,
                                        pane.map(|p| p.shell.as_str()).unwrap_or("unknown"),
                                        pane.map(|p| p.sx).unwrap_or(0),
                                        pane.map(|p| p.sy).unwrap_or(0)
                                    )
                                })
                                .collect();
                            let response = lines.join("\n");
                            self.send_to(
                                token,
                                &Message::Command {
                                    argc: 0,
                                    argv: vec![";".into(), response],
                                },
                            )?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn cmd_swap_pane(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        // -s source-pane, -t destination-pane (by pane id).
        let mut src: Option<PaneId> = None;
        let mut dst: Option<PaneId> = None;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-s" if i + 1 < args.len() => {
                    if let Ok(pid) = args[i + 1].parse::<u32>() {
                        src = Some(pid);
                    }
                    i += 2;
                }
                "-t" if i + 1 < args.len() => {
                    if let Ok(pid) = args[i + 1].parse::<u32>() {
                        dst = Some(pid);
                    }
                    i += 2;
                }
                _ => i += 1,
            }
        }
        if let (Some(s), Some(d)) = (src, dst) {
            if let Some(client) = self.clients.get(&token) {
                if let Some(sid) = client.session_id {
                    if let Some(session) = self.sessions.get(&sid) {
                        if let Some(wl) = session.current_winlink() {
                            let wid = wl.window_id;
                            let swapped = {
                                let window = match self.windows.get_mut(&wid) {
                                    Some(w) => w,
                                    None => return Ok(()),
                                };
                                let (sc, dc) = match (
                                    window.panes.get(&s).and_then(|p| p.layout_cell),
                                    window.panes.get(&d).and_then(|p| p.layout_cell),
                                ) {
                                    (Some(sc), Some(dc)) => (sc, dc),
                                    _ => return Ok(()),
                                };
                                let tmp = window.cells[sc].pane_id;
                                window.cells[sc].pane_id = window.cells[dc].pane_id;
                                window.cells[dc].pane_id = tmp;
                                if let Some(p) = window.panes.get_mut(&s) {
                                    p.layout_cell = Some(dc);
                                }
                                if let Some(p) = window.panes.get_mut(&d) {
                                    p.layout_cell = Some(sc);
                                }
                                layout::fix_layout_panes(window);
                                true
                            };
                            if swapped {
                                self.broadcast_redraw(sid, wid, false);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn cmd_list_clients(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        let lines: Vec<String> = self
            .clients
            .iter()
            .map(|(t, c)| {
                let sid = c.session_id.map(|id| id.to_string()).unwrap_or_else(|| "-".to_string());
                format!(
                    "client {:?} session={} term={} attached={}",
                    t,
                    sid,
                    c.term_name,
                    c.attached
                )
            })
            .collect();
        let response = lines.join("\n");
        self.send_to(
            token,
            &Message::Command {
                argc: 0,
                argv: vec![";".into(), response],
            },
        )?;
        Ok(())
    }

    /// show-options [-g|-s|-w|-p] — list options at a scope.
    ///
    /// Defaults to session scope for the requesting client. `-g` shows the
    /// global table, `-s` session, `-w` window, `-p` the active pane.
    fn cmd_show_options(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        let target = if args.contains(&"-g".to_string()) {
            Scope::Global
        } else if args.contains(&"-p".to_string()) {
            Scope::Pane
        } else if args.contains(&"-w".to_string()) {
            Scope::Window
        } else {
            Scope::Session
        };
        let opts: Option<&Options> = match target {
            Scope::Global => Some(&self.global_options),
            Scope::Session => self
                .clients
                .get(&token)
                .and_then(|c| c.session_id)
                .and_then(|sid| self.sessions.get(&sid))
                .map(|s| &s.options),
            Scope::Window => self
                .clients
                .get(&token)
                .and_then(|c| c.session_id)
                .and_then(|sid| self.sessions.get(&sid))
                .and_then(|s| s.current_winlink())
                .and_then(|wl| self.windows.get(&wl.window_id))
                .map(|w| &w.options),
            Scope::Pane => self
                .clients
                .get(&token)
                .and_then(|c| c.session_id)
                .and_then(|sid| self.sessions.get(&sid))
                .and_then(|s| s.current_winlink())
                .and_then(|wl| self.windows.get(&wl.window_id))
                .and_then(|w| w.active_pane_id)
                .and_then(|pid| self.windows.values().find_map(|w| w.panes.get(&pid)))
                .map(|p| &p.options),
        };
        let response = match opts {
            Some(o) => o
                .iter()
                .map(|e| format!("{} \"{:?}\"", e.name, e.value))
                .collect::<Vec<_>>()
                .join("\n"),
            None => String::new(),
        };
        self.send_to(
            token,
            &Message::Command {
                argc: 0,
                argv: vec![";".into(), response],
            },
        )?;
        Ok(())
    }

    /// set-option [-g|-s|-w|-p] <name> <value> — set an option at a scope.
    /// `-g` sets the server global table; otherwise the requested client's
    /// session/window/pane options (panes inherit from the window).
    fn cmd_set_option(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        let mut target = Scope::Session;
        let mut rest = args;
        if rest.first().map(|s| s.starts_with('-')).unwrap_or(false) {
            match rest.first().map(String::as_str) {
                Some("-g") => target = Scope::Global,
                Some("-s") => target = Scope::Session,
                Some("-w") => target = Scope::Window,
                Some("-p") | Some("-o") => target = Scope::Pane,
                _ => {}
            }
            rest = &rest[1..];
        }
        if rest.len() < 2 {
            self.send_to(
                token,
                &Message::Command {
                    argc: 0,
                    argv: vec![";".into(), "usage: set-option [-gsw] name value".into()],
                },
            )?;
            return Ok(());
        }
        let name = rest[0].clone();
        let value = rest[1].clone();
        let opts: Option<&mut Options> = match target {
            Scope::Global => Some(&mut self.global_options),
            Scope::Session => self
                .clients
                .get(&token)
                .and_then(|c| c.session_id)
                .and_then(|sid| self.sessions.get_mut(&sid))
                .map(|s| &mut s.options),
            Scope::Window => self
                .clients
                .get(&token)
                .and_then(|c| c.session_id)
                .and_then(|sid| self.sessions.get_mut(&sid))
                .and_then(|s| s.current_winlink().map(|wl| wl.window_id))
                .and_then(|wid| self.windows.get_mut(&wid))
                .map(|w| &mut w.options),
            Scope::Pane => {
                let target: Option<(WindowId, PaneId)> = self
                    .clients
                    .get(&token)
                    .and_then(|c| c.session_id)
                    .and_then(|sid| self.sessions.get(&sid))
                    .and_then(|s| s.current_winlink().map(|wl| wl.window_id))
                    .and_then(|wid| {
                        self.windows
                            .get(&wid)
                            .and_then(|w| w.active_pane_id.map(|pid| (wid, pid)))
                    });
                match target {
                    Some((wid, pid)) => self
                        .windows
                        .get_mut(&wid)
                        .and_then(|w| w.panes.get_mut(&pid))
                        .map(|p| &mut p.options),
                    _ => None,
                }
            }
        };
        if let Some(o) = opts {
            let _ = o.set_value(&name, &value);
        }
        Ok(())
    }

    /// Run a shell command and send its combined output back to the caller.
    /// Basic form for B2; full control-mode semantics are P2.
    fn cmd_run_shell(&mut self, token: Token, args: &[String]) -> io::Result<()> {
        if args.is_empty() {
            self.send_to(
                token,
                &Message::Command {
                    argc: 0,
                    argv: vec![";".into(), "usage: run-shell <command>".into()],
                },
            )?;
            return Ok(());
        }
        let cmd = args.join(" ");
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            .output();
        let response = match output {
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                let err = String::from_utf8_lossy(&o.stderr);
                if !err.is_empty() {
                    s.push_str("\n");
                    s.push_str(&err);
                }
                s
            }
            Err(e) => format!("run-shell failed: {}", e),
        };
        self.send_to(
            token,
            &Message::Command {
                argc: 0,
                argv: vec![";".into(), response],
            },
        )?;
        Ok(())
    }

    /// B4: enter copy-mode on the active pane.
    fn cmd_copy_mode(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        let token = match self.clients.get(&token) {
            Some(c) if c.session_id.is_some() => token,
            _ => return Ok(()),
        };
        let sid = self.clients.get(&token).unwrap().session_id.unwrap();
        let wid = match self.sessions.get(&sid).and_then(|s| s.current_winlink()) {
            Some(wl) => wl.window_id,
            None => return Ok(()),
        };
        if let Some(window) = self.windows.get_mut(&wid) {
            if let Some(pid) = window.active_pane_id {
                if let Some(pane) = window.panes.get_mut(&pid) {
                    pane.copy.enter(pane.sx, pane.sy);
                }
            }
        }
        self.broadcast_redraw(sid, wid, false);
        Ok(())
    }

    /// B4: paste the global paste buffer into the active pane's PTY.
    fn cmd_paste_buffer(&mut self, token: Token, _args: &[String]) -> io::Result<()> {
        let token = match self.clients.get(&token) {
            Some(c) if c.session_id.is_some() => token,
            _ => return Ok(()),
        };
        let sid = self.clients.get(&token).unwrap().session_id.unwrap();
        let buf = self.paste_buffer.clone();
        if buf.is_empty() {
            return Ok(());
        }
        let wid = match self.sessions.get(&sid).and_then(|s| s.current_winlink()) {
            Some(wl) => wl.window_id,
            None => return Ok(()),
        };
        if let Some(window) = self.windows.get(&wid) {
            if let Some(pid) = window.active_pane_id {
                if let Some(pane) = window.panes.get(&pid) {
                    if let Some(pfd) = pane.fd {
                        let bfd = unsafe { BorrowedFd::borrow_raw(pfd) };
                        let _ = nix::unistd::write(&bfd, buf.as_bytes());
                    }
                }
            }
        }
        Ok(())
    }

    /// B4: process one keypress for a pane in copy-mode. The key never
    /// reaches the PTY; the view/selection updates instead.
    fn copy_mode_key(&mut self, sid: SessionId, wid: WindowId, pid: PaneId, key: &[u8]) {
        if self.copy_mode_step(wid, pid, key) {
            self.broadcast_redraw(sid, wid, false);
        }
    }

    /// Advance the copy-mode state machine for one key. Returns true when
    /// the view or selection changed (a redraw is needed).
    fn copy_mode_step(&mut self, wid: WindowId, pid: PaneId, key: &[u8]) -> bool {
        let (redraw, yank): (bool, Option<String>) = {
            let window = match self.windows.get_mut(&wid) {
                Some(w) => w,
                None => return false,
            };
            let pane = match window.panes.get_mut(&pid) {
                Some(p) => p,
                None => return false,
            };
            if !pane.copy.active {
                return false;
            }
            step_copy_mode(pane, key)
        };
        if let Some(text) = yank {
            self.paste_buffer = text;
        }
        redraw
    }

    /// Handle a decoded mouse event (B5).
    ///
    /// Coordinates are the 1-based client terminal cell. Left press focuses
    /// the pane under the cursor (or selects the window when the status line
    /// is clicked). Wheel events scroll copy-mode / history on the pane under
    /// the cursor. Clicking a window in the status bar selects it.
    fn handle_mouse_event(&mut self, token: Token, button: u32, sx: u32, sy: u32, release: bool) {
        if release {
            return;
        }
        let sid = match self.clients.get(&token).and_then(|c| c.session_id) {
            Some(s) => s,
            None => return,
        };
        let wid = match self.sessions.get(&sid).and_then(|s| s.current_winlink()) {
            Some(wl) => wl.window_id,
            None => return,
        };
        // Convert to 0-based client cell coordinates and locate the target
        // cell (content area or status line) within the window grid.
        let (mx, my) = (sx.saturating_sub(1), sy.saturating_sub(1));
        let window_sy = match self.windows.get(&wid) {
            Some(w) => w.sy,
            None => return,
        };

        // Status line: the bottom row(s) of the client terminal, at or below
        // the window's content height.
        if my >= window_sy {
            self.select_window_at_status(token, &wid, mx);
            return;
        }

        // Wheel events act on the pane under the cursor.
        if button & 0x3f == 64 || button & 0x3f == 65 {
            if let Some(pid) = self
                .windows
                .get(&wid)
                .and_then(|w| pane_at(w, mx, my))
            {
                if self.mouse_scroll_pane(&wid, pid, button & 0x3f == 64) {
                    self.broadcast_redraw(sid, wid, false);
                }
            }
            return;
        }

        // Left button press focuses the pane under the cursor.
        if button & 0x3f == 0 {
            let changed = {
                if let Some(window) = self.windows.get_mut(&wid) {
                    if let Some(pid) = pane_at(window, mx, my) {
                        if window.active_pane_id != Some(pid) {
                            window.set_active_pane(pid);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            if changed {
                self.broadcast_redraw(sid, wid, false);
            }
        }
    }

    /// Scroll a pane's copy-mode / history using the mouse wheel.
    /// `up` is true for wheel-up (scroll into history), false for wheel-down.
    /// If the pane is not in copy-mode, wheel-up focuses it, enters copy-mode
    /// and scrolls up. Returns true when a client redraw is needed.
    fn mouse_scroll_pane(&mut self, wid: &WindowId, pid: PaneId, up: bool) -> bool {
        // Focus the pane (mutates the window) before borrowing the pane.
        if up {
            if let Some(window) = self.windows.get_mut(wid) {
                if window.active_pane_id != Some(pid) {
                    window.set_active_pane(pid);
                }
            }
        }
        let window = match self.windows.get_mut(wid) {
            Some(w) => w,
            None => return false,
        };
        let pane = match window.panes.get_mut(&pid) {
            Some(p) => p,
            None => return false,
        };

        if up {
            if !pane.copy.active {
                pane.copy.enter(pane.sx, pane.sy);
            }
            let hsize = pane.screen.grid.hsize;
            if pane.copy.scroll < hsize {
                pane.copy.scroll += 1;
            }
            true
        } else if pane.copy.active {
            if pane.copy.scroll > 0 {
                pane.copy.scroll -= 1;
                true
            } else {
                pane.copy.exit();
                true
            }
        } else {
            false
        }
    }

    /// Select the window whose status-line entry is under column `mx`.
    fn select_window_at_status(&mut self, token: Token, wid: &WindowId, mx: u32) {
        let sid = match self.clients.get(&token).and_then(|c| c.session_id) {
            Some(s) => s,
            None => return,
        };
        // The status line in `status_segments` starts with the session-name
        // segment (" name "), so account for it before the window entries.
        let mut x: u32 = {
            if let Some(session) = self.sessions.get(&sid) {
                let name = if session.name.is_empty() {
                    sid.to_string()
                } else {
                    session.name.clone()
                };
                format!(" {} ", name).chars().count() as u32
            } else {
                0
            }
        };
        let mut target: Option<(i32, WindowId)> = None;
        if let Some(session) = self.sessions.get(&sid) {
            for (idx, wl) in &session.windows {
                let name = window_display_name(&self.windows, wl.window_id);
                let cell = format!(" {}:{} ", idx, name);
                let width = cell.chars().count() as u32;
                if mx >= x && mx < x + width {
                    target = Some((*idx, wl.window_id));
                    break;
                }
                x += width;
            }
        }
        if let Some((idx, target_wid)) = target {
            if let Some(session) = self.sessions.get_mut(&sid) {
                session.set_current_window(idx);
            }
            // Redraw for the requesting client at the newly-selected window.
            if *wid != target_wid {
                let _ = self.redraw_for_client(token, target_wid, true);
            }
            self.broadcast_redraw(sid, target_wid, false);
        }
    }

    fn do_split_window(&mut self, token: Token, vertical: bool) -> io::Result<()> {
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(session) = self.sessions.get(&sid) {
                    if let Some(wl) = session.current_winlink() {
                        let wid = wl.window_id;

                        // Snapshot active pane info (ends borrows before mutation).
                        let (active_pane_id, cwd, (sx, sy)) = {
                            let window = match self.windows.get(&wid) {
                                Some(w) => w,
                                None => return Ok(()),
                            };
                            let active = match window.active_pane_id {
                                Some(a) => a,
                                None => return Ok(()),
                            };
                            let cwd = window
                                .panes
                                .get(&active)
                                .map(|p| p.cwd.clone())
                                .unwrap_or_else(|| "/".to_string());
                            let (sx, sy) = window
                                .panes
                                .get(&active)
                                .map(|p| (p.sx, p.sy))
                                .unwrap_or((window.sx, window.sy));
                            (active, cwd, (sx, sy))
                        };

                        // Split the layout (P0-8: the new pane gets a real shell).
                        let new_pane = self
                            .windows
                            .get_mut(&wid)
                            .and_then(|window| layout::layout_split_pane(window, active_pane_id, vertical));

                        if let Some(pid) = new_pane {
                            if let Some(window) = self.windows.get_mut(&wid) {
                                window.set_active_pane(pid);
                            }
                            self.spawn_pane_in(wid, pid, sx, sy, &cwd);
                        }
                        self.broadcast_redraw(sid, wid, false);
                    }
                }
            }
        }
        Ok(())
    }

    fn do_new_window(&mut self, token: Token) -> io::Result<()> {
        if let Some(client) = self.clients.get(&token) {
            if let Some(sid) = client.session_id {
                if let Some(session) = self.sessions.get(&sid) {
                    if let Some(wl) = session.current_winlink() {
                        let wid = wl.window_id;
                        let (sx, sy) = self
                            .windows
                            .get(&wid)
                            .map(|w| (w.sx, w.sy))
                            .unwrap_or((80, 24));
                        let cwd = self.clients.get(&token).map(|c| c.cwd.clone()).unwrap_or_else(|| "/".into());
                        let session_opts = self.sessions.get(&sid).map(|s| s.options.clone());
                        let mut window = Window::new(sx, sy);
                        if let Some(opts) = session_opts {
                            window.options.set_parent(opts);
                        } else {
                            window.options.set_parent(self.global_options.clone());
                        }
                        let new_wid = window.id;
                        self.windows.insert(new_wid, window);
                        self.spawn_pane(new_wid, sx, sy, &cwd);
                        if let Some(session) = self.sessions.get_mut(&sid) {
                            let next_idx = session.windows.keys().max().map(|i| i + 1).unwrap_or(0);
                            session.attach_window(next_idx, new_wid);
                            session.set_current_window(next_idx);
                        }
                        self.broadcast_redraw(sid, new_wid, true);
                    }
                }
            }
        }
        Ok(())
    }

    /// Spawn a shell into an already-created pane (used by split-window).
    fn spawn_pane_in(&mut self, wid: WindowId, pane_id: PaneId, sx: u32, sy: u32, cwd: &str) -> Option<RawFd> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        match spawner::spawn_pty(&[shell.clone()], cwd, sx, sy) {
            Ok((child_pid, master_fd)) => {
                let pid = child_pid.as_raw() as u32;
                let _ = nix::fcntl::fcntl(
                    master_fd,
                    nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
                );
                let pty_token = Token(PTY_BASE + self.next_pty_token);
                self.next_pty_token += 1;
                let mut source = SourceFd(&master_fd);
                if self
                    .poll
                    .registry()
                    .register(&mut source, pty_token, Interest::READABLE)
                    .is_ok()
                {
                    self.pty_fds.insert(pty_token, (master_fd, pane_id));
                }
                self.parsers.insert(pane_id, Parser::new());
                if let Some(window) = self.windows.get_mut(&wid) {
                    // Pane options inherit from the window's options.
                    let window_opts = window.options.clone();
                    if let Some(pane) = window.panes.get_mut(&pane_id) {
                        pane.fd = Some(master_fd);
                        pane.pid = Some(pid);
                        pane.shell = shell;
                        pane.cwd = cwd.to_string();
                        pane.options.set_parent(window_opts);
                    }
                }
                Some(master_fd)
            }
            Err(e) => {
                loom_core::log_error!(self.log, "spawn_in", "spawn_pty FAILED: {}", e);
                None
            }
        }
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

/// Set a PTY's window size via TIOCSWINSZ.
fn set_pty_size(fd: RawFd, sx: u32, sy: u32) {
    let ws = nix::libc::winsize {
        ws_row: sy as u16,
        ws_col: sx as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        nix::libc::ioctl(fd, nix::libc::TIOCSWINSZ, &ws);
    }
}

/// (B3) Build the status-line segments for session `sid`, window `wid`.
/// Left: session name. Middle: window list (active inverted, alerts starred).
fn status_segments(
    sessions: &HashMap<SessionId, Session>,
    windows: &HashMap<WindowId, Window>,
    sid: SessionId,
    wid: WindowId,
) -> Vec<(String, redraw::StatusStyle)> {
    let mut segs: Vec<(String, redraw::StatusStyle)> = Vec::new();
    let session = match sessions.get(&sid) {
        Some(s) => s,
        None => return segs,
    };

    let name = if session.name.is_empty() {
        sid.to_string()
    } else {
        session.name.clone()
    };
    segs.push((format!(" {} ", name), redraw::StatusStyle::Normal));

    let cur = session.curw_idx;
    for (idx, wl) in &session.windows {
        // Active = this client's current window (per-client view), falling
        // back to the session's current window index.
        let active = wl.window_id == wid || cur == Some(*idx);
        let alerted = wl.flags & WINLINK_ALERTFLAGS != 0 && !active;
        let wname = window_display_name(windows, wl.window_id);
        let prefix = if alerted { "*" } else { "" };
        let style = if active {
            redraw::StatusStyle::Active
        } else if alerted {
            redraw::StatusStyle::Alert
        } else {
            redraw::StatusStyle::Normal
        };
        segs.push((format!(" {}:{}{} ", idx, prefix, wname), style));
    }
    segs
}

/// Human-readable name for a window: OSC title, else the active pane's
/// shell basename, else "shell".
fn window_display_name(windows: &HashMap<WindowId, Window>, wid: WindowId) -> String {
    let window = match windows.get(&wid) {
        Some(w) => w,
        None => return "shell".into(),
    };
    if !window.name.is_empty() {
        return window.name.clone();
    }
    if let Some(pid) = window.active_pane_id {
        if let Some(pane) = window.panes.get(&pid) {
            if !pane.shell.is_empty() {
                if let Some(base) = pane.shell.rsplit('/').next() {
                    if !base.is_empty() {
                        return base.to_string();
                    }
                }
            }
        }
    }
    "shell".into()
}

/// B4: advance the copy-mode state machine for one keypress.
///
/// Returns `(redraw_needed, yank_text)`. `yank_text` is `Some` when the key
/// is `y` and a selection existed: the text is the yanked selection, and
/// copy mode is exited. Every key is consumed while copy mode is active, so
/// nothing reaches the PTY.
fn step_copy_mode(pane: &mut WindowPane, key: &[u8]) -> (bool, Option<String>) {
    let sx = pane.sx.max(1);
    let sy = pane.sy.max(1);
    let hsize = pane.screen.grid.hsize;
    let c = &mut pane.copy;
    let mut redraw = false;
    let mut yank: Option<String> = None;

    match key {
        b"q" | b"\x1b" => {
            c.exit();
            redraw = true;
        }
        b"h" | b"\x1b[D" => {
            if c.cx > 0 {
                c.cx -= 1;
                redraw = true;
            }
        }
        b"l" | b"\x1b[C" => {
            if c.cx + 1 < sx {
                c.cx += 1;
                redraw = true;
            }
        }
        b"k" | b"\x1b[A" => {
            if c.cy > 0 {
                c.cy -= 1;
                redraw = true;
            } else if c.scroll < hsize {
                c.scroll += 1;
                redraw = true;
            }
        }
        b"j" | b"\x1b[B" => {
            if c.cy + 1 < sy {
                c.cy += 1;
                redraw = true;
            } else if c.scroll > 0 {
                c.scroll -= 1;
                redraw = true;
            }
        }
        b" " => {
            // Page down.
            let before = c.scroll;
            c.scroll = c.scroll.saturating_sub(sy);
            if c.scroll != before {
                redraw = true;
            }
        }
        b"?" => {
            // Page up.
            let before = c.scroll;
            c.scroll = (c.scroll + sy).min(hsize);
            if c.scroll != before {
                redraw = true;
            }
        }
        b"0" => {
            if c.cx != 0 {
                c.cx = 0;
                redraw = true;
            }
        }
        b"$" => {
            if c.cx != sx - 1 {
                c.cx = sx - 1;
                redraw = true;
            }
        }
        b"G" => {
            if c.scroll != 0 || c.cy != sy - 1 {
                c.scroll = 0;
                c.cy = sy - 1;
                redraw = true;
            }
        }
        b"g" => {
            if c.last_g {
                c.last_g = false;
                if c.scroll != hsize || c.cy != 0 {
                    c.scroll = hsize;
                    c.cy = 0;
                    redraw = true;
                }
            } else {
                c.last_g = true;
            }
        }
        b"v" => {
            if c.visual {
                c.visual = false;
                c.sel_anchor = None;
            } else {
                c.visual = true;
                c.sel_anchor = Some(c.cursor_abs(hsize));
            }
            redraw = true;
        }
        b"y" => {
            // Yank the current selection into the paste buffer, then exit.
            if let Some((lo, hi)) = c.selection_range(hsize) {
                yank = Some(pane.screen.grid.extract_selection(lo, hi));
            }
            c.exit();
            redraw = true;
        }
        b"w" => {
            let abs = c.abs_line(c.cy, hsize);
            let (la, lc) = word_forward(&pane.screen.grid, abs, c.cx, sx);
            let (old_cy, old_cx) = (c.cy, c.cx);
            apply_word_jump(c, hsize, sy, la, lc);
            if (c.cy, c.cx) != (old_cy, old_cx) {
                redraw = true;
            }
        }
        b"b" => {
            let abs = c.abs_line(c.cy, hsize);
            let (la, lc) = word_backward(&pane.screen.grid, abs, c.cx, sx);
            let (old_cy, old_cx) = (c.cy, c.cx);
            apply_word_jump(c, hsize, sy, la, lc);
            if (c.cy, c.cx) != (old_cy, old_cx) {
                redraw = true;
            }
        }
        _ => {}
    }
    (redraw, yank)
}

/// Convert an absolute (line, col) target back to view coordinates, clamped
/// to the visible view window.
fn apply_word_jump(c: &mut CopyMode, hsize: u32, sy: u32, abs_line: u32, col: u32) {
    let view_row = abs_line.saturating_sub(hsize.saturating_sub(c.scroll));
    c.cy = view_row.min(sy.saturating_sub(1));
    c.cx = col;
}

/// Target for vi `w`: the first non-space at or after (line, x+1); when the
/// rest of the line is blank, the first column of the next line.
fn word_forward(grid: &Grid, line: u32, x: u32, sx: u32) -> (u32, u32) {
    let total = grid.total_lines();
    let mut l = line;
    let mut x = x + 1;
    let ch = |l: u32, x: u32| grid.get_cell(x, l).map(|c| c.data.to_char()).unwrap_or(' ');
    while l < total {
        while x < sx && ch(l, x) == ' ' {
            x += 1;
        }
        if x < sx {
            return (l, x);
        }
        l += 1;
        x = 0;
    }
    (line, sx.saturating_sub(1))
}

/// Target for vi `b`: the start of the word containing (line, x), or the
/// start of the previous word when (line, x) is a space.
fn word_backward(grid: &Grid, line: u32, x: u32, sx: u32) -> (u32, u32) {
    let ch = |l: u32, x: u32| grid.get_cell(x, l).map(|c| c.data.to_char()).unwrap_or(' ');
    let mut l = line;
    let mut x = x;
    loop {
        if x == 0 {
            if l == 0 {
                return (0, 0);
            }
            l -= 1;
            x = sx.saturating_sub(1);
            continue;
        }
        x -= 1;
        if ch(l, x) != ' ' {
            while x > 0 && ch(l, x - 1) != ' ' {
                x -= 1;
            }
            return (l, x);
        }
    }
}

/// Find the pane under a 0-based window-grid cell (mx, my), if any.
fn pane_at(window: &Window, mx: u32, my: u32) -> Option<PaneId> {
    for (pid, p) in &window.panes {
        let x = mx as i32 - p.xoff;
        let y = my as i32 - p.yoff;
        if x >= 0 && y >= 0 && x < p.sx as i32 && y < p.sy as i32 {
            return Some(*pid);
        }
    }
    None
}

/// Find the pane to select when moving in direction `dir` from the active
/// pane: the nearest pane strictly in that direction.
fn pane_in_direction(window: &Window, dir: &str) -> Option<PaneId> {
    let active = window.active_pane_id?;
    let a = window.panes.get(&active)?;
    let mut best: Option<(PaneId, i32)> = None;
    for (pid, p) in &window.panes {
        if *pid == active {
            continue;
        }
        let dx = p.xoff - a.xoff;
        let dy = p.yoff - a.yoff;
        let (ok, dist) = match dir {
            "L" => (dx < 0, -dx + dy.abs()),
            "R" => (dx > 0, dx + dy.abs()),
            "U" => (dy < 0, -dy + dx.abs()),
            "D" => (dy > 0, dy + dx.abs()),
            _ => (false, 0),
        };
        if ok && best.map(|(_, d)| dist < d).unwrap_or(true) {
            best = Some((*pid, dist));
        }
    }
    best.map(|(pid, _)| pid)
}

/// Kill a process and its group. The child called `setsid()` so its pid is
/// the process-group id; SIGKILL to the group takes down its children too.
fn kill_process_group(pid: u32) {
    let pid = pid as i32;
    unsafe {
        nix::libc::kill(-pid, nix::libc::SIGKILL);
        nix::libc::kill(pid, nix::libc::SIGKILL);
    }
    let mut status: i32 = 0;
    unsafe {
        nix::libc::waitpid(pid, &mut status, 0);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Kill all panes' processes before dropping.
        let panes: Vec<(PaneId, Option<u32>, Option<RawFd>)> = self
            .windows
            .values()
            .flat_map(|w| w.panes.values().map(|p| (p.id, p.pid, p.fd)))
            .collect();
        for (pane_id, pid, fd) in panes {
            self.close_pane_process(pane_id, pid, fd);
        }
        // Dropping the listener closes the socket fd; remove the socket file.
        self.listener = None;
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

    /// Build a server with one 80x24 window/pane, filled with two text
    /// lines, and enter copy mode on the pane.
    fn server_with_copy_pane() -> (Server, WindowId, PaneId) {
        use loom_core::grid_cell::GridCell;
        use loom_core::utf8::Utf8Data;

        let config = ServerConfig {
            socket_path: format!("/tmp/loom-copy-{}.sock", std::process::id()),
            socket_mode: 0o600,
        };
        let mut server = Server::new(config).unwrap();
        let mut window = Window::new(80, 24);
        let wid = window.id;
        let mut pane = WindowPane::new(wid, 80, 24);
        let pid = pane.id;
        for (i, ch) in "hello world".chars().enumerate() {
            pane.screen.grid.set_cell(i as u32, 0, &GridCell {
                data: Utf8Data::new(ch),
                ..GridCell::default_cell()
            });
        }
        for (i, ch) in "foo bar".chars().enumerate() {
            pane.screen.grid.set_cell(i as u32, 1, &GridCell {
                data: Utf8Data::new(ch),
                ..GridCell::default_cell()
            });
        }
        window.panes.insert(pid, pane);
        window.active_pane_id = Some(pid);
        window.pane_order.push_back(pid);
        server.windows.insert(wid, window);

        server
            .windows
            .get_mut(&wid)
            .unwrap()
            .panes
            .get_mut(&pid)
            .unwrap()
            .copy
            .enter(80, 24);
        (server, wid, pid)
    }

    #[test]
    fn test_copy_mode_move_and_yank() {
        let (mut server, wid, pid) = server_with_copy_pane();
        // From bottom-right, jump to top-left (gg + 0), select visually,
        // extend right 4 cells and yank: "hell".
        let _ = server.copy_mode_step(wid, pid, b"g");
        let _ = server.copy_mode_step(wid, pid, b"g");
        let _ = server.copy_mode_step(wid, pid, b"0");
        let _ = server.copy_mode_step(wid, pid, b"v");
        for _ in 0..4 {
            let _ = server.copy_mode_step(wid, pid, b"l");
        }
        let _ = server.copy_mode_step(wid, pid, b"y");
        assert_eq!(server.paste_buffer, "hell");
        // Yank exits copy mode.
        assert!(!server.windows.get(&wid).unwrap().panes.get(&pid).unwrap().copy.active);
    }

    #[test]
    fn test_copy_mode_scroll_and_quit() {
        let (mut server, wid, pid) = server_with_copy_pane();

        // Move to the top (gg). With no history, scrolling up further is a
        // no-op and must not request a redraw.
        let _ = server.copy_mode_step(wid, pid, b"g");
        let _ = server.copy_mode_step(wid, pid, b"g");
        assert!(!server.copy_mode_step(wid, pid, b"k"));
        // Quit returns to normal mode and triggers a redraw.
        assert!(server.copy_mode_step(wid, pid, b"q"));
        let pane = server.windows.get(&wid).unwrap().panes.get(&pid).unwrap();
        assert!(!pane.copy.active);
    }

    /// A window with two side-by-side panes (each 40x24) for hit-testing.
    fn server_with_two_panes() -> (Server, WindowId, PaneId, PaneId) {
        use loom_core::grid_cell::GridCell;
        use loom_core::utf8::Utf8Data;

        let config = ServerConfig {
            socket_path: format!("/tmp/loom-mouse-{}.sock", std::process::id()),
            socket_mode: 0o600,
        };
        let mut server = Server::new(config).unwrap();
        let mut window = Window::new(80, 24);
        let wid = window.id;
        let mut p1 = WindowPane::new(wid, 40, 24);
        let p1id = p1.id;
        p1.xoff = 0;
        p1.yoff = 0;
        for (i, ch) in "left".chars().enumerate() {
            p1.screen.grid.set_cell(i as u32, 0, &GridCell {
                data: Utf8Data::new(ch),
                ..GridCell::default_cell()
            });
        }
        let mut p2 = WindowPane::new(wid, 40, 24);
        let p2id = p2.id;
        p2.xoff = 40;
        p2.yoff = 0;
        window.panes.insert(p1id, p1);
        window.panes.insert(p2id, p2);
        window.active_pane_id = Some(p1id);
        window.pane_order.push_back(p1id);
        window.pane_order.push_back(p2id);
        server.windows.insert(wid, window);
        (server, wid, p1id, p2id)
    }

    #[test]
    fn test_pane_at_hit_testing() {
        let (server, wid, p1id, p2id) = server_with_two_panes();
        let w = server.windows.get(&wid).unwrap();
        assert_eq!(pane_at(w, 10, 12), Some(p1id));
        assert_eq!(pane_at(w, 50, 12), Some(p2id));
        // Below the content area (a status row) => no pane.
        assert_eq!(pane_at(w, 10, 24), None);
    }

    #[test]
    fn test_mouse_wheel_enters_and_exits_copy_mode() {
        let (mut server, wid, p1id, _p2id) = server_with_two_panes();
        // Wheel-up on the left pane: enters copy-mode and scrolls history.
        assert!(server.mouse_scroll_pane(&wid, p1id, true));
        let pane = server.windows.get(&wid).unwrap().panes.get(&p1id).unwrap();
        assert!(pane.copy.active);
        // Wheel-down on the same pane: back to the live screen, exits.
        assert!(server.mouse_scroll_pane(&wid, p1id, false));
        let pane = server.windows.get(&wid).unwrap().panes.get(&p1id).unwrap();
        assert!(!pane.copy.active);
    }

    /// B8: an options child inherits the defaults table and shadows with a
    /// local set; the parent is unchanged.
    #[test]
    fn test_options_scope_shadows_parent() {
        let mut global = Options::with_defaults();
        global.set_value("set-titles", "0");
        let window = Options::child_of(global);
        // Unset value resolves through the parent.
        assert_eq!(window.get_number("status-interval"), 15);
        // The window's own set shadows the global default.
        assert!(window.get_flag("set-titles") == false);
    }

    /// B8: `set-option -g` writes to the server global options.
    #[test]
    fn test_set_option_global_updates_server_options() {
        let config = ServerConfig {
            socket_path: format!("/tmp/loom-opt-{}.sock", std::process::id()),
            socket_mode: 0o600,
        };
        let mut server = Server::new(config).unwrap();
        assert_eq!(server.global_options.get_number("history-limit"), 2000);
        server.global_options.set_value("history-limit", "500");
        assert_eq!(server.global_options.get_number("history-limit"), 500);
    }

    /// Phase C: named layout presets redistribute pane geometry.
    #[test]
    fn test_layout_preset_even_horizontal() {
        let (mut server, wid, p1id, p2id) = server_with_two_panes();
        let w = server.windows.get_mut(&wid).unwrap();
        assert!(layout::layout_preset(w, layout::LayoutPreset::EvenHorizontal));
        // Two panes each get half the width (40), full height (24).
        assert_eq!(w.panes.get(&p1id).unwrap().sx, 40);
        assert_eq!(w.panes.get(&p1id).unwrap().xoff, 0);
        assert_eq!(w.panes.get(&p2id).unwrap().sx, 40);
        assert_eq!(w.panes.get(&p2id).unwrap().xoff, 40);
        assert_eq!(w.panes.get(&p2id).unwrap().yoff, 0);
    }

    /// Phase C: a named preset re-points pane.layout_cell and reflows.
    #[test]
    fn test_layout_preset_even_vertical() {
        let (mut server, wid, p1id, _p2id) = server_with_two_panes();
        let w = server.windows.get_mut(&wid).unwrap();
        assert!(layout::layout_preset(w, layout::LayoutPreset::EvenVertical));
        assert_eq!(w.panes.get(&p1id).unwrap().sy, 12);
        assert_eq!(w.panes.get(&p1id).unwrap().yoff, 0);
        assert_eq!(w.panes.get(&_p2id).unwrap().sy, 12);
        assert_eq!(w.panes.get(&_p2id).unwrap().yoff, 12);
        assert!(w.panes.get(&p1id).unwrap().layout_cell.is_some());
    }
}
