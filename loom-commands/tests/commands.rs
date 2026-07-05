use std::collections::HashMap;
use loom_commands::cmd::{CmdCtx, CmdRetval, Cmd, Registry};
use loom_commands::commands::all_commands;
use loom_commands::parser;
use loom_core::session::{Session, Window};

fn run(reg: &Registry, sessions: &mut HashMap<u32, Session>,
    windows: &mut HashMap<u32, Window>, sid: Option<u32>, cmdline: &str) -> (CmdRetval, Option<u32>)
{
    let (name, args) = parser::parse_command_line(cmdline).unwrap();
    let cmd: &dyn Cmd = reg.get(name).unwrap();
    let mut ctx = CmdCtx { sessions, windows, session_id: sid, client_flags: 0 };
    let ret = cmd.exec(&mut ctx, &args);
    (ret, ctx.session_id)
}

#[test]
fn test_new_session() {
    let reg = Registry::new(&all_commands());
    let mut sessions = HashMap::new();
    let mut windows = HashMap::new();
    let (_, sid) = run(&reg, &mut sessions, &mut windows, None, "new-session -s test");
    let s = sid.unwrap();
    assert_eq!(sessions.get(&s).unwrap().name, "test");
    assert_eq!(sessions.get(&s).unwrap().windows.len(), 1);
}

#[test]
fn test_split() {
    let reg = Registry::new(&all_commands());
    let mut sessions = HashMap::new();
    let mut windows = HashMap::new();
    let (_, s1) = run(&reg, &mut sessions, &mut windows, None, "new-session");
    let s = s1.unwrap();
    let wid = sessions.get(&s).unwrap().current_winlink().unwrap().window_id;
    let n = windows.get(&wid).unwrap().panes.len();
    run(&reg, &mut sessions, &mut windows, Some(s), "split-window -h");
    assert_eq!(windows.get(&wid).unwrap().panes.len(), n + 1);
}

#[test]
fn test_kill() {
    let reg = Registry::new(&all_commands());
    let mut sessions = HashMap::new();
    let mut windows = HashMap::new();
    let (_, s1) = run(&reg, &mut sessions, &mut windows, None, "new-session");
    assert_eq!(sessions.len(), 1);
    run(&reg, &mut sessions, &mut windows, s1, "kill-session");
    assert_eq!(sessions.len(), 0);
}

#[test]
fn test_detach() {
    let reg = Registry::new(&all_commands());
    let mut sessions = HashMap::new();
    let mut windows = HashMap::new();
    let (_, s1) = run(&reg, &mut sessions, &mut windows, None, "new-session");
    assert!(s1.is_some());
    let (ret, s2) = run(&reg, &mut sessions, &mut windows, s1, "detach-client");
    assert_eq!(ret, CmdRetval::Stop);
    assert!(s2.is_none());
}
