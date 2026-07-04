use std::collections::HashMap;

use loom_core::session::{Session, SessionId, WindowId};

/// Command return values, matching tmux's enum cmd_retval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdRetval {
    Normal,
    Error,
    Wait,
    Stop,
}

/// Parsed command arguments.
#[derive(Clone, Debug, Default)]
pub struct Args {
    pub flags: HashMap<char, String>,
    pub positional: Vec<String>,
}

impl Args {
    pub fn has(&self, flag: char) -> bool {
        self.flags.contains_key(&flag)
    }

    pub fn get(&self, flag: char) -> Option<&str> {
        self.flags.get(&flag).map(|s| s.as_str())
    }

    pub fn count(&self) -> usize {
        self.positional.len()
    }
}

/// Command execution context.
pub struct CmdCtx<'a> {
    pub sessions: &'a mut HashMap<SessionId, Session>,
    pub windows: &'a mut HashMap<WindowId, loom_core::session::Window>,
    pub session_id: Option<SessionId>,
    pub client_flags: u64,
}

impl<'a> CmdCtx<'a> {
    pub fn session(&self) -> Option<&Session> {
        self.session_id.and_then(|id| self.sessions.get(&id))
    }

    pub fn session_mut(&mut self) -> Option<&mut Session> {
        self.session_id.and_then(|id| self.sessions.get_mut(&id))
    }

    pub fn current_window(&self) -> Option<&loom_core::session::Window> {
        self.session()
            .and_then(|s| s.current_winlink())
            .and_then(|wl| self.windows.get(&wl.window_id))
    }

    pub fn current_window_mut(&mut self) -> Option<&mut loom_core::session::Window> {
        let wid = self
            .session()
            .and_then(|s| s.current_winlink())
            .map(|wl| wl.window_id);
        wid.and_then(move |id| self.windows.get_mut(&id))
    }
}

/// A command implementation.
pub trait Cmd: Send + Sync {
    fn name(&self) -> &'static str;
    fn alias(&self) -> &'static str;
    fn usage(&self) -> &'static str;
    fn exec(&self, ctx: &mut CmdCtx, args: &Args) -> CmdRetval;
}

/// Command registry (name -> implementation).
pub struct Registry {
    by_name: HashMap<&'static str, &'static dyn Cmd>,
    by_alias: HashMap<&'static str, &'static dyn Cmd>,
}

impl Registry {
    pub fn new(cmds: &[&'static dyn Cmd]) -> Self {
        let mut by_name = HashMap::new();
        let mut by_alias = HashMap::new();
        for cmd in cmds {
            by_name.insert(cmd.name(), *cmd);
            if !cmd.alias().is_empty() {
                by_alias.insert(cmd.alias(), *cmd);
            }
        }
        Self { by_name, by_alias }
    }

    pub fn get(&self, name: &str) -> Option<&'static dyn Cmd> {
        self.by_name
            .get(name)
            .copied()
            .or_else(|| self.by_alias.get(name).copied())
    }

    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.by_name.keys().copied().collect();
        names.sort();
        names
    }
}
