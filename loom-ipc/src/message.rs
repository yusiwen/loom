use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Protocol version.
pub const PROTOCOL_VERSION: u32 = 8;

/// Messages exchanged between client and server.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Message {
    // ── Identify phase (client → server) ──
    /// Client announces its flags.
    IdentifyFlags(u64),
    /// Client sends long flags (extended flags).
    IdentifyLongFlags(u64),
    /// Client sends its terminal type (e.g. "xterm-256color").
    IdentifyTerm(String),
    /// Client sends supported terminal features.
    IdentifyFeatures(String),
    /// Client sends its TTY name (e.g. "/dev/pts/0").
    IdentifyTtyName(String),
    /// Client sends its current working directory.
    IdentifyCwd(String),
    /// Client sends its PID.
    IdentifyClientPid(u32),
    /// Client sends terminfo capabilities.
    IdentifyTerminfo(Vec<(String, String)>),
    /// Client sends an environment variable.
    IdentifyEnviron(HashMap<String, String>),
    /// Client signals identify phase is complete.
    IdentifyDone,

    // ── Fd-passing markers ──
    /// Client sends stdin fd (fd follows via SCM_RIGHTS).
    FdStdin,
    /// Client sends stdout fd (fd follows via SCM_RIGHTS).
    FdStdout,

    // ── Command phase ──
    /// Client sends a command to execute.
    Command {
        argc: u32,
        argv: Vec<String>,
    },
    /// Client detaches from session.
    Detach,
    /// Client detaches and kills the session.
    DetachKill,
    /// Client exits.
    Exit,
    /// Client has exited (server → client).
    Exited,
    /// Server is exiting.
    Exiting,
    /// Lock the session.
    Lock,
    /// Server is ready (server → client).
    Ready,
    /// Terminal resize.
    Resize { sx: u32, sy: u32 },
    /// Start a shell.
    Shell,
    /// Shut down the server.
    Shutdown,
    /// Suspend the client.
    Suspend,
    /// Unlock the session.
    Unlock,
    /// Wake up the client.
    WakeUp,
    /// Execute a command on the client's machine.
    Exec { argv: Vec<String> },
    /// Client flags.
    Flags(u64),

    // ── File I/O (300-level) ──
    /// Open a file for reading.
    ReadOpen { stream: u32 },
    /// Read data from a file.
    ReadData { stream: u32 },
    /// File read is complete.
    ReadDone { stream: u32, error: i32 },
    /// Cancel a file read.
    ReadCancel { stream: u32 },
    /// Open a file for writing.
    WriteOpen { stream: u32, flags: u32 },
    /// Write data to a file.
    WriteData { stream: u32 },
    /// Server is ready to receive more write data.
    WriteReady { stream: u32, error: i32 },
    /// Close a file being written.
    WriteClose { stream: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_command() {
        let msg = Message::Command {
            argc: 2,
            argv: vec!["new-session".into(), "-s".into()],
        };
        let encoded = bincode::serialize(&msg).unwrap();
        let decoded: Message = bincode::deserialize(&encoded).unwrap();
        match decoded {
            Message::Command { argc, argv } => {
                assert_eq!(argc, 2);
                assert_eq!(argv[0], "new-session");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_roundtrip_identify() {
        let msg = Message::IdentifyFlags(42);
        let encoded = bincode::serialize(&msg).unwrap();
        let decoded: Message = bincode::deserialize(&encoded).unwrap();
        assert!(matches!(decoded, Message::IdentifyFlags(42)));
    }
}
