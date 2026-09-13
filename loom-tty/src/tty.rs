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

        if fg != 8 { self.push_colour(&mut sgr, fg, 30); }
        if bg != 8 { self.push_colour(&mut sgr, bg, 40); }
        if us != 8 { self.push_colour(&mut sgr, us, 50); }

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
        // Track the hardware cursor: it now sits one column past the drawn
        // cell (wide cells advance by their width), so consecutive cells on
        // the same row need no CUP.
        self.cx = (x as i32) + (cell.data.width.max(1) as i32);
        self.cy = y as i32;
    }

    /// Clear the screen.
    pub fn clear_screen(&mut self) {
        let _ = write!(self.out, "\x1b[2J\x1b[H");
        self.cx = 0;
        self.cy = 0;
    }

    /// Emit the colour part of an SGR sequence.
    ///
    /// `prefix` is the *base* SGR parameter: 30 for fg, 40 for bg, 50 for
    /// underline colour. The extended forms (256-colour and RGB) use base + 8
    /// (38/48/58), and bright palette entries 8..15 use base + 60 + (idx - 8)
    /// (90..97 for fg, 100..107 for bg). Passing 38/48/58 here instead of
    /// 30/40/50 silently rewrites basic colours into default/background codes.
    fn push_colour(&self, sgr: &mut String, colour: i32, prefix: i32) {
        use std::fmt::Write as _;
        let ext = prefix + 8;
        if colour & COLOUR_FLAG_RGB != 0 {
            let r = ((colour >> 16) & 0xff) as u8;
            let g = ((colour >> 8) & 0xff) as u8;
            let b = (colour & 0xff) as u8;
            let _ = write!(sgr, ";{};2;{};{};{}", ext, r, g, b);
        } else if colour >= 16 || colour & 0x01000000 != 0 {
            let _ = write!(sgr, ";{};5;{}", ext, colour & 0xff);
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

#[cfg(test)]
mod tests {
    use super::*;
    use loom_core::colour::COLOUR_FLAG_256;

    fn sgr_for(cell: &GridCell) -> String {
        let mut tty = Tty::new(80, 24);
        tty.tty_attributes(cell);
        String::from_utf8_lossy(&tty.take_output()).into_owned()
    }

    #[test]
    fn test_basic_palette_colours() {
        // 0..7 map to 30..37 (fg) / 40..47 (bg).
        let cell = GridCell { fg: 1, bg: 4, ..GridCell::default_cell() };
        let s = sgr_for(&cell);
        assert!(s.contains(";31"), "fg red -> 31, got {s:?}");
        assert!(s.contains(";44"), "bg blue -> 44, got {s:?}");
    }

    #[test]
    fn test_bright_palette_colours() {
        // Flagged 256 entries 8..15 emit 38;5;N / 48;5;N.
        let cell = GridCell {
            fg: 9 | COLOUR_FLAG_256,
            bg: 8 | COLOUR_FLAG_256,
            ..GridCell::default_cell()
        };
        let s = sgr_for(&cell);
        assert!(s.contains("38;5;9"), "bright red fg, got {s:?}");
        assert!(s.contains("48;5;8"), "bright black bg, got {s:?}");
    }

    /// Regression: an unflagged palette index 8..15 (legacy path) must emit the
    /// aixterm bright code, not the off-by-eight 98/108 range.
    #[test]
    fn test_unflagged_bright_index_maps_to_aixterm_codes() {
        let cell = GridCell { fg: 9, bg: 10, ..GridCell::default_cell() };
        let s = sgr_for(&cell);
        assert!(s.contains(";91"), "idx 9 fg -> 91, got {s:?}");
        assert!(s.contains(";102"), "idx 10 bg -> 102, got {s:?}");
    }

    #[test]
    fn test_default_colour_emits_no_fg_or_bg() {
        let cell = GridCell::default_cell();
        let s = sgr_for(&cell);
        assert!(!s.contains("38;"), "no fg for default, got {s:?}");
        assert!(!s.contains("48;"), "no bg for default, got {s:?}");
    }
}
