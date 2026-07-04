use std::collections::HashMap;
use loom_core::session::{Session, Window};
use loom_server::layout;
use crate::cmd::{Cmd, CmdCtx, CmdRetval, Args};

// ── helpers ──

fn find_session(sessions: &HashMap<u32, Session>, sid: Option<u32>, target: &str) -> Option<u32> {
    for (&id, s) in sessions {
        if s.name == target || format!("{}", id) == target { return Some(id); }
    }
    sid
}

fn current_window_id(sessions: &HashMap<u32, Session>, sid: u32) -> Option<u32> {
    let s = sessions.get(&sid)?;
    let wl = s.current_winlink()?;
    Some(wl.window_id)
}

fn active_pane_id(sessions: &HashMap<u32, Session>, windows: &HashMap<u32, Window>, sid: u32) -> Option<u32> {
    let wid = current_window_id(sessions, sid)?;
    windows.get(&wid)?.active_pane_id
}

fn find_pane(sessions: &HashMap<u32, Session>, windows: &HashMap<u32, Window>, sid: u32, target: &str) -> Option<u32> {
    if let Ok(n) = target.parse::<u32>() { return Some(n); }
    let wid = current_window_id(sessions, sid)?;
    let win = windows.get(&wid)?;
    if let Ok(idx) = target.parse::<usize>() {
        for (i, &pid) in win.pane_order.iter().enumerate() {
            if i == idx { return Some(pid); }
        }
    }
    active_pane_id(sessions, windows, sid)
}

fn current_pane_id(sessions: &HashMap<u32, Session>, windows: &HashMap<u32, Window>, ctx: &CmdCtx) -> Option<u32> {
    let sid = ctx.session_id?;
    active_pane_id(sessions, windows, sid)
}

fn get_or_create_session(ctx: &mut CmdCtx, args: &Args) -> Option<u32> {
    if let Some(sid) = ctx.session_id { return Some(sid); }
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "/".into());
    let name = args.get('s').map(|s| s.to_string());
    let mut session = Session::new(name.as_deref(), &cwd);
    let mut window = Window::new(80, 24);
    window.create_pane(80, 24);
    let wid = window.id;
    session.attach_window(0, wid);
    let sid = session.id;
    ctx.sessions.insert(sid, session);
    ctx.windows.insert(wid, window);
    ctx.session_id = Some(sid);
    Some(sid)
}

// ── macro to reduce boilerplate ──
macro_rules! with_window {
    ($ctx:expr, $code:block) => {{
        let sid = match $ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        let wid = match current_window_id(&$ctx.sessions, sid) { Some(w) => w, None => return CmdRetval::Error };
        if let Some(window) = $ctx.windows.get_mut(&wid) { $code }
        CmdRetval::Normal
    }};
}

// ── new-session ──
pub struct NewSession;
impl Cmd for NewSession {
    fn name(&self) -> &'static str { "new-session" }
    fn alias(&self) -> &'static str { "new" }
    fn usage(&self) -> &'static str { "new-session [-AdDEPX] [-c dir] [-F fmt] [-n name] [-s name] [cmd]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let cwd = args.get('c').unwrap_or("/tmp");
        let name = args.get('s').map(|s| s.to_string());
        let mut session = Session::new(name.as_deref(), cwd);
        let mut window = Window::new(80, 24);
        window.create_pane(80, 24);
        let wid = window.id;
        session.attach_window(0, wid);
        let sid = session.id;
        ctx.sessions.insert(sid, session);
        ctx.windows.insert(wid, window);
        ctx.session_id = Some(sid);
        println!("Created session {}", name.as_deref().unwrap_or("unnamed"));
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
        let sid = ctx.session_id;
        if let Some(sid) = sid {
            let to_remove: Vec<u32> = ctx.sessions.get(&sid)
                .map(|s| s.windows.values().map(|wl| wl.window_id).collect()).unwrap_or_default();
            ctx.sessions.remove(&sid);
            for wid in to_remove { ctx.windows.remove(&wid); }
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
            println!("{}: {} windows (attached {})", session.name, session.windows.len(), session.attached);
        }
        CmdRetval::Normal
    }
}

// ── new-window ──
pub struct NewWindow;
impl Cmd for NewWindow {
    fn name(&self) -> &'static str { "new-window" }
    fn alias(&self) -> &'static str { "neww" }
    fn usage(&self) -> &'static str { "new-window [-adkP] [-c dir] [-F fmt] [-n name] [-t target] [cmd]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let sid = match get_or_create_session(ctx, args) { Some(s) => s, None => return CmdRetval::Error };
        let idx = { let s = ctx.sessions.get(&sid).unwrap(); (0i32..).find(|i| !s.windows.contains_key(i)).unwrap_or(0) };
        let mut window = Window::new(80, 24);
        window.create_pane(80, 24);
        let wid = window.id;
        if let Some(session) = ctx.sessions.get_mut(&sid) { session.attach_window(idx, wid); session.set_current_window(idx); }
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
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        let idx = ctx.sessions.get(&sid).and_then(|s| s.curw_idx);
        if let Some(idx) = idx {
            if let Some(wid) = ctx.sessions.get_mut(&sid).and_then(|s| s.detach_window(idx)) {
                ctx.windows.remove(&wid);
            }
        }
        CmdRetval::Normal
    }
}

// ── list-windows ──
pub struct ListWindows;
impl Cmd for ListWindows {
    fn name(&self) -> &'static str { "list-windows" }
    fn alias(&self) -> &'static str { "lsw" }
    fn usage(&self) -> &'static str { "list-windows [-a] [-F format] [-t target-session]" }
    fn exec(&self, ctx: &mut CmdCtx, _args: &Args) -> CmdRetval {
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        if let Some(session) = ctx.sessions.get(&sid) {
            for (idx, wl) in &session.windows {
                let active = session.curw_idx == Some(*idx);
                if let Some(window) = ctx.windows.get(&wl.window_id) {
                    println!("{}: {} [{}x{}] {} {} panes", idx, window.name, window.sx, window.sy,
                        if active { "(active)" } else { "" }, window.panes.len());
                }
            }
        }
        CmdRetval::Normal
    }
}

// ── list-panes ──
pub struct ListPanes;
impl Cmd for ListPanes {
    fn name(&self) -> &'static str { "list-panes" }
    fn alias(&self) -> &'static str { "lsp" }
    fn usage(&self) -> &'static str { "list-panes [-a] [-F format] [-s] [-t target]" }
    fn exec(&self, ctx: &mut CmdCtx, _args: &Args) -> CmdRetval {
        for (_, window) in ctx.windows.iter() {
            for (_, pane) in &window.panes {
                let active = window.active_pane_id == Some(pane.id);
                println!("%{}: {}x{} at {}x{} {}", pane.id, pane.sx, pane.sy, pane.xoff, pane.yoff,
                    if active { "(active)" } else { "" });
            }
        }
        CmdRetval::Normal
    }
}

// ── split-window ──
pub struct SplitWindow;
impl Cmd for SplitWindow {
    fn name(&self) -> &'static str { "split-window" }
    fn alias(&self) -> &'static str { "splitw" }
    fn usage(&self) -> &'static str { "split-window [-bdfhIvPZ] [-c dir] [-e env] [-l size] [-t target] [cmd]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        let vertical = !args.has('h');
        let wid = match current_window_id(&ctx.sessions, sid) { Some(w) => w, None => return CmdRetval::Error };
        let pid = {
            if let Some(t) = args.get('t') {
                find_pane(&ctx.sessions, &ctx.windows, sid, t)
                    .unwrap_or_else(|| active_pane_id(&ctx.sessions, &ctx.windows, sid).unwrap_or(0))
            } else {
                active_pane_id(&ctx.sessions, &ctx.windows, sid).unwrap_or(0)
            }
        };
        if pid == 0 { return CmdRetval::Error; }
        if let Some(window) = ctx.windows.get_mut(&wid) {
            if let Some(id) = layout::layout_split_pane(window, pid, vertical) {
                println!("Created pane %{}", id);
            }
        }
        CmdRetval::Normal
    }
}

// ── select-pane ──
pub struct SelectPane;
impl Cmd for SelectPane {
    fn name(&self) -> &'static str { "select-pane" }
    fn alias(&self) -> &'static str { "selectp" }
    fn usage(&self) -> &'static str { "select-pane [-DLlRU] [-e] [-T title] [-t target-pane]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        let wid = match current_window_id(&ctx.sessions, sid) { Some(w) => w, None => return CmdRetval::Error };
        let pid = { args.get('t').and_then(|t| find_pane(&ctx.sessions, &ctx.windows, sid, t)) };
        if let Some(window) = ctx.windows.get_mut(&wid) {
            if let Some(p) = pid { window.set_active_pane(p); }
            else if args.has('U') || args.has('L') { if let Some(&p) = window.pane_order.back() { window.set_active_pane(p); } }
            else { if let Some(&p) = window.pane_order.front() { window.set_active_pane(p); } }
        }
        CmdRetval::Normal
    }
}

// ── select-window ──
pub struct SelectWindow;
impl Cmd for SelectWindow {
    fn name(&self) -> &'static str { "select-window" }
    fn alias(&self) -> &'static str { "selectw" }
    fn usage(&self) -> &'static str { "select-window [-lnpT] [-t target-window]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        if let Some(session) = ctx.sessions.get_mut(&sid) {
            if let Some(target) = args.get('t') {
                if let Ok(idx) = target.parse::<i32>() { if session.windows.contains_key(&idx) { session.set_current_window(idx); } }
            } else if args.has('n') { if let Some(idx) = session.windows.keys().next().copied() { session.set_current_window(idx); } }
        }
        CmdRetval::Normal
    }
}

// ── send-keys ──
pub struct SendKeys;
impl Cmd for SendKeys {
    fn name(&self) -> &'static str { "send-keys" }
    fn alias(&self) -> &'static str { "send" }
    fn usage(&self) -> &'static str { "send-keys [-FHlMRX] [-c target-client] [-N repeat-count] [-t target-pane] key ..." }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        let wid = match current_window_id(&ctx.sessions, sid) { Some(w) => w, None => return CmdRetval::Error };
        let pid = {
            match args.get('t').and_then(|t| find_pane(&ctx.sessions, &ctx.windows, sid, t)) {
                Some(p) => p, None => match active_pane_id(&ctx.sessions, &ctx.windows, sid) { Some(p) => p, None => return CmdRetval::Error }
            }
        };
        if let Some(window) = ctx.windows.get_mut(&wid) {
            if let Some(pane) = window.panes.get_mut(&pid) {
                let screen = &mut pane.screen;
                for key in &args.positional {
                    match key.as_str() {
                        "Enter" => { screen.cx = 0; screen.cy += 1;
                            if screen.cy >= screen.size_y() { screen.cy = screen.size_y() - 1; screen.grid.scroll_history(); } }
                        "Space" => { let gc = loom_core::grid_cell::GridCell { data: loom_core::utf8::Utf8Data::new(' '), ..Default::default() };
                            screen.grid.view_set_cell(screen.cx, screen.cy, &gc); screen.cx += 1; }
                        "Backspace" | "BS" => { if screen.cx > 0 { screen.cx -= 1; } }
                        "Tab" => { screen.cx = ((screen.cx / 8) + 1) * 8;
                            if screen.cx >= screen.size_x() { screen.cx = screen.size_x() - 1; } }
                        "Escape" | "Esc" => {}
                        "C-c" | "C-c" => {}
                        _ => { for ch in key.chars() {
                            let gc = loom_core::grid_cell::GridCell { data: loom_core::utf8::Utf8Data::new(ch), ..Default::default() };
                            screen.grid.view_set_cell(screen.cx, screen.cy, &gc); screen.cx += 1;
                            if screen.cx >= screen.size_x() { screen.cx = 0; screen.cy += 1;
                                if screen.cy >= screen.size_y() { screen.cy = screen.size_y() - 1; screen.grid.scroll_history(); } }
                        }}
                    }
                }
            }
        }
        CmdRetval::Normal
    }
}

// ── resize-pane ──
pub struct ResizePane;
impl Cmd for ResizePane {
    fn name(&self) -> &'static str { "resize-pane" }
    fn alias(&self) -> &'static str { "resizep" }
    fn usage(&self) -> &'static str { "resize-pane [-DLMRUZ] [-x width] [-y height] [-t target-pane] [adjustment]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let amount = args.positional.first().and_then(|s| s.parse::<i32>().ok()).unwrap_or(1) as u32;
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        let wid = match current_window_id(&ctx.sessions, sid) { Some(w) => w, None => return CmdRetval::Error };
        let pid = {
            args.get('t').and_then(|t| find_pane(&ctx.sessions, &ctx.windows, sid, t))
                .unwrap_or_else(|| active_pane_id(&ctx.sessions, &ctx.windows, sid).unwrap_or(0))
        };
        if let Some(window) = ctx.windows.get_mut(&wid) {
            if let Some(pane) = window.panes.get_mut(&pid) {
                if args.has('U') { pane.sy = pane.sy.saturating_sub(amount); }
                else if args.has('D') { pane.sy = pane.sy.saturating_add(amount); }
                else if args.has('L') { pane.sx = pane.sx.saturating_sub(amount); }
                else if args.has('R') { pane.sx = pane.sx.saturating_add(amount); }
                else if let Some(x) = args.get('x') { if let Ok(x) = x.parse::<u32>() { pane.sx = x; } }
                else if let Some(y) = args.get('y') { if let Ok(y) = y.parse::<u32>() { pane.sy = y; } }
                pane.screen.resize(pane.sx, pane.sy);
                if let Some(cell_idx) = pane.layout_cell {
                    if let Some(cell) = window.cells.get_mut(cell_idx) { cell.sx = pane.sx; cell.sy = pane.sy; }
                }
            }
        }
        CmdRetval::Normal
    }
}

// ── kill-pane ──
pub struct KillPane;
impl Cmd for KillPane {
    fn name(&self) -> &'static str { "kill-pane" }
    fn alias(&self) -> &'static str { "killp" }
    fn usage(&self) -> &'static str { "kill-pane [-a] [-t target-pane]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        let wid = match current_window_id(&ctx.sessions, sid) { Some(w) => w, None => return CmdRetval::Error };
        let pid = {
            args.get('t').and_then(|t| find_pane(&ctx.sessions, &ctx.windows, sid, t))
                .unwrap_or_else(|| active_pane_id(&ctx.sessions, &ctx.windows, sid).unwrap_or(0))
        };
        if let Some(window) = ctx.windows.get_mut(&wid) {
            if window.panes.len() > 1 { window.remove_pane(pid); layout::fix_layout_panes(window); }
        }
        CmdRetval::Normal
    }
}

// ── swap-pane ──
pub struct SwapPane;
impl Cmd for SwapPane {
    fn name(&self) -> &'static str { "swap-pane" }
    fn alias(&self) -> &'static str { "swapp" }
    fn usage(&self) -> &'static str { "swap-pane [-DDU] [-d] [-s src-pane] [-t dst-pane]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        let wid = match current_window_id(&ctx.sessions, sid) { Some(w) => w, None => return CmdRetval::Error };
        let (src, dst) = {
            let s = args.get('s').and_then(|t| find_pane(&ctx.sessions, &ctx.windows, sid, t));
            let d = args.get('t').and_then(|t| find_pane(&ctx.sessions, &ctx.windows, sid, t));
            (s, d)
        };
        if let Some(window) = ctx.windows.get_mut(&wid) {
            if let (Some(s), Some(d)) = (src, dst) {
                let sc = window.panes.get(&s).and_then(|p| p.layout_cell);
                let dc = window.panes.get(&d).and_then(|p| p.layout_cell);
                if let (Some(sc), Some(dc)) = (sc, dc) {
                    let tmp = window.cells[sc].pane_id;
                    window.cells[sc].pane_id = window.cells[dc].pane_id;
                    window.cells[dc].pane_id = tmp;
                    if let Some(p) = window.panes.get_mut(&s) { p.layout_cell = Some(dc); }
                    if let Some(p) = window.panes.get_mut(&d) { p.layout_cell = Some(sc); }
                    layout::fix_layout_panes(window);
                }
            }
        }
        CmdRetval::Normal
    }
}

// ── switch-client ──
pub struct SwitchClient;
impl Cmd for SwitchClient {
    fn name(&self) -> &'static str { "switch-client" }
    fn alias(&self) -> &'static str { "switchc" }
    fn usage(&self) -> &'static str { "switch-client [-E] [-c target-client] [-t target-session] [-T key-table] [-n] [-p]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        if let Some(target) = args.get('t') {
            for (sid, session) in ctx.sessions.iter() {
                if session.name == target || format!("{}", sid) == target { ctx.session_id = Some(*sid); break; }
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
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Error };
        if let Some(name) = args.get('t').or_else(|| args.positional.first().map(|s| s.as_str())) {
            if let Some(session) = ctx.sessions.get_mut(&sid) { session.name = name.to_string(); }
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
        let exists = match target { Some(name) => ctx.sessions.values().any(|s| s.name == name), None => ctx.session_id.is_some() };
        if exists { CmdRetval::Normal } else { CmdRetval::Error }
    }
}

// ── attach-session ──
pub struct AttachSession;
impl Cmd for AttachSession {
    fn name(&self) -> &'static str { "attach-session" }
    fn alias(&self) -> &'static str { "attach" }
    fn usage(&self) -> &'static str { "attach-session [-dErx] [-c target-client] [-t target-session]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        if let Some(target) = args.get('t') {
            for (sid, session) in ctx.sessions.iter() {
                if session.name == target || format!("{}", sid) == target { ctx.session_id = Some(*sid); return CmdRetval::Normal; }
            }
        }
        if let Some(&sid) = ctx.sessions.keys().next() { ctx.session_id = Some(sid); CmdRetval::Normal } else { CmdRetval::Error }
    }
}

// ── detach-client ──
pub struct DetachClient;
impl Cmd for DetachClient {
    fn name(&self) -> &'static str { "detach-client" }
    fn alias(&self) -> &'static str { "detach" }
    fn usage(&self) -> &'static str { "detach-client [-a] [-E] [-P] [-c target-client] [-t target-session]" }
    fn exec(&self, ctx: &mut CmdCtx, _args: &Args) -> CmdRetval {
        ctx.session_id = None;
        CmdRetval::Stop
    }
}

// ── set-option ──
pub struct SetOption;
impl Cmd for SetOption {
    fn name(&self) -> &'static str { "set-option" }
    fn alias(&self) -> &'static str { "set" }
    fn usage(&self) -> &'static str { "set-option [-aFgopqsuUw] [-t target-window] option value" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        if let (Some(key), Some(val)) = (args.positional.first(), args.positional.get(1)) {
            if args.has('g') { println!("set -g {} = {}", key, val); }
            else if let Some(sid) = ctx.session_id {
                if let Some(session) = ctx.sessions.get_mut(&sid) { session.options.set_string(key, val); }
            }
        }
        CmdRetval::Normal
    }
}

// ── show-options ──
pub struct ShowOptions;
impl Cmd for ShowOptions {
    fn name(&self) -> &'static str { "show-options" }
    fn alias(&self) -> &'static str { "show" }
    fn usage(&self) -> &'static str { "show-options [-gopqsvw] [-t target-window] [option]" }
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval {
        let sid = match ctx.session_id { Some(s) => s, None => return CmdRetval::Normal };
        if let Some(session) = ctx.sessions.get(&sid) {
            for entry in session.options.iter() {
                println!("{} \"{:?}\"", entry.name, entry.value);
            }
        }
        CmdRetval::Normal
    }
}

// ── list-commands ──
pub struct ListCommands;
impl Cmd for ListCommands {
    fn name(&self) -> &'static str { "list-commands" }
    fn alias(&self) -> &'static str { "" }
    fn usage(&self) -> &'static str { "list-commands" }
    fn exec(&self, _ctx: &mut CmdCtx, _args: &Args) -> CmdRetval { CmdRetval::Normal }
}

pub fn all_commands() -> Vec<&'static dyn Cmd> {
    vec![
        &NewSession, &KillSession, &ListSessions,
        &NewWindow, &KillWindow, &ListWindows, &ListPanes,
        &SplitWindow, &SelectPane, &SelectWindow,
        &SendKeys, &ResizePane, &KillPane, &SwapPane,
        &SwitchClient, &RenameSession, &HasSession,
        &AttachSession, &DetachClient,
        &SetOption, &ShowOptions, &ListCommands,
    ]
}
