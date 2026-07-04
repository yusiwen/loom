use loom_core::session::Window;

use crate::cmd::{Cmd, CmdCtx, CmdRetval, Args};

// ── Helper: find or create session/window ──

fn get_or_create_session(ctx: &mut CmdCtx, args: &Args) -> Option<loom_core::session::SessionId> {
    if let Some(sid) = ctx.session_id {
        return Some(sid);
    }
    // Create a new session with a window
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".to_string());
    let name = args.get('s').map(|s| s.to_string());
    let mut session = loom_core::session::Session::new(name.as_deref(), &cwd);
    let mut window = Window::new(80, 24);
    let _pid = window.create_pane(80, 24);
    let wid = window.id;
    session.attach_window(0, wid);
    let sid = session.id;
    ctx.sessions.insert(sid, session);
    ctx.windows.insert(wid, window);
    ctx.session_id = Some(sid);
    Some(sid)
}

// ── new-session ──

pub struct NewSession;

impl Cmd for NewSession {
    fn name(&self) -> &'static str { "new-session" }
    fn alias(&self) -> &'static str { "new" }
    fn usage(&self) -> &'static str { "new-session [-AdDEPX] [-c dir] [-e env] [-F fmt] [-n name] [-s name] [-x w] [-y h] [cmd]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let cwd = args.get('c').unwrap_or("/tmp");
        let name = args.get('s').map(|s| s.to_string());
        let mut session = loom_core::session::Session::new(name.as_deref(), cwd);
        let mut window = Window::new(
            args.get('x').and_then(|x| x.parse().ok()).unwrap_or(80),
            args.get('y').and_then(|y| y.parse().ok()).unwrap_or(24),
        );
        let _pid = window.create_pane(80, 24);
        let wid = window.id;
        session.attach_window(0, wid);
        let sid = session.id;
        ctx.sessions.insert(sid, session);
        ctx.windows.insert(wid, window);
        ctx.session_id = Some(sid);
        CmdRetval::Normal
    }
}

// ── kill-session ──

pub struct KillSession;

impl Cmd for KillSession {
    fn name(&self) -> &'static str { "kill-session" }
    fn alias(&self) -> &'static str { "" }
    fn usage(&self) -> &'static str { "kill-session [-t target-session]" }
    fn exec(&self, ctx: &mut CmdCtx, _args: &Args) -> CmdRetval {
        if let Some(sid) = ctx.session_id {
            let to_remove: Vec<loom_core::session::WindowId> = ctx
                .sessions
                .get(&sid)
                .map(|s| s.windows.values().map(|wl| wl.window_id).collect())
                .unwrap_or_default();
            ctx.sessions.remove(&sid);
            for wid in to_remove {
                ctx.windows.remove(&wid);
            }
            ctx.session_id = None;
        }
        CmdRetval::Normal
    }
}

// ── list-sessions ──

pub struct ListSessions;

impl Cmd for ListSessions {
    fn name(&self) -> &'static str { "list-sessions" }
    fn alias(&self) -> &'static str { "ls" }
    fn usage(&self) -> &'static str { "list-sessions [-F format]" }
    fn exec(&self, ctx: &mut CmdCtx, _args: &Args) -> CmdRetval {
        for (_, session) in ctx.sessions.iter() {
            println!("{}: {} windows (attached {})",
                session.name, session.windows.len(), session.attached);
        }
        CmdRetval::Normal
    }
}

// ── new-window ──

pub struct NewWindow;

impl Cmd for NewWindow {
    fn name(&self) -> &'static str { "new-window" }
    fn alias(&self) -> &'static str { "neww" }
    fn usage(&self) -> &'static str { "new-window [-adkP] [-c dir] [-e env] [-F fmt] [-n name] [-t target] [cmd]" }
    fn exec(&self, ctx: &mut CmdCtx, _args: &Args) -> CmdRetval {
        let sid = match get_or_create_session(ctx, _args) {
            Some(s) => s,
            None => return CmdRetval::Error,
        };
        let idx = {
            let session = match ctx.sessions.get(&sid) {
                Some(s) => s,
                None => return CmdRetval::Error,
            };
            (0i32..).find(|i| !session.windows.contains_key(i)).unwrap_or(0)
        };

        let mut window = Window::new(80, 24);
        let _pid = window.create_pane(80, 24);
        let wid = window.id;

        if let Some(session) = ctx.sessions.get_mut(&sid) {
            session.attach_window(idx, wid);
            session.set_current_window(idx);
        }
        ctx.windows.insert(wid, window);

        CmdRetval::Normal
    }
}

// ── kill-window ──

pub struct KillWindow;

impl Cmd for KillWindow {
    fn name(&self) -> &'static str { "kill-window" }
    fn alias(&self) -> &'static str { "killw" }
    fn usage(&self) -> &'static str { "kill-window [-t target-window]" }
    fn exec(&self, ctx: &mut CmdCtx, _args: &Args) -> CmdRetval {
        let sid = match ctx.session_id {
            Some(s) => s,
            None => return CmdRetval::Error,
        };
        let idx = ctx.sessions.get(&sid).and_then(|s| s.curw_idx);

        if let Some(idx) = idx {
            if let Some(wid) = ctx.sessions.get_mut(&sid).and_then(|s| s.detach_window(idx)) {
                ctx.windows.remove(&wid);
            }
        }
        CmdRetval::Normal
    }
}

// ── rename-session ──

pub struct RenameSession;

impl Cmd for RenameSession {
    fn name(&self) -> &'static str { "rename-session" }
    fn alias(&self) -> &'static str { "rename" }
    fn usage(&self) -> &'static str { "rename-session [-t target-session] new-name" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let sid = match ctx.session_id {
            Some(s) => s,
            None => return CmdRetval::Error,
        };
        let name = args.get('t').or_else(|| args.positional.first().map(|s| s.as_str()));
        if let Some(name) = name {
            if let Some(session) = ctx.sessions.get_mut(&sid) {
                session.name = name.to_string();
            }
        }
        CmdRetval::Normal
    }
}

// ── has-session ──

pub struct HasSession;

impl Cmd for HasSession {
    fn name(&self) -> &'static str { "has-session" }
    fn alias(&self) -> &'static str { "has" }
    fn usage(&self) -> &'static str { "has-session [-t target-session]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let target = args.get('t');
        let exists = match target {
            Some(name) => ctx.sessions.values().any(|s| s.name == name),
            None => ctx.session_id.is_some(),
        };
        if exists { CmdRetval::Normal } else { CmdRetval::Error }
    }
}

/// All implemented commands.
pub fn all_commands() -> Vec<&'static dyn Cmd> {
    vec![
        &NewSession,
        &KillSession,
        &ListSessions,
        &NewWindow,
        &KillWindow,
        &RenameSession,
        &HasSession,
    ]
}
