use std::io;

use loom_core::grid_cell::*;
use loom_core::session::Window;
use loom_tty::tty::Tty;

/// Draw a complete window to a writable output using Tty incremental rendering.
/// Only outputs SGR and cursor positioning when state changes.
pub fn redraw_window(window: &Window, output: &mut impl io::Write) -> io::Result<()> {
    let mut tty = Tty::new(output, window.sx, window.sy);
    let default_cell = GridCell::default_cell();

    // Clear screen and draw all cells
    tty.clear_screen()?;

    for y in 0..window.sy {
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
                let wy = y;
                tty.draw_cell(wx, wy, cell.fg, cell.bg, cell.attr, cell.data.to_char())?;
            }
        }
    }

    // Position cursor at active pane's cursor
    if let Some(pid) = window.active_pane_id {
        if let Some(pane) = window.panes.get(&pid) {
            let cx = (pane.xoff + pane.screen.cx as i32) as u32;
            let cy = (pane.yoff + pane.screen.cy as i32) as u32;
            tty.set_cursor(cx, cy)?;
        }
    }

    tty.flush()
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
    }
}
