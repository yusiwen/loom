use crate::tty::Tty;
use loom_core::grid_cell::{GridCell, GRID_FLAG_PADDING, GRID_FLAG_CLEARED};
use loom_core::screen::Screen;

/// State machine states for `tty_draw_line()`, matching tmux's `enum tty_draw_line_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DrawState {
    First,    // initial state
    New1,     // first different cell
    New2,     // second consecutive different cell
    Same,     // same as last cell
    Empty,    // empty/cleared cell
    Flush,    // buffer full, must flush
}

/// Draw a line from a screen to the terminal.
///
/// Parameters (matching tmux's `tty_draw_line()`):
/// - `px, py`: source position in the screen grid
/// - `nx`: number of cells to draw
/// - `atx, aty`: target position on the terminal
///
/// Uses a state machine to merge identical adjacent cells and only
/// output attributes when they change.
pub fn tty_draw_line(
    tty: &mut Tty,
    screen: &Screen,
    px: u32,
    py: u32,
    nx: u32,
    atx: u32,
    aty: u32,
) {
    let grid = &screen.grid;
    let default_cell = GridCell::default_cell();
    // Clamp nx to terminal width and available grid cells
    let nx = nx.min(tty.sx.saturating_sub(atx));
    if nx == 0 {
        return;
    }

    let mut state = DrawState::First;
    let mut last_gc = default_cell;
    let mut i = 0;
    let mut buf_x = 0; // buffer start x
    let mut buf = Vec::with_capacity(256); // buffered characters

    while i < nx {
        let cell = grid
            .view_get_cell(px + i, py)
            .unwrap_or(&default_cell);

        if cell.is_padding() || cell.is_cleared() {
            // Empty/padding cell — flush buffer and clear
            if !buf.is_empty() {
                flush_buffer(tty, atx + buf_x, aty, &buf, &last_gc);
                buf.clear();
            }
            tty.tty_cursor(atx + i, aty);
            tty.out.push(b' ');
            tty.cx = (atx + i) as i32;
            i += 1;
            state = DrawState::Empty;
            continue;
        }

        let ch = cell.data.to_char();

        // Compare with last cell to determine state
        let cells_equal = cells_equal(&last_gc, cell);
        let next_state = match state {
            DrawState::First => DrawState::Same,
            DrawState::Same | DrawState::New1 | DrawState::New2 => {
                if cells_equal {
                    if buf.len() > 128 { DrawState::Flush } else { DrawState::Same }
                } else {
                    match state {
                        DrawState::New1 => DrawState::New2,
                        _ => DrawState::New1,
                    }
                }
            }
            DrawState::Empty => {
                if cells_equal {
                    DrawState::Same
                } else {
                    DrawState::New1
                }
            }
            _ => DrawState::New1,
        };

        // State transition — flush accumulated buffer if needed
        if next_state != state {
            match state {
                DrawState::New1 | DrawState::New2 | DrawState::Same => {
                    if !buf.is_empty() {
                        flush_buffer(tty, atx + buf_x, aty, &buf, &last_gc);
                        buf.clear();
                    }
                }
                DrawState::Flush => {
                    flush_buffer(tty, atx + buf_x, aty, &buf, &last_gc);
                    buf.clear();
                }
                _ => {}
            }
            buf_x = i;
            state = next_state;
        }

        // Flush if buffer too full
        if next_state == DrawState::Flush {
            state = DrawState::Same;
            buf_x = i;
        }

        // Accumulate character
        buf.push(ch as u8);
        last_gc = *cell;

        if cell.data.width > 1 {
            i += cell.data.width as u32;
        } else {
            i += 1;
        }
    }

    // Final flush
    if !buf.is_empty() {
        flush_buffer(tty, atx + buf_x, aty, &buf, &last_gc);
    }
}

/// Flush accumulated buffer: output attributes + characters at (x, y).
fn flush_buffer(tty: &mut Tty, x: u32, y: u32, buf: &[u8], gc: &GridCell) {
    tty.tty_cursor(x, y);
    tty.tty_attributes(gc);
    if buf.len() == 1 && buf[0] == b' ' && gc.attr == 0 && gc.fg == 8 && gc.bg == 8 {
        tty.out.push(b' ');
    } else {
        tty.out.extend_from_slice(buf);
    }
    // Update cursor tracking
    tty.cx = (x + buf.len() as u32 - 1) as i32;
}

/// Compare two cells for equality in the draw sense (fg, bg, attr).
fn cells_equal(a: &GridCell, b: &GridCell) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.attr == b.attr && a.us == b.us
}
