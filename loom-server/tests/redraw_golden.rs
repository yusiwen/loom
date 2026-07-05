use std::io::Write;

use loom_server::redraw;
use loom_server::layout;
use loom_core::session::Window;
use loom_core::grid_cell::*;
use loom_core::utf8::Utf8Data;

/// Strip ANSI escape sequences from a string for plain-text comparison.
fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut in_escape = false;
    let mut in_csi = false;
    for ch in s.chars() {
        if in_csi {
            // In a CSI sequence: look for terminator letters
            match ch {
                '0'..='9' | ';' => {} // parameter chars, continue
                _ => {
                    // Any non-parameter char terminates CSI
                    in_escape = false;
                    in_csi = false;
                }
            }
        } else if in_escape {
            // After \x1b but before CSI introducer
            match ch {
                '[' => { in_csi = true; }
                _ => { in_escape = false; } // unknown escape, drop
            }
        } else {
            // Normal text
            match ch {
                '\x1b' => { in_escape = true; }
                _ => { result.push(ch); }
            }
        }
    }
    result
}

/// Render a window to an ANSI string.
fn render_raw(window: &Window) -> String {
    let mut buf = Vec::new();
    redraw::redraw_window(window, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

/// Render a window to plain text.
fn render_plain(window: &Window) -> String {
    let raw = render_raw(window);
    strip_ansi(&raw)
}

/// Write a character at (x, y) in a window's active pane.
fn put_char(window: &mut Window, x: u32, y: u32, ch: char, attr: u16) {
    if let Some(pid) = window.active_pane_id {
        if let Some(pane) = window.panes.get_mut(&pid) {
            let gc = GridCell {
                data: Utf8Data::new(ch),
                attr,
                ..Default::default()
            };
            pane.screen.grid.view_set_cell(x, y, &gc);
        }
    }
}

/// Run a golden test. If GENERATE env var is set, (re)create the golden file.
fn check_golden(name: &str, window: &Window) {
    let raw = render_raw(window);
    let plain = strip_ansi(&raw);
    let golden_path = format!("tests/golden/{}.txt", name);

    if std::env::var("DEBUG_RAW").is_ok() {
        eprintln!("=== RAW ANSI for {} ===", name);
        for (i, b) in raw.bytes().enumerate() {
            let c = if b == b'\x1b' { "␛".to_string() }
                    else if b.is_ascii_graphic() || b == b' ' { (b as char).to_string() }
                    else { format!("\\x{:02x}", b) };
            if i % 100 == 0 { eprint!("\n[{:04}] ", i); }
            eprint!("{}", c);
        }
        eprintln!();
    }

    if std::env::var("GENERATE").is_ok() {
        let mut f = std::fs::File::create(&golden_path).unwrap();
        f.write_all(plain.as_bytes()).unwrap();
        eprintln!("generated {} ({} chars)", golden_path, plain.len());
        return;
    }

    let expected = std::fs::read_to_string(&golden_path)
        .unwrap_or_else(|_| panic!("golden file not found: {}. Run with GENERATE=1 to create it.", golden_path));
    let expected = expected.trim_end_matches('\n').to_string();

    if plain != expected {
        // Show diff
        let diff_file = format!("/tmp/loom-golden-diff-{}.txt", name);
        let mut f = std::fs::File::create(&diff_file).unwrap();
        f.write_all(plain.as_bytes()).unwrap();
        panic!(
            "golden mismatch for '{}':\n\
             expected ({}):\n{}\n\
             actual ({}):\n{}\n\
             full actual written to {}",
            name,
            expected.len(),
            expected,
            plain.len(),
            plain,
            diff_file,
        );
    }
}

// ── Tests ──

#[test]
fn test_empty_window() {
    // A single pane window with no content
    let mut window = Window::new(20, 3);
    window.create_pane(20, 3);
    check_golden("empty_window", &window);
}

#[test]
fn test_text_content() {
    let mut window = Window::new(20, 5);
    window.create_pane(20, 5);
    put_char(&mut window, 0, 0, 'H', GRID_ATTR_BRIGHT);
    put_char(&mut window, 1, 0, 'e', 0);
    put_char(&mut window, 2, 0, 'l', 0);
    put_char(&mut window, 3, 0, 'l', 0);
    put_char(&mut window, 4, 0, 'o', 0);
    put_char(&mut window, 0, 1, 'W', GRID_ATTR_BRIGHT);
    put_char(&mut window, 1, 1, 'o', 0);
    put_char(&mut window, 2, 1, 'r', 0);
    put_char(&mut window, 3, 1, 'l', 0);
    put_char(&mut window, 4, 1, 'd', 0);
    check_golden("text_content", &window);
}

#[test]
fn test_split_two_panes() {
    let mut window = Window::new(20, 5);
    let p1 = window.create_pane(20, 5);
    put_char(&mut window, 0, 0, 'L', 0);
    let p2 = layout::layout_split_pane(&mut window, p1, false).unwrap();
    window.set_active_pane(p2);
    put_char(&mut window, 0, 0, 'R', 0);
    window.set_active_pane(p1);
    check_golden("split_two_panes", &window);
}

#[test]
fn test_split_vertical() {
    let mut window = Window::new(20, 6);
    let p1 = window.create_pane(20, 6);
    put_char(&mut window, 0, 0, 'T', 0);
    let p2 = layout::layout_split_pane(&mut window, p1, true).unwrap();
    window.set_active_pane(p2);
    put_char(&mut window, 0, 0, 'B', 0);
    window.set_active_pane(p1);
    check_golden("split_vertical", &window);
}
