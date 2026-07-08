use std::fs;
use std::io::Write;

use loom_server::redraw;
use loom_core::session::Window;
use loom_core::grid_cell::*;
use loom_core::utf8::Utf8Data;

/// Strip ANSI escape sequences.
fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut esc = false;
    let mut csi = false;
    for ch in s.chars() {
        if csi {
            match ch {
                '0'..='9' | ';' => {}
                _ => { csi = false; esc = false; }
            }
        } else if esc {
            if ch == '[' { csi = true; }
            else { esc = false; }
        } else if ch == '\x1b' {
            esc = true;
        } else {
            result.push(ch);
        }
    }
    result
}

fn render_plain(window: &Window) -> String {
    let mut buf = Vec::new();
    redraw::redraw_window(window, &mut buf).unwrap();
    let ansi = String::from_utf8(buf).unwrap();
    strip_ansi(&ansi)
}

fn put_char(window: &mut Window, x: u32, y: u32, ch: char, attr: u16) {
    if let Some(pid) = window.active_pane_id {
        if let Some(pane) = window.panes.get_mut(&pid) {
            let gc = GridCell { data: Utf8Data::new(ch), attr, ..Default::default() };
            pane.screen.grid.view_set_cell(x, y, &gc);
        }
    }
}

fn put_str(window: &mut Window, x: u32, y: u32, s: &str, attr: u16) {
    for (i, ch) in s.chars().enumerate() {
        put_char(window, x + i as u32, y, ch, attr);
    }
}

fn check_scenario(name: &str, window: &Window) {
    let plain = render_plain(window);
    let golden_path = format!("loom-server/tests/mock_golden/{}.txt", name);

    if std::env::var("GENERATE").is_ok() {
        let mut f = fs::File::create(&golden_path).unwrap();
        f.write_all(plain.as_bytes()).unwrap();
        eprintln!("generated {}.txt ({} chars)", name, plain.len());
        return;
    }

    let expected = fs::read_to_string(&golden_path)
        .unwrap_or_else(|_| panic!("golden not found at {}. Run with GENERATE=1", golden_path));
    let expected = expected.trim_end_matches('\n').to_string();

    if plain != expected {
        let diff = format!("/tmp/loom-render-diff-{}.txt", name);
        let mut f = fs::File::create(&diff).unwrap();
        f.write_all(plain.as_bytes()).unwrap();
        panic!(
            "golden mismatch '{}': expected {} chars, got {} chars.\nActual in {}",
            name, expected.len(), plain.len(), diff
        );
    }
}

// ── Tests ──

#[test]
fn test_empty_80x24() {
    let mut window = Window::new(80, 24);
    window.create_pane(80, 24);
    check_scenario("empty_80x24", &window);
}

#[test]
fn test_hello_world_80x24() {
    let mut window = Window::new(80, 24);
    window.create_pane(80, 24);
    put_str(&mut window, 0, 0, "Hello, World!", 0);
    check_scenario("hello_world_80x24", &window);
}

#[test]
fn test_resize_106x61() {
    let mut window = Window::new(106, 61);
    window.create_pane(106, 61);
    put_str(&mut window, 0, 0, "This is a 106x61 window", GRID_ATTR_BRIGHT);
    check_scenario("resize_106x61", &window);
}

#[test]
fn test_right_prompt_80() {
    // Simulate a Powerlevel10k-style right prompt at column 62 (80 - 18)
    let mut window = Window::new(80, 24);
    window.create_pane(80, 24);
    put_str(&mut window, 62, 0, "yusiwen@nuc12wski5", 0);
    put_str(&mut window, 0, 0, "~/git/loom main", 0);
    check_scenario("right_prompt_80", &window);
}

#[test]
fn test_right_prompt_106() {
    // Right prompt at column 88 on 106-wide terminal
    let mut window = Window::new(106, 61);
    window.create_pane(106, 61);
    put_str(&mut window, 88, 0, "yusiwen@nuc12wski5", 0);
    put_str(&mut window, 0, 0, "~/git/loom main", 0);
    check_scenario("right_prompt_106", &window);
}

#[test]
fn test_split_two_panes() {
    let mut window = Window::new(80, 24);
    let p1 = window.create_pane(80, 24);
    put_str(&mut window, 0, 0, "LEFT PANE", 0);
    let _p2 = loom_server::layout::layout_split_pane(&mut window, p1, false).unwrap();
    // Set active to p2
    check_scenario("split_two_panes", &window);
}

#[test]
fn test_bold_and_normal() {
    let mut window = Window::new(80, 24);
    window.create_pane(80, 24);
    put_str(&mut window, 0, 0, "BOLD: ", GRID_ATTR_BRIGHT);
    put_str(&mut window, 6, 0, "bold", GRID_ATTR_BRIGHT);
    put_str(&mut window, 10, 0, " normal", 0);
    put_str(&mut window, 18, 0, " dim", GRID_ATTR_DIM);
    check_scenario("bold_and_normal", &window);
}
