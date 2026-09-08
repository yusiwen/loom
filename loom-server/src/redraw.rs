use loom_core::grid_cell::{GridCell, GRID_ATTR_BRIGHT};
use loom_core::session::{Window, WindowPane, WINDOW_ZOOMED};
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
            if pane.copy.active {
                // B4: copy-mode view — history/live rows at the pane's scroll
                // offset, with the active selection in reverse video.
                draw_copy_line(tty, pane, pane_y, y);
            } else {
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
}

/// Draw one row of a pane's copy-mode view: the grid row at the pane's
/// scroll offset, with cells in the active selection rendered in reverse
/// video.
fn draw_copy_line(tty: &mut Tty, pane: &WindowPane, view_y: u32, target_y: u32) {
    use loom_core::grid_cell::GRID_ATTR_REVERSE;

    let grid = &pane.screen.grid;
    let hsize = grid.hsize;
    let abs = pane.copy.abs_line(view_y, hsize);
    let sel = pane.copy.selection_range(hsize);
    for x in 0..pane.sx {
        let mut cell = grid.get_cell(x, abs).copied().unwrap_or_default();
        if let Some((lo, hi)) = sel {
            if (abs, x) >= lo && (abs, x) <= hi {
                cell.attr |= GRID_ATTR_REVERSE;
            }
        }
        tty.tty_cell(pane.xoff as u32 + x, target_y, &cell);
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
pub fn draw_status_line(tty: &mut Tty, window: &Window, segments: &[(String, StatusStyle)]) {
    let y = tty.sy.saturating_sub(1);
    let width = tty.sx;
    tty.tty_cursor(0, y);

    let mut x: u32 = 0;
    for (text, style) in segments {
        if x >= width {
            break;
        }
        let cell = status_cell(window, *style);
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
        ..status_cell(window, StatusStyle::Normal)
    };
    while x < width {
        tty.tty_cell(x, y, &pad);
        x += 1;
    }
    // Reset to default attributes so content drawing starts clean.
    let reset = GridCell::default_cell();
    tty.tty_attributes(&reset);
}

/// GridCell style for a status-line segment, drawn from the window's options
/// (B8): `status-fg`/`status-bg` for Normal, `status-active-*` for Active,
/// `status-alert-*` for Alert. Options fall back to the defaults table.
fn status_cell(window: &Window, style: StatusStyle) -> GridCell {
    use loom_core::colour::COLOUR_FLAG_256;
    let o = &window.options;
    let (mut fg, mut bg, mut attr): (i32, i32, u16) = match style {
        StatusStyle::Normal => (
            o.get_number("status-fg") as i32,
            o.get_number("status-bg") as i32,
            0,
        ),
        StatusStyle::Active => (
            o.get_number("status-active-fg") as i32,
            o.get_number("status-active-bg") as i32,
            0,
        ),
        StatusStyle::Alert => (
            o.get_number("status-alert-fg") as i32,
            o.get_number("status-alert-bg") as i32,
            GRID_ATTR_BRIGHT,
        ),
    };
    // A colour value of 8 means "default"; leave it unflagged when so.
    if fg == 8 {
        fg = 8;
    } else {
        fg |= COLOUR_FLAG_256;
    }
    if bg != 8 {
        bg |= COLOUR_FLAG_256;
    } else {
        bg = 8;
    }
    attr &= 0xffff;
    GridCell {
        fg: fg as i32,
        bg: bg as i32,
        attr,
        ..GridCell::default_cell()
    }
}

/// Position cursor at the active pane's cursor position.
pub fn position_cursor(tty: &mut Tty, window: &Window) {
    if let Some(pid) = window.active_pane_id {
        if let Some(pane) = window.panes.get(&pid) {
            // B4: while the active pane is in copy-mode, the visible cursor
            // is the copy-mode cursor, not the live screen's.
            let (cx, cy) = if pane.copy.active {
                (pane.copy.cx, pane.copy.cy)
            } else {
                (pane.screen.cx, pane.screen.cy)
            };
            let cx = (pane.xoff as u32).saturating_add(cx).min(window.sx.saturating_sub(1));
            let cy = (pane.yoff as u32).saturating_add(cy).min(window.sy.saturating_sub(1));
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

/// Draw a centered popup overlay (Phase C) over the current Tty content.
/// `title`/`lines`/`width`/`height` come from `Server::popup`; the box is
/// drawn with reverse-video borders so it reads as an overlay. Called after
/// the status line and content so it sits on top.
pub fn draw_popup(
    tty: &mut Tty,
    title: &str,
    lines: &[String],
    width: u32,
    height: u32,
) {
    let sw = tty.sx;
    let sh = tty.sy.saturating_sub(1); // keep above the status row
    let w = width.clamp(10, sw.saturating_sub(2)).max(10);
    let h = height.clamp(3, sh.saturating_sub(2)).max(3);
    let x0 = (sw.saturating_sub(w)) / 2;
    let y0 = (sh.saturating_sub(h)) / 2;

    let bottom_mid = '\u{2534}'; // ┴
    let vert = '\u{2502}'; // │
    let horiz = '\u{2500}'; // ─
    let tl = '\u{250c}'; // ┌
    let tr = '\u{2510}'; // ┐
    let bl = '\u{2514}'; // └
    let br = '\u{2518}'; // ┘

    let mut put = |x: u32, y: u32, ch: char, fg: i32| {
        let mut cell = GridCell {
            fg: fg | 0x01000000,
            bg: 236 | 0x01000000,
            attr: GRID_ATTR_BRIGHT,
            ..GridCell::default_cell()
        };
        cell.data = Utf8Data::new(ch);
        tty.tty_cell(x, y, &cell);
    };

    let title_str: Vec<char> = title.chars().collect();
    // Top border.
    for x in 0..w {
        let ch = if x == 0 { tl } else if x + 1 == w { tr } else { horiz };
        put(x0 + x, y0, ch, 39);
    }
    // Title row (row 1): corners + title text.
    if h >= 2 {
        put(x0, y0 + 1, vert, 33);
        let mut ti = 0;
        for x in 1..w.saturating_sub(1) {
            let ch = if ti < title_str.len() { title_str[ti] } else { ' ' };
            put(x0 + x, y0 + 1, ch, 33);
            ti += 1;
        }
        put(x0 + w - 1, y0 + 1, vert, 33);
    }
    // Body rows.
    for r in 0..h.saturating_sub(2) {
        let y = y0 + 2 + r;
        put(x0, y, vert, 39);
        let line = lines.get(r as usize).cloned().unwrap_or_default();
        let mut li = 0;
        for x in 1..w.saturating_sub(1) {
            let ch = line.chars().nth(li).unwrap_or(' ');
            put(x0 + x, y, ch, 39);
            li += 1;
        }
        put(x0 + w - 1, y, vert, 39);
    }
    // Bottom border.
    let yb = y0 + h - 1;
    for x in 0..w {
        let ch = if x == 0 { bl } else if x + 1 == w { br } else { bottom_mid };
        put(x0 + x, yb, ch, 39);
    }
    // Reset attributes.
    tty.tty_attributes(&GridCell::default_cell());
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
        let window = Window::new(80, 24);
        let segments = vec![
            (" main ".to_string(), StatusStyle::Normal),
            (" 0:shell ".to_string(), StatusStyle::Active),
            (" *1:vim ".to_string(), StatusStyle::Alert),
        ];
        draw_status_line(&mut tty, &window, &segments);
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
        let window = Window::new(80, 24);
        let segments = vec![(" main ".to_string(), StatusStyle::Normal)];
        draw_status_line(&mut tty, &window, &segments);
        let first = tty.take_output();
        draw_status_line(&mut tty, &window, &segments);
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

    /// B4: a pane in copy-mode renders its selection in reverse video and
    /// places the cursor at the copy-mode position, not the live screen's.
    #[test]
    fn test_copy_mode_renders_selection_and_cursor() {
        use loom_core::grid_cell::GridCell;
        use loom_core::session::WindowPane;
        use loom_core::utf8::Utf8Data;

        let mut window = Window::new(20, 5);
        let wid = window.id;
        let mut pane = WindowPane::new(wid, 20, 5);
        let pid = pane.id;
        // "hello world" on the top live row.
        for (i, ch) in "hello world".chars().enumerate() {
            pane.screen.grid.set_cell(i as u32, 0, &GridCell {
                data: Utf8Data::new(ch),
                ..GridCell::default_cell()
            });
        }
        // Enter copy mode: cursor at col 4, row 0; visual selection from
        // (line 0, col 0) to the cursor (line 0, col 4) => "hell".
        pane.copy.active = true;
        pane.copy.scroll = 0;
        pane.copy.cx = 4;
        pane.copy.cy = 0;
        pane.copy.visual = true;
        pane.copy.sel_anchor = Some((0, 0));
        window.panes.insert(pid, pane);
        window.active_pane_id = Some(pid);

        let mut tty = Tty::new(20, 5);
        redraw_window(&mut tty, &window);
        position_cursor(&mut tty, &window);
        let s = String::from_utf8_lossy(&tty.take_output()).into_owned();

        // The selection must be rendered with the reverse attribute (SGR 7).
        assert!(
            s.contains(";7m"),
            "expected reverse-video SGR in copy-mode output, got: {s}"
        );
        // The copy-mode cursor (row 0 -> screen row 1, col 4 -> screen col 5)
        // must be positioned, not the live screen cursor.
        assert!(
            s.contains("\x1b[1;5H"),
            "expected CUP to copy-mode cursor position, got: {s}"
        );
    }

    /// Phase C: draw_popup renders a centered box with a border and title,
    /// visible in the output bytes.
    #[test]
    fn test_draw_popup_renders_box() {
        let mut tty = Tty::new(80, 24);
        let lines = vec!["hello".to_string(), "world".to_string()];
        draw_popup(&mut tty, "title", &lines, 20, 5);
        let s = String::from_utf8_lossy(&tty.take_output()).into_owned();
        // Box glyphs from the Unicode box-drawing set are present.
        assert!(s.contains('\u{250c}')); // ┌ top-left
        assert!(s.contains('\u{2518}')); // ┘ bottom-right
        assert!(s.contains("title"));
        assert!(s.contains("hello"));
    }
}
