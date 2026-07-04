use std::collections::VecDeque;

use crate::cmd::{CmdCtx, CmdRetval, Registry};
use crate::parser;

/// A single item in the command queue.
pub enum CmdItem {
    /// Execute a parsed command string.
    Command {
        cmdline: String,
    },
    /// Execute a callback function.
    Callback {
        name: String,
        func: Box<dyn FnOnce(&mut CmdCtx) -> CmdRetval + Send>,
    },
}

/// Stateful command queue for sequential execution.
pub struct CmdQueue {
    items: VecDeque<CmdItem>,
    pub waiting: bool,
}

impl CmdQueue {
    pub fn new() -> Self {
        Self {
            items: VecDeque::new(),
            waiting: false,
        }
    }

    /// Append a command string to the queue.
    pub fn append(&mut self, cmdline: &str) {
        self.items.push_back(CmdItem::Command {
            cmdline: cmdline.to_string(),
        });
    }

    /// Append a callback to the queue.
    pub fn append_callback(
        &mut self,
        name: &str,
        func: Box<dyn FnOnce(&mut CmdCtx) -> CmdRetval + Send>,
    ) {
        self.items.push_back(CmdItem::Callback {
            name: name.to_string(),
            func,
        });
    }

    /// Process the next item in the queue.
    /// Returns CmdRetval from the executed command.
    pub fn process_next(
        &mut self,
        ctx: &mut CmdCtx,
        registry: &Registry,
    ) -> Option<CmdRetval> {
        if self.waiting {
            return None;
        }

        let item = self.items.pop_front()?;

        match item {
            CmdItem::Command { cmdline } => {
                // Parse and execute the command
                match parser::parse_command_line(&cmdline) {
                    Ok((name, args)) => {
                        if let Some(cmd) = registry.get(name) {
                            let retval = cmd.exec(ctx, &args);
                            if retval == CmdRetval::Wait {
                                self.waiting = true;
                            }
                            Some(retval)
                        } else {
                            eprintln!("unknown command: {}", name);
                            Some(CmdRetval::Error)
                        }
                    }
                    Err(e) => {
                        eprintln!("parse error: {}", e);
                        Some(CmdRetval::Error)
                    }
                }
            }
            CmdItem::Callback { name: _, func } => {
                let retval = func(ctx);
                if retval == CmdRetval::Wait {
                    self.waiting = true;
                }
                Some(retval)
            }
        }
    }

    /// Process all items in the queue until empty or blocked.
    pub fn process_all(&mut self, ctx: &mut CmdCtx, registry: &Registry) {
        while !self.items.is_empty() && !self.waiting {
            match self.process_next(ctx, registry) {
                Some(CmdRetval::Stop) | Some(CmdRetval::Error) => {
                    self.items.clear();
                    break;
                }
                _ => continue,
            }
        }
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Get the number of pending items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Clear all pending items.
    pub fn clear(&mut self) {
        self.items.clear();
        self.waiting = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::all_commands;
    use crate::cmd::{CmdCtx, Registry};
    use std::collections::HashMap;
    use loom_core::session::{Session, Window};

    #[test]
    fn test_basic_queue() {
        let mut queue = CmdQueue::new();
        queue.append("new-session -s test");
        queue.append("list-sessions");

        let registry = Registry::new(&all_commands());
        let mut sessions = HashMap::new();
        let mut windows = HashMap::new();
        let mut ctx = CmdCtx {
            sessions: &mut sessions,
            windows: &mut windows,
            session_id: None,
            client_flags: 0,
        };

        queue.process_all(&mut ctx, &registry);
        assert!(queue.is_empty());
        assert!(ctx.session_id.is_some());
    }

    #[test]
    fn test_multi_command() {
        let mut queue = CmdQueue::new();
        queue.append("new-session -s multi");
        queue.append("new-window");
        queue.append("split-window -h");

        let registry = Registry::new(&all_commands());
        let mut sessions = HashMap::new();
        let mut windows = HashMap::new();
        let mut ctx = CmdCtx {
            sessions: &mut sessions,
            windows: &mut windows,
            session_id: None,
            client_flags: 0,
        };

        queue.process_all(&mut ctx, &registry);
        assert!(queue.is_empty());

        // Should have 1 session with windows
        let sid = ctx.session_id.unwrap();
        let session = ctx.sessions.get(&sid).unwrap();
        assert_eq!(session.windows.len(), 2);
    }
}
