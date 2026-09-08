use crate::grid_cell::{Grid, GridCell};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Default,
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug)]
pub enum ProgressBarState {
    Hidden = 0,
    Normal = 1,
    Error = 2,
    Indeterminate = 3,
    Paused = 4,
}

#[derive(Clone, Copy, Debug)]
pub struct ProgressBar {
    pub state: ProgressBarState,
    pub progress: i32,
}

#[derive(Clone, Debug)]
pub struct ScreenSel {
    // Selection state - to be expanded
}

#[derive(Clone, Debug)]
pub struct Screen {
    pub title: String,
    pub path: String,
    pub grid: Grid,
    pub cx: u32,
    pub cy: u32,
    pub cstyle: CursorStyle,
    pub default_cstyle: CursorStyle,
    pub ccolour: i32,
    pub default_ccolour: i32,
    pub rupper: u32,
    pub rlower: u32,
    pub mode: u16,
    pub default_mode: u16,
    pub tabs: Vec<bool>,
    pub sel: Option<ScreenSel>,
    pub hyperlinks: bool,
    /// Registered OSC 8 hyperlink URIs; `GridCell.link` indexes into this.
    pub links: Vec<String>,
    pub progress_bar: ProgressBar,
    // ── Alt-screen support ──
    pub in_alt: bool,
    saved_grid: Option<Grid>,
    saved_cx: u32,
    saved_cy: u32,
    saved_rupper: u32,
    saved_rlower: u32,
    saved_mode: u16,
    saved_tabs: Vec<bool>,
    // DECSC/DECRC (ESC 7 / ESC 8, CSI s / CSI u) saved cursor
    pub decsc_cx: u32,
    pub decsc_cy: u32,
}

impl Screen {
    pub fn new(sx: u32, sy: u32) -> Self {
        let mut tabs = vec![false; sx as usize];
        for i in (0..sx).step_by(8) {
            if (i as usize) < tabs.len() {
                tabs[i as usize] = true;
            }
        }
        Self {
            title: String::new(),
            path: String::new(),
            grid: Grid::new(sx, sy),
            cx: 0,
            cy: 0,
            cstyle: CursorStyle::Default,
            default_cstyle: CursorStyle::Default,
            ccolour: 8,
            default_ccolour: 8,
            rupper: 0,
            rlower: sy.saturating_sub(1),
            mode: 0,
            default_mode: 0,
            tabs,
            sel: None,
            hyperlinks: false,
            links: Vec::new(),
            progress_bar: ProgressBar {
                state: ProgressBarState::Hidden,
                progress: 0,
            },
            in_alt: false,
            saved_grid: None,
            saved_cx: 0,
            saved_cy: 0,
            saved_rupper: 0,
            saved_rlower: 0,
            saved_mode: 0,
            saved_tabs: Vec::new(),
            decsc_cx: 0,
            decsc_cy: 0,
        }
    }

    pub fn size_x(&self) -> u32 {
        self.grid.sx
    }

    pub fn size_y(&self) -> u32 {
        self.grid.sy
    }

    pub fn hsize(&self) -> u32 {
        self.grid.hsize
    }

    pub fn hlimit(&self) -> u32 {
        self.grid.hlimit
    }

    pub fn set_cursor(&mut self, cx: u32, cy: u32) {
        self.cx = cx.min(self.size_x().saturating_sub(1));
        self.cy = cy.min(self.size_y().saturating_sub(1));
    }

    pub fn resize(&mut self, sx: u32, sy: u32) {
        self.grid.reflow(sx);
        self.grid.sy = sy;
        self.rlower = sy.saturating_sub(1);
        if self.cy >= sy {
            self.cy = sy.saturating_sub(1);
        }
        if self.cx >= sx {
            self.cx = sx.saturating_sub(1);
        }
        // Adjust tabs
        self.tabs.resize(sx as usize, false);
    }

    pub fn default_cell(&self) -> GridCell {
        GridCell::default_cell()
    }

    /// Switch to the alternate screen buffer. Saves the main screen state.
    pub fn to_alt(&mut self) {
        if self.in_alt {
            return;
        }
        self.in_alt = true;
        let (gx, gy) = (self.grid.sx, self.grid.sy);
        self.saved_grid = Some(std::mem::replace(
            &mut self.grid,
            Grid::new(gx, gy),
        ));
        self.saved_cx = self.cx;
        self.saved_cy = self.cy;
        self.saved_rupper = self.rupper;
        self.saved_rlower = self.rlower;
        self.saved_mode = self.mode;
        self.saved_tabs = std::mem::replace(
            &mut self.tabs,
            Self::make_default_tabs(self.grid.sx),
        );
        self.cx = 0;
        self.cy = 0;
        self.rupper = 0;
        self.rlower = self.grid.sy.saturating_sub(1);
        self.mode = 0;
    }

    /// Switch back to the main screen buffer. Restores the saved main screen state.
    pub fn to_main(&mut self) {
        if !self.in_alt {
            return;
        }
        self.in_alt = false;
        if let Some(g) = self.saved_grid.take() {
            self.grid = g;
        }
        self.cx = self.saved_cx;
        self.cy = self.saved_cy;
        self.rupper = self.saved_rupper;
        self.rlower = self.saved_rlower;
        self.mode = self.saved_mode;
        self.tabs = std::mem::take(&mut self.saved_tabs);
        if self.tabs.is_empty() {
            self.tabs = Self::make_default_tabs(self.grid.sx);
        }
    }

    fn make_default_tabs(sx: u32) -> Vec<bool> {
        let mut tabs = vec![false; sx as usize];
        for i in (0..sx).step_by(8) {
            if (i as usize) < tabs.len() {
                tabs[i as usize] = true;
            }
        }
        tabs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screen_create() {
        let s = Screen::new(80, 24);
        assert_eq!(s.size_x(), 80);
        assert_eq!(s.size_y(), 24);
        assert_eq!(s.rlower, 23);
    }

    #[test]
    fn test_resize() {
        let mut s = Screen::new(80, 24);
        s.resize(132, 43);
        assert_eq!(s.size_x(), 132);
        assert_eq!(s.size_y(), 43);
    }
}
