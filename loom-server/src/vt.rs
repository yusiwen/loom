//! Minimal VT100 emulator used to reconstruct a screen from a terminal byte
//! stream.
//!
//! It is deliberately small — enough for tests and oracles to answer "what
//! would a user actually see": printing, CR/LF/BS/TAB, CUP/ED/EL, wide
//! characters, and SGR (colours + attributes).
//!
//! Colours use loom's conventions (`8` = default; palette entries carry
//! `COLOUR_FLAG_256`), so a replayed cell compares directly with the
//! server-side `GridCell` that produced it, and the AIX bright forms
//! (`\x1b[90m`) normalise to the same value as `38;5;8`.

/// A cell as a terminal would hold it after replaying a byte stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VtCell {
    pub ch: char,
    pub fg: i32,
    pub bg: i32,
    pub attr: u16,
}

impl Default for VtCell {
    fn default() -> Self {
        Self { ch: ' ', fg: 8, bg: 8, attr: 0 }
    }
}

/// The emulator. Feed it bytes with [`Vt::feed`], then read cells via
/// [`Vt::cell_at`] or the text helpers.
pub struct Vt {
    pub sx: u32,
    pub sy: u32,
    pub cells: Vec<Vec<VtCell>>,
    pub cx: u32,
    pub cy: u32,
    /// Current SGR pen, applied to each printed cell.
    pub fg: i32,
    pub bg: i32,
    pub attr: u16,
}

impl Vt {
    pub fn new(sx: u32, sy: u32) -> Self {
        Self {
            sx,
            sy,
            cells: vec![vec![VtCell::default(); sx as usize]; sy as usize],
            cx: 0,
            cy: 0,
            fg: 8,
            bg: 8,
            attr: 0,
        }
    }

    /// The cell at (x, y) as the terminal holds it, if in bounds.
    pub fn cell_at(&self, x: u32, y: u32) -> Option<VtCell> {
        self.cells.get(y as usize).and_then(|r| r.get(x as usize)).copied()
    }

    /// Foreground colour at (x, y); `8` means default.
    pub fn fg_at(&self, x: u32, y: u32) -> i32 {
        self.cell_at(x, y).map(|c| c.fg).unwrap_or(8)
    }

    /// Background colour at (x, y); `8` means default.
    pub fn bg_at(&self, x: u32, y: u32) -> i32 {
        self.cell_at(x, y).map(|c| c.bg).unwrap_or(8)
    }

    /// Attribute bits (`GRID_ATTR_*`) at (x, y).
    pub fn attr_at(&self, x: u32, y: u32) -> u16 {
        self.cell_at(x, y).map(|c| c.attr).unwrap_or(0)
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
                    // Plain LF: a real terminal moves down only (CR handled
                    // separately), which is what producer streams rely on.
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
        let pen = VtCell { ch: c, fg: self.fg, bg: self.bg, attr: self.attr };
        if y < self.cells.len() && x < self.cells[y].len() {
            self.cells[y][x] = pen;
            if wide && x + 1 < self.cells[y].len() {
                self.cells[y][x + 1] = VtCell { ch: ' ', ..pen };
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
                            *cell = VtCell::default();
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
                        self.cells[y][x] = VtCell::default();
                    }
                }
            }
            b'm' => self.sgr(&p),
            _ => {}
        }
    }

    /// Apply one SGR sequence to the pen. Uses loom's colour conventions so a
    /// replayed cell compares equal to the server-side `GridCell` that
    /// produced it (`8` = default; palette entries carry `COLOUR_FLAG_256`).
    fn sgr(&mut self, p: &[i64]) {
        use loom_core::colour::COLOUR_FLAG_256;
        use loom_core::grid_cell::*;

        let params: Vec<i64> = if p.is_empty() { vec![0] } else { p.to_vec() };
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    self.fg = 8;
                    self.bg = 8;
                    self.attr = 0;
                }
                1 => self.attr |= GRID_ATTR_BRIGHT,
                2 => self.attr |= GRID_ATTR_DIM,
                3 => self.attr |= GRID_ATTR_ITALICS,
                4 => self.attr |= GRID_ATTR_UNDERSCORE,
                5 => self.attr |= GRID_ATTR_BLINK,
                7 => self.attr |= GRID_ATTR_REVERSE,
                8 => self.attr |= GRID_ATTR_HIDDEN,
                9 => self.attr |= GRID_ATTR_STRIKETHROUGH,
                22 => self.attr &= !(GRID_ATTR_BRIGHT | GRID_ATTR_DIM),
                23 => self.attr &= !GRID_ATTR_ITALICS,
                24 => self.attr &= !GRID_ATTR_UNDERSCORE,
                25 => self.attr &= !GRID_ATTR_BLINK,
                27 => self.attr &= !GRID_ATTR_REVERSE,
                28 => self.attr &= !GRID_ATTR_HIDDEN,
                29 => self.attr &= !GRID_ATTR_STRIKETHROUGH,
                30..=37 => self.fg = (params[i] - 30) as i32,
                39 => self.fg = 8,
                40..=47 => self.bg = (params[i] - 40) as i32,
                49 => self.bg = 8,
                90..=97 => self.fg = (params[i] - 90 + 8) as i32 | COLOUR_FLAG_256,
                100..=107 => self.bg = (params[i] - 100 + 8) as i32 | COLOUR_FLAG_256,
                38 | 48 => {
                    let fg = params[i] == 38;
                    let sel = params.get(i + 1).copied().unwrap_or(0);
                    if sel == 5 {
                        let idx = params.get(i + 2).copied().unwrap_or(0) as i32;
                        if fg {
                            self.fg = idx | COLOUR_FLAG_256
                        } else {
                            self.bg = idx | COLOUR_FLAG_256
                        }
                        i += 2;
                    } else if sel == 2 {
                        let r = params.get(i + 2).copied().unwrap_or(0) as i32;
                        let g = params.get(i + 3).copied().unwrap_or(0) as i32;
                        let b = params.get(i + 4).copied().unwrap_or(0) as i32;
                        let v = loom_core::colour::COLOUR_FLAG_RGB | (r << 16) | (g << 8) | b;
                        if fg {
                            self.fg = v
                        } else {
                            self.bg = v
                        }
                        i += 4;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// The screen as text rows (trailing whitespace trimmed).
    pub fn lines(&self) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    pub fn row(&self, y: u32) -> String {
        self.cells
            .get(y as usize)
            .map(|r| r.iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
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
