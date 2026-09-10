//! In-process end-to-end harness: drive a real `Server` without a PTY.
//!
//! The sandbox denies `posix_openpt`, so the pane's "PTY" is one end of a
//! `UnixStream::pair`. The server reads it exactly as it would a real PTY
//! (mio-registered, non-blocking), and the harness writes the bytes a shell
//! (e.g. eza) would emit. The client link is a second socketpair carried by a
//! real `loom_ipc::Peer`, so messages use the production framing. A minimal
//! VT emulator (`Vt`) replays the `ScreenUpdate` bytes so a test can assert
//! on what a user would actually see.
//!
//! This module is test-only; it never ships.

#![cfg(test)]

use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;

use loom_core::session::{PaneId, Session, SessionId, Window, WindowId, WindowPane};
use loom_ipc::message::Message;
use loom_ipc::peer::Peer;
use mio::{Interest, Token};

use crate::server::{ClientState, Server, ServerConfig, CLIENT_BASE, PTY_BASE};

// ── Minimal VT emulator ─────────────────────────────────────────────────

/// A small VT100 terminal emulator used to replay `ScreenUpdate` bytes.
/// Understands printing, CR/LF/BS/TAB, CUP/ED/EL and plain SGR (attributes
/// are ignored — we assert on text) plus wide characters (2 cells).
pub struct Vt {
    pub sx: u32,
    pub sy: u32,
    pub cells: Vec<Vec<char>>,
    pub cx: u32,
    pub cy: u32,
}

impl Vt {
    pub fn new(sx: u32, sy: u32) -> Self {
        Self {
            sx,
            sy,
            cells: vec![vec![' '; sx as usize]; sy as usize],
            cx: 0,
            cy: 0,
        }
    }

    pub fn feed(&mut self, data: &[u8]) {
        let mut i = 0usize;
        while i < data.len() {
            let b = data[i];
            match b {
                0x1b => {
                    if i + 1 < data.len() && data[i + 1] == b'[' {
                        let mut j = i + 2;
                        let start = j;
                        while j < data.len() && !(0x40..=0x7e).contains(&data[j]) {
                            j += 1;
                        }
                        if j >= data.len() {
                            break;
                        }
                        let params = String::from_utf8_lossy(&data[start..j]).to_string();
                        let fin = data[j];
                        self.csi(&params, fin);
                        i = j + 1;
                    } else if i + 1 < data.len() && data[i + 1] == b']' {
                        // OSC: skip to BEL or ST
                        let mut j = i + 2;
                        while j < data.len() && data[j] != 0x07 {
                            if data[j] == 0x1b && j + 1 < data.len() && data[j + 1] == b'\\' {
                                j += 1;
                                break;
                            }
                            j += 1;
                        }
                        i = (j + 1).min(data.len());
                    } else {
                        i += 2;
                    }
                }
                0x07 => i += 1,
                0x08 => {
                    self.cx = self.cx.saturating_sub(1);
                    i += 1;
                }
                0x09 => {
                    self.cx = ((self.cx / 8) + 1) * 8;
                    if self.cx >= self.sx {
                        self.cx = self.sx.saturating_sub(1);
                    }
                    i += 1;
                }
                0x0a | 0x0b | 0x0c => {
                    // Plain LF: looms emits CR/LF, so treat LF as down-only
                    // here (CR handled separately) like a real terminal.
                    self.cy = (self.cy + 1).min(self.sy.saturating_sub(1));
                    i += 1;
                }
                0x0d => {
                    self.cx = 0;
                    i += 1;
                }
                _ => {
                    let len = utf8_len(b);
                    if i + len > data.len() {
                        break;
                    }
                    let s = String::from_utf8_lossy(&data[i..i + len]).to_string();
                    if let Some(c) = s.chars().next() {
                        self.put(c);
                    }
                    i += len;
                }
            }
        }
    }

    fn put(&mut self, c: char) {
        if self.cx >= self.sx || self.cy >= self.sy {
            return;
        }
        let wide = is_wide(c);
        let x = self.cx as usize;
        let y = self.cy as usize;
        if y < self.cells.len() && x < self.cells[y].len() {
            self.cells[y][x] = c;
            if wide && x + 1 < self.cells[y].len() {
                self.cells[y][x + 1] = ' ';
            }
        }
        self.cx = (self.cx + if wide { 2 } else { 1 }).min(self.sx);
    }

    fn csi(&mut self, params: &str, fin: u8) {
        let p: Vec<i64> = params
            .trim_start_matches('?')
            .split(';')
            .map(|s| s.parse::<i64>().unwrap_or(0))
            .collect();
        let arg = |idx: usize, dflt: i64| -> i64 {
            if idx < p.len() && p[idx] != 0 {
                p[idx]
            } else {
                dflt
            }
        };
        match fin {
            b'H' | b'f' => {
                self.cy = ((arg(0, 1).max(1) - 1) as u32).min(self.sy.saturating_sub(1));
                self.cx = ((arg(1, 1).max(1) - 1) as u32).min(self.sx.saturating_sub(1));
            }
            b'A' => self.cy = self.cy.saturating_sub(arg(0, 1) as u32),
            b'B' => self.cy = (self.cy + arg(0, 1) as u32).min(self.sy.saturating_sub(1)),
            b'C' => self.cx = (self.cx + arg(0, 1) as u32).min(self.sx.saturating_sub(1)),
            b'D' => self.cx = self.cx.saturating_sub(arg(0, 1) as u32),
            b'G' => self.cx = ((arg(0, 1).max(1) - 1) as u32).min(self.sx.saturating_sub(1)),
            b'd' => self.cy = ((arg(0, 1).max(1) - 1) as u32).min(self.sy.saturating_sub(1)),
            b'J' => {
                if p.first().copied().unwrap_or(0) == 2 {
                    for row in self.cells.iter_mut() {
                        for cell in row.iter_mut() {
                            *cell = ' ';
                        }
                    }
                }
            }
            b'K' => {
                let mode = p.first().copied().unwrap_or(0);
                let y = self.cy as usize;
                if y < self.cells.len() {
                    let (from, to) = match mode {
                        1 => (0usize, self.cx as usize + 1),
                        2 => (0usize, self.sx as usize),
                        _ => (self.cx as usize, self.sx as usize),
                    };
                    let len = self.cells[y].len();
                    for x in from..to.min(len) {
                        self.cells[y][x] = ' ';
                    }
                }
            }
            _ => {}
        }
    }

    pub fn lines(&self) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect()
    }

    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    pub fn row(&self, y: u32) -> String {
        self.cells
            .get(y as usize)
            .map(|r| r.iter().collect::<String>().trim_end().to_string())
            .unwrap_or_default()
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >= 0xf0 {
        4
    } else if b >= 0xe0 {
        3
    } else if b >= 0xc0 {
        2
    } else {
        1
    }
}

fn is_wide(c: char) -> bool {
    let code = c as u32;
    (0x1100..=0x115F).contains(&code)
        || (0x2E80..=0xA4CF).contains(&code)
        || (0xAC00..=0xD7A3).contains(&code)
        || (0xF900..=0xFAFF).contains(&code)
        || (0xFF00..=0xFF60).contains(&code)
        || (0x1F300..=0x1F9FF).contains(&code)
        || (0x20000..=0x3FFFD).contains(&code)
}

// ── Harness ─────────────────────────────────────────────────────────────

pub struct Loom {
    pub server: Server,
    /// The server side of the fake PTY (kept alive so the fd stays valid).
    pub pty_master: StdUnixStream,
    /// Write here to simulate shell/eza output.
    pub shell: StdUnixStream,
    /// The client side, as a real Peer (production framing).
    pub client: Peer,
    pub client_token: Token,
    pub pane_id: PaneId,
    pub window_id: WindowId,
    pub session_id: SessionId,
    pub vt: Vt,
    pub sx: u32,
    pub sy: u32,
}

impl Loom {
    /// Build a harness with one window/pane + an attached client.
    pub fn new(sx: u32, sy: u32) -> Self {
        let config = ServerConfig {
            socket_path: format!(
                "/tmp/loom-harness-{}-{}-{}.sock",
                std::process::id(),
                sx,
                sy
            ),
            socket_mode: 0o600,
        };
        let mut server = Server::new(config).unwrap();

        // Fake PTY: (pty_master -> server, shell -> test).
        let (pty_master, shell) = StdUnixStream::pair().unwrap();
        pty_master.set_nonblocking(true).unwrap();
        shell.set_nonblocking(true).unwrap();
        // The server treats `pane.fd` as its own and closes it on drop, so
        // hand it a dup; `pty_master` stays owned by the harness.
        let pty_fd = unsafe { nix::libc::dup(pty_master.as_raw_fd()) };

        // Client link: (client_read -> server, client -> test).
        let (client_read, client_std) = StdUnixStream::pair().unwrap();
        client_read.set_nonblocking(true).unwrap();
        client_std.set_nonblocking(true).unwrap();

        // Window + pane (content height reserves the status row).
        let content_sy = if sy > 1 { sy - 1 } else { sy };
        let mut window = Window::new(sx, content_sy);
        let wid = window.id;
        let pane_id = {
            let mut pane = WindowPane::new(wid, sx, content_sy);
            let pid = pane.id;
            pane.fd = Some(pty_fd);
            window.panes.insert(pid, pane);
            window.active_pane_id = Some(pid);
            window.pane_order.push_back(pid);
            pid
        };
        server.windows.insert(wid, window);

        let mut session = Session::new(None, "/tmp");
        session.attach_window(0, wid);
        let sid = session.id;
        server.sessions.insert(sid, session);

        // Register the fake PTY with mio so process_once() drives the same
        // path a real PTY would.
        let pty_token = Token(PTY_BASE);
        {
            let mut source = mio::unix::SourceFd(&pty_fd);
            server
                .poll
                .registry()
                .register(&mut source, pty_token, Interest::READABLE)
                .unwrap();
            server.pty_fds.insert(pty_token, (pty_fd, pane_id));
            server
                .parsers
                .insert(pane_id, loom_input::input::Parser::new());
        }

        // Attached client with a Tty.
        let client_token = Token(CLIENT_BASE);
        {
            let stream = mio::net::UnixStream::from_std(client_read);
            let mut c = ClientState {
                peer: Peer::new(stream),
                flags: 0,
                session_id: Some(sid),
                identified: true,
                term_name: "xterm-256color".into(),
                tty_name: String::new(),
                cwd: "/tmp".into(),
                pid: 0,
                attached: true,
                pending_size: Some((sx, sy)),
                tty: Some(loom_tty::tty::Tty::new(sx, sy)),
                tty_initialized: true,
            };
            c.peer
                .register(
                    server.poll.registry(),
                    client_token,
                    Interest::READABLE | Interest::WRITABLE,
                )
                .unwrap();
            server.clients.insert(client_token, c);
        }

        let client_stream = mio::net::UnixStream::from_std(client_std);
        let client = Peer::new(client_stream);

        let mut harness = Self {
            server,
            pty_master,
            shell,
            client,
            client_token,
            pane_id,
            window_id: wid,
            session_id: sid,
            vt: Vt::new(sx, sy),
            sx,
            sy,
        };
        // Force an initial full redraw so the base screen exists.
        harness.redraw_full();
        harness.pump(8);
        harness
    }

    fn redraw_full(&mut self) {
        let (sid, wid) = (self.session_id, self.window_id);
        self.server.broadcast_redraw(sid, wid, true);
    }

    /// Run the server event loop a bounded number of iterations, draining the
    /// client each time (so the server's send queue never grows unbounded).
    pub fn pump(&mut self, max: usize) {
        for _ in 0..max {
            let _ = self.server.process_once();
            self.drain_client();
        }
    }

    /// Pump until things settle (bounded).
    pub fn settle(&mut self) {
        self.pump(300);
    }

    /// Write bytes as if the shell printed them.
    pub fn shell_write(&mut self, bytes: &[u8]) {
        let _ = self.shell.write_all(bytes);
        let _ = self.shell.flush();
    }

    /// Feed shell output then let the server process it.
    pub fn shell_print(&mut self, bytes: &[u8]) {
        self.shell_write(bytes);
        self.settle();
    }

    /// Read everything the client received and replay it into the VT.
    pub fn drain_client(&mut self) {
        loop {
            match self.client.recv() {
                Ok(Some(Message::ScreenUpdate { data })) => self.vt.feed(&data),
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => break,
            }
        }
    }

    /// Send a protocol message from the client.
    pub fn client_send(&mut self, msg: &Message) {
        let _ = self.client.send(msg);
        // Keep flushing until the queue drains into the socket, then let the
        // server read it.
        for _ in 0..10 {
            match self.client.flush() {
                Ok(true) => break,
                _ => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        self.pump(20);
    }

    /// Send a command (argv), as the `:` prompt would.
    pub fn command(&mut self, argv: &[&str]) {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        self.client_send(&Message::Command {
            argc: argv.len() as u32,
            argv,
        });
    }

    /// Send a keystroke.
    pub fn keys(&mut self, key: &[u8]) {
        self.client_send(&Message::KeyPress { key: key.to_vec() });
    }

    /// Send a mouse event.
    pub fn mouse(&mut self, button: u32, x: u32, y: u32, release: bool) {
        self.client_send(&Message::Mouse {
            button,
            sx: x,
            sy: y,
            release,
        });
    }

    /// Read back whatever the server wrote to the pane's PTY.
    pub fn shell_read(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match self.shell.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        out
    }

    /// The current screen as text.
    pub fn screen(&self) -> String {
        self.vt.text()
    }
}

/// Convenience constructor.
pub fn loom_80x24() -> Loom {
    Loom::new(80, 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke: the harness brings up a screen (status line present) and the
    /// client sees it.
    #[test]
    fn harness_shows_status_line() {
        let mut h = loom_80x24();
        h.settle();
        let screen = h.screen();
        // The status line is the bottom row; the session name/placeholder
        // should be rendered there.
        assert!(!screen.trim().is_empty(), "screen should have content");
        let bottom = h.vt.row(23);
        assert!(
            bottom.contains("0:") || !bottom.is_empty(),
            "status row should render, got: {bottom:?}"
        );
    }

    /// Shell output lands on screen: what the shell prints is what the client
    /// sees (this is the "type a command, see the result" loop).
    #[test]
    fn shell_output_appears_on_screen() {
        let mut h = loom_80x24();
        h.shell_print(b"hello harness\r\n");
        let screen = h.screen();
        assert!(screen.contains("hello harness"), "screen:\n{screen}");
    }

    /// Colored output: eza-style SGR must not corrupt the text or columns.
    #[test]
    fn colored_columns_align_on_screen() {
        let mut h = loom_80x24();
        // Two coloured columns separated by spaces, like eza -l.
        h.shell_print(
            b"\x1b[1;33myusiwen\x1b[0m 9 Sep 13:48 \x1b[1;34mCargo.toml\x1b[0m\r\n",
        );
        let row0 = h.vt.row(0);
        assert!(row0.contains("yusiwen"), "row0: {row0:?}");
        assert!(row0.contains("Cargo.toml"), "row0: {row0:?}");
        // The colour reset must not shift the second field.
        assert!(row0.starts_with("yusiwen 9 Sep"), "row0: {row0:?}");
    }

    /// Keystrokes reach the pane's "PTY" (forwarding works end-to-end).
    #[test]
    fn keys_reach_the_pty() {
        let mut h = loom_80x24();
        h.keys(b"ls\r");
        let written = h.shell_read();
        assert_eq!(written, b"ls\r", "PTY should receive the keystrokes");
    }

    /// A DSR query in shell output is answered back to the PTY (the parser's
    /// response path), which is what lets zle/vim probe the cursor.
    #[test]
    fn dsr_query_gets_a_response() {
        let mut h = loom_80x24();
        h.shell_print(b"\x1b[6n");
        let resp = h.shell_read();
        assert!(
            resp.starts_with(b"\x1b["),
            "expected a cursor-position report, got {:?}",
            resp
        );
    }

    /// A burst of eza-style output must not hang the loop and must land on
    /// screen. This is the automated version of the manual `ls -l` test.
    #[test]
    fn eza_style_burst_does_not_hang() {
        let mut h = loom_80x24();
        let line = b"\x1b[1;34md\x1b[33mr\x1b[31mw\x1b[32mx\x1b[0m\x1b[33mr\x1b[1;90m-\x1b[0m\x1b[32mx\x1b[0m@    \x1b[1;90m-\x1b[0m \x1b[1;33myusiwen\x1b[0m \x1b[34m 9 Sep 13:48\x1b[0m \x1b[1;34mbenches\x1b[0m\r\n";
        let start = std::time::Instant::now();
        for _ in 0..200 {
            h.shell_write(line);
        }
        h.settle();
        assert!(
            start.elapsed().as_secs() < 20,
            "eza burst hung the loop: {:?}",
            start.elapsed()
        );
        // The screen shows the tail of the burst.
        let screen = h.screen();
        assert!(screen.contains("benches"), "screen:\n{screen}");
    }






}
