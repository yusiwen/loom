use std::io::{self, Write};

use loom_core::colour::COLOUR_FLAG_RGB;
use loom_core::grid_cell::*;

/// TTY output driver: tracks terminal state and only sends deltas.
///
/// Based on tmux's `struct tty` in tty.c:
/// - Tracks last-known cursor position (cx, cy)
/// - Tracks last-known SGR state (fg, bg, attr)
/// - Only outputs sequences when state changes
pub struct Tty<W: Write> {
    out: W,
    pub sx: u32,
    pub sy: u32,
    cx: i32,        // -1 = unknown
    cy: i32,
    last_fg: i32,   // 8 = default
    last_bg: i32,
    last_attr: u16,
}

impl<W: Write> Tty<W> {
    pub fn new(out: W, sx: u32, sy: u32) -> Self {
        Self {
            out,
            sx, sy,
            cx: -1, cy: -1,
            last_fg: 8,
            last_bg: 8,
            last_attr: 0,
        }
    }

    /// Flush output buffer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// Draw a single cell at (x, y) — only outputs changes.
    pub fn draw_cell(&mut self, x: u32, y: u32, fg: i32, bg: i32, attr: u16, ch: char) -> io::Result<()> {
        // Move cursor only if position changed
        self.cursor_goto(x, y)?;

        // Output SGR only if attributes changed
        let mut sgr = String::new();
        if attr != self.last_attr || fg != self.last_fg || bg != self.last_bg {
            sgr.push_str("\x1b[0");
            if attr & GRID_ATTR_BRIGHT != 0 { sgr.push_str(";1"); }
            if attr & GRID_ATTR_DIM != 0 { sgr.push_str(";2"); }
            if attr & GRID_ATTR_ITALICS != 0 { sgr.push_str(";3"); }
            if attr & GRID_ATTR_UNDERSCORE != 0 { sgr.push_str(";4"); }
            if attr & GRID_ATTR_BLINK != 0 { sgr.push_str(";5"); }
            if attr & GRID_ATTR_REVERSE != 0 { sgr.push_str(";7"); }
            if attr & GRID_ATTR_HIDDEN != 0 { sgr.push_str(";8"); }
            if attr & GRID_ATTR_STRIKETHROUGH != 0 { sgr.push_str(";9"); }

            // Foreground
            if fg != 8 && fg != self.last_fg {
                self.push_colour(&mut sgr, fg, 38);
            }
            // Background
            if bg != 8 && bg != self.last_bg {
                self.push_colour(&mut sgr, bg, 48);
            }
            sgr.push('m');
            write!(self.out, "{}", sgr)?;

            self.last_attr = attr;
            self.last_fg = fg;
            self.last_bg = bg;
        }

        // Write character
        write!(self.out, "{}", ch)?;
        Ok(())
    }

    fn push_colour(&self, sgr: &mut String, colour: i32, prefix: i32) {
        if colour & COLOUR_FLAG_RGB != 0 {
            let r = ((colour >> 16) & 0xff) as u8;
            let g = ((colour >> 8) & 0xff) as u8;
            let b = (colour & 0xff) as u8;
            sgr.push_str(&format!(";{};2;{};{};{}", prefix, r, g, b));
        } else if colour >= 16 || colour & 0x01000000 != 0 {
            sgr.push_str(&format!(";{};5;{}", prefix, colour & 0xff));
        } else {
            let idx = colour & 0xff;
            if idx < 8 {
                sgr.push_str(&format!(";{}", prefix + idx));
            } else {
                sgr.push_str(&format!(";{}", prefix + 60 + idx - 8));
            }
        }
    }

    fn cursor_goto(&mut self, x: u32, y: u32) -> io::Result<()> {
        if self.cx == x as i32 && self.cy == y as i32 {
            return Ok(());
        }
        write!(self.out, "\x1b[{};{}H", y + 1, x + 1)?;
        self.cx = x as i32;
        self.cy = y as i32;
        Ok(())
    }

    /// Clear the entire screen.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        write!(self.out, "\x1b[2J\x1b[H")?;
        self.cx = 0;
        self.cy = 0;
        self.last_fg = 8;
        self.last_bg = 8;
        self.last_attr = 0;
        Ok(())
    }

    /// Set cursor to an absolute position (used after redraw).
    pub fn set_cursor(&mut self, x: u32, y: u32) -> io::Result<()> {
        self.cx = -1; // force reposition
        self.cy = -1;
        self.cursor_goto(x, y)
    }

    /// Move cursor relative.
    pub fn cursor_right(&mut self, n: u32) -> io::Result<()> {
        if self.cx >= 0 {
            self.cx = (self.cx as u32 + n).min(self.sx - 1) as i32;
        }
        // Don't output anything — caller will reposition via cursor_goto
        Ok(())
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }
}

impl<W: Write> Write for Tty<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}
