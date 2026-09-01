use crate::utf8::Utf8Data;

pub const GRID_FLAG_FG256: u8 = 0x01;
pub const GRID_FLAG_BG256: u8 = 0x02;
pub const GRID_FLAG_PADDING: u8 = 0x04;
pub const GRID_FLAG_EXTENDED: u8 = 0x08;
pub const GRID_FLAG_SELECTED: u8 = 0x10;
pub const GRID_FLAG_NOPALETTE: u8 = 0x20;
pub const GRID_FLAG_CLEARED: u8 = 0x40;
pub const GRID_FLAG_TAB: u8 = 0x80;

/// Grid flag: scrolling / history is enabled for this grid.
pub const GRID_FLAG_HISTORY: u16 = 0x100;

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

impl Default for GridCell {
    fn default() -> Self {
        Self::default_cell()
    }
}

impl GridCell {
    pub const fn default_cell() -> Self {
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

/// The default cell returned for unwritten screen positions (blank, default
/// colours). Keeps `get_cell`/`view_get_cell` total instead of `None`.
static DEFAULT_CELL: GridCell = GridCell::default_cell();

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
    pub flags: u16,
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
            flags: GRID_FLAG_HISTORY,
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
        // Unwritten cells read back as the default blank cell (tmux semantics).
        line.cells.get(x as usize).or(Some(&DEFAULT_CELL))
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

    /// Scroll the entire visible area up by one line.
    /// The top visible line moves into history; a blank line appears at the bottom.
    pub fn scroll_up(&mut self) {
        if self.flags & GRID_FLAG_HISTORY == 0 {
            return;
        }
        let new_line = GridLine::new();
        self.linedata.push(new_line);
        self.hsize += 1;
        self.collect_history();
    }

    /// Scroll within a sub-region [rupper, rlower].
    /// The top line of the region is lost; a blank line appears at the bottom.
    pub fn scroll_region_up(&mut self, rupper: u32, rlower: u32) {
        if rupper >= rlower {
            return;
        }
        let top = (self.hsize + rupper) as usize;
        let bot = (self.hsize + rlower) as usize;
        let len = self.linedata.len();
        if top >= len || bot >= len {
            return;
        }
        // Remove the top line of the region
        self.linedata.remove(top);
        // Insert a blank line at the bottom of the region
        let insert_at = bot.min(self.linedata.len());
        self.linedata.insert(insert_at, GridLine::new());
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

    /// Number of cells written on absolute line `line` (0 when unknown).
    pub fn cellused(&self, line: u32) -> u32 {
        self.get_line(line).map(|l| l.cellused).unwrap_or(0)
    }

    /// Extract the text between two absolute (line, column) endpoints.
    /// Lines are joined with newlines; trailing spaces are trimmed.
    /// If the selection ends at column 0 of a line, that line contributes
    /// nothing (matching how a cursor at line start selects nothing of it).
    pub fn extract_selection(&self, a: (u32, u32), b: (u32, u32)) -> String {
        let (l1, c1) = if a <= b { a } else { b };
        let (l2, c2) = if a <= b { b } else { a };
        let total = self.total_lines();
        if l2 >= total {
            return String::new();
        }
        let mut lines: Vec<String> = Vec::new();
        for line in l1..=l2 {
            let end = if line == l2 { c2 } else { self.cellused(line).min(self.sx) };
            let start = if line == l1 { c1.min(end) } else { 0 };
            let mut text = String::new();
            for x in start..end {
                if let Some(cell) = self.get_cell(x, line) {
                    if cell.is_visible() {
                        text.push(cell.data.to_char());
                    }
                }
            }
            text = text.trim_end().to_string();
            lines.push(text);
        }
        // A selection ending at the start of a line leaves a trailing empty
        // line; drop it so `copy-paste` doesn't append a blank line.
        if lines.last() == Some(&String::new()) && lines.len() > 1 {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Set or clear the WRAPPED flag on a visible line.
    pub fn set_line_wrapped(&mut self, view_y: u32, wrapped: bool) {
        if let Some(line) = self.get_line_mut(self.view_line(view_y)) {
            line.set_wrapped(wrapped);
        }
    }

    /// ICH: Insert *n* blank cells at (x, view_y), shifting cells right.
    pub fn insert_chars(&mut self, x: u32, view_y: u32, n: u32) {
        if view_y >= self.sy || n == 0 {
            return;
        }
        let sx = self.sx as usize;
        let x = (x as usize).min(sx);
        let n = (n as usize).min(sx.saturating_sub(x));
        let idx = self.view_line(view_y);
        let line = match self.get_line_mut(idx) { Some(l) => l, None => return };
        // Extend line if needed
        while line.cells.len() < sx {
            line.cells.push(GridCell::default_cell());
        }
        for i in (x..sx).rev() {
            if i >= x + n {
                line.cells[i] = line.cells[i - n];
            } else {
                line.cells[i] = GridCell::default_cell();
            }
        }
        line.cellused = sx as u32;
    }

    /// DCH: Delete *n* cells at (x, view_y), shifting cells left.
    pub fn delete_chars(&mut self, x: u32, view_y: u32, n: u32) {
        if view_y >= self.sy || n == 0 {
            return;
        }
        let sx = self.sx as usize;
        let x = (x as usize).min(sx);
        let n = (n as usize).min(sx.saturating_sub(x));
        let idx = self.view_line(view_y);
        let line = match self.get_line_mut(idx) { Some(l) => l, None => return };
        while line.cells.len() < sx {
            line.cells.push(GridCell::default_cell());
        }
        for i in 0..(sx - n) {
            let src = i + n;
            if src < sx {
                line.cells[i] = line.cells[src];
            } else {
                line.cells[i] = GridCell::default_cell();
            }
        }
        line.cellused = sx as u32;
    }

    /// ECH: Erase *n* cells starting at (x, view_y).
    pub fn erase_chars(&mut self, x: u32, view_y: u32, n: u32) {
        if view_y >= self.sy {
            return;
        }
        let dx = x.saturating_add(n).min(self.sx);
        for xx in x..dx {
            self.view_set_cell(xx, view_y, &GridCell::default_cell());
        }
    }

    /// IL: Insert *n* blank lines at view_y, within scroll region [top, bottom].
    pub fn insert_lines(&mut self, top: u32, bottom: u32, view_y: u32, n: u32) {
        if n == 0 || view_y < top || view_y > bottom || top > bottom {
            return;
        }
        let n = n.min(bottom - view_y + 1);
        let ti = (self.hsize + top) as usize;
        let bi = (self.hsize + bottom) as usize;
        if bi >= self.linedata.len() {
            return;
        }
        let region_len = (bottom - top + 1) as usize;
        let mut region: Vec<GridLine> = self.linedata[ti..=bi].to_vec();
        let pos = (view_y - top) as usize;
        for _ in 0..n {
            region.insert(pos, GridLine::new());
        }
        region.truncate(region_len);
        for (j, line) in region.into_iter().enumerate() {
            self.linedata[ti + j] = line;
        }
    }

    /// DL: Delete *n* lines at view_y within scroll region [top, bottom].
    pub fn delete_lines(&mut self, top: u32, bottom: u32, view_y: u32, n: u32) {
        if n == 0 || view_y < top || view_y > bottom || top > bottom {
            return;
        }
        let n = n.min(bottom - view_y + 1);
        let ti = (self.hsize + top) as usize;
        let bi = (self.hsize + bottom) as usize;
        if bi >= self.linedata.len() {
            return;
        }
        let region_len = (bottom - top + 1) as usize;
        let mut region: Vec<GridLine> = self.linedata[ti..=bi].to_vec();
        let pos = (view_y - top) as usize;
        for _ in 0..n {
            region.remove(pos);
        }
        while region.len() < region_len {
            region.push(GridLine::new());
        }
        for (j, line) in region.into_iter().enumerate() {
            self.linedata[ti + j] = line;
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
        for _ in 0..10 {
            g.scroll_up();
        }
        assert_eq!(g.hsize, 10);
        assert!(g.linedata.len() >= 24);
    }

    #[test]
    fn test_view_coords() {
        let mut g = Grid::new(80, 24);
        g.scroll_up();
        let cell = GridCell::default_cell();
        g.view_set_cell(0, 0, &cell);
        assert!(g.view_get_cell(0, 0).is_some());
    }

    /// Fill absolute line `line` (0..cellused on the visible row when
    /// scrolling) with a repeated char for selection tests.
    fn fill_line(g: &mut Grid, line: u32, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            g.set_cell(i as u32, line, &GridCell {
                data: Utf8Data::new(ch),
                ..GridCell::default_cell()
            });
        }
    }

    #[test]
    fn test_extract_selection_same_line() {
        let mut g = Grid::new(80, 24);
        // Line 23 is the last visible row.
        fill_line(&mut g, 23, "hello world");
        let text = g.extract_selection((23, 0), (23, 5));
        assert_eq!(text, "hello");
        // Reversed endpoints give the same result.
        assert_eq!(g.extract_selection((23, 5), (23, 0)), "hello");
    }

    #[test]
    fn test_extract_selection_multi_line() {
        let mut g = Grid::new(80, 24);
        // Push three lines into history via scroll_up.
        for _ in 0..3 {
            g.scroll_up();
        }
        // hsize == 3 now; live rows are 3..27.
        fill_line(&mut g, 25, "foo bar");
        fill_line(&mut g, 26, "baz qux");
        // Select from "o bar" on line 25 to "ba" on line 26.
        let text = g.extract_selection((25, 1), (26, 2));
        assert_eq!(text, "oo bar\nba");
    }

    #[test]
    fn test_extract_selection_trailing_blank_dropped() {
        let mut g = Grid::new(80, 24);
        fill_line(&mut g, 22, "abc");
        // Ending at column 0 of the next line must not append a blank line.
        let text = g.extract_selection((22, 0), (23, 0));
        assert_eq!(text, "abc");
    }
}
