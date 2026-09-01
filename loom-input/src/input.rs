use loom_core::grid_cell::*;
use loom_core::screen::{CursorStyle, Screen};

// ── State machine ──

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputState {
    Ground,
    EscEnter,
    EscIntermediate,
    CsiEnter,
    CsiParameter,
    CsiIntermediate,
    CsiIgnore,
    DcsEnter,
    DcsParameter,
    DcsIntermediate,
    DcsHandler,
    DcsEscape,
    DcsIgnore,
    OscString,
    ApcString,
    RenameString,
    ConsumeSt,
}

#[allow(dead_code)]
const ANYWHERE: &[Transition] = &[
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x1b, 0x1b, None, Some(InputState::EscEnter)),
];

pub struct Transition {
    first: u8,
    last: u8,
    handler: Option<fn(&mut Parser, u8, &mut Screen)>,
    next_state: Option<InputState>,
}

impl Transition {
    pub const fn new(
        first: u8,
        last: u8,
        handler: Option<fn(&mut Parser, u8, &mut Screen)>,
        next_state: Option<InputState>,
    ) -> Self {
        Self { first, last, handler, next_state }
    }
}

// ── CSI command types ──

#[derive(Clone, Copy, Debug)]
enum CsiType {
    Ich, Cuu, Cud, Cuf, Cub, Cnl, Cpl, Hpa,
    Cup, Ed, El, Il, Dl, Dch, Su, Sd, Ech, Cbt, Rep,
    Da, DaTwo, Vpa, Tbc, Sm, SmPrivate, Rm, RmPrivate,
    Sgr, Decscusr, Decstbm, Scp, Rcp, Winops,
    Dsr, DsrPrivate, Modset, Modoff,
    Query, QueryPrivate, Xda,
    SmGraphics,
    Unknown,
}

// ── Persistent parser (owned, no lifetime) ──

pub struct Parser {
    pub state: InputState,
    interm_buf: [u8; 4],
    interm_len: usize,
    param_buf: [u8; 64],
    param_len: usize,
    params: [i32; 24],
    nparams: usize,
    input_buf: Vec<u8>,
    pub cell: GridCell,
    // UTF-8 accumulation
    utf8_buf: [u8; 4],
    utf8_have: u8,
    utf8_expected: u8,
    /// Response bytes to write back to the PTY (DSR, DA, etc.)
    pub response: Vec<u8>,
    /// Set when the shell rang the bell (BEL) since the last `take_bell`.
    bell: bool,
    /// Set when parsing modified the screen (content, cursor, scroll, attrs).
    /// Used by the server to skip redraws for query-only sequences (DSR/DA).
    dirty: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: InputState::Ground,
            interm_buf: [0u8; 4],
            interm_len: 0,
            param_buf: [0u8; 64],
            param_len: 0,
            params: [0i32; 24],
            nparams: 0,
            input_buf: Vec::with_capacity(256),
            cell: GridCell::default_cell(),
            utf8_buf: [0u8; 4],
            utf8_have: 0,
            utf8_expected: 0,
            response: Vec::new(),
            bell: false,
            dirty: false,
        }
    }

    /// Parse a single byte of terminal output.
    pub fn parse(&mut self, ch: u8, screen: &mut Screen) {
        // Handle in-progress UTF-8 multi-byte sequence before state machine dispatch.
        if self.utf8_expected > 0 {
            if (0x80..=0xBF).contains(&ch) {
                self.utf8_buf[self.utf8_have as usize] = ch;
                self.utf8_have += 1;
                if self.utf8_have == self.utf8_expected {
                    self.flush_utf8(screen);
                }
                return;
            }
            // Invalid continuation byte — abort pending sequence
            self.utf8_expected = 0;
            self.utf8_have = 0;
            // fall through to normal dispatch
        }

        let state = self.state;
        let table = match state {
            InputState::Ground => GROUND_TABLE,
            InputState::EscEnter => ESC_ENTER_TABLE,
            InputState::EscIntermediate => ESC_INTERMEDIATE_TABLE,
            InputState::CsiEnter => CSI_ENTER_TABLE,
            InputState::CsiParameter => CSI_PARAMETER_TABLE,
            InputState::CsiIntermediate => CSI_INTERMEDIATE_TABLE,
            InputState::CsiIgnore => CSI_IGNORE_TABLE,
            InputState::DcsEnter => DCS_ENTER_TABLE,
            InputState::DcsParameter => DCS_PARAMETER_TABLE,
            InputState::DcsIntermediate => DCS_INTERMEDIATE_TABLE,
            InputState::DcsHandler => DCS_HANDLER_TABLE,
            InputState::DcsEscape => DCS_ESCAPE_TABLE,
            InputState::DcsIgnore => DCS_IGNORE_TABLE,
            InputState::OscString => OSC_STRING_TABLE,
            InputState::ApcString => APC_STRING_TABLE,
            InputState::RenameString => RENAME_STRING_TABLE,
            InputState::ConsumeSt => CONSUME_ST_TABLE,
        };

        for tr in table {
            if tr.first == 255 {
                break;
            }
            if ch >= tr.first && ch <= tr.last {
                if let Some(handler) = tr.handler {
                    handler(self, ch, screen);
                }
                if let Some(next) = tr.next_state {
                    // Start a fresh body buffer when entering a string state.
                    match next {
                        InputState::OscString
                        | InputState::ApcString
                        | InputState::RenameString
                        | InputState::DcsHandler => {
                            self.input_buf.clear();
                        }
                        _ => {}
                    }
                    self.state = next;
                }
                return;
            }
        }
    }

    /// Parse a buffer of terminal output bytes.
    pub fn parse_buf(&mut self, screen: &mut Screen, data: &[u8]) {
        for &ch in data {
            self.parse(ch, screen);
        }
    }

    /// Write out any pending DSR/DA responses and clear the buffer.
    pub fn take_response(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.response)
    }

    /// Consume and report whether the shell rang the bell since the last call.
    pub fn take_bell(&mut self) -> bool {
        let had = self.bell;
        self.bell = false;
        had
    }

    /// Consume and report whether the screen was modified since the last call.
    pub fn take_dirty(&mut self) -> bool {
        let had = self.dirty;
        self.dirty = false;
        had
    }

    fn flush_utf8(&mut self, screen: &mut Screen) {
        let len = self.utf8_expected as usize;
        let decoded = core::str::from_utf8(&self.utf8_buf[..len])
            .ok()
            .and_then(|s| s.chars().next());
        self.utf8_expected = 0;
        self.utf8_have = 0;
        if let Some(c) = decoded {
            self.write_char(c, screen);
        }
        // Invalid UTF-8: silently discard
    }

    /// Write a decoded character to the screen at the current cursor position.
    fn write_char(&mut self, c: char, screen: &mut Screen) {
        self.dirty = true;
        let width = char_width(c).max(1) as u32;
        let sx = screen.size_x();
        let sy = screen.size_y();
        let cx = screen.cx;
        let cy = screen.cy;

        // Auto-wrap: if the char would exceed the right edge, move to next line.
        if cx + width > sx {
            if cx > 0 {
                screen.grid.set_line_wrapped(cy, true);
            }
            screen.cx = 0;
            screen.cy = cy.saturating_add(1);
            if screen.cy >= sy {
                screen.cy = sy.saturating_sub(1);
                if screen.rupper == 0 && screen.rlower == sy.saturating_sub(1) {
                    screen.grid.scroll_up();
                } else {
                    screen.grid.scroll_region_up(screen.rupper, screen.rlower);
                }
            }
        }

        let data = loom_core::utf8::Utf8Data::new(c);
        let gc = GridCell { data, ..self.cell };
        screen.grid.view_set_cell(screen.cx, screen.cy, &gc);
        screen.cx = screen.cx.saturating_add(width);
    }

    fn parse_params(&mut self) {
        self.nparams = 0;
        if self.param_len == 0 {
            self.params[0] = 0;
            self.nparams = 1;
            return;
        }
        let mut val: i32 = 0;
        let mut have_val = false;
        for &b in &self.param_buf[..self.param_len] {
            match b {
                b'0'..=b'9' => {
                    val = val.saturating_mul(10).saturating_add((b - b'0') as i32);
                    have_val = true;
                }
                b';' => {
                    if self.nparams < self.params.len() {
                        self.params[self.nparams] = if have_val { val } else { -1 };
                        self.nparams += 1;
                    }
                    val = 0;
                    have_val = false;
                }
                _ => {}
            }
        }
        if self.nparams < self.params.len() {
            self.params[self.nparams] = if have_val { val } else { -1 };
            self.nparams += 1;
        }
    }

    fn param(&self, idx: usize) -> i32 {
        if idx < self.nparams {
            self.params[idx]
        } else {
            0
        }
    }

    fn param_or(&self, idx: usize, default: i32) -> i32 {
        if idx < self.nparams && self.params[idx] >= 0 {
            self.params[idx]
        } else {
            default
        }
    }

    fn interm_str(&self) -> &str {
        core::str::from_utf8(&self.interm_buf[..self.interm_len]).unwrap_or("")
    }

    /// Reset the parser state (e.g. after a fatal decode error).
    pub fn reset(&mut self) {
        self.state = InputState::Ground;
        self.interm_len = 0;
        self.param_len = 0;
        self.nparams = 0;
        self.input_buf.clear();
        self.cell = GridCell::default_cell();
        self.utf8_expected = 0;
        self.utf8_have = 0;
        self.response.clear();
    }
}

// ── Character width helper ──

fn char_width(c: char) -> usize {
    if c.is_ascii() {
        return 1;
    }
    let code = c as u32;
    if (0x1100..=0x115F).contains(&code)
        || code == 0x2329 || code == 0x232A
        || (0x2E80..=0x303E).contains(&code)
        || (0x3040..=0xA4CF).contains(&code)
        || (0xA960..=0xA97F).contains(&code)
        || (0xAC00..=0xD7A3).contains(&code)
        || (0xD7B0..=0xD7FF).contains(&code)
        || (0xFE10..=0xFE19).contains(&code)
        || (0xFE30..=0xFE6F).contains(&code)
        || (0xFF01..=0xFF60).contains(&code)
        || (0xFFE0..=0xFFE6).contains(&code)
        || (0x1B000..=0x1B0FF).contains(&code)
        || (0x1B100..=0x1B12F).contains(&code)
        || (0x1F004..=0x1F9CF).contains(&code)
        || (0x20000..=0x2FFFD).contains(&code)
        || (0x30000..=0x3FFFD).contains(&code)
    {
        2
    } else {
        1
    }
}

// ── Handler functions ──

fn handle_c0(p: &mut Parser, ch: u8, screen: &mut Screen) {
    match ch {
        0x07 => {
            p.bell = true; // BEL — report to the server for window alerts (B6)
        }
        0x08 => {
            // Backspace
            p.dirty = true;
            screen.cx = screen.cx.saturating_sub(1);
        }
        0x09 => {
            // Horizontal tab
            p.dirty = true;
            let tab_width = 8u32;
            let next = ((screen.cx / tab_width) + 1) * tab_width;
            screen.cx = next.min(screen.size_x().saturating_sub(1));
        }
        0x0a | 0x0b | 0x0c => {
            // LF / VT / FF
            p.dirty = true;
            screen.cx = 0;
            screen.cy = screen.cy.saturating_add(1);
            if screen.cy >= screen.size_y() {
                screen.cy = screen.size_y().saturating_sub(1);
                if screen.rupper == 0 && screen.rlower == screen.size_y().saturating_sub(1) {
                    screen.grid.scroll_up();
                } else {
                    screen.grid.scroll_region_up(screen.rupper, screen.rlower);
                }
            }
        }
        0x0d => {
            // CR
            p.dirty = true;
            screen.cx = 0;
        }
        _ => {}
    }
}

fn handle_print(p: &mut Parser, ch: u8, screen: &mut Screen) {
    // ch is 0x20..=0x7E: always a valid ASCII printable character
    p.write_char(ch as char, screen);
}

fn handle_highbyte(p: &mut Parser, ch: u8, _screen: &mut Screen) {
    // Multi-byte UTF-8 leading byte: 0xC0-0xF4
    let expected = match ch {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0, // stray continuation byte or invalid
    };
    if expected > 0 {
        p.utf8_buf[0] = ch;
        p.utf8_have = 1;
        p.utf8_expected = expected;
    }
    // Stray continuation byte (0x80-0xBF): silently discard
}

fn handle_intermediate(p: &mut Parser, ch: u8, _screen: &mut Screen) {
    if p.interm_len < p.interm_buf.len() {
        p.interm_buf[p.interm_len] = ch;
        p.interm_len += 1;
    }
}

fn handle_parameter(p: &mut Parser, ch: u8, _screen: &mut Screen) {
    if p.param_len < p.param_buf.len() {
        p.param_buf[p.param_len] = ch;
        p.param_len += 1;
    }
}

fn handle_csi_dispatch(p: &mut Parser, ch: u8, screen: &mut Screen) {
    p.parse_params();
    let interm = p.interm_str();
    let cmd = lookup_csi(ch, interm);
    dispatch_csi_command(p, cmd, screen);
    // Query sequences (DSR / DA) only produce a response, no screen change.
    if !matches!(cmd, CsiType::Dsr | CsiType::DsrPrivate | CsiType::Da | CsiType::DaTwo) {
        p.dirty = true;
    }
    p.interm_len = 0;
    p.param_len = 0;
}

fn handle_esc_dispatch(p: &mut Parser, ch: u8, screen: &mut Screen) {
    p.dirty = true;
    dispatch_esc(p, ch, screen);
}

fn handle_dcs_dispatch(_p: &mut Parser, _ch: u8, _screen: &mut Screen) {
    // DCS strings are discarded for now
}

/// Accumulate a byte of DCS/OSC/APC string body.
fn handle_input(p: &mut Parser, ch: u8, _screen: &mut Screen) {
    if p.input_buf.len() < 8192 {
        p.input_buf.push(ch);
    }
}

fn handle_osc_finish(p: &mut Parser, _ch: u8, screen: &mut Screen) {
    // OSC body is "Ps;Pt". Ps = 0/1/2 sets the window title to Pt.
    if let Some(pos) = p.input_buf.iter().position(|&b| b == b';') {
        let mut s = String::new();
        for &b in &p.input_buf[..pos] {
            s.push(b as char);
        }
        let cmd: i32 = s.parse().unwrap_or(-1);
        let title = String::from_utf8_lossy(&p.input_buf[pos + 1..]).to_string();
        match cmd {
            0 | 1 | 2 => {
                screen.title = title;
            }
            _ => {}
        }
    }
    p.input_buf.clear();
}

// ── CSI dispatch ──

fn lookup_csi(ch: u8, interm: &str) -> CsiType {
    use CsiType::*;
    match (ch, interm) {
        (b'@', _) => Ich,
        (b'A', _) => Cuu,
        (b'B', _) => Cud,
        (b'C', _) => Cuf,
        (b'D', _) => Cub,
        (b'E', _) => Cnl,
        (b'F', _) => Cpl,
        (b'G', _) => Hpa,
        (b'H', _) => Cup,
        (b'J', _) => Ed,
        (b'K', _) => El,
        (b'L', _) => Il,
        (b'M', _) => Dl,
        (b'P', _) => Dch,
        (b'S', "") => Su,
        (b'S', "?") => SmGraphics,
        (b'T', _) => Sd,
        (b'X', _) => Ech,
        (b'Z', _) => Cbt,
        (b'`', _) => Hpa,
        (b'b', _) => Rep,
        (b'c', "") => Da,
        (b'c', ">") => DaTwo,
        (b'd', _) => Vpa,
        (b'f', _) => Cup,
        (b'g', _) => Tbc,
        (b'h', "") => Sm,
        (b'h', "?") => SmPrivate,
        (b'l', "") => Rm,
        (b'l', "?") => RmPrivate,
        (b'm', "") => Sgr,
        (b'm', ">") => Modset,
        (b'n', "") => Dsr,
        (b'n', ">") => Modoff,
        (b'n', "?") => DsrPrivate,
        (b'p', "$") => Query,
        (b'p', "?$") => QueryPrivate,
        (b'q', " ") => Decscusr,
        (b'q', ">") => Xda,
        (b'r', _) => Decstbm,
        (b's', _) => Scp,
        (b't', _) => Winops,
        (b'u', _) => Rcp,
        _ => Unknown,
    }
}

fn dispatch_csi_command(p: &mut Parser, cmd: CsiType, screen: &mut Screen) {
    use CsiType::*;
    match cmd {
        Cup => {
            let row = p.param_or(0, 1).max(1) as u32;
            let col = p.param_or(1, 1).max(1) as u32;
            screen.cx = col.saturating_sub(1).min(screen.size_x().saturating_sub(1));
            screen.cy = row.saturating_sub(1).min(screen.size_y().saturating_sub(1));
        }
        Cuu => {
            let n = p.param_or(0, 1).max(1) as u32;
            screen.cy = screen.cy.saturating_sub(n);
        }
        Cud => {
            let n = p.param_or(0, 1).max(1) as u32;
            let cy = screen.cy.saturating_add(n);
            screen.cy = cy.min(screen.size_y().saturating_sub(1));
        }
        Cuf => {
            let n = p.param_or(0, 1).max(1) as u32;
            let cx = screen.cx.saturating_add(n);
            screen.cx = cx.min(screen.size_x().saturating_sub(1));
        }
        Cub => {
            let n = p.param_or(0, 1).max(1) as u32;
            screen.cx = screen.cx.saturating_sub(n);
        }
        Cnl => {
            let n = p.param_or(0, 1).max(1) as u32;
            let cx = screen.cx.saturating_add(n).min(screen.size_x().saturating_sub(1));
            screen.cx = cx;
        }
        Cpl => {
            let n = p.param_or(0, 1).max(1) as u32;
            let cy = screen.cy.saturating_add(n).min(screen.size_y().saturating_sub(1));
            screen.cy = cy;
        }
        Su => {
            // CSI S — Scroll *n* lines up within the current scroll region.
            let n = p.param_or(0, 1).max(1) as u32;
            for _ in 0..n {
                screen.grid.scroll_region_up(screen.rupper, screen.rlower);
            }
        }
        Sd => {
            // CSI T — Scroll down. Not supported by the grid yet; no-op for P0.
            let _ = p.param_or(0, 1);
        }
        Hpa => {
            let col = p.param_or(0, 1).max(1) as u32;
            screen.cx = col.saturating_sub(1).min(screen.size_x().saturating_sub(1));
        }
        Vpa => {
            let row = p.param_or(0, 1).max(1) as u32;
            screen.cy = row.saturating_sub(1).min(screen.size_y().saturating_sub(1));
        }
        Ed => {
            let n = p.param(0);
            match n {
                0 | -1 => {
                    // Clear from cursor to end of screen
                    for y in screen.cy..screen.size_y() {
                        let x_start = if y == screen.cy { screen.cx } else { 0 };
                        for x in x_start..screen.size_x() {
                            screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                        }
                    }
                }
                1 => {
                    // Clear from beginning of screen to cursor
                    for y in 0..=screen.cy {
                        let x_end = if y == screen.cy {
                            screen.cx.saturating_add(1)
                        } else {
                            screen.size_x()
                        };
                        for x in 0..x_end {
                            screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                        }
                    }
                }
                2 | 3 => {
                    // Clear entire screen
                    for y in 0..screen.size_y() {
                        for x in 0..screen.size_x() {
                            screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                        }
                    }
                }
                _ => {}
            }
        }
        El => {
            let n = p.param(0);
            let y = screen.cy;
            match n {
                0 | -1 => {
                    for x in screen.cx..screen.size_x() {
                        screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                    }
                }
                1 => {
                    for x in 0..=screen.cx {
                        screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                    }
                }
                2 => {
                    for x in 0..screen.size_x() {
                        screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                    }
                }
                _ => {}
            }
        }
        Sgr => {
            let mut i = 0;
            while i < p.nparams {
                let s = p.param(i);
                match s {
                    0 => p.cell = GridCell::default_cell(),
                    1 => p.cell.attr |= GRID_ATTR_BRIGHT,
                    2 => p.cell.attr |= GRID_ATTR_DIM,
                    3 => p.cell.attr |= GRID_ATTR_ITALICS,
                    4 => p.cell.attr |= GRID_ATTR_UNDERSCORE,
                    5 => p.cell.attr |= GRID_ATTR_BLINK,
                    7 => p.cell.attr |= GRID_ATTR_REVERSE,
                    8 => p.cell.attr |= GRID_ATTR_HIDDEN,
                    9 => p.cell.attr |= GRID_ATTR_STRIKETHROUGH,
                    22 => p.cell.attr &= !(GRID_ATTR_BRIGHT | GRID_ATTR_DIM),
                    23 => p.cell.attr &= !GRID_ATTR_ITALICS,
                    24 => p.cell.attr &= !GRID_ATTR_UNDERSCORE,
                    25 => p.cell.attr &= !GRID_ATTR_BLINK,
                    27 => p.cell.attr &= !GRID_ATTR_REVERSE,
                    28 => p.cell.attr &= !GRID_ATTR_HIDDEN,
                    29 => p.cell.attr &= !GRID_ATTR_STRIKETHROUGH,
                    30..=37 => p.cell.fg = (s - 30) as i32,
                    38 => {
                        i += 1;
                        handle_sgr_256_or_rgb(p, &mut i, true);
                    }
                    39 => p.cell.fg = 8,
                    40..=47 => p.cell.bg = (s - 40) as i32,
                    48 => {
                        i += 1;
                        handle_sgr_256_or_rgb(p, &mut i, false);
                    }
                    49 => p.cell.bg = 8,
                    53 => p.cell.attr |= GRID_ATTR_OVERLINE,
                    55 => p.cell.attr &= !GRID_ATTR_OVERLINE,
                    90..=97 => p.cell.fg = (s - 90 + 8) as i32,
                    100..=107 => p.cell.bg = (s - 100 + 8) as i32,
                    _ => {}
                }
                i += 1;
            }
        }
        Decstbm => {
            // DECSTBM: 1-based inclusive rows.
            let top = p.param_or(0, 1).max(1) as u32;
            let bot = p.param_or(1, screen.size_y() as i32).max(1) as u32;
            let last = screen.size_y().saturating_sub(1);
            screen.rupper = top.saturating_sub(1).min(last);
            screen.rlower = bot.saturating_sub(1).min(last);
            screen.cx = 0;
            screen.cy = screen.rupper;
        }
        Decscusr => {
            let n = p.param_or(0, 0);
            screen.cstyle = match n {
                0 | 1 => CursorStyle::Block,
                2 => CursorStyle::Underline,
                3 | 4 => CursorStyle::Bar,
                _ => CursorStyle::Default,
            };
        }
        SmPrivate | RmPrivate => {
            let is_set = matches!(cmd, SmPrivate);
            for i in 0..p.nparams {
                let m = p.param(i);
                handle_private_mode(m, is_set, screen);
            }
        }
        Sm => {
            for _i in 0..p.nparams {
                let _ = p.param(_i);
            }
        }
        Rm => {
            for _i in 0..p.nparams {
                let _ = p.param(_i);
            }
        }
        // ── Insert / Delete / Erase ──
        Ich => {
            let n = p.param_or(0, 1).max(1) as u32;
            screen.grid.insert_chars(screen.cx, screen.cy, n);
        }
        Dch => {
            let n = p.param_or(0, 1).max(1) as u32;
            screen.grid.delete_chars(screen.cx, screen.cy, n);
        }
        Il => {
            let n = p.param_or(0, 1).max(1) as u32;
            screen.grid.insert_lines(screen.rupper, screen.rlower, screen.cy, n);
        }
        Dl => {
            let n = p.param_or(0, 1).max(1) as u32;
            screen.grid.delete_lines(screen.rupper, screen.rlower, screen.cy, n);
        }
        Ech => {
            let n = p.param_or(0, 1).max(1) as u32;
            screen.grid.erase_chars(screen.cx, screen.cy, n);
        }
        Cbt => {
            let mut cx = screen.cx;
            loop {
                if cx == 0 {
                    break;
                }
                cx -= 1;
                if screen.tabs.get(cx as usize).copied().unwrap_or(false) {
                    break;
                }
            }
            screen.cx = cx;
        }
        Rep => {
            // CSI b — Repeat Character. Would repeat the last graphic char,
            // but we don't track it yet; treat as no-op.
            let _ = p.param_or(0, 1);
        }
        // ── Device queries (write response back to PTY) ──
        Dsr => {
            let q = p.param(0);
            match q {
                5 => {
                    // Device status: ready
                    p.response.extend_from_slice(b"\x1b[0n");
                }
                6 => {
                    // Cursor position report
                    let row = screen.cy + 1;
                    let col = screen.cx + 1;
                    p.response.extend_from_slice(
                        format!("\x1b[{};{}R", row, col).as_bytes(),
                    );
                }
                _ => {}
            }
        }
        DsrPrivate => {
            let q = p.param(0);
            match q {
                6 => {
                    let row = screen.cy + 1;
                    let col = screen.cx + 1;
                    p.response.extend_from_slice(
                        format!("\x1b[{};{}R", row, col).as_bytes(),
                    );
                }
                15 => {
                    p.response.extend_from_slice(b"\x1b[?62c");
                }
                _ => {}
            }
        }
        Da => {
            // Primary device attributes (VT100)
            p.response.extend_from_slice(b"\x1b[?6c");
        }
        DaTwo => {
            // Secondary device attributes
            p.response.extend_from_slice(b"\x1b[?62;115c");
        }
        Tbc => {
            let n = p.param(0);
            match n {
                0 => {
                    if let Some(pos) = screen.tabs.get_mut(screen.cx as usize) {
                        *pos = false;
                    }
                }
                3 => {
                    screen.tabs.fill(false);
                }
                _ => {}
            }
        }
        Scp => {
            // Save cursor position
            screen.decsc_cx = screen.cx;
            screen.decsc_cy = screen.cy;
        }
        Rcp => {
            // Restore cursor position
            screen.cx = screen.decsc_cx;
            screen.cy = screen.decsc_cy;
        }
        Winops => {}
        Query | QueryPrivate | Xda | Modset | Modoff | SmGraphics => {}
        Unknown => {}
    }
}

fn handle_private_mode(mode: i32, is_set: bool, screen: &mut Screen) {
    match mode {
        1 => {
            // DECCKM - cursor key mode
        }
        7 => {
            // DECAWM - auto wrap
            if is_set {
                screen.mode |= 1 << 0;
            } else {
                screen.mode &= !(1 << 0);
            }
        }
        25 => {
            // DECTCEM - cursor visibility
            if is_set {
                screen.mode |= 1 << 1;
            } else {
                screen.mode &= !(1 << 1);
            }
        }
        47 | 1047 | 1049 => {
            if is_set {
                screen.to_alt();
            } else {
                screen.to_main();
            }
        }
        _ => {}
    }
}

/// Handle `38`/`48` extension: `5;<n>` (256-colour) or `2;<r>;<g>;<b>` (RGB).
/// `i` points at the selector byte (5 or 2); advances past the consumed params.
fn handle_sgr_256_or_rgb(p: &mut Parser, i: &mut usize, foreground: bool) {
    let selector = p.param(*i);
    match selector {
        5 => {
            // 38;5;N
            let n = p.param(*i + 1);
            let idx = n & 0xff;
            if foreground {
                p.cell.fg = idx | 0x01000000;
            } else {
                p.cell.bg = idx | 0x01000000;
            }
            *i += 1;
        }
        2 => {
            // 38;2;R;G;B
            let r = p.param(*i + 1);
            let g = p.param(*i + 2);
            let b = p.param(*i + 3);
            let rgb = loom_core::colour::COLOUR_FLAG_RGB
                | ((r & 0xff) << 16)
                | ((g & 0xff) << 8)
                | (b & 0xff);
            if foreground {
                p.cell.fg = rgb;
            } else {
                p.cell.bg = rgb;
            }
            *i += 3;
        }
        _ => {
            *i += 1;
        }
    }
}

// ── ESC dispatch ──

fn dispatch_esc(p: &mut Parser, ch: u8, screen: &mut Screen) {
    if p.interm_len == 0 {
        match ch {
            b'D' => {
                // IND - Index
                screen.cy = screen.cy.saturating_add(1);
                if screen.cy >= screen.size_y() {
                    screen.cy = screen.size_y().saturating_sub(1);
                    if screen.rupper == 0 && screen.rlower == screen.size_y().saturating_sub(1) {
                        screen.grid.scroll_up();
                    } else {
                        screen.grid.scroll_region_up(screen.rupper, screen.rlower);
                    }
                }
            }
            b'E' => {
                // NEL - Next Line
                screen.cx = 0;
                screen.cy = screen.cy.saturating_add(1);
                if screen.cy >= screen.size_y() {
                    screen.cy = screen.size_y().saturating_sub(1);
                    if screen.rupper == 0 && screen.rlower == screen.size_y().saturating_sub(1) {
                        screen.grid.scroll_up();
                    } else {
                        screen.grid.scroll_region_up(screen.rupper, screen.rlower);
                    }
                }
            }
            b'M' => {
                // RI - Reverse Index
                screen.cy = screen.cy.saturating_sub(1);
            }
            b'7' => {
                // DECSC - Save cursor
                screen.decsc_cx = screen.cx;
                screen.decsc_cy = screen.cy;
            }
            b'8' => {
                // DECRC - Restore cursor
                screen.cx = screen.decsc_cx;
                screen.cy = screen.decsc_cy;
            }
            b'c' => {
                // RIS - Reset to initial state
                screen.cx = 0;
                screen.cy = 0;
                p.cell = GridCell::default_cell();
                screen.rupper = 0;
                screen.rlower = screen.size_y().saturating_sub(1);
            }
            _ => {}
        }
    }
}

// ── Transition tables ──

#[rustfmt::skip]
const GROUND_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x1b, 0x1b, None, Some(InputState::EscEnter)),
    Transition::new(0x1c, 0x1f, Some(handle_c0), None),
    Transition::new(0x20, 0x7e, Some(handle_print), None),
    Transition::new(0x7f, 0x7f, None, None),
    Transition::new(0x80, 0xff, Some(handle_highbyte), None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const ESC_ENTER_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), Some(InputState::EscIntermediate)),
    Transition::new(0x30, 0x4f, Some(handle_esc_dispatch), Some(InputState::Ground)),
    Transition::new(0x50, 0x50, None, Some(InputState::DcsEnter)),
    Transition::new(0x51, 0x57, Some(handle_esc_dispatch), Some(InputState::Ground)),
    Transition::new(0x58, 0x58, None, Some(InputState::ConsumeSt)),
    Transition::new(0x59, 0x59, Some(handle_esc_dispatch), Some(InputState::Ground)),
    Transition::new(0x5a, 0x5a, Some(handle_esc_dispatch), Some(InputState::Ground)),
    Transition::new(0x5b, 0x5b, None, Some(InputState::CsiEnter)),
    Transition::new(0x5c, 0x5c, Some(handle_esc_dispatch), Some(InputState::Ground)),
    Transition::new(0x5d, 0x5d, None, Some(InputState::OscString)),
    Transition::new(0x5e, 0x5e, None, Some(InputState::ConsumeSt)),
    Transition::new(0x5f, 0x5f, None, Some(InputState::ApcString)),
    Transition::new(0x60, 0x6a, Some(handle_esc_dispatch), Some(InputState::Ground)),
    Transition::new(0x6b, 0x6b, None, Some(InputState::RenameString)),
    Transition::new(0x6c, 0x7e, Some(handle_esc_dispatch), Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const ESC_INTERMEDIATE_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), None),
    Transition::new(0x30, 0x7e, Some(handle_esc_dispatch), Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const CSI_ENTER_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), Some(InputState::CsiIntermediate)),
    Transition::new(0x30, 0x39, Some(handle_parameter), Some(InputState::CsiParameter)),
    Transition::new(0x3a, 0x3a, Some(handle_parameter), Some(InputState::CsiParameter)),
    Transition::new(0x3b, 0x3b, Some(handle_parameter), Some(InputState::CsiParameter)),
    Transition::new(0x3c, 0x3f, Some(handle_intermediate), Some(InputState::CsiParameter)),
    Transition::new(0x40, 0x7e, Some(handle_csi_dispatch), Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const CSI_PARAMETER_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), Some(InputState::CsiIntermediate)),
    Transition::new(0x30, 0x39, Some(handle_parameter), None),
    Transition::new(0x3a, 0x3a, Some(handle_parameter), None),
    Transition::new(0x3b, 0x3b, Some(handle_parameter), None),
    Transition::new(0x3c, 0x3f, None, Some(InputState::CsiIgnore)),
    Transition::new(0x40, 0x7e, Some(handle_csi_dispatch), Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const CSI_INTERMEDIATE_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), None),
    Transition::new(0x30, 0x3f, None, Some(InputState::CsiIgnore)),
    Transition::new(0x40, 0x7e, Some(handle_csi_dispatch), Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const CSI_IGNORE_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x20, 0x3f, None, None),
    Transition::new(0x40, 0x7e, None, Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const DCS_ENTER_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x18, 0x18, None, None),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1a, 0x1a, None, None),
    Transition::new(0x1b, 0x1b, None, Some(InputState::DcsIgnore)),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), Some(InputState::DcsIntermediate)),
    Transition::new(0x30, 0x39, Some(handle_parameter), Some(InputState::DcsParameter)),
    Transition::new(0x3a, 0x3a, None, Some(InputState::DcsIgnore)),
    Transition::new(0x3b, 0x3b, Some(handle_parameter), Some(InputState::DcsParameter)),
    Transition::new(0x3c, 0x3f, Some(handle_intermediate), Some(InputState::DcsParameter)),
    Transition::new(0x40, 0x7e, Some(handle_input), Some(InputState::DcsHandler)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const DCS_PARAMETER_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x18, 0x18, None, None),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1a, 0x1a, None, None),
    Transition::new(0x1b, 0x1b, None, Some(InputState::DcsIgnore)),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), Some(InputState::DcsIntermediate)),
    Transition::new(0x30, 0x39, Some(handle_parameter), None),
    Transition::new(0x3a, 0x3a, None, Some(InputState::DcsIgnore)),
    Transition::new(0x3b, 0x3b, Some(handle_parameter), None),
    Transition::new(0x3c, 0x3f, None, Some(InputState::DcsIgnore)),
    Transition::new(0x40, 0x7e, Some(handle_input), Some(InputState::DcsHandler)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const DCS_INTERMEDIATE_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x18, 0x18, None, None),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1a, 0x1a, None, None),
    Transition::new(0x1b, 0x1b, None, Some(InputState::DcsIgnore)),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), None),
    Transition::new(0x30, 0x3f, None, Some(InputState::DcsIgnore)),
    Transition::new(0x40, 0x7e, Some(handle_input), Some(InputState::DcsHandler)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const DCS_HANDLER_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x1a, Some(handle_input), None),
    Transition::new(0x1b, 0x1b, None, Some(InputState::DcsEscape)),
    Transition::new(0x1c, 0xff, Some(handle_input), None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const DCS_ESCAPE_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x5b, Some(handle_input), Some(InputState::DcsHandler)),
    Transition::new(0x5c, 0x5c, Some(handle_dcs_dispatch), Some(InputState::Ground)),
    Transition::new(0x5d, 0xff, Some(handle_input), Some(InputState::DcsHandler)),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const DCS_IGNORE_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x18, 0x18, None, None),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1a, 0x1a, None, None),
    Transition::new(0x1b, 0x1b, None, Some(InputState::DcsIgnore)),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const OSC_STRING_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x06, None, None),
    Transition::new(0x07, 0x07, Some(handle_osc_finish), Some(InputState::Ground)),
    Transition::new(0x08, 0x17, None, None),
    Transition::new(0x18, 0x18, None, Some(InputState::Ground)),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1a, 0x1a, None, Some(InputState::Ground)),
    Transition::new(0x1b, 0x1b, None, Some(InputState::EscEnter)),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0xff, Some(handle_input), None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const APC_STRING_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x18, 0x18, None, Some(InputState::Ground)),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1a, 0x1a, None, Some(InputState::Ground)),
    Transition::new(0x1b, 0x1b, None, Some(InputState::EscEnter)),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0xff, Some(handle_input), None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const RENAME_STRING_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x18, 0x18, None, Some(InputState::Ground)),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1a, 0x1a, None, Some(InputState::Ground)),
    Transition::new(0x1b, 0x1b, None, Some(InputState::EscEnter)),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0xff, Some(handle_input), None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const CONSUME_ST_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x18, 0x18, None, Some(InputState::Ground)),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1a, 0x1a, None, Some(InputState::Ground)),
    Transition::new(0x1b, 0x1b, None, Some(InputState::EscEnter)),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0x2f, None, None),
    Transition::new(0x30, 0x7e, None, Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sgr_colors() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();

        // ESC [ 3 1 m - set red foreground
        p.parse_buf(&mut screen, b"\x1b[31m");

        assert_eq!(p.cell.fg, 1);
    }

    #[test]
    fn test_sgr_bold_red() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();

        // ESC [ 1 ; 31 m - bold, red
        p.parse_buf(&mut screen, b"\x1b[1;31m");

        assert!(p.cell.attr & GRID_ATTR_BRIGHT != 0);
        assert_eq!(p.cell.fg, 1);
    }

    #[test]
    fn test_cursor_movement() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();

        // ESC [ 10 ; 20 H - cursor to (19, 9)
        p.parse_buf(&mut screen, b"\x1b[10;20H");

        assert_eq!(screen.cy, 9);
        assert_eq!(screen.cx, 19);
    }

    #[test]
    fn test_clear_screen() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();

        // Write a character at cursor
        p.parse_buf(&mut screen, b"A");
        assert_eq!(screen.cx, 1);
        assert_eq!(screen.cy, 0);

        // ESC [ 2 J - clear entire screen
        p.parse_buf(&mut screen, b"\x1b[2J");

        // ED does not move cursor
        assert_eq!(screen.cx, 1);
        assert_eq!(screen.cy, 0);
    }

    #[test]
    fn test_scroll_region() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();

        // ESC [ 3 ; 22 r - scroll region rows 3-22
        p.parse_buf(&mut screen, b"\x1b[3;22r");

        assert_eq!(screen.rupper, 2);
        assert_eq!(screen.rlower, 21);
    }

    #[test]
    fn test_utf8_cjk() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();
        // Feed "中" (E4 B8 AD) split across calls
        p.parse(0xE4, &mut screen);
        p.parse(0xB8, &mut screen);
        p.parse(0xAD, &mut screen);
        assert_eq!(screen.grid.view_get_cell(0, 0).unwrap().data.to_char(), '中');
        assert_eq!(screen.cx, 2); // wide char takes 2 columns
    }

    #[test]
    fn test_utf8_across_reads() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();
        // Feed "中" split into two parse_buf calls
        p.parse_buf(&mut screen, &[0xE4, 0xB8]);
        assert_eq!(screen.grid.view_get_cell(0, 0).unwrap().data.to_char(), ' ');
        p.parse_buf(&mut screen, &[0xAD]);
        assert_eq!(screen.grid.view_get_cell(0, 0).unwrap().data.to_char(), '中');
    }

    #[test]
    fn test_dsr_response() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();
        // Move cursor to row 4, col 6
        p.parse_buf(&mut screen, b"\x1b[4;6H");
        // Request cursor position report
        p.parse_buf(&mut screen, b"\x1b[6n");
        assert_eq!(p.take_response(), b"\x1b[4;6R");
    }

    #[test]
    fn test_da_response() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();
        p.parse_buf(&mut screen, b"\x1b[c");
        assert_eq!(p.take_response(), b"\x1b[?6c");
    }

    #[test]
    fn test_alt_screen() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();

        // Write to main screen
        p.parse_buf(&mut screen, b"HELLO");
        assert_eq!(screen.grid.view_get_cell(0, 0).unwrap().data.to_char(), 'H');

        // Switch to alt screen
        p.parse_buf(&mut screen, b"\x1b[?1049h");
        assert!(screen.in_alt);
        assert_eq!(screen.grid.view_get_cell(0, 0).unwrap().data.to_char(), ' ');

        // Write to alt screen
        p.parse_buf(&mut screen, b"VIM");
        assert_eq!(screen.grid.view_get_cell(0, 0).unwrap().data.to_char(), 'V');

        // Switch back to main screen
        p.parse_buf(&mut screen, b"\x1b[?1049l");
        assert!(!screen.in_alt);
        assert_eq!(screen.grid.view_get_cell(0, 0).unwrap().data.to_char(), 'H');
    }

    #[test]
    fn test_insert_chars() {
        let mut screen = Screen::new(80, 24);
        let mut p = Parser::new();
        p.parse_buf(&mut screen, b"Hello World");
        // Cursor is at col 11
        p.parse_buf(&mut screen, b"\x1b[1;1H"); // cursor to (0,0)
        p.parse_buf(&mut screen, b"\x1b[3@");   // ICH 3
        let s: String = (0..14).map(|i| screen.grid.view_get_cell(i, 0).unwrap().data.to_char()).collect();
        assert_eq!(s, "   Hello World");
    }
}
