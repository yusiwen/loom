# Loom — Rust TMUX Roadmap

## Overview

Loom is a Rust implementation of a terminal multiplexer inspired by [tmux](https://github.com/tmux/tmux). This document tracks the translation progress.

Reference codebase: `tmux` (~97,000 lines of C across 149 files).

Target: ~60,000–80,000 lines of Rust, organized as a Cargo workspace.

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Async runtime | **mio** (manual event loop) | Closest to libevent's single-threaded model, simplest C→Rust mapping |
| IPC serialization | **serde + bincode** | Type-safe, compile-time checked protocol |
| Config parser | **nom** | Pure Rust combinators, standard ecosystem choice |
| Terminfo | **terminfo crate** | Avoid reimplementing binary database parsing |
| Translation strategy | **Module-level rewrite** | Idiomatic Rust per module, not line-by-line C port |

## Phase 1: Core Data Types (✅ Complete)

**Crate:** `loom-core`

| Module | File | Status |
|--------|------|--------|
| UTF-8 handling | `utf8.rs` | ✅ Done |
| Grid cell / Grid / GridLine | `grid_cell.rs` | ✅ Done |
| Colour system | `colour.rs` | ✅ Done |
| Style / Attributes | `style.rs` | ✅ Done |
| Screen abstraction | `screen.rs` | ✅ Done |
| Options tree | `options.rs` | ✅ Done |

**Tests:** 20 passing, 0 warnings, 0 errors.

## Phase 2: IPC Layer & Event Loop (📋 Pending)

**Crate:** `loom-ipc`

- Define IPC message types with serde + bincode
- Implement `TmuxProc` / `TmuxPeer` process abstraction
- mio event loop (signal handling, timers)
- UnixStream-based client-server connection
- SCM_RIGHTS file descriptor passing

**Key C sources to translate:** `proc.c`, `tmux-protocol.h`, `compat/imsg/*`

## Phase 3: TTY & Terminal I/O (📋 Pending)

**Crate:** `loom-tty`

- TTY raw mode (termios) via nix crate
- Terminfo capability loading via terminfo crate
- Output buffering and cursor/colour/region commands
- Keyboard input parsing (trie-based key matching)
- Alternate character set (ACS) mapping

**Key C sources to translate:** `tty.c`, `tty-term.c`, `tty-keys.c`, `tty-draw.c`, `tty-acs.c`, `tty-features.c`

## Phase 4: Terminal Emulator (📋 Pending)

**Crate:** `loom-input`

- VT100/xterm escape sequence state machine (12 states)
- CSI / ESC / OSC / DCS / APC dispatch tables
- UTF-8 multi-byte handling in-band
- screen_write operations (~40 functions)

**Key C sources to translate:** `input.c`, `input-keys.c`, `screen-write.c`

## Phase 5: Client-Server & Session Management (📋 Pending)

**Crates:** `loom-server`, `loom-client`

- Session / Window / Pane lifecycle management
- Layout cell tree (recursive split algorithm)
- Client connection, identify phase, message dispatch
- Server main loop with client I/O
- Screen redraw logic
- Process spawning (PTY management)
- Copy mode, tree mode, customization modes

**Key C sources:** `server.c`, `server-client.c`, `session.c`, `window*.c`, `layout*.c`, `spawn.c`, `resize.c`, `screen-redraw.c`

## Phase 6: Commands & Configuration (📋 Pending)

**Crates:** `loom-commands`, `loom-config`

- Command definition trait and dispatch (~60 commands)
- Config file parser using nom
- Command queue (stateful sequential execution)
- Target resolution (-t flag parsing)
- Format string expansion (#{} syntax)
- Key binding tables
- Status line, prompts, menus, popups

**Key C sources:** `cmd*.c`, `cmd-parse.y`, `cfg.c`, `format*.c`, `key-bindings.c`, `status.c`, `prompt*.c`, `menu.c`, `popup.c`

## Architecture

```
loom/                  # Binary entry point
├── loom-core/         # Core types (phase 1) ✅
├── loom-ipc/          # IPC + event loop (phase 2)
├── loom-tty/          # TTY I/O (phase 3)
├── loom-input/        # Terminal emulation (phase 4)
├── loom-server/       # Server main loop (phase 5)
├── loom-client/       # Client (phase 5)
├── loom-commands/     # Command definitions (phase 6)
└── loom-config/       # Config parser (phase 6)
```

## Data Flow

```
Terminal → [loom-tty] → [loom-input] → Screen Grid → [loom-server]
                ↑                                          |
                |                                     [loom-ipc]
                |                                          |
                +------- Client TTY ← [loom-client] ←------+
```

## Notes

- Phase dependencies are strict: each phase builds on the previous.
- Testing strategy: unit tests per module, integration tests for IPC and full command execution.
- The grid cell inline/extended optimization can be deferred; start with simple `Vec<GridCell>`.
