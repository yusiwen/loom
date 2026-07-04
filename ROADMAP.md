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

**Tests:** 25 passing, 0 warnings, 0 errors.

## Phase 2: IPC Layer & Event Loop (✅ Complete)

**Crate:** `loom-ipc`

| Module | File | Status |
|--------|------|--------|
| Message types (serde enum) | `message.rs` | ✅ Done |
| Peer (framed send/recv over UnixStream) | `peer.rs` | ✅ Done |
| Proc (mio event loop, peer management) | `proc.rs` | ✅ Done |

**Tests:** 9 passing, 0 warnings, 0 errors.

**Key C sources translated:** `proc.c`, `tmux-protocol.h`

**Remaining:** SCM_RIGHTS fd passing (deferred to phase 5 when PTY is needed).

## Phase 3: TTY & Terminal I/O (✅ Complete)

**Crate:** `loom-tty`

| Module | File | Status |
|--------|------|--------|
| Terminfo capability loading (terminfo crate) | `terminfo.rs` | ✅ Done |
| TTY raw mode (termios via nix) | `tty.rs` | ✅ Done |
| Output buffer and cursor/colour/region commands | `tty.rs` | ✅ Done |

**Tests:** 3 passing, 0 warnings, 0 errors.

**Key C sources translated:** `tty.c`, `tty-term.c` (core).

**Remaining (deferred):** Keyboard input trie parsing (`tty-keys.c`), ACS mapping (`tty-acs.c`), cell drawing (`tty-draw.c`).

## Phase 4: Terminal Emulator (✅ Complete)

**Crate:** `loom-input`

| Module | File | Status |
|--------|------|--------|
| VT100 state machine (17 states + transition tables) | `input.rs` | ✅ Done |
| CSI dispatch (CUP/CUU/CUD/CUF/CUB, ED/EL, SGR, DECSTBM, DECSCUSR, SM/RM) | `input.rs` | ✅ Done |
| ESC dispatch (DECSC, DECRC, RIS, IND, NEL, RI) | `input.rs` | ✅ Done |
| C0 control codes (BS, HT, LF, CR) | `input.rs` | ✅ Done |
| Parameter parsing (digits + separators) | `input.rs` | ✅ Done |
| 256-colour and 24-bit RGB SGR support | `input.rs` | ✅ Done |

**Tests:** 5 passing, 0 warnings, 0 errors.

**Key C sources translated:** `input.c` (core state machine and CSI/ESC dispatch).

**Remaining (deferred):** OSC dispatch (title, palette, clipboard), DCS passthrough (Sixel).

## Phase 5: Client-Server & Session Management (🔄 In Progress)

**Crates:** `loom-server`, `loom-client`

| Module | File | Status |
|--------|------|--------|
| Session / Winlink / Window / WindowPane types | `loom-core/src/session.rs` | ✅ Done |
| LayoutCell recursive tree | `loom-core/src/session.rs` | ✅ Done |
| Session lifecycle (create, attach/detach, set_current) | `loom-core/src/session.rs` | ✅ Done |
| Window/Pane lifecycle (create, remove, set_active) | `loom-core/src/session.rs` | ✅ Done |
| Server socket creation (AF_UNIX bind+listen) | `loom-server/src/server.rs` | ✅ Done |
| Server accept + peer registration | `loom-server/src/server.rs` | ✅ Done |
| Server event loop (mio Poll) | `loom-server/src/server.rs` | ✅ Done |
| Client dispatch (identify phase, command dispatch) | `loom-server/src/server.rs` | ✅ Done |
| Basic commands (new-session, kill-session, list-sessions) | `loom-server/src/server.rs` | ✅ Done |
| Layout split / resize operations | — | 📋 Pending |
| PTY spawn (fork + forkpty) | — | 📋 Pending |
| Screen redraw (scene caching + tty draw) | — | 📋 Pending |
| Client binary (connect + identify flow) | — | 📋 Pending |
| Copy mode / tree mode / interactive modes | — | 📋 Pending |

**Tests:** 1 passing (loom-server), 25 passing (loom-core session types).

**Key C sources translated:** `session.c`, `window.c` (core), `server.c`, `server-client.c` (core), `layout.c` (basic).

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
├── loom-ipc/          # IPC + event loop (phase 2) ✅
├── loom-tty/          # TTY I/O (phase 3) ✅
├── loom-input/        # Terminal emulation (phase 4) ✅
├── loom-server/       # Server main loop (phase 5) ✅
├── loom-client/       # Client (phase 5)          📋
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
