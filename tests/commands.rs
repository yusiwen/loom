use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use loom_commands::cmd::{CmdCtx, CmdRetval, Registry};
use loom_commands::commands::all_commands;
use loom_core::session::{Session, Window};

fn make_ctx(sessions: &mut HashMap<u32, Session>, windows: &mut HashMap<u32, Window>) -> CmdCtx {
    CmdCtx {
        sessions,
        windows,
        session_id: None,
        client_flags: 0,
    }
}

#[test]
fn test_new_session() {
    let registry = Registry::new(&all_commands());
    let mut sessions = HashMap::new();
    let mut windows = HashMap::new();
    let mut ctx = make_ctx(&mut sessions, &mut windows);

    let result = registry.get("new-session").unwrap().exec(
        &mut ctx,
        &loom_commands::parser::parse_command_line("new-session -s test").unwrap().1,
    );
    assert_eq!(result, CmdRetval::Normal);
    assert!(ctx.session_id.is_some());
    let sid = ctx.session_id.unwrap();
    let session = sessions.get(&sid).unwrap();
    assert_eq!(session.name, "test");
    assert_eq!(session.windows.len(), 1);
}

#[test]
fn test_split_window() {
    let registry = Registry::new(&all_commands());
    let mut sessions = HashMap::new();
    let mut windows = HashMap::new();
    let mut ctx = make_ctx(&mut sessions, &mut windows);

    // Create session first
    registry.get("new-session").unwrap().exec(
        &mut ctx,
        &loom_commands::parser::parse_command_line("new-session -s split-test").unwrap().1,
    );

    let sid = ctx.session_id.unwrap();
    let session = sessions.get(&sid).unwrap();
    assert_eq!(session.windows.len(), 1);
    let wid = session.current_winlink().unwrap().window_id;
    let window = windows.get(&wid).unwrap();
    let initial_panes = window.panes.len();

    // Split window
    registry.get("split-window").unwrap().exec(
        &mut ctx,
        &loom_commands::parser::parse_command_line("split-window -h").unwrap().1,
    );

    let window = windows.get(&wid).unwrap();
    assert_eq!(window.panes.len(), initial_panes + 1);
}

#[test]
fn test_multi_commands() {
    let registry = Registry::new(&all_commands());
    let mut sessions = HashMap::new();
    let mut windows = HashMap::new();
    let mut ctx = make_ctx(&mut sessions, &mut windows);

    // Sequence: new-session → new-window → list-windows
    registry.get("new-session").unwrap().exec(
        &mut ctx,
        &loom_commands::parser::parse_command_line("new-session -s multi").unwrap().1,
    );
    registry.get("new-window").unwrap().exec(
        &mut ctx,
        &loom_commands::parser::parse_command_line("new-window").unwrap().1,
    );

    let sid = ctx.session_id.unwrap();
    let session = sessions.get(&sid).unwrap();
    assert_eq!(session.windows.len(), 2);
}

#[test]
fn test_kill_session() {
    let registry = Registry::new(&all_commands());
    let mut sessions = HashMap::new();
    let mut windows = HashMap::new();
    let mut ctx = make_ctx(&mut sessions, &mut windows);

    registry.get("new-session").unwrap().exec(
        &mut ctx,
        &loom_commands::parser::parse_command_line("new-session -s killme").unwrap().1,
    );
    assert_eq!(sessions.len(), 1);

    registry.get("kill-session").unwrap().exec(
        &mut ctx,
        &loom_commands::parser::parse_command_line("kill-session").unwrap().1,
    );
    assert_eq!(sessions.len(), 0);
}
