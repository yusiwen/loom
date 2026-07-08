use std::io;

use loom_core::grid_cell::*;
use loom_core::session::Window;
use loom_tty::tty::Tty;

/// Draw all cells. Used for initial full-screen render.
pub fn redraw_window(window: &Window, output: &mut impl io::Write) -> io::Result<()> {
    let mut tty = Tty::new(output, window.sx, window.sy);
    draw_range(&mut tty, window, 0, window.sy)
}

/// Draw only rows that changed. Used for incremental updates after PTY data.
pub fn redraw_rows(window: &Window, output: &mut impl io::Write, start_y: u32, end_y: u32) -> io::Result<()> {
    let mut tty = Tty::new(output, window.sx, window.sy);
    draw_range(&mut tty, window, start_y, end_y.min(window.sy))
}

/// Find cursor position: after the last non-whitespace char on the active line.
fn cursor_after_prompt(window: &Window) -> (u32, u32) {
    if let Some(pid) = window.active_pane_id {
        if let Some(pane) = window.panes.get(&pid) {
            let y = pane.screen.cy;
            for x in (0..pane.sx).rev() {
                if let Some(cell) = pane.screen.grid.view_get_cell(x, y) {
                    if !cell.is_cleared() && !cell.is_padding() && cell.data.to_char() != ' ' {
                        let col = x + 1;
                        return ((pane.xoff + col as i32) as u32, (pane.yoff + y as i32) as u32);
                    }
                }
            }
        }
    }
    (0, 0)
}

fn draw_range<W: io::Write>(tty: &mut Tty<W>, window: &Window, sy_min: u32, sy_max: u32) -> io::Result<()> {
    let default_cell = GridCell::default_cell();

    for y in sy_min..sy_max {
        for (_, pane) in &window.panes {
            let pane_y = y as i32 - pane.yoff;
            if pane_y < 0 || pane_y >= pane.sy as i32 {
                continue;
            }
            let pane_y = pane_y as u32;
            let screen = &pane.screen;
            let grid = &screen.grid;

            for x in 0..pane.sx {
                let cell = grid.view_get_cell(x, pane_y).unwrap_or(&default_cell);
                if cell.is_padding() {
                    continue;
                }
                let wx = (pane.xoff + x as i32) as u32;
                tty.draw_cell(wx, y, cell.fg, cell.bg, cell.attr, cell.data.to_char())?;
            }
        }
    }

    let (cx, cy) = cursor_after_prompt(window);
    tty.set_cursor(cx, cy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::layout_split_pane;
    use loom_core::session::Window;

    #[test]
    fn test_redraw_no_panics() {
        let mut window = Window::new(80, 24);
        let p1 = window.create_pane(80, 24);
        let _p2 = layout_split_pane(&mut window, p1, false).unwrap();
        let mut buf = Vec::new();
        redraw_window(&window, &mut buf).unwrap();
        assert!(!buf.is_empty());
        let mut buf2 = Vec::new();
        redraw_rows(&window, &mut buf2, 0, 10).unwrap();
        assert!(!buf2.is_empty());
    }
}
