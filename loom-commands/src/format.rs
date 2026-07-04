use std::collections::HashMap;

use loom_core::session::{Session, Window, WindowPane};

/// Context for format expansion.
pub struct FormatCtx<'a> {
    pub session: Option<&'a Session>,
    pub window: Option<&'a Window>,
    pub pane: Option<&'a WindowPane>,
    pub extra: HashMap<String, String>,
}

impl<'a> FormatCtx<'a> {
    pub fn new() -> Self {
        Self {
            session: None,
            window: None,
            pane: None,
            extra: HashMap::new(),
        }
    }
}

/// Known format variables and their values.
fn format_value(key: &str, ctx: &FormatCtx) -> Option<String> {
    // Check extra vars first
    if let Some(v) = ctx.extra.get(key) {
        return Some(v.clone());
    }
    match key {
        // Session variables
        "session_id" => ctx.session.map(|s| format!("${}", s.id)),
        "session_name" => ctx.session.map(|s| s.name.clone()),
        "session_windows" => ctx.session.map(|s| s.windows.len().to_string()),
        "session_attached" => ctx.session.map(|s| s.attached.to_string()),
        "session_created" => ctx.session.map(|s| s.creation_time.to_string()),

        // Window variables
        "window_id" => ctx.window.map(|w| format!("@{}", w.id)),
        "window_name" => ctx.window.map(|w| w.name.clone()),
        "window_width" => ctx.window.map(|w| w.sx.to_string()),
        "window_height" => ctx.window.map(|w| w.sy.to_string()),
        "window_panes" => ctx.window.map(|w| w.panes.len().to_string()),
        "window_index" => ctx.window.and_then(|_| {
            ctx.session.and_then(|s| {
                s.windows.iter().find(|(_, wl)| {
                    ctx.window.map(|w| wl.window_id == w.id).unwrap_or(false)
                }).map(|(idx, _)| idx.to_string())
            })
        }),
        "window_active" => ctx.window.map(|w| {
            if w.active_pane_id.is_some() { "1" } else { "0" }
        }.to_string()),

        // Pane variables
        "pane_id" => ctx.pane.map(|p| format!("%{}", p.id)),
        "pane_width" => ctx.pane.map(|p| p.sx.to_string()),
        "pane_height" => ctx.pane.map(|p| p.sy.to_string()),
        "pane_left" => ctx.pane.map(|p| p.xoff.to_string()),
        "pane_top" => ctx.pane.map(|p| p.yoff.to_string()),
        "pane_pid" => ctx.pane.and_then(|p| p.pid.map(|pid| pid.to_string())),
        "pane_active" => ctx.window.zip(ctx.pane).map(|(w, p)| {
            (if w.active_pane_id == Some(p.id) { "1" } else { "0" }).to_string()
        }),

        // Literal values
        "cursor_x" => ctx.pane.map(|p| p.screen.cx.to_string()),
        "cursor_y" => ctx.pane.map(|p| p.screen.cy.to_string()),

        // System
        "host" => std::env::var("HOSTNAME").or_else(|_| std::env::var("HOST")).ok(),
        "host_short" => std::env::var("HOSTNAME").or_else(|_| std::env::var("HOST")).ok()
            .map(|h| h.split('.').next().unwrap_or(&h).to_string()),
        "pid" => Some(std::process::id().to_string()),
        "uid" => Some(format!("{}", unsafe { nix::libc::getuid() })),

        _ => None,
    }
}

/// Expand format strings replacing #{var} with values.
pub fn format_expand(template: &str, ctx: &FormatCtx) -> String {
    let mut result = String::new();
    let mut i = 0;
    let bytes = template.as_bytes();

    while i < bytes.len() {
        if bytes[i] == b'#' && i + 2 < bytes.len() && bytes[i + 1] == b'{' {
            // Find matching }
            let start = i + 2;
            let mut depth = 1;
            let mut j = start;
            while j < bytes.len() && depth > 0 {
                if bytes[j] == b'{' { depth += 1; }
                else if bytes[j] == b'}' { depth -= 1; }
                j += 1;
            }
            if depth == 0 {
                let key = &template[start..j - 1];
                let value = format_value(key, ctx)
                    .unwrap_or_else(|| format!("#{{{}}}", key));
                result.push_str(&value);
                i = j;
            } else {
                result.push('#');
                i += 1;
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_core::session::{Session, Window, WindowPane};

    #[test]
    fn test_simple_var() {
        let ctx = FormatCtx::new();
        assert_eq!(format_expand("hello", &ctx), "hello");
    }

    #[test]
    fn test_session_name() {
        let mut session = Session::new(Some("test-session"), "/tmp");
        let mut window = Window::new(80, 24);
        let pid = window.create_pane(80, 24);
        let pane = window.panes.get(&pid).unwrap();

        let ctx = FormatCtx {
            session: Some(&session),
            window: Some(&window),
            pane: Some(pane),
            extra: HashMap::new(),
        };

        let result = format_expand("#{session_name}", &ctx);
        assert_eq!(result, "test-session");
    }

    #[test]
    fn test_session_name_with_prefix() {
        let mut session = Session::new(Some("s1"), "/tmp");
        let mut window = Window::new(80, 24);
        session.attach_window(0, window.id);
        window.id = 5;

        let ctx = FormatCtx {
            session: Some(&session),
            window: Some(&window),
            pane: None,
            extra: HashMap::new(),
        };

        let result = format_expand("Session: #{session_name}", &ctx);
        assert_eq!(result, "Session: s1");
    }

    #[test]
    fn test_unknown_var() {
        let ctx = FormatCtx::new();
        let result = format_expand("#{unknown_var}", &ctx);
        assert_eq!(result, "#{unknown_var}");
    }
}
