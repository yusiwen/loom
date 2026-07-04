use loom_core::grid_cell::*;
use loom_core::session::Window;

/// Draw a complete window to a writable output (e.g. stdout or a TTY file).
pub fn redraw_window(window: &Window, output: &mut impl std::io::Write) -> std::io::Result<()> {
    for y in 0..window.sy {
        // For each visible line, draw the content from each pane
        for (_, pane) in &window.panes {
            // Check if this pane occupies this y-coordinate
            let pane_y = y as i32 - pane.yoff;
            if pane_y < 0 || pane_y >= pane.sy as i32 {
                continue;
            }
            let pane_y = pane_y as u32;
            let screen = &pane.screen;
            let grid = &screen.grid;

            // Position cursor at pane offset
            write!(output, "\x1b[{};{}H", y + 1, pane.xoff + 1)?;

            // Draw each cell in this line
            let default_cell = GridCell::default_cell();
            for x in 0..pane.sx {
                let cell = grid
                    .view_get_cell(x, pane_y)
                    .unwrap_or(&default_cell);

                if cell.is_padding() {
                    write!(output, " ")?;
                    continue;
                }

                // Set attributes if changed
                let fg = cell.fg;
                let bg = cell.bg;
                let attr = cell.attr;

                // Build SGR
                let mut sgr = String::from("\x1b[0");
                if attr & GRID_ATTR_BRIGHT != 0 { sgr.push_str(";1"); }
                if attr & GRID_ATTR_DIM != 0 { sgr.push_str(";2"); }
                if attr & GRID_ATTR_ITALICS != 0 { sgr.push_str(";3"); }
                if attr & GRID_ATTR_UNDERSCORE != 0 { sgr.push_str(";4"); }
                if attr & GRID_ATTR_BLINK != 0 { sgr.push_str(";5"); }
                if attr & GRID_ATTR_REVERSE != 0 { sgr.push_str(";7"); }
                if attr & GRID_ATTR_HIDDEN != 0 { sgr.push_str(";8"); }
                if attr & GRID_ATTR_STRIKETHROUGH != 0 { sgr.push_str(";9"); }

                // Foreground color
                if fg != 8 {
                    sgr.push_str(&format!(
                        ";38;2;{};{};{}",
                        ((fg >> 16) & 0xff) as u8,
                        ((fg >> 8) & 0xff) as u8,
                        (fg & 0xff) as u8,
                    ));
                }
                // Background color
                if bg != 8 {
                    sgr.push_str(&format!(
                        ";48;2;{};{};{}",
                        ((bg >> 16) & 0xff) as u8,
                        ((bg >> 8) & 0xff) as u8,
                        (bg & 0xff) as u8,
                    ));
                }
                sgr.push('m');
                write!(output, "{}", sgr)?;

                // Write character
                let ch = cell.data.to_char();
                write!(output, "{}", ch)?;
            }
        }
        // Fill remaining space on line with blanks
        write!(output, "\x1b[0m\x1b[K")?;
    }
    output.flush()?;
    Ok(())
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
        let p2 = layout_split_pane(&mut window, p1, false).unwrap();
        assert!(window.panes.contains_key(&p2));

        // Add some content
        if let Some(pane) = window.panes.get_mut(&p1) {
            pane.screen.cx = 5;
            pane.screen.cy = 3;
        }

        let mut buf = Vec::new();
        redraw_window(&window, &mut buf).unwrap();
        assert!(!buf.is_empty());
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("\x1b["));
    }
}
