use crate::utf8::Utf8Data;

pub const GRID_FLAG_FG256: u8 = 0x01;
pub const GRID_FLAG_BG256: u8 = 0x02;
pub const GRID_FLAG_PADDING: u8 = 0x04;
pub const GRID_FLAG_EXTENDED: u8 = 0x08;
pub const GRID_FLAG_SELECTED: u8 = 0x10;
pub const GRID_FLAG_NOPALETTE: u8 = 0x20;
pub const GRID_FLAG_CLEARED: u8 = 0x40;
pub const GRID_FLAG_TAB: u8 = 0x80;

pub const GRID_LINE_WRAPPED: u8 = 0x01;
pub const GRID_LINE_EXTENDED: u8 = 0x02;
pub const GRID_LINE_DEAD: u8 = 0x04;
pub const GRID_LINE_START_PROMPT: u8 = 0x08;
pub const GRID_LINE_START_OUTPUT: u8 = 0x10;
pub const GRID_LINE_HYPERLINK: u8 = 0x20;

pub const GRID_ATTR_BRIGHT: u16 = 0x0001;
pub const GRID_ATTR_DIM: u16 = 0x0002;
pub const GRID_ATTR_UNDERSCORE: u16 = 0x0004;
pub const GRID_ATTR_BLINK: u16 = 0x0008;
pub const GRID_ATTR_REVERSE: u16 = 0x0010;
pub const GRID_ATTR_HIDDEN: u16 = 0x0020;
pub const GRID_ATTR_ITALICS: u16 = 0x0040;
pub const GRID_ATTR_CHARSET: u16 = 0x0080;
pub const GRID_ATTR_STRIKETHROUGH: u16 = 0x0100;
pub const GRID_ATTR_UNDERSCORE_2: u16 = 0x0200;
pub const GRID_ATTR_UNDERSCORE_3: u16 = 0x0400;
pub const GRID_ATTR_UNDERSCORE_4: u16 = 0x0800;
pub const GRID_ATTR_UNDERSCORE_5: u16 = 0x1000;
pub const GRID_ATTR_OVERLINE: u16 = 0x2000;
pub const GRID_ATTR_NOATTR: u16 = 0x4000;

pub const GRID_ATTR_ALL_UNDERSCORE: u16 = GRID_ATTR_UNDERSCORE
    | GRID_ATTR_UNDERSCORE_2
    | GRID_ATTR_UNDERSCORE_3
    | GRID_ATTR_UNDERSCORE_4
    | GRID_ATTR_UNDERSCORE_5;

#[derive(Clone, Copy, Debug)]
pub struct GridCell {
    pub data: Utf8Data,
    pub attr: u16,
    pub flags: u8,
    pub fg: i32,
    pub bg: i32,
    pub us: i32,
    pub link: u32,
}

impl GridCell {
    pub fn default_cell() -> Self {
        Self {
            data: Utf8Data::space(),
            attr: 0,
            flags: 0,
            fg: 8,
            bg: 8,
            us: 8,
            link: 0,
        }
    }

    pub fn padding_cell() -> Self {
        Self {
            data: Utf8Data::space(),
            attr: 0,
            flags: GRID_FLAG_PADDING,
            fg: 8,
            bg: 8,
            us: 8,
            link: 0,
        }
    }

    pub fn is_padding(&self) -> bool {
        self.flags & GRID_FLAG_PADDING != 0
    }

    pub fn is_cleared(&self) -> bool {
        self.flags & GRID_FLAG_CLEARED != 0
    }

    pub fn is_inline(&self) -> bool {
        self.flags & GRID_FLAG_EXTENDED == 0
    }

    pub fn has_attributes_overflow(&self) -> bool {
        self.attr > 0xff
            || self.flags & (GRID_FLAG_FG256 | GRID_FLAG_BG256) != 0
            || self.us != 8
            || self.link != 0
    }

    pub fn is_visible(&self) -> bool {
        !self.is_padding() && !self.is_cleared()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GridExtdEntry {
    pub data: u32,
    pub attr: u16,
    pub flags: u8,
    pub fg: i32,
    pub bg: i32,
    pub us: i32,
    pub link: u32,
}

#[derive(Clone, Debug)]
pub struct GridLine {
    pub cells: Vec<GridCell>,
    pub cellused: u32,
    pub flags: u8,
}

impl GridLine {
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            cellused: 0,
            flags: 0,
        }
    }

    pub fn with_capacity(cap: u32) -> Self {
        Self {
            cells: Vec::with_capacity(cap as usize),
            cellused: 0,
            flags: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.cellused == 0
    }

    pub fn is_wrapped(&self) -> bool {
        self.flags & GRID_LINE_WRAPPED != 0
    }

    pub fn set_wrapped(&mut self, wrapped: bool) {
        if wrapped {
            self.flags |= GRID_LINE_WRAPPED;
        } else {
            self.flags &= !GRID_LINE_WRAPPED;
        }
    }

    pub fn clear(&mut self) {
        self.cells.clear();
        self.cellused = 0;
    }
}

impl Default for GridLine {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub struct Grid {
    pub flags: u8,
    pub sx: u32,
    pub sy: u32,
    pub hscrolled: u32,
    pub hsize: u32,
    pub hlimit: u32,
    pub linedata: Vec<GridLine>,
}

impl Grid {
    pub fn new(sx: u32, sy: u32) -> Self {
        let hlimit = 2000;
        let mut linedata = Vec::with_capacity((hlimit + sy) as usize);
        for _ in 0..sy {
            linedata.push(GridLine::new());
        }
        Self {
            flags: 0,
            sx,
            sy,
            hscrolled: 0,
            hsize: 0,
            hlimit,
            linedata,
        }
    }

    pub fn total_lines(&self) -> u32 {
        self.hsize + self.sy
    }

    pub fn visible_lines(&self) -> u32 {
        self.sy
    }

    fn get_line_mut(&mut self, line: u32) -> Option<&mut GridLine> {
        let total = self.total_lines();
        if line >= total {
            return None;
        }
        // Ensure line exists
        while self.linedata.len() <= line as usize {
            self.linedata.push(GridLine::new());
        }
        self.linedata.get_mut(line as usize)
    }

    fn get_line(&self, line: u32) -> Option<&GridLine> {
        let total = self.total_lines();
        if line >= total {
            return None;
        }
        self.linedata.get(line as usize)
    }

    pub fn get_cell(&self, x: u32, y: u32) -> Option<&GridCell> {
        let line = self.get_line(y)?;
        line.cells.get(x as usize)
    }

    pub fn get_cell_mut(&mut self, x: u32, y: u32) -> Option<&mut GridCell> {
        let line = self.get_line_mut(y)?;
        let idx = x as usize;
        if idx >= line.cells.len() {
            line.cells.resize(idx + 1, GridCell::default_cell());
        }
        if idx as u32 >= line.cellused {
            line.cellused = idx as u32 + 1;
        }
        line.cells.get_mut(idx)
    }

    pub fn set_cell(&mut self, x: u32, y: u32, gc: &GridCell) {
        if let Some(cell) = self.get_cell_mut(x, y) {
            *cell = *gc;
        }
    }

    pub fn scroll_history(&mut self) {
        if self.flags & 1 == 0 {
            return;
        }
        let mut new_line = GridLine::new();
        new_line.flags |= GRID_LINE_WRAPPED;
        self.linedata.push(new_line);
        self.hsize += 1;
        self.collect_history();
    }

    pub fn scroll_history_region(&mut self, rupper: u32, rlower: u32) {
        if self.flags & 1 == 0 {
            return;
        }
        if rupper >= rlower {
            return;
        }
        let top = (self.hsize + rupper) as usize;
        let bot = (self.hsize + rlower) as usize;
        let len = self.linedata.len();
        if top >= bot || top >= len {
            return;
        }
        self.linedata.remove(top);
        let mut new_line = GridLine::new();
        new_line.flags |= GRID_LINE_WRAPPED;
        if bot <= len {
            self.linedata.insert(bot, new_line);
        } else {
            self.linedata.push(new_line);
        }
        self.hsize += 1;
        self.collect_history();
    }

    pub fn collect_history(&mut self) {
        if self.hsize <= self.hlimit {
            return;
        }
        let remove = (self.hsize - self.hlimit) as usize;
        let remove = if remove > 10 { remove } else { remove.max(1) };
        let len = self.linedata.len();
        let drain_end = remove.min(len);
        if drain_end > 0 {
            self.linedata.drain(..drain_end);
        }
        self.hsize -= drain_end as u32;
    }

    pub fn view_line(&self, y: u32) -> u32 {
        self.hsize + y
    }

    pub fn view_get_cell(&self, x: u32, y: u32) -> Option<&GridCell> {
        self.get_cell(x, self.view_line(y))
    }

    pub fn view_get_cell_mut(&mut self, x: u32, y: u32) -> Option<&mut GridCell> {
        let vy = self.view_line(y);
        let line = self.get_line_mut(vy)?;
        let idx = x as usize;
        if idx >= line.cells.len() {
            line.cells.resize(idx + 1, GridCell::default_cell());
        }
        if idx as u32 >= line.cellused {
            line.cellused = idx as u32 + 1;
        }
        line.cells.get_mut(idx)
    }

    pub fn view_set_cell(&mut self, x: u32, y: u32, gc: &GridCell) {
        if let Some(cell) = self.view_get_cell_mut(x, y) {
            *cell = *gc;
        }
    }

    pub fn reflow(&mut self, new_sx: u32) {
        if new_sx == self.sx {
            return;
        }
        let total = self.total_lines();
        let old_sx = self.sx;
        self.sx = new_sx;

        let mut new_lines: Vec<GridLine> = Vec::new();
        let mut carry_cells: Vec<GridCell> = Vec::new();

        for i in 0..total {
            let line = if let Some(l) = self.linedata.get(i as usize) {
                l
            } else {
                continue;
            };
            let mut cells = carry_cells.clone();
            cells.extend_from_slice(&line.cells[..line.cellused as usize]);
            carry_cells.clear();

            if cells.is_empty() {
                new_lines.push(GridLine::new());
                continue;
            }

            if line.is_wrapped() {
                // This line was a continuation, try to join
                if new_sx >= old_sx {
                    // Width increased, join is natural
                    if let Some(last) = new_lines.last_mut() {
                        last.cells.extend(cells);
                        last.cellused = last.cells.len() as u32;
                    }
                    continue;
                }
            }

            // Split into new_sx chunks
            let mut pos = 0;
            while pos < cells.len() {
                let end = (pos + new_sx as usize).min(cells.len());
                let chunk: Vec<GridCell> = cells[pos..end].to_vec();
                let is_wrapped = end < cells.len();
                let gl = GridLine {
                    cellused: chunk.len() as u32,
                    cells: chunk,
                    flags: if is_wrapped { GRID_LINE_WRAPPED } else { 0 },
                };
                new_lines.push(gl);
                pos = end;
            }
        }

        self.linedata = new_lines;
        self.hsize = self.linedata.len().saturating_sub(self.sy as usize) as u32;
        self.collect_history();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_create() {
        let g = Grid::new(80, 24);
        assert_eq!(g.sx, 80);
        assert_eq!(g.sy, 24);
        assert_eq!(g.hsize, 0);
    }

    #[test]
    fn test_set_get_cell() {
        let mut g = Grid::new(80, 24);
        let cell = GridCell {
            data: Utf8Data::new('X'),
            attr: GRID_ATTR_BRIGHT,
            ..GridCell::default_cell()
        };
        g.set_cell(10, 5, &cell);
        let got = g.get_cell(10, 5).unwrap();
        assert_eq!(got.attr, GRID_ATTR_BRIGHT);
        assert_eq!(got.data.to_char(), 'X');
    }

    #[test]
    fn test_scroll() {
        let mut g = Grid::new(80, 24);
        g.flags |= 1;
        for _ in 0..10 {
            g.scroll_history();
        }
        assert_eq!(g.hsize, 10);
        assert!(g.linedata.len() >= 24);
    }

    #[test]
    fn test_view_coords() {
        let mut g = Grid::new(80, 24);
        g.flags |= 1;
        g.scroll_history();
        let cell = GridCell::default_cell();
        g.view_set_cell(0, 0, &cell);
        assert!(g.view_get_cell(0, 0).is_some());
    }
}
