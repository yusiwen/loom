//! Host-only differential oracle: does loom render `eza`'s output the same as
//! a real terminal?
//!
//! Method: take one fixed byte stream (real `eza -l --color=always` output
//! captured from a PTY) and render it twice through the *same* independent
//! terminal emulator (`tmux`, with an isolated server socket and
//! `-f /dev/null` so no user config interferes):
//!
//! 1. **reference** — the raw eza bytes, as a terminal would show them;
//! 2. **loom** — loom's own re-emitted screen, after parsing the very same
//!    bytes through the production parser + renderer.
//!
//! Both captures are then decoded with the same `Vt` into a cell grid
//! (character + fg + bg + attr) and compared cell by cell. Decoding is what
//! makes the comparison meaningful: `\x1b[90m` and `\x1b[38;5;8m` are the same
//! bright black, and `\x1b[1m\x1b[33m` equals `\x1b[0;1;33m` — byte strings
//! differ, cells do not.
//!
//! Runs only where `tmux` and a PTY are available (skips otherwise), so it is
//! safe to include in the default suite; it is meaningful on a host.
//!
//! Regenerate the input fixture (needs a PTY):
//!   `script -q /dev/null eza -l --color=always > tests/fixtures/eza_l.raw`
//! (strip the macOS `script` "^D\b\b" prologue).

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use loom_core::session::Window;
use loom_input::input::Parser;
use loom_server::redraw;
use loom_server::vt::Vt;

const SX: u32 = 80;
const SY: u32 = 40;

fn fixture() -> &'static [u8] {
    include_bytes!("fixtures/eza_l.raw")
}

/// Render raw terminal bytes through loom's production parser + renderer.
fn loom_render(bytes: &[u8]) -> Vec<u8> {
    let mut window = Window::new(SX, SY);
    let pane_id = window.create_pane(SX, SY);
    let mut parser = Parser::new();
    if let Some(pane) = window.panes.get_mut(&pane_id) {
        parser.parse_buf(&mut pane.screen, bytes);
    }
    redraw::render_to_buffer(&window)
}

/// Render raw terminal bytes through a real terminal (tmux) and return the
/// resulting `capture-pane -e` stream. `None` means tmux/PTY unavailable.
fn tmux_capture(label: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    let base = PathBuf::from(format!("/tmp/loom-oracle-{}-{}", std::process::id(), label));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).ok()?;
    let sock = base.join("tmux.sock");
    let sock = sock.to_str()?;
    let input = base.join("in.raw");
    let done = base.join("done");
    std::fs::write(&input, bytes).ok()?;

    let tmux = |args: &[&str]| {
        Command::new("tmux")
            .args(["-S", sock, "-f", "/dev/null"])
            .args(args)
            .output()
    };

    // A done-file avoids writing a marker into the pane, which would shift
    // the captured rows.
    let cmdline = format!(
        "cat {}; : > {}; sleep 60",
        input.display(),
        done.display()
    );
    let out = tmux(&["new-session", "-d", "-x", "80", "-y", "40", "--", &cmdline]).ok()?;
    if !out.status.success() {
        eprintln!(
            "SKIP eza_oracle: tmux could not start a pane: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        let _ = tmux(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&base);
        return None;
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    while !done.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }

    let cap = tmux(&["capture-pane", "-p", "-e", "-t", "0"]).ok()?;
    let _ = tmux(&["kill-server"]);
    let _ = std::fs::remove_dir_all(&base);
    Some(cap.stdout)
}

/// Decode a `capture-pane` stream into a screen. capture-pane separates rows
/// with bare LF, so add CR to home each row before feeding the emulator.
fn decode(capture: &[u8]) -> Vt {
    let mut stream = Vec::with_capacity(capture.len() + 64);
    for (i, b) in capture.iter().enumerate() {
        if *b == b'\n' && (i == 0 || capture[i - 1] != b'\r') {
            stream.push(b'\r');
        }
        stream.push(*b);
    }
    let mut vt = Vt::new(SX, SY);
    vt.feed(&stream);
    vt
}

/// Render `raw` through loom and through a real terminal, then compare the
/// resulting screens cell by cell. `Err("real terminal unavailable")` means
/// tmux/PTY is missing; any other `Err` is a human-readable diff.
fn compare_with_real_terminal(label: &str, raw: &[u8]) -> Result<(), String> {
    let loom_bytes = loom_render(raw);
    let _ = std::fs::write(format!("/tmp/loom-oracle-{label}-render.raw"), &loom_bytes);

    let reference_raw = tmux_capture(&format!("{label}-ref"), raw)
        .ok_or_else(|| "real terminal unavailable".to_string())?;
    let loom_raw = tmux_capture(&format!("{label}-loom"), &loom_bytes)
        .ok_or_else(|| "real terminal unavailable".to_string())?;
    let _ = std::fs::write(format!("/tmp/loom-oracle-{label}-ref.raw"), &reference_raw);
    let _ = std::fs::write(format!("/tmp/loom-oracle-{label}-loom.raw"), &loom_raw);

    let reference = decode(&reference_raw);
    let loom = decode(&loom_raw);

    let mut diffs = Vec::new();
    for y in 0..SY {
        for x in 0..SX {
            let a = reference.cell_at(x, y).unwrap_or_default();
            let b = loom.cell_at(x, y).unwrap_or_default();
            if a != b {
                diffs.push(format!("({x},{y}) terminal={a:?} loom={b:?}"));
            }
        }
    }
    if diffs.is_empty() {
        Ok(())
    } else {
        Err(diffs.iter().take(20).cloned().collect::<Vec<_>>().join("\n  "))
    }
}

fn report(label: &str, result: Result<(), String>) {
    match result {
        Ok(()) => eprintln!("{label}: OK — all {SX}x{SY} cells match a real terminal"),
        Err(e) if e == "real terminal unavailable" => eprintln!("SKIP {label}: {e}"),
        Err(diff) => panic!("loom's {label} rendering differs from a real terminal:\n  {diff}"),
    }
}

#[test]
fn loom_renders_eza_the_same_as_a_real_terminal() {
    report("eza_oracle", compare_with_real_terminal("eza", fixture()));
}

/// Regression: a real p10k prompt captured from zsh must render identically.
/// This is the case where `❯` (U+276F) was truncated to 'o' and Nerd Font fill
/// glyphs became invalid UTF-8 (rendered as U+FFFD).
#[test]
fn loom_renders_zsh_prompt_the_same_as_a_real_terminal() {
    let raw = include_bytes!("fixtures/zsh_p10k_prompt.raw");
    report("prompt_oracle", compare_with_real_terminal("prompt", raw));
}
