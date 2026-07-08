use std::io::Write;

use loom_core::colour::COLOUR_FLAG_RGB;
use loom_core::grid_cell::*;

/// TTY output driver: tracks terminal state and only sends deltas.
///
/// Based on tmux's `struct tty` in tty.c:
/// - Persistent across redraws — knows what was last sent to the terminal
/// - `tty_attributes()` compares with `last_cell`, only emits changed SGR
/// - `tty_cursor()` only emits cursor positioning when position changed
/// - `tty_draw_line()` is the per-line drawing primitive
pub struct Tty {
    pub out: Vec<u8>,
    pub sx: u32,
    pub sy: u32,
    pub cx: i32,        // -1 = unknown
    pub cy: i32,
    pub last_cell: GridCell,  // last attribute state sent
}

impl Tty {
    pub fn new(sx: u32, sy: u32) -> Self {
        Self {
            out: Vec::with_capacity(4096),
            sx, sy,
            cx: -1, cy: -1,
            last_cell: GridCell::default_cell(),
        }
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Force a full attribute reset on next output.
    pub fn invalidate(&mut self) {
        self.cx = -1;
        self.cy = -1;
        self.last_cell = GridCell::default_cell();
    }

    /// Move cursor to (x, y). Only emits if position differs.
    /// Based on tmux's `tty_cursor()`.
    pub fn tty_cursor(&mut self, x: u32, y: u32) {
        if self.cx == x as i32 && self.cy == y as i32 {
            return;
        }
        let _ = write!(self.out, "\x1b[{};{}H", y + 1, x + 1);
        self.cx = x as i32;
        self.cy = y as i32;
    }

    /// Set SGR attributes for a cell. Only emits if changed from `last_cell`.
    /// Based on tmux's `tty_attributes()` and `tty_colours()`.
    pub fn tty_attributes(&mut self, cell: &GridCell) {
        let fg = if cell.fg == 8 { 8 } else { cell.fg };
        let bg = if cell.bg == 8 { 8 } else { cell.bg };
        let attr = cell.attr;
        let us = cell.us;

        // Compare with last_cell — skip if nothing changed
        if self.last_cell.fg == fg
            && self.last_cell.bg == bg
            && self.last_cell.attr == attr
            && self.last_cell.us == us
            && self.last_cell.link == cell.link
        {
            return;
        }

        // Build SGR
        let mut sgr = String::from("\x1b[0");
        if attr & GRID_ATTR_BRIGHT != 0 { sgr.push_str(";1"); }
        if attr & GRID_ATTR_DIM != 0 { sgr.push_str(";2"); }
        if attr & GRID_ATTR_ITALICS != 0 { sgr.push_str(";3"); }
        if attr & GRID_ATTR_UNDERSCORE != 0 { sgr.push_str(";4"); }
        if attr & GRID_ATTR_BLINK != 0 { sgr.push_str(";5"); }
        if attr & GRID_ATTR_REVERSE != 0 { sgr.push_str(";7"); }
        if attr & GRID_ATTR_HIDDEN != 0 { sgr.push_str(";8"); }
        if attr & GRID_ATTR_STRIKETHROUGH != 0 { sgr.push_str(";9"); }

        if fg != 8 { self.push_colour(&mut sgr, fg, 38); }
        if bg != 8 { self.push_colour(&mut sgr, bg, 48); }
        if us != 8 { self.push_colour(&mut sgr, us, 58); }

        sgr.push('m');
        let _ = write!(self.out, "{}", sgr);

        // Update last_cell
        self.last_cell.fg = fg;
        self.last_cell.bg = bg;
        self.last_cell.attr = attr;
        self.last_cell.us = us;
        self.last_cell.link = cell.link;
    }

    /// Draw a single cell at (x, y) with attributes + character.
    pub fn tty_cell(&mut self, x: u32, y: u32, cell: &GridCell) {
        self.tty_cursor(x, y);
        self.tty_attributes(cell);
        let ch = cell.data.to_char();
        if ch == ' ' && cell.attr == 0 && cell.fg == 8 && cell.bg == 8 {
            // Space with default attributes — use EL or just skip
            self.out.push(b' ');
        } else {
            let _ = write!(self.out, "{}", ch);
        }
    }

    /// Clear the screen.
    pub fn clear_screen(&mut self) {
        let _ = write!(self.out, "\x1b[2J\x1b[H");
        self.cx = 0;
        self.cy = 0;
    }

    fn push_colour(&self, sgr: &mut String, colour: i32, prefix: i32) {
        use std::fmt::Write as _;
        if colour & COLOUR_FLAG_RGB != 0 {
            let r = ((colour >> 16) & 0xff) as u8;
            let g = ((colour >> 8) & 0xff) as u8;
            let b = (colour & 0xff) as u8;
            let _ = write!(sgr, ";{};2;{};{};{}", prefix, r, g, b);
        } else if colour >= 16 || colour & 0x01000000 != 0 {
            let _ = write!(sgr, ";{};5;{}", prefix, colour & 0xff);
        } else {
            let idx = colour & 0xff;
            if idx < 8 {
                let _ = write!(sgr, ";{}", prefix + idx);
            } else {
                let _ = write!(sgr, ";{}", prefix + 60 + idx - 8);
            }
        }
    }
}
