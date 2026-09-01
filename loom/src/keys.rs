//! Client-side prefix-key state machine (Phase B, P1-1).
//!
//! Mirrors tmux's prefix model: while in the default (normal) mode every byte is
//! forwarded to the active pane's PTY, except the prefix key (`C-b`). Pressing
//! the prefix key arms a one-shot "prefix" state in which the *next* key is
//! interpreted as a keybinding. The bindings are a fixed table so the set of
//! interactive commands is explicit and testable.
//!
//! Modes:
//! - Normal: forward bytes to the PTY; `C-b` transitions to Prefix.
//! - Prefix: interpret the next key via the binding table (single byte, or an
//!   arrow-key escape sequence `ESC [ A/B/C/D`). Unbound keys beep and return
//!   to Normal (the key is NOT forwarded, matching tmux's default).
//! - CmdPrompt: after `:`, read a command line; Enter sends it as a command,
//!   Esc cancels.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Normal,
    /// Prefix key was just consumed; waiting for the next key.
    Prefix,
    /// In Prefix, saw `ESC`; waiting for `[`.
    PrefixEsc,
    /// In Prefix, saw `ESC [`; waiting for the final byte (A/B/C/D).
    PrefixEscBracket,
}

/// An action produced by feeding a byte to the key handler.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Forward these raw bytes to the active pane's PTY.
    Forward(Vec<u8>),
    /// Send a tmux-style command to the server (argv[0] is the command name).
    Command(Vec<String>),
    /// Detach: the client disconnects; the server keeps the session alive.
    Detach,
    /// Print text to the local terminal (e.g. the command-prompt prompt).
    Echo(String),
    /// Bell (unbound prefix key, etc.).
    Beep,
}

/// The prefix key: `C-b`.
pub const PREFIX_KEY: u8 = 0x02;

fn binding_for(key: u8) -> Option<&'static [u8]> {
    Some(match key {
        b'c' => b"new-window",
        b'&' => b"kill-window",
        b'%' => b"split-window -h",
        b'"' => b"split-window -v",
        b'n' => b"select-window -n",
        b'p' => b"select-window -p",
        b'h' => b"select-pane -L",
        b'j' => b"select-pane -D",
        b'k' => b"select-pane -U",
        b'l' => b"select-pane -R",
        b'z' => b"resize-pane -Z",
        b'[' => b"copy-mode",
        b'}' => b"paste-buffer",
        _ => return None,
    })
}

pub struct KeyHandler {
    state: KeyState,
    /// Command-prompt buffer (only used in CmdPrompt).
    prompt_buf: Vec<u8>,
    in_prompt: bool,
}

impl KeyHandler {
    pub fn new() -> Self {
        Self {
            state: KeyState::Normal,
            prompt_buf: Vec::new(),
            in_prompt: false,
        }
    }

    /// Feed a slice of bytes read from stdin. Returns all actions in order.
    /// Consecutive `Forward` actions are coalesced into one.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Action> {
        let mut actions = Vec::new();
        for &b in bytes {
            let mut one = Vec::new();
            self.feed_one(b, &mut one);
            if let Some(action) = one.pop() {
                match action {
                    Action::Forward(more) => {
                        if let Some(Action::Forward(last)) = actions.last_mut() {
                            last.extend_from_slice(&more);
                        } else {
                            actions.push(Action::Forward(more));
                        }
                    }
                    other => actions.push(other),
                }
            }
        }
        actions
    }

    fn feed_one(&mut self, b: u8, actions: &mut Vec<Action>) {
        if self.in_prompt {
            self.feed_prompt(b, actions);
            return;
        }

        match self.state {
            KeyState::Normal => {
                if b == PREFIX_KEY {
                    self.state = KeyState::Prefix;
                } else {
                    actions.push(Action::Forward(vec![b]));
                }
            }
            KeyState::Prefix => {
                if b == 0x1b {
                    // Possible arrow-key escape sequence.
                    self.state = KeyState::PrefixEsc;
                } else {
                    self.state = KeyState::Normal;
                    self.apply_prefix_key(b, actions);
                }
            }
            KeyState::PrefixEsc => {
                if b == b'[' {
                    self.state = KeyState::PrefixEscBracket;
                } else {
                    // Not an arrow; treat the ESC as unbound.
                    self.state = KeyState::Normal;
                    actions.push(Action::Beep);
                }
            }
            KeyState::PrefixEscBracket => {
                self.state = KeyState::Normal;
                match b {
                    b'A' => actions.push(Action::Command(
                        vec!["select-pane".into(), "-U".into()],
                    )),
                    b'B' => actions.push(Action::Command(
                        vec!["select-pane".into(), "-D".into()],
                    )),
                    b'C' => actions.push(Action::Command(
                        vec!["select-pane".into(), "-R".into()],
                    )),
                    b'D' => actions.push(Action::Command(
                        vec!["select-pane".into(), "-L".into()],
                    )),
                    _ => actions.push(Action::Beep),
                }
            }
        }
    }

    fn apply_prefix_key(&mut self, b: u8, actions: &mut Vec<Action>) {
        match b {
            b'd' => actions.push(Action::Detach),
            b':' => {
                self.in_prompt = true;
                self.prompt_buf.clear();
                actions.push(Action::Echo(": ".into()));
            }
            b'?' => {
                actions.push(Action::Echo(HELP_TEXT.to_string()));
            }
            b'0'..=b'9' => {
                let mut argv = vec!["select-window".to_string()];
                argv.push(char::from(b).to_string());
                actions.push(Action::Command(argv));
            }
            _ => {
                if let Some(cmd) = binding_for(b) {
                    let argv: Vec<String> = String::from_utf8_lossy(cmd)
                        .split_whitespace()
                        .map(|s| s.to_string())
                        .collect();
                    actions.push(Action::Command(argv));
                } else {
                    actions.push(Action::Beep);
                }
            }
        }
    }

    fn feed_prompt(&mut self, b: u8, actions: &mut Vec<Action>) {
        match b {
            b'\n' | b'\r' => {
                let line: String =
                    String::from_utf8_lossy(&self.prompt_buf).into_owned();
                self.in_prompt = false;
                self.prompt_buf.clear();
                self.state = KeyState::Normal;
                actions.push(Action::Echo("\r\n".into()));
                if !line.trim().is_empty() {
                    let argv: Vec<String> =
                        line.split_whitespace().map(|s| s.to_string()).collect();
                    actions.push(Action::Command(argv));
                }
            }
            0x1b => {
                // Cancel the prompt.
                self.in_prompt = false;
                self.prompt_buf.clear();
                self.state = KeyState::Normal;
                actions.push(Action::Echo("\r\x1b[K\r".into()));
            }
            0x7f | 0x08 => {
                if !self.prompt_buf.is_empty() {
                    self.prompt_buf.pop();
                    actions.push(Action::Echo("\x08 \x08".into()));
                }
            }
            _ => {
                self.prompt_buf.push(b);
                actions.push(Action::Echo(
                    char::from(b).to_string(),
                ));
            }
        }
    }
}

impl Default for KeyHandler {
    fn default() -> Self {
        Self::new()
    }
}

const HELP_TEXT: &str = "\
Loom keybindings (press C-b first):
  c new-window        & kill-window      % split (left/right)
  \u{22} split (top/bottom)   d detach         : command prompt
  0-9 select window   n/p next/prev window
  h/j/k/l or arrows   select pane        z zoom
  [ copy-mode (vi keys: hjkl/w/b/v/y/q, gg/G, space/?)
  } paste-buffer     ? this help
";

#[cfg(test)]
mod tests {
    use super::*;

    fn actions_for(bytes: &[u8]) -> Vec<Action> {
        let mut h = KeyHandler::new();
        h.feed(bytes)
    }

    #[test]
    fn forwards_in_normal_mode() {
        let a = actions_for(b"ls\r");
        assert_eq!(a, vec![Action::Forward(b"ls\r".to_vec())]);
    }

    #[test]
    fn prefix_c_not_forwarded() {
        // C-b alone should not forward anything; it arms prefix state.
        let mut h = KeyHandler::new();
        let a = h.feed(&[PREFIX_KEY]);
        assert!(a.is_empty());
        assert_eq!(h.state, KeyState::Prefix);
    }

    #[test]
    fn prefix_new_window() {
        let a = actions_for(&[PREFIX_KEY, b'c']);
        assert_eq!(
            a,
            vec![Action::Command(vec!["new-window".into()])]
        );
    }

    #[test]
    fn prefix_detach() {
        let a = actions_for(&[PREFIX_KEY, b'd']);
        assert_eq!(a, vec![Action::Detach]);
    }

    #[test]
    fn prefix_split_h() {
        let a = actions_for(&[PREFIX_KEY, b'%']);
        assert_eq!(
            a,
            vec![Action::Command(vec![
                "split-window".into(),
                "-h".into()
            ])]
        );
    }

    #[test]
    fn prefix_open_copy_mode() {
        // C-b [ enters copy-mode (tmux default binding).
        let a = actions_for(&[PREFIX_KEY, b'[']);
        assert_eq!(a, vec![Action::Command(vec!["copy-mode".into()])]);
    }

    #[test]
    fn prefix_paste_buffer() {
        let a = actions_for(&[PREFIX_KEY, b'}']);
        assert_eq!(a, vec![Action::Command(vec!["paste-buffer".into()])]);
    }

    #[test]
    fn prefix_select_window_digit() {
        let a = actions_for(&[PREFIX_KEY, b'3']);
        assert_eq!(
            a,
            vec![Action::Command(vec![
                "select-window".into(),
                "3".into()
            ])]
        );
    }

    #[test]
    fn prefix_arrow_up_selects_pane_up() {
        let a = actions_for(&[PREFIX_KEY, 0x1b, b'[', b'A']);
        assert_eq!(
            a,
            vec![Action::Command(vec![
                "select-pane".into(),
                "-U".into()
            ])]
        );
    }

    #[test]
    fn prefix_unbound_beeps() {
        let a = actions_for(&[PREFIX_KEY, b'x']);
        assert_eq!(a, vec![Action::Beep]);
    }

    #[test]
    fn command_prompt_sends_command() {
        // ':' then "kill-window" then Enter.
        let mut h = KeyHandler::new();
        let a = h.feed(&[PREFIX_KEY, b':']);
        assert_eq!(a.last(), Some(&Action::Echo(": ".into())));
        let _ = h.feed(b"kill-window"); // each char echoes
        let cmd = h.feed(b"\r").pop().unwrap();
        assert_eq!(cmd, Action::Command(vec!["kill-window".into()]));
    }

    #[test]
    fn command_prompt_esc_cancels() {
        let mut h = KeyHandler::new();
        h.feed(&[PREFIX_KEY, b':']);
        h.feed(b"partial");
        let a = h.feed(&[0x1b]);
        assert_eq!(a, vec![Action::Echo("\r\x1b[K\r".into())]);
        // Back to normal: a plain byte forwards again.
        let a2 = h.feed(b"x");
        assert_eq!(a2, vec![Action::Forward(b"x".to_vec())]);
    }
}
