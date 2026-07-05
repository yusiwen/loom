use std::collections::HashMap;

use loom_commands::format::{format_expand, FormatCtx};
use loom_core::session::{Session, Window};

#[test]
fn test_format_session_name() {
    let session = Session::new(Some("my-session"), "/home/user");
    let ctx = FormatCtx {
        session: Some(&session),
        window: None,
        pane: None,
        extra: HashMap::new(),
    };

    let result = format_expand("Session: #{session_name}", &ctx);
    assert_eq!(result, "Session: my-session");
}

#[test]
fn test_format_window_and_pane() {
    let session = Session::new(Some("test"), "/tmp");
    let mut window = Window::new(120, 40);
    let pid = window.create_pane(120, 40);
    let pane = window.panes.get(&pid).unwrap();
    window.set_active_pane(pid);

    let ctx = FormatCtx {
        session: Some(&session),
        window: Some(&window),
        pane: Some(pane),
        extra: HashMap::new(),
    };

    let result = format_expand("#{session_name}:#{window_width}x#{window_height} %#{pane_id}", &ctx);
    assert_eq!(result, format!("test:120x40 %{}", pid));
}

#[test]
fn test_format_with_extra_vars() {
    let mut extra = HashMap::new();
    extra.insert("custom_var".to_string(), "hello".to_string());

    let ctx = FormatCtx {
        session: None,
        window: None,
        pane: None,
        extra,
    };

    let result = format_expand("value=#{custom_var}", &ctx);
    assert_eq!(result, "value=hello");
}
