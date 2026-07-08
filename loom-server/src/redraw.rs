use loom_core::grid_cell::*;
use loom_core::session::Window;
use loom_tty::tty::Tty;
use loom_tty::tty_draw;

/// Draw a full window to a Tty. Used for initial full-screen render on attach.
pub fn redraw_window(tty: &mut Tty, window: &Window) {
    tty.invalidate();
    tty.clear_screen();
    draw_all_panes(tty, window);
}

/// Redraw a range of rows in an existing Tty. Used for incremental updates.
pub fn redraw_rows(tty: &mut Tty, window: &Window, _start_y: u32, _end_y: u32) {
    draw_all_panes(tty, window);
}

/// Draw all pane content via tty_draw_line for each visible line.
fn draw_all_panes(tty: &mut Tty, window: &Window) {
    for y in 0..window.sy {
        for (_, pane) in &window.panes {
            let pane_y = y as i32 - pane.yoff;
            if pane_y < 0 || pane_y >= pane.sy as i32 {
                continue;
            }
            let pane_y = pane_y as u32;
            let screen = &pane.screen;

            // Draw the pane line
            tty_draw::tty_draw_line(
                tty,
                screen,
                0,           // source x
                pane_y,      // source y
                pane.sx,     // width
                pane.xoff as u32, // target x
                y,           // target y
            );
        }
    }
}

/// Find cursor position: after the last non-space character on the active line.
fn cursor_after_prompt(window: &Window) -> (u32, u32) {
    if let Some(pid) = window.active_pane_id {
        if let Some(pane) = window.panes.get(&pid) {
            let y = pane.screen.cy;
            for x in (0..pane.sx).rev() {
                if let Some(cell) = pane.screen.grid.view_get_cell(x, y) {
                    if !cell.is_cleared() && !cell.is_padding() && cell.data.to_char() != ' ' {
                        return ((pane.xoff as u32) + x + 1, (pane.yoff as u32) + y);
                    }
                }
            }
        }
    }
    (0, 0)
}

/// Position cursor at the active pane's input position.
pub fn position_cursor(tty: &mut Tty, window: &Window) {
    let (cx, cy) = cursor_after_prompt(window);
    tty.tty_cursor(cx, cy);
}
