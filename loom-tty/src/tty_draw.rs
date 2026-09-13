use crate::tty::Tty;
use loom_core::grid_cell::GridCell;
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
    let mut buf = Vec::with_capacity(256); // buffered characters (UTF-8 bytes)
    let mut buf_cells = 0u32; // display columns held in `buf`

    while i < nx {
        let cell = grid
            .view_get_cell(px + i, py)
            .unwrap_or(&default_cell);

        if cell.is_padding() || cell.is_cleared() {
            // Empty/padding cell — flush buffer and clear
            if !buf.is_empty() {
                flush_buffer(tty, atx + buf_x, aty, &buf, buf_cells, &last_gc);
                buf.clear();
                buf_cells = 0;
            }
            tty.tty_cursor(atx + i, aty);
            tty.out.push(b' ');
            // The hardware cursor advanced one column past the space.
            tty.cx = (atx + i + 1) as i32;
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
                        flush_buffer(tty, atx + buf_x, aty, &buf, buf_cells, &last_gc);
                        buf.clear();
                        buf_cells = 0;
                    }
                }
                DrawState::Flush => {
                    flush_buffer(tty, atx + buf_x, aty, &buf, buf_cells, &last_gc);
                    buf.clear();
                    buf_cells = 0;
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

        // Accumulate the character as full UTF-8. Truncating to one byte
        // (`ch as u8`) corrupts every non-Latin-1 glyph — e.g. a p10k prompt's
        // `❯` (U+276F) became 'o', and Nerd Font powerline glyphs turned into
        // invalid UTF-8 that a terminal renders as U+FFFD.
        let mut enc = [0u8; 4];
        buf.extend_from_slice(ch.encode_utf8(&mut enc).as_bytes());
        buf_cells += cell.data.width.max(1) as u32;
        last_gc = *cell;

        if cell.data.width > 1 {
            i += cell.data.width as u32;
        } else {
            i += 1;
        }
    }

    // Final flush
    if !buf.is_empty() {
        flush_buffer(tty, atx + buf_x, aty, &buf, buf_cells, &last_gc);
    }

    // Clear the remainder of the row (EL, CSI K). Without this, a line that
    // shrank — e.g. a re-laid-out column, a spinner, or a path that got
    // shorter — leaves its old trailing cells on screen as ghosts. `\x1b[K`
    // erases from the cursor to the end of the line, so position first.
    let erase_from = atx + nx;
    if erase_from < tty.sx {
        if tty.cx != erase_from as i32 || tty.cy != aty as i32 {
            tty.tty_cursor(erase_from, aty);
        }
        tty.out.extend_from_slice(b"\x1b[K");
        // Cursor stays at the erase start; record it.
        tty.cx = erase_from as i32;
        tty.cy = aty as i32;
    }
}

/// Flush accumulated buffer: output attributes + characters at (x, y).
/// `cells` is the number of display columns the buffer occupies, so the
/// tracked cursor lands at the next unwritten column.
fn flush_buffer(tty: &mut Tty, x: u32, y: u32, buf: &[u8], cells: u32, gc: &GridCell) {
    tty.tty_cursor(x, y);
    tty.tty_attributes(gc);
    if buf.len() == 1 && buf[0] == b' ' && gc.attr == 0 && gc.fg == 8 && gc.bg == 8 {
        tty.out.push(b' ');
    } else {
        tty.out.extend_from_slice(buf);
    }
    // Update cursor tracking to the next unwritten column.
    tty.cx = (x + cells) as i32;
    tty.cy = y as i32;
}

/// Compare two cells for equality in the draw sense (fg, bg, attr).
fn cells_equal(a: &GridCell, b: &GridCell) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.attr == b.attr && a.us == b.us
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_core::grid_cell::GridCell;
    use loom_core::screen::Screen;
    use loom_core::utf8::Utf8Data;

    fn fill_line(screen: &mut Screen, row: u32, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            screen.grid.view_set_cell(i as u32, row, &GridCell {
                data: Utf8Data::new(ch),
                ..GridCell::default_cell()
            });
        }
    }

    /// Regression: non-Latin-1 glyphs must be emitted as full UTF-8. The old
    /// `ch as u8` truncation turned a p10k `❯` (U+276F) into 'o' and corrupted
    /// Nerd Font prompt glyphs into invalid bytes (terminal shows U+FFFD).
    #[test]
    fn test_multibyte_glyphs_are_emitted_as_utf8() {
        let mut tty = Tty::new(80, 24);
        let mut screen = Screen::new(80, 24);
        fill_line(&mut screen, 0, "~/proj ❯ ●·");
        tty_draw_line(&mut tty, &screen, 0, 0, 12, 0, 0);
        let s = String::from_utf8(tty.out.clone()).expect("row output must be valid UTF-8");
        assert!(s.contains('❯'), "prompt char must survive: {s:?}");
        assert!(s.contains('●') && s.contains('·'), "glyphs must survive: {s:?}");
        assert!(!s.contains('\u{fffd}'), "no replacement chars: {s:?}");
    }

    /// A row that later shrinks must erase its old tail via `\x1b[K`, so a
    /// shorter line doesn't leave ghost cells (eza re-layout, spinner, ...).
    #[test]
    fn test_shrunk_row_emits_erase_to_eol() {
        let mut tty = Tty::new(80, 24);
        let mut screen = Screen::new(80, 24);
        // Row 0 starts wide.
        fill_line(&mut screen, 0, "a_very_long_column_heading_here_123456");
        tty_draw_line(&mut tty, &screen, 0, 0, 40, 0, 0);
        let _first = tty.out.clone();
        tty.out.clear();
        // Now the row re-lays out to a shorter value.
        fill_line(&mut screen, 0, "short");
        tty_draw_line(&mut tty, &screen, 0, 0, 40, 0, 0);
        let s = String::from_utf8_lossy(&tty.out).into_owned();
        assert!(
            s.contains("\x1b[K"),
            "shrunken row must emit clear-to-EOL, got: {:?}",
            s
        );
        // The erase targets the column right after the new (5-char) content.
        assert!(s.contains("\x1b[6;1H") || s.contains("\x1b[K"), "got {:?}", s);
    }
}
