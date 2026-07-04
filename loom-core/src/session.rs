use std::collections::{BTreeMap, VecDeque};

use crate::options::Options;
use crate::screen::Screen;

/// ── IDs ──
pub type SessionId = u32;
pub type WindowId = u32;
pub type PaneId = u32;

static mut NEXT_SESSION_ID: SessionId = 0;
static mut NEXT_WINDOW_ID: WindowId = 0;
static mut NEXT_PANE_ID: PaneId = 0;
static mut NEXT_ACTIVE_POINT: u32 = 0;

pub fn next_session_id() -> SessionId {
    unsafe { let id = NEXT_SESSION_ID; NEXT_SESSION_ID += 1; id }
}
pub fn next_window_id() -> WindowId {
    unsafe { let id = NEXT_WINDOW_ID; NEXT_WINDOW_ID += 1; id }
}
pub fn next_pane_id() -> PaneId {
    unsafe { let id = NEXT_PANE_ID; NEXT_PANE_ID += 1; id }
}
pub fn next_active_point() -> u32 {
    unsafe { let id = NEXT_ACTIVE_POINT; NEXT_ACTIVE_POINT += 1; id }
}

/// ── Pane flags ──
pub const PANE_REDRAW: u32 = 0x0001;
pub const PANE_DROP: u32 = 0x0002;
pub const PANE_FOCUSED: u32 = 0x0004;
pub const PANE_VISITED: u32 = 0x0008;
pub const PANE_ZOOMED: u32 = 0x0010;
pub const PANE_STYLECHANGED: u32 = 0x0020;
pub const PANE_RESIZE: u32 = 0x0040;
pub const PANE_RESIZING: u32 = 0x0080;
pub const PANE_EMPTY: u32 = 0x0100;
pub const PANE_CHANGED: u32 = 0x0200;
pub const PANE_SYNC_DRAWNING: u32 = 0x0400;

/// ── Winlink flags ──
pub const WINLINK_BELL: u32 = 0x1;
pub const WINLINK_ACTIVITY: u32 = 0x2;
pub const WINLINK_SILENCE: u32 = 0x4;
pub const WINLINK_VISITED: u32 = 0x8;
pub const WINLINK_ALERTFLAGS: u32 = WINLINK_BELL | WINLINK_ACTIVITY | WINLINK_SILENCE;

/// ── Window flags ──
pub const WINDOW_BELL: u32 = 0x1;
pub const WINDOW_ACTIVITY: u32 = 0x2;
pub const WINDOW_SILENCE: u32 = 0x4;
pub const WINDOW_ZOOMED: u32 = 0x8;
pub const WINDOW_WASZOOMED: u32 = 0x10;
pub const WINDOW_RESIZE: u32 = 0x20;

/// ── Layout types ──
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutType {
    LeftRight,
    TopBottom,
    WindowPane,
}

/// ── Layout cell (recursive tree node) ──
#[derive(Clone, Debug)]
pub struct LayoutCell {
    pub cell_type: LayoutType,
    pub flags: u32,
    pub sx: u32,
    pub sy: u32,
    pub xoff: i32,
    pub yoff: i32,
    pub saved_sx: u32,
    pub saved_sy: u32,
    pub saved_xoff: i32,
    pub saved_yoff: i32,
    pub children: Vec<LayoutCell>,
    pub pane_id: Option<PaneId>,
}

impl LayoutCell {
    pub fn new_leaf() -> Self {
        Self {
            cell_type: LayoutType::WindowPane,
            flags: 0,
            sx: u32::MAX,
            sy: u32::MAX,
            xoff: i32::MAX,
            yoff: i32::MAX,
            saved_sx: 0,
            saved_sy: 0,
            saved_xoff: 0,
            saved_yoff: 0,
            children: Vec::new(),
            pane_id: None,
        }
    }

    pub fn new_node(cell_type: LayoutType) -> Self {
        Self {
            cell_type,
            flags: 0,
            sx: u32::MAX,
            sy: u32::MAX,
            xoff: i32::MAX,
            yoff: i32::MAX,
            saved_sx: 0,
            saved_sy: 0,
            saved_xoff: 0,
            saved_yoff: 0,
            children: Vec::new(),
            pane_id: None,
        }
    }

    pub fn is_leaf(&self) -> bool {
        self.cell_type == LayoutType::WindowPane
    }

    pub fn is_floating(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// ── Window Pane ──
#[derive(Debug)]
pub struct WindowPane {
    pub id: PaneId,
    pub active_point: u32,
    pub window_id: WindowId,
    pub sx: u32,
    pub sy: u32,
    pub xoff: i32,
    pub yoff: i32,
    pub flags: u32,
    pub pid: Option<u32>,
    pub fd: Option<i32>,
    pub screen: Screen,
    pub alt_screen: Option<Screen>,
    pub shell: String,
    pub cwd: String,
    pub options: Options,
    pub layout_cell: Option<usize>, // index into window's layout
    pub last_activity: u64,
}

impl WindowPane {
    pub fn new(window_id: WindowId, sx: u32, sy: u32) -> Self {
        Self {
            id: next_pane_id(),
            active_point: next_active_point(),
            window_id,
            sx,
            sy,
            xoff: 0,
            yoff: 0,
            flags: PANE_STYLECHANGED,
            pid: None,
            fd: None,
            screen: Screen::new(sx, sy),
            alt_screen: None,
            shell: String::new(),
            cwd: String::new(),
            options: Options::new(),
            layout_cell: None,
            last_activity: 0,
        }
    }

    pub fn is_cursor_visible(&self) -> bool {
        (self.screen.mode & 2) == 0
    }
}

/// ── Winlink (session-to-window bridge) ──
#[derive(Debug)]
pub struct Winlink {
    pub idx: i32,
    pub session_id: SessionId,
    pub window_id: WindowId,
    pub flags: u32,
}

/// ── Window ──
#[derive(Debug)]
pub struct Window {
    pub id: WindowId,
    pub name: String,
    pub sx: u32,
    pub sy: u32,
    pub active_pane_id: Option<PaneId>,
    pub panes: BTreeMap<PaneId, WindowPane>,
    pub pane_order: VecDeque<PaneId>,
    pub last_panes: VecDeque<PaneId>,
    pub layout_root: Option<usize>,
    pub cells: Vec<LayoutCell>,
    pub flags: u32,
    pub options: Options,
    pub winlinks: Vec<Winlink>,
    pub lastlayout: i32,
    pub last_activity: u64,
}

impl Window {
    pub fn new(sx: u32, sy: u32) -> Self {
        Self {
            id: next_window_id(),
            name: String::new(),
            sx,
            sy,
            active_pane_id: None,
            panes: BTreeMap::new(),
            pane_order: VecDeque::new(),
            last_panes: VecDeque::new(),
            layout_root: None,
            cells: Vec::new(),
            flags: 0,
            options: Options::new(),
            winlinks: Vec::new(),
            lastlayout: -1,
            last_activity: 0,
        }
    }

    pub fn create_pane(&mut self, sx: u32, sy: u32) -> PaneId {
        let pane = WindowPane::new(self.id, sx, sy);
        let id = pane.id;
        self.panes.insert(id, pane);
        self.pane_order.push_back(id);
        if self.active_pane_id.is_none() {
            self.active_pane_id = Some(id);
        }
        id
    }

    pub fn remove_pane(&mut self, pane_id: PaneId) {
        self.panes.remove(&pane_id);
        self.pane_order.retain(|&id| id != pane_id);
        self.last_panes.retain(|&id| id != pane_id);

        if self.active_pane_id == Some(pane_id) {
            // Pick new active from last_panes stack, then pane_order
            self.active_pane_id = self.last_panes.pop_back()
                .or_else(|| self.pane_order.back().copied());
        }
    }

    pub fn set_active_pane(&mut self, pane_id: PaneId) {
        if self.active_pane_id == Some(pane_id) {
            return;
        }
        if let Some(old) = self.active_pane_id {
            self.last_panes.push_back(old);
        }
        self.active_pane_id = Some(pane_id);
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            pane.active_point = next_active_point();
        }
    }

    pub fn active_pane(&self) -> Option<&WindowPane> {
        self.active_pane_id.and_then(|id| self.panes.get(&id))
    }

    pub fn active_pane_mut(&mut self) -> Option<&mut WindowPane> {
        self.active_pane_id.and_then(|id| self.panes.get_mut(&id))
    }
}

/// ── Session ──
#[derive(Debug)]
pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub cwd: String,
    pub creation_time: u64,
    pub activity_time: u64,
    pub attached: u32,
    pub flags: u32,
    pub options: Options,
    pub windows: BTreeMap<i32, Winlink>,
    pub lastw: VecDeque<i32>,
    pub curw_idx: Option<i32>,
}

impl Session {
    pub fn new(name: Option<&str>, cwd: &str) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let id = next_session_id();
        Self {
            id,
            name: name.unwrap_or(&format!("{}", id)).to_string(),
            cwd: cwd.to_string(),
            creation_time: now,
            activity_time: now,
            attached: 0,
            flags: 0,
            options: Options::new(),
            windows: BTreeMap::new(),
            lastw: VecDeque::new(),
            curw_idx: None,
        }
    }

    pub fn attach_window(&mut self, idx: i32, window_id: WindowId) {
        let wl = Winlink {
            idx,
            session_id: self.id,
            window_id,
            flags: 0,
        };
        self.windows.insert(idx, wl);
        self.curw_idx = Some(idx);
    }

    pub fn detach_window(&mut self, idx: i32) -> Option<WindowId> {
        let wl = self.windows.remove(&idx);
        self.lastw.retain(|&i| i != idx);
        if self.curw_idx == Some(idx) {
            self.curw_idx = self.lastw.pop_back()
                .or_else(|| self.windows.keys().next_back().copied());
        }
        wl.map(|w| w.window_id)
    }

    pub fn set_current_window(&mut self, idx: i32) {
        if let Some(old) = self.curw_idx {
            if old != idx {
                self.lastw.push_back(old);
            }
        }
        self.curw_idx = Some(idx);
    }

    pub fn current_winlink(&self) -> Option<&Winlink> {
        self.curw_idx.and_then(|idx| self.windows.get(&idx))
    }

    pub fn update_activity(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.activity_time = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_create() {
        let s = Session::new(Some("test"), "/home/user");
        assert_eq!(s.name, "test");
        assert_eq!(s.cwd, "/home/user");
    }

    #[test]
    fn test_window_pane_create() {
        let w = Window::new(80, 24);
        assert_eq!(w.panes.len(), 0);
        assert!(w.active_pane_id.is_none());
    }

    #[test]
    fn test_pane_create_remove() {
        let mut w = Window::new(80, 24);
        let p1 = w.create_pane(80, 24);
        let p2 = w.create_pane(80, 24);
        w.set_active_pane(p1);
        w.remove_pane(p1);
        assert_eq!(w.active_pane_id, Some(p2));
    }

    #[test]
    fn test_session_attach_detach() {
        let mut w = Window::new(80, 24);
        let mut s = Session::new(Some("sess"), "/tmp");
        s.attach_window(0, w.id);
        assert_eq!(s.curw_idx, Some(0));
        let wid = s.detach_window(0);
        assert_eq!(wid, Some(w.id));
        assert!(s.curw_idx.is_none());
    }

    #[test]
    fn test_layout_cell() {
        let leaf = LayoutCell::new_leaf();
        assert!(leaf.is_leaf());
        let node = LayoutCell::new_node(LayoutType::LeftRight);
        assert!(!node.is_leaf());
    }
}
