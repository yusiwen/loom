use std::io;
use std::os::unix::io::{BorrowedFd, RawFd};

use nix::sys::termios;
use nix::unistd;

use crate::terminfo::TtyTerm;

/// TTY mode flags.
pub const TTY_OPENED: u32 = 0x0001;
pub const TTY_STARTED: u32 = 0x0002;
pub const TTY_FREEZE: u32 = 0x0004;
pub const TTY_NOCURSOR: u32 = 0x0008;
pub const TTY_BLOCK: u32 = 0x0010;
pub const TTY_TIMER: u32 = 0x0020;
pub const TTY_NOBLOCK: u32 = 0x0040;
pub const TTY_HAVEDA: u32 = 0x0080;
pub const TTY_HAVEDA2: u32 = 0x0100;
pub const TTY_HAVEXDA: u32 = 0x0200;
pub const TTY_BRACKETPASTE: u32 = 0x0400;
pub const TTY_SYNCING: u32 = 0x0800;

/// A TTY connected to a real terminal.
pub struct Tty {
    /// File descriptor for the terminal.
    pub fd: RawFd,
    /// Terminal size (columns).
    pub sx: u32,
    /// Terminal size (rows).
    pub sy: u32,
    /// Cursor X position.
    pub cx: u32,
    /// Cursor Y position.
    pub cy: u32,
    /// Scroll region top.
    pub rupper: u32,
    /// Scroll region bottom.
    pub rlower: u32,
    /// Current foreground colour (-1 if unset).
    pub fg: i32,
    /// Current background colour (-1 if unset).
    pub bg: i32,
    /// Current text attributes.
    pub attr: u16,
    /// TTY flags.
    pub flags: u32,
    /// Output buffer.
    pub out: Vec<u8>,
    /// Input buffer.
    pub inp: Vec<u8>,
    /// Original termios settings (saved for restore).
    saved_tio: termios::Termios,
    /// Current termios settings.
    pub tio: termios::Termios,
    /// Terminfo database.
    pub term: Option<TtyTerm>,
}

/// Helper to convert RawFd to BorrowedFd for nix APIs.
fn as_fd<'a>(fd: &'a RawFd) -> BorrowedFd<'a> {
    unsafe { BorrowedFd::borrow_raw(*fd) }
}

impl Tty {
    /// Create a new TTY for the given fd.
    pub fn new(fd: RawFd) -> io::Result<Self> {
        let tio = termios::tcgetattr(as_fd(&fd)).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("tcgetattr: {}", e))
        })?;

        let mut ws = nix::libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            nix::libc::ioctl(fd, nix::libc::TIOCGWINSZ, &mut ws);
        }

        Ok(Self {
            fd,
            sx: ws.ws_col as u32,
            sy: ws.ws_row as u32,
            cx: 0,
            cy: 0,
            rupper: 0,
            rlower: (ws.ws_row.max(1) - 1) as u32,
            fg: -1,
            bg: -1,
            attr: 0,
            flags: 0,
            out: Vec::with_capacity(4096),
            inp: Vec::with_capacity(4096),
            saved_tio: tio.clone(),
            tio,
            term: None,
        })
    }

    /// Initialize the TTY with the given terminal name.
    pub fn init(&mut self, term_name: &str) -> io::Result<()> {
        let term = TtyTerm::load(term_name)?;
        self.term = Some(term);
        self.flags |= TTY_OPENED;
        self.start();
        Ok(())
    }

    /// Put terminal into raw mode.
    pub fn start(&mut self) {
        use termios::{InputFlags, LocalFlags, OutputFlags, SpecialCharacterIndices};

        let mut tio = self.tio.clone();

        tio.input_flags &= !(InputFlags::IXON
            | InputFlags::IXOFF
            | InputFlags::ICRNL
            | InputFlags::INLCR
            | InputFlags::IGNCR
            | InputFlags::IMAXBEL
            | InputFlags::ISTRIP);
        tio.input_flags |= InputFlags::IGNBRK;

        tio.output_flags &= !(OutputFlags::OPOST
            | OutputFlags::ONLCR
            | OutputFlags::OCRNL
            | OutputFlags::ONLRET);

        tio.local_flags &= !(LocalFlags::IEXTEN
            | LocalFlags::ICANON
            | LocalFlags::ECHO
            | LocalFlags::ECHOE
            | LocalFlags::ECHONL
            | LocalFlags::ECHOCTL
            | LocalFlags::ECHOPRT
            | LocalFlags::ECHOKE
            | LocalFlags::ISIG);

        tio.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        tio.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;

        let _ = termios::tcsetattr(as_fd(&self.fd), termios::SetArg::TCSANOW, &tio);
        let _ = termios::tcflush(as_fd(&self.fd), termios::FlushArg::TCOFLUSH);

        self.tio = tio;

        self.put_code("smcup");
        self.put_code("smkx");
        self.put_code("clear");
        self.put_code("cnorm");

        self.flags |= TTY_STARTED;
    }

    /// Restore terminal to original settings.
    pub fn stop(&mut self) {
        let _ = termios::tcsetattr(as_fd(&self.fd), termios::SetArg::TCSANOW, &self.saved_tio);
        self.put_code("rmcup");
        self.put_code("rmkx");
        let _ = self.flush();
    }

    /// Resize the TTY (query window size).
    pub fn resize(&mut self) {
        let mut ws = nix::libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            if nix::libc::ioctl(self.fd, nix::libc::TIOCGWINSZ, &mut ws) == 0 {
                self.sx = ws.ws_col as u32;
                self.sy = ws.ws_row as u32;
                self.rlower = (ws.ws_row.max(1) - 1) as u32;
            }
        }
    }

    // ── Output operations ──

    /// Write raw bytes to the output buffer.
    pub fn write_raw(&mut self, data: &[u8]) {
        self.out.extend_from_slice(data);
    }

    /// Write a single byte to the output buffer.
    pub fn put_char(&mut self, c: u8) {
        self.out.push(c);
    }

    /// Write a terminfo capability string (if available).
    pub fn put_code(&mut self, name: &str) {
        if let Some(ref term) = self.term {
            if let Some(s) = term.string(name) {
                self.out.extend_from_slice(s.as_bytes());
            }
        }
    }

    /// Write an ANSI escape sequence.
    pub fn put_str(&mut self, s: &str) {
        self.out.extend_from_slice(s.as_bytes());
    }

    /// Flush the output buffer to the terminal.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        let result = loop {
            match unistd::write(as_fd(&self.fd), &self.out) {
                Ok(n) => {
                    if n == self.out.len() {
                        self.out.clear();
                        break Ok(());
                    }
                    self.out.drain(..n);
                }
                Err(nix::errno::Errno::EAGAIN) | Err(nix::errno::Errno::EINTR) => {
                    continue;
                }
                Err(e) => {
                    break Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("tty write: {}", e),
                    ));
                }
            }
        };
        result
    }

    /// Write cursor position sequence.
    pub fn cursor_goto(&mut self, x: u32, y: u32) {
        let s = format!("\x1b[{};{}H", y + 1, x + 1);
        self.out.extend_from_slice(s.as_bytes());
        self.cx = x;
        self.cy = y;
    }

    /// Set scroll region.
    pub fn region_set(&mut self, rupper: u32, rlower: u32) {
        let s = format!("\x1b[{};{}r", rupper + 1, rlower + 1);
        self.out.extend_from_slice(s.as_bytes());
        self.rupper = rupper;
        self.rlower = rlower;
    }

    /// Set cursor style (0=default, 1=block, 2=underline, 3=bar).
    pub fn cursor_style(&mut self, style: u8) {
        let s = format!("\x1b[{} q", style);
        self.out.extend_from_slice(s.as_bytes());
    }

    /// Write SGR sequence to set foreground colour (24-bit RGB).
    pub fn set_fg(&mut self, r: u8, g: u8, b: u8) {
        if self.fg == i32::from_be_bytes([0, r, g, b]) {
            return;
        }
        self.fg = i32::from_be_bytes([0, r, g, b]);
        let s = format!("\x1b[38;2;{};{};{}m", r, g, b);
        self.out.extend_from_slice(s.as_bytes());
    }

    /// Write SGR sequence to set background colour (24-bit RGB).
    pub fn set_bg(&mut self, r: u8, g: u8, b: u8) {
        if self.bg == i32::from_be_bytes([0, r, g, b]) {
            return;
        }
        self.bg = i32::from_be_bytes([0, r, g, b]);
        let s = format!("\x1b[48;2;{};{};{}m", r, g, b);
        self.out.extend_from_slice(s.as_bytes());
    }

    /// Reset all attributes to default.
    pub fn attr_reset(&mut self) {
        self.attr = 0;
        self.fg = -1;
        self.bg = -1;
        self.out.extend_from_slice(b"\x1b[0m");
    }

    /// Set text attributes via SGR.
    pub fn set_attr(&mut self, attr: u16) {
        use loom_core::grid_cell::*;

        let mut codes: Vec<&str> = Vec::new();
        if attr & GRID_ATTR_BRIGHT != 0 {
            codes.push("1");
        }
        if attr & GRID_ATTR_DIM != 0 {
            codes.push("2");
        }
        if attr & GRID_ATTR_ITALICS != 0 {
            codes.push("3");
        }
        if attr & GRID_ATTR_UNDERSCORE != 0 {
            codes.push("4");
        }
        if attr & GRID_ATTR_BLINK != 0 {
            codes.push("5");
        }
        if attr & GRID_ATTR_REVERSE != 0 {
            codes.push("7");
        }
        if attr & GRID_ATTR_HIDDEN != 0 {
            codes.push("8");
        }
        if attr & GRID_ATTR_STRIKETHROUGH != 0 {
            codes.push("9");
        }
        if attr & GRID_ATTR_OVERLINE != 0 {
            codes.push("53");
        }

        if !codes.is_empty() {
            self.out.extend_from_slice(b"\x1b[");
            self.out.extend_from_slice(codes.join(";").as_bytes());
            self.out.extend_from_slice(b"m");
        }
        self.attr = attr;
    }

    /// Clear the screen.
    pub fn clear(&mut self) {
        self.out.extend_from_slice(b"\x1b[2J\x1b[H");
        self.cx = 0;
        self.cy = 0;
    }

    /// Clear to end of line.
    pub fn clear_to_eol(&mut self) {
        self.out.extend_from_slice(b"\x1b[K");
    }

    /// Clear to end of screen.
    pub fn clear_to_eos(&mut self) {
        self.out.extend_from_slice(b"\x1b[J");
    }

    /// Write a UTF-8 string directly.
    pub fn write_str(&mut self, s: &str) {
        self.out.extend_from_slice(s.as_bytes());
    }


}

impl Drop for Tty {
    fn drop(&mut self) {
        if self.flags & TTY_STARTED != 0 {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tty_smoke() {
        use std::os::unix::io::AsRawFd;
        let fd = std::fs::File::open("/dev/null")
            .map(|f| f.as_raw_fd())
            .unwrap_or(0);
        let mut tty = Tty::new(fd).unwrap_or_else(|_| {
            // Fallback if tcgetattr fails (not a tty)
            Tty {
                fd,
                sx: 80,
                sy: 24,
                cx: 0,
                cy: 0,
                rupper: 0,
                rlower: 23,
                fg: -1,
                bg: -1,
                attr: 0,
                flags: 0,
                out: Vec::new(),
                inp: Vec::new(),
                saved_tio: unsafe { std::mem::zeroed() },
                tio: unsafe { std::mem::zeroed() },
                term: None,
            }
        });

        tty.cursor_goto(10, 5);
        tty.write_str("hello");
        tty.set_attr(1);
        tty.write_str("world");
        tty.attr_reset();
        assert!(!tty.out.is_empty());
    }
}
