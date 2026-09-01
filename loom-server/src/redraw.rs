use loom_core::grid_cell::GridCell;
use loom_core::session::{Window, WINDOW_ZOOMED};
use loom_core::utf8::Utf8Data;
use loom_tty::tty::Tty;
use loom_tty::tty_draw;

/// Draw a full window to a Tty. Used for initial full-screen render on attach.
/// This invalidates all previous state and clears the screen.
pub fn redraw_window(tty: &mut Tty, window: &Window) {
    tty.invalidate();
    tty.clear_screen();
    draw_all_panes(tty, window);
}

/// Incremental update — draw all panes without clearing the screen.
/// Tty's persistent state ensures only changed cells produce output.
pub fn redraw_update(tty: &mut Tty, window: &Window) {
    draw_all_panes(tty, window);
}

/// Draw all pane content via tty_draw_line for each visible line.
/// When the window is zoomed, only the active pane is drawn (it covers the
/// whole window).
fn draw_all_panes(tty: &mut Tty, window: &Window) {
    let zoomed = window.flags & WINDOW_ZOOMED != 0;
    for y in 0..window.sy {
        for (_, pane) in &window.panes {
            if zoomed && Some(pane.id) != window.active_pane_id {
                continue;
            }
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

/// Status-line segment style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusStyle {
    /// Session name / padding.
    Normal,
    /// The active window entry (inverted).
    Active,
    /// A window with a pending bell/activity alert.
    Alert,
}

/// Draw the status line on the bottom row (`sy - 1`) from pre-built segments.
/// Uses the normal Tty cell-drawing path (cursor + SGR + chars), so an
/// unchanged status line produces no output on subsequent redraws.
pub fn draw_status_line(tty: &mut Tty, segments: &[(String, StatusStyle)]) {
    let y = tty.sy.saturating_sub(1);
    let width = tty.sx;
    tty.tty_cursor(0, y);

    let mut x: u32 = 0;
    for (text, style) in segments {
        if x >= width {
            break;
        }
        let cell = status_cell(*style);
        for ch in text.chars() {
            if x >= width {
                break;
            }
            let mut c = cell;
            c.data = Utf8Data::new(ch);
            tty.tty_cell(x, y, &c);
            x += 1;
        }
    }
    // Pad the remainder of the row with the base style.
    let pad = GridCell {
        data: Utf8Data::new(' '),
        ..status_cell(StatusStyle::Normal)
    };
    while x < width {
        tty.tty_cell(x, y, &pad);
        x += 1;
    }
    // Reset to default attributes so content drawing starts clean.
    let reset = GridCell::default_cell();
    tty.tty_attributes(&reset);
}

/// GridCell style for a status-line segment.
fn status_cell(style: StatusStyle) -> GridCell {
    use loom_core::colour::COLOUR_FLAG_256;
    use loom_core::grid_cell::GRID_ATTR_BRIGHT;
    let (fg, bg, attr) = match style {
        StatusStyle::Normal => (250, 236, 0),
        StatusStyle::Active => (16, 252, 0),
        StatusStyle::Alert => (214, 236, GRID_ATTR_BRIGHT),
    };
    GridCell {
        fg: fg as i32 | COLOUR_FLAG_256,
        bg: bg as i32 | COLOUR_FLAG_256,
        attr: attr as u16,
        ..GridCell::default_cell()
    }
}

/// Position cursor at the active pane's cursor position.
pub fn position_cursor(tty: &mut Tty, window: &Window) {
    if let Some(pid) = window.active_pane_id {
        if let Some(pane) = window.panes.get(&pid) {
            let cx = (pane.xoff as u32).saturating_add(pane.screen.cx)
                .min(window.sx.saturating_sub(1));
            let cy = (pane.yoff as u32).saturating_add(pane.screen.cy)
                .min(window.sy.saturating_sub(1));
            tty.tty_cursor(cx, cy);
        }
    }
}

/// Convenience: render a window into a byte buffer (for tests).
pub fn render_to_buffer(window: &Window) -> Vec<u8> {
    let mut tty = Tty::new(window.sx, window.sy);
    redraw_window(&mut tty, window);
    tty.take_output()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip ANSI escape sequences, leaving only the visible cell bytes.
    fn visible_bytes(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut bytes = s.bytes();
        while let Some(c) = bytes.next() {
            if c == 0x1b {
                // Skip an escape sequence: CSI introducer '[' (if any) plus
                // parameters, up to and including the final byte (0x40-0x7E).
                while let Some(nxt) = bytes.next() {
                    if nxt == b'[' {
                        while let Some(p) = bytes.next() {
                            if (0x40..=0x7e).contains(&p) {
                                break;
                            }
                        }
                        break;
                    }
                    // Non-CSI escape (e.g. "\x1bB"): stop at its final byte.
                    if (0x40..=0x7e).contains(&nxt) {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn test_status_line_renders_bottom_row() {
        let mut tty = Tty::new(80, 24);
        let segments = vec![
            (" main ".to_string(), StatusStyle::Normal),
            (" 0:shell ".to_string(), StatusStyle::Active),
            (" *1:vim ".to_string(), StatusStyle::Alert),
        ];
        draw_status_line(&mut tty, &segments);
        let out = tty.take_output();
        let s = String::from_utf8_lossy(&out);
        // Cursor moved to the bottom row.
        assert!(s.contains("\x1b[24;1H"));
        // Active window uses the inverted style (fg 16 / bg 252).
        assert!(s.contains("38;5;16;48;5;252"));
        // Session name uses the base style (fg 250 / bg 236).
        assert!(s.contains("38;5;250;48;5;236"));
        let visible = String::from_utf8_lossy(&visible_bytes(&s)).into_owned();
        assert!(visible.contains("main"));
        assert!(visible.contains("*1:vim"));
        // The line is padded to the full width (80 visible cells).
        assert_eq!(visible_bytes(&s).len(), 80);
    }

    #[test]
    fn test_status_line_redraw_has_no_cursor_flood() {
        let mut tty = Tty::new(80, 24);
        let segments = vec![(" main ".to_string(), StatusStyle::Normal)];
        draw_status_line(&mut tty, &segments);
        let first = tty.take_output();
        draw_status_line(&mut tty, &segments);
        let second = tty.take_output();
        // Re-drawing the identical line must not re-emit per-cell CUPs:
        // one positioning move plus the text, well under a full-screen blob.
        let s = String::from_utf8_lossy(&second);
        let cup_count = s.matches("\x1b[24;").count();
        assert!(
            cup_count <= 2,
            "second draw emitted {} CUPs to the status row",
            cup_count
        );
        assert!(second.len() < first.len() + 16);
    }
}
