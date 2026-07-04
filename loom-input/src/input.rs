use loom_core::grid_cell::*;
use loom_core::screen::{CursorStyle, Screen};
use loom_core::utf8::Utf8Data;

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

const ANYWHERE: &[Transition] = &[
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x1b, 0x1b, None, Some(InputState::EscEnter)),
];

pub struct Transition {
    first: u8,
    last: u8,
    handler: Option<fn(&mut InputCtx, u8)>,
    next_state: Option<InputState>,
}

impl Transition {
    pub const fn new(
        first: u8,
        last: u8,
        handler: Option<fn(&mut InputCtx, u8)>,
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
}

// ── Input parser context ──

pub struct InputCtx<'a> {
    pub screen: &'a mut Screen,
    pub state: InputState,

    // intermediate bytes buffer
    interm_buf: [u8; 4],
    interm_len: usize,

    // parameter bytes buffer (raw)
    param_buf: [u8; 64],
    param_len: usize,

    // parsed parameters
    params: [i32; 24],
    nparams: usize,

    // input buffer for OSC/DCS/APC
    input_buf: Vec<u8>,

    // current cell being built
    pub cell: GridCell,
    // flags
    flags: u8,

    // UTF-8 state
    utf8_data: Utf8Data,
    utf8_started: bool,
}

impl<'a> InputCtx<'a> {
    pub fn new(screen: &'a mut Screen) -> Self {
        Self {
            screen,
            state: InputState::Ground,
            interm_buf: [0u8; 4],
            interm_len: 0,
            param_buf: [0u8; 64],
            param_len: 0,
            params: [0i32; 24],
            nparams: 0,
            input_buf: Vec::with_capacity(256),
            cell: GridCell::default_cell(),
            flags: 0,
            utf8_data: Utf8Data::space(),
            utf8_started: false,
        }
    }

    /// Parse a single byte of terminal input.
    pub fn parse(&mut self, ch: u8) {
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

        // Find matching transition
        for tr in table {
            if tr.first == 255 { break; }
            if ch >= tr.first && ch <= tr.last {
                if let Some(handler) = tr.handler {
                    handler(self, ch);
                }
                if let Some(next) = tr.next_state {
                    self.state = next;
                }
                return;
            }
        }
    }

    /// Parse a buffer of input data.
    pub fn parse_buf(&mut self, buf: &[u8]) {
        for &ch in buf {
            self.parse(ch);
        }
    }

    fn clear(&mut self) {
        self.interm_len = 0;
        self.param_len = 0;
        self.nparams = 0;
        self.input_buf.clear();
    }

    fn collect_parameter(&mut self) {
        if self.param_len >= self.param_buf.len() {
            return;
        }
        self.param_buf[self.param_len] = self.param_buf[self.param_len]; // no-op, we track param_len separately
        self.param_len += 1;
    }

    fn collect_intermediate(&mut self, ch: u8) {
        if self.interm_len >= self.interm_buf.len() {
            return;
        }
        self.interm_buf[self.interm_len] = ch;
        self.interm_len += 1;
    }

    fn collect_input(&mut self, ch: u8) {
        self.input_buf.push(ch);
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
        for &b in self.param_buf[..self.param_len].iter() {
            match b {
                b'0'..=b'9' => {
                    val = val * 10 + (b - b'0') as i32;
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
                _ => {
                    // private marker or something else
                    if b == b'?' || b == b'>' || b == b'\'' || b == b'"' || b == b' ' {
                        // These are handled by intermediate bytes
                    }
                }
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

    fn has_intermediate(&self, c: u8) -> bool {
        self.interm_len == 1 && self.interm_buf[0] == c
    }

    fn interm_str(&self) -> &str {
        core::str::from_utf8(&self.interm_buf[..self.interm_len])
            .unwrap_or("")
    }
}

// ── Handler functions ──

fn handle_c0(ctx: &mut InputCtx, ch: u8) {
    // C0 control codes
    match ch {
        0x07 => {} // BEL - ignore for now
        0x08 => { ctx.screen.cx = ctx.screen.cx.saturating_sub(1); } // BS
        0x09 => { // HT - horizontal tab
            let tab = (ctx.screen.cx / 8 + 1) * 8;
            ctx.screen.cx = tab.min(ctx.screen.size_x().saturating_sub(1));
        }
        0x0a | 0x0b | 0x0c => { // LF, VT, FF
            ctx.screen.cy += 1;
            if ctx.screen.cy >= ctx.screen.size_y() {
                ctx.screen.cy = ctx.screen.size_y() - 1;
                ctx.screen.grid.scroll_history();
            }
        }
        0x0d => { ctx.screen.cx = 0; } // CR
        0x1b => {} // ESC - handled by state machine
        _ => {}
    }
}

fn handle_print(ctx: &mut InputCtx, ch: u8) {
    // Output a printable character to the screen
    if ctx.screen.cx as usize >= ctx.screen.size_x() as usize {
        return;
    }
    let gc = GridCell {
        data: Utf8Data::new(ch as char),
        ..ctx.cell
    };
    ctx.screen.grid.view_set_cell(ctx.screen.cx, ctx.screen.cy, &gc);
    ctx.screen.cx += 1;
    if ctx.screen.cx >= ctx.screen.size_x() {
        ctx.screen.cx = 0;
        ctx.screen.cy += 1;
        if ctx.screen.cy >= ctx.screen.size_y() {
            ctx.screen.cy = ctx.screen.size_y() - 1;
            ctx.screen.grid.scroll_history();
        }
    }
}

fn handle_intermediate(ctx: &mut InputCtx, ch: u8) {
    if ctx.interm_len < ctx.interm_buf.len() {
        ctx.interm_buf[ctx.interm_len] = ch;
        ctx.interm_len += 1;
    }
}

fn handle_parameter(ctx: &mut InputCtx, ch: u8) {
    if ctx.param_len < ctx.param_buf.len() {
        ctx.param_buf[ctx.param_len] = ch;
        ctx.param_len += 1;
    }
}

fn handle_csi_dispatch(ctx: &mut InputCtx, ch: u8) {
    ctx.parse_params();
    dispatch_csi(ctx, ch);
}

fn handle_esc_dispatch(ctx: &mut InputCtx, ch: u8) {
    dispatch_esc(ctx, ch);
}

fn handle_input(ctx: &mut InputCtx, ch: u8) {
    ctx.input_buf.push(ch);
}

fn handle_dcs_dispatch(_ctx: &mut InputCtx, _ch: u8) {}

fn handle_top_bit(_ctx: &mut InputCtx, _ch: u8) {
}

fn handle_end_bel(_ctx: &mut InputCtx, _ch: u8) {}

// ── CSI dispatch ──

fn dispatch_csi(ctx: &mut InputCtx, ch: u8) {
    let interm = ctx.interm_str();
    let csi_type = lookup_csi(ch, interm);
    dispatch_csi_command(ctx, csi_type);
}

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
        _ => Sgr, // default fallback
    }
}

fn dispatch_csi_command(ctx: &mut InputCtx, cmd: CsiType) {
    use CsiType::*;
    match cmd {
        Cup => {
            let row = (ctx.param_or(0, 1).max(1) - 1) as u32;
            let col = (ctx.param_or(1, 1).max(1) - 1) as u32;
            ctx.screen.cx = col.min(ctx.screen.size_x().saturating_sub(1));
            ctx.screen.cy = row.min(ctx.screen.size_y().saturating_sub(1));
        }
        Cuu => {
            let n = ctx.param_or(0, 1).max(1) as u32;
            ctx.screen.cy = ctx.screen.cy.saturating_sub(n);
        }
        Cud => {
            let n = ctx.param_or(0, 1).max(1) as u32;
            ctx.screen.cy = (ctx.screen.cy + n).min(ctx.screen.size_y().saturating_sub(1));
        }
        Cuf => {
            let n = ctx.param_or(0, 1).max(1) as u32;
            ctx.screen.cx = (ctx.screen.cx + n).min(ctx.screen.size_x().saturating_sub(1));
        }
        Cub => {
            let n = ctx.param_or(0, 1).max(1) as u32;
            ctx.screen.cx = ctx.screen.cx.saturating_sub(n);
        }
        Ed => {
            // 0 = cursor to end, 1 = start to cursor, 2 = all
            let n = ctx.param(0);
            match n {
                0 | -1 => {
                    // clear from cursor to end of screen
                    for y in ctx.screen.cy..ctx.screen.size_y() {
                        for x in 0..ctx.screen.size_x() {
                            ctx.screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                        }
                    }
                }
                1 => {
                    // clear from start of screen to cursor
                    for y in 0..=ctx.screen.cy {
                        let max_x = if y == ctx.screen.cy { ctx.screen.cx + 1 } else { ctx.screen.size_x() };
                        for x in 0..max_x {
                            ctx.screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                        }
                    }
                }
                2 | 3 => {
                    // clear entire screen
                    for y in 0..ctx.screen.size_y() {
                        for x in 0..ctx.screen.size_x() {
                            ctx.screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                        }
                    }
                }
                _ => {}
            }
        }
        El => {
            let n = ctx.param(0);
            let y = ctx.screen.cy;
            match n {
                0 | -1 => {
                    for x in ctx.screen.cx..ctx.screen.size_x() {
                        ctx.screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                    }
                }
                1 => {
                    for x in 0..=ctx.screen.cx {
                        ctx.screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                    }
                }
                2 => {
                    for x in 0..ctx.screen.size_x() {
                        ctx.screen.grid.view_set_cell(x, y, &GridCell::default_cell());
                    }
                }
                _ => {}
            }
        }
        Sgr => {
            // SGR - Select Graphic Rendition
            let mut i = 0;
            while i < ctx.nparams {
                let p = ctx.param_or(i, 0);
                match p {
                    0 => ctx.cell = GridCell::default_cell(),
                    1 => ctx.cell.attr |= GRID_ATTR_BRIGHT,
                    2 => ctx.cell.attr |= GRID_ATTR_DIM,
                    3 => ctx.cell.attr |= GRID_ATTR_ITALICS,
                    4 => ctx.cell.attr |= GRID_ATTR_UNDERSCORE,
                    5 => ctx.cell.attr |= GRID_ATTR_BLINK,
                    7 => ctx.cell.attr |= GRID_ATTR_REVERSE,
                    8 => ctx.cell.attr |= GRID_ATTR_HIDDEN,
                    9 => ctx.cell.attr |= GRID_ATTR_STRIKETHROUGH,
                    22 => ctx.cell.attr &= !(GRID_ATTR_BRIGHT | GRID_ATTR_DIM),
                    23 => ctx.cell.attr &= !GRID_ATTR_ITALICS,
                    24 => ctx.cell.attr &= !GRID_ATTR_UNDERSCORE,
                    25 => ctx.cell.attr &= !GRID_ATTR_BLINK,
                    27 => ctx.cell.attr &= !GRID_ATTR_REVERSE,
                    28 => ctx.cell.attr &= !GRID_ATTR_HIDDEN,
                    29 => ctx.cell.attr &= !GRID_ATTR_STRIKETHROUGH,
                    30..=37 => ctx.cell.fg = (p - 30) as i32,
                    38 => {
                        i += 1;
                        handle_sgr_256_or_rgb(ctx, &mut i, true);
                    }
                    39 => ctx.cell.fg = 8,
                    40..=47 => ctx.cell.bg = (p - 40) as i32,
                    48 => {
                        i += 1;
                        handle_sgr_256_or_rgb(ctx, &mut i, false);
                    }
                    49 => ctx.cell.bg = 8,
                    53 => ctx.cell.attr |= GRID_ATTR_OVERLINE,
                    55 => ctx.cell.attr &= !GRID_ATTR_OVERLINE,
                    90..=97 => ctx.cell.fg = (p - 90 + 8) as i32,
                    100..=107 => ctx.cell.bg = (p - 100 + 8) as i32,
                    _ => {}
                }
                i += 1;
            }
        }
        Decstbm => {
            let top = ctx.param_or(0, 1).max(1).min(ctx.screen.size_y() as i32) as u32 - 1;
            let bot = ctx.param_or(1, ctx.screen.size_y() as i32).max(1).min(ctx.screen.size_y() as i32) as u32 - 1;
            ctx.screen.rupper = top;
            ctx.screen.rlower = bot;
        }
        Decscusr => {
            let n = ctx.param_or(0, 0);
            let style = match n {
                0 | 1 => CursorStyle::Block,
                2 => CursorStyle::Underline,
                3 | 4 => CursorStyle::Bar,
                _ => CursorStyle::Default,
            };
            ctx.screen.cstyle = style;
        }
        SmPrivate | RmPrivate => {
            let is_set = matches!(cmd, SmPrivate);
            for i in 0..ctx.nparams {
                let p = ctx.param(i);
                handle_private_mode(ctx, p, is_set);
            }
        }
        _ => {
            // Other commands - not yet implemented
        }
    }
}

fn handle_sgr_256_or_rgb(ctx: &mut InputCtx, i: &mut usize, is_fg: bool) {
    if *i >= ctx.nparams {
        return;
    }
    let p = ctx.param(*i);
    match p {
        2 => {
            // RGB
            if *i + 3 < ctx.nparams {
                let r = ctx.param(*i + 1) as u8;
                let g = ctx.param(*i + 2) as u8;
                let b = ctx.param(*i + 3) as u8;
                let colour = (r as i32) << 16 | (g as i32) << 8 | b as i32;
                if is_fg {
                    ctx.cell.fg = colour | 0x02000000;
                } else {
                    ctx.cell.bg = colour | 0x02000000;
                }
                *i += 3;
            }
        }
        5 => {
            // 256-colour
            if *i + 1 < ctx.nparams {
                let idx = ctx.param(*i + 1) as u8;
                if is_fg {
                    ctx.cell.fg = idx as i32 | 0x01000000;
                } else {
                    ctx.cell.bg = idx as i32 | 0x01000000;
                }
                *i += 1;
            }
        }
        _ => {}
    }
}

fn handle_private_mode(ctx: &mut InputCtx, mode: i32, is_set: bool) {
    // Handle DEC private mode settings
    // These are the ? preceding a CSI sequence
    match mode {
        1 => { /* DECCKM - cursor keys */ }
        7 => { /* DECAWM - auto-wrap */ if !is_set { ctx.screen.mode |= 1; } }
        25 => { /* DECTCEM - cursor visibility */ }
        1049 => { /* save/restore cursor + alt screen */ }
        _ => {}
    }
}

// ── ESC dispatch ──

fn dispatch_esc(ctx: &mut InputCtx, _ch: u8) {
    // ESC dispatch - handle common sequences
    if ctx.interm_len == 0 {
        match _ch {
            b'D' => { ctx.screen.cy += 1; /* IND */ }
            b'E' => { ctx.screen.cx = 0; ctx.screen.cy += 1; /* NEL */ }
            b'M' => { ctx.screen.cy = ctx.screen.cy.saturating_sub(1); /* RI */ }
            b'7' => { /* DECSC - save cursor */ ctx.screen.mode |= 2; }
            b'8' => { /* DECRC - restore cursor */ ctx.screen.mode &= !2; }
            b'c' => { /* RIS - reset */ ctx.screen.cx = 0; ctx.screen.cy = 0; }
            b'=' => { /* DECKPAM */ }
            b'>' => { /* DECKPNM */ }
            _ => {}
        }
    }
}

// ── Transition tables ──

#[rustfmt::skip]
const GROUND_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1c, 0x1f, Some(handle_c0), None),
    Transition::new(0x20, 0x7e, Some(handle_print), None),
    Transition::new(0x7f, 0x7f, None, None),
    Transition::new(0x80, 0xff, Some(handle_top_bit), None),
    Transition::new(0x18, 0x18, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x1a, 0x1a, Some(handle_c0), Some(InputState::Ground)),
    Transition::new(0x1b, 0x1b, None, Some(InputState::EscEnter)),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const ESC_ENTER_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1c, 0x1f, Some(handle_c0), None),
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
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1c, 0x1f, Some(handle_c0), None),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), None),
    Transition::new(0x30, 0x7e, Some(handle_esc_dispatch), Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const CSI_ENTER_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1c, 0x1f, Some(handle_c0), None),
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
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1c, 0x1f, Some(handle_c0), None),
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
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1c, 0x1f, Some(handle_c0), None),
    Transition::new(0x20, 0x2f, Some(handle_intermediate), None),
    Transition::new(0x30, 0x3f, None, Some(InputState::CsiIgnore)),
    Transition::new(0x40, 0x7e, Some(handle_csi_dispatch), Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const CSI_IGNORE_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, Some(handle_c0), None),
    Transition::new(0x19, 0x19, Some(handle_c0), None),
    Transition::new(0x1c, 0x1f, Some(handle_c0), None),
    Transition::new(0x20, 0x3f, None, None),
    Transition::new(0x40, 0x7e, None, Some(InputState::Ground)),
    Transition::new(0x7f, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const DCS_ENTER_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x19, 0x19, None, None),
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
    Transition::new(0x19, 0x19, None, None),
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
    Transition::new(0x19, 0x19, None, None),
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
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const OSC_STRING_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x06, None, None),
    Transition::new(0x07, 0x07, Some(handle_end_bel), Some(InputState::Ground)),
    Transition::new(0x08, 0x17, None, None),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0xff, Some(handle_input), None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const APC_STRING_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0xff, Some(handle_input), None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const RENAME_STRING_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0xff, Some(handle_input), None),
    Transition::new(255, 255, None, None),
];

#[rustfmt::skip]
const CONSUME_ST_TABLE: &[Transition] = &[
    Transition::new(0x00, 0x17, None, None),
    Transition::new(0x19, 0x19, None, None),
    Transition::new(0x1c, 0x1f, None, None),
    Transition::new(0x20, 0xff, None, None),
    Transition::new(255, 255, None, None),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sgr_colors() {
        let mut screen = Screen::new(80, 24);
        let mut ctx = InputCtx::new(&mut screen);

        // ESC [ 31 m - set red foreground
        ctx.parse(0x1b);
        ctx.parse(0x5b);
        ctx.parse(b'3');
        ctx.parse(b'1');
        ctx.parse(b'm');

        assert_eq!(ctx.cell.fg, 1);
    }

    #[test]
    fn test_cursor_movement() {
        let mut screen = Screen::new(80, 24);
        let mut ctx = InputCtx::new(&mut screen);

        // ESC [ 10 ; 20 H - cursor to (19, 9)
        ctx.parse(0x1b);
        ctx.parse(0x5b);
        ctx.parse(b'1');
        ctx.parse(b'0');
        ctx.parse(b';');
        ctx.parse(b'2');
        ctx.parse(b'0');
        ctx.parse(b'H');

        assert_eq!(ctx.screen.cy, 9);
        assert_eq!(ctx.screen.cx, 19);
    }

    #[test]
    fn test_clear_screen() {
        let mut screen = Screen::new(80, 24);
        let mut ctx = InputCtx::new(&mut screen);

        // Write a cell at cursor
        ctx.parse(b'A');
        assert_eq!(ctx.screen.cx, 1);
        assert_eq!(ctx.screen.cy, 0);

        // ESC [ 2 J - clear entire screen (cursor stays)
        ctx.parse(0x1b);
        ctx.parse(0x5b);
        ctx.parse(b'2');
        ctx.parse(b'J');

        // ED does not move cursor
        assert_eq!(ctx.screen.cx, 1);
        assert_eq!(ctx.screen.cy, 0);
    }

    #[test]
    fn test_multiple_sgr() {
        let mut screen = Screen::new(80, 24);
        let mut ctx = InputCtx::new(&mut screen);

        // ESC [ 1 ; 31 m - bold, red
        ctx.parse(0x1b);
        ctx.parse(0x5b);
        ctx.parse(b'1');
        ctx.parse(b';');
        ctx.parse(b'3');
        ctx.parse(b'1');
        ctx.parse(b'm');

        assert!(ctx.cell.attr & GRID_ATTR_BRIGHT != 0);
        assert_eq!(ctx.cell.fg, 1);
    }

    #[test]
    fn test_scroll_region() {
        let mut screen = Screen::new(80, 24);
        let mut ctx = InputCtx::new(&mut screen);

        // ESC [ 3 ; 22 r - scroll region rows 3-22
        ctx.parse(0x1b);
        ctx.parse(0x5b);
        ctx.parse(b'3');
        ctx.parse(b';');
        ctx.parse(b'2');
        ctx.parse(b'2');
        ctx.parse(b'r');

        assert_eq!(ctx.screen.rupper, 2);
        assert_eq!(ctx.screen.rlower, 21);
    }
}
