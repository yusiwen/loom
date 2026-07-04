# Loom — Rust TMUX Roadmap

## Overview

Loom is a Rust implementation of a terminal multiplexer inspired by [tmux](https://github.com/tmux/tmux). This document tracks the translation progress.

Reference codebase: `tmux` (~97,000 lines of C across 149 files).

Target: ~60,000–80,000 lines of Rust, organized as a Cargo workspace.

## Progress Summary

| Phase | Crate | Status | Tests |
|-------|-------|--------|-------|
| 1 | `loom-core` (Grid, Screen, Colour, Style, UTF-8, Options) | ✅ | 25 |
| 2 | `loom-ipc` (serde message framing, mio event loop) | ✅ | 9 |
| 3 | `loom-tty` (terminfo, termios raw mode, output commands) | ✅ | 3 |
| 4 | `loom-input` (VT100 state machine, CSI/ESC dispatch) | ✅ | 5 |
| 5 | `loom-server` + `loom` binary (session/window/pane, socket, dispatch, PTY spawn, client, redraw, layout split) | ✅ | 28 |
| 6 | `loom-commands` + `loom-config` (command trait, parser, 7 commands) | 🔄 | 3 |

**Total:** ~4,700 lines of Rust across 6 crates.

## Recommended Next Steps

1. **More commands (~60 total)** — fill out remaining tmux commands
2. **Config parser** — nom-based `.loom.conf` parser
3. **Screen redraw** — scene caching + tty draw pipeline
4. **Layout split/resize** — complete pane split/grow/shrink operations
5. **Copy/tree modes** — interactive pane modes

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

## Phase 5: Client-Server & Session Management (✅ Complete)

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
| Layout split / resize operations | `loom-server/src/layout.rs` | ✅ Done |
| PTY spawn (forkpty + child I/O) | `loom-server/src/spawn.rs` | ✅ Done |
| Screen redraw (render window panes to terminal) | `loom-server/src/redraw.rs` | ✅ Done |
| Client binary (connect + identify flow) | `loom/src/main.rs` | ✅ Done |
| Copy mode / tree mode / interactive modes | — | 📋 Pending |

**Tests:** 3 passing (loom-server: server + spawn + redraw/layout), 25 passing (loom-core session types).

**Key C sources translated:** `session.c`, `window.c` (core), `server.c`, `server-client.c` (core), `layout.c` (basic), `spawn.c` (forkpty).

### Remaining

- Copy/tree modes — interactive features

## Phase 6: Commands & Configuration (🔄 In Progress)

**Crates:** `loom-commands`, `loom-config`

| Module | File | Status |
|--------|------|--------|
| Command trait + CmdCtx + Registry | `loom-commands/src/cmd.rs` | ✅ Done |
| Command parser (nom-based flags + positional) | `loom-commands/src/parser.rs` | ✅ Done |
| Core commands (new-session, kill-session, list-sessions) | `loom-commands/src/commands.rs` | ✅ Done |
| Window commands (new-window, kill-window) | `loom-commands/src/commands.rs` | ✅ Done |
| Rename/has commands | `loom-commands/src/commands.rs` | ✅ Done |
| Config file parser (nom) | — | 📋 Pending |
| Command queue (stateful sequential execution) | — | 📋 Pending |
| All ~60 tmux commands | — | 📋 Pending |
| Target resolution (-t flag parsing) | — | 📋 Pending |
| Format string expansion (#{} syntax) | — | 📋 Pending |
| Key binding tables | — | 📋 Pending |
| Status line, prompts, menus, popups | — | 📋 Pending |

**Tests:** 3 passing.

**Key C sources translated:** `cmd.c` (command table), `cmd-new-session.c`, `cmd-new-window.c`, `cmd-kill-session.c`, `cmd-list-sessions.c`, `arguments.c` (basic), `cmd-parse.y` (basic nom replacement).

## Architecture

```
loom/                  # Binary entry point (phase 5) ✅
├── loom-core/         # Core types (phase 1) ✅
├── loom-ipc/          # IPC + event loop (phase 2) ✅
├── loom-tty/          # TTY I/O (phase 3) ✅
├── loom-input/        # Terminal emulation (phase 4) ✅
├── loom-server/       # Server main loop (phase 5) ✅
├── loom-commands/     # Command definitions (phase 6)
└── loom-config/       # Config parser (phase 6)
```

## Data Flow

```
Terminal → [loom-tty] → [loom-input] → Screen Grid → [loom-server]
                ↑                                          |
                |                                     [loom-ipc]
                |                                          |
                +------- Client TTY ← [loom] ←-------------+
```

## Notes

- Phase dependencies are strict: each phase builds on the previous.
- Testing strategy: unit tests per module, integration tests for IPC and full command execution.
- The grid cell inline/extended optimization can be deferred; start with simple `Vec<GridCell>`.
