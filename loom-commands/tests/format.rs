use std::collections::HashMap;
use loom_commands::format::{format_expand, FormatCtx};
use loom_core::session::{Session, Window};

#[test]
fn test_session_name() {
    let session = Session::new(Some("my-session"), "/home/user");
    let ctx = FormatCtx {
        session: Some(&session),
        window: None,
        pane: None,
        extra: HashMap::new(),
    };
    assert_eq!(format_expand("#{session_name}", &ctx), "my-session");
}

#[test]
fn test_window_dimensions() {
    let mut window = Window::new(120, 40);
    let pid = window.create_pane(120, 40);
    let pane = window.panes.get(&pid).unwrap();

    let ctx = FormatCtx {
        session: None,
        window: Some(&window),
        pane: Some(pane),
        extra: HashMap::new(),
    };
    let result = format_expand("#{window_width}x#{window_height}", &ctx);
    assert_eq!(result, "120x40");
}

#[test]
fn test_extra_vars() {
    let mut extra = HashMap::new();
    extra.insert("custom".to_string(), "hello".to_string());
    let ctx = FormatCtx { session: None, window: None, pane: None, extra };
    assert_eq!(format_expand("#{custom}", &ctx), "hello");
}

#[test]
fn test_unknown_var() {
    let ctx = FormatCtx { session: None, window: None, pane: None, extra: HashMap::new() };
    assert_eq!(format_expand("#{nonexistent}", &ctx), "#{nonexistent}");
}

#[test]
fn test_no_template() {
    let ctx = FormatCtx { session: None, window: None, pane: None, extra: HashMap::new() };
    assert_eq!(format_expand("plain text", &ctx), "plain text");
}
