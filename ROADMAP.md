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
| 6 | `loom-commands` + `loom-config` (21 commands, queue, format, config parser) | ✅ | 13 |
| 7 | Interactive I/O loop (raw mode, PTY I/O, ScreenUpdate, attach) | 🔄 | — |

**Total:** ~5,900 lines of Rust across 6 crates + 1 binary. (Target: ~7,000+ after Phase 7.)

## Recommended Next Steps

1. **Phase 7: Interactive I/O loop** — connect TTY input → PTS → redraw output cycle
2. **Key binding tables** — key table + prefix key support
3. **Status line** — render status bar with format strings
4. **Copy mode** — interactive scrollback search/selection

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

## Phase 6: Commands & Configuration (✅ Complete)

**Crates:** `loom-commands`, `loom-config`

| Module | File | Status |
|--------|------|--------|
| Command trait + CmdCtx + Registry | `loom-commands/src/cmd.rs` | ✅ Done |
| Command parser (nom-based flags + positional) | `loom-commands/src/parser.rs` | ✅ Done |
| Core commands (new-session, kill-session, list-sessions) | `loom-commands/src/commands.rs` | ✅ Done |
| Window/pane commands (new-window, kill-window, list-windows, list-panes) | `loom-commands/src/commands.rs` | ✅ Done |
| Pane management (split-window, select-pane, resize-pane, kill-pane, swap-pane) | `loom-commands/src/commands.rs` | ✅ Done |
| Navigation (select-window, switch-client, send-keys) | `loom-commands/src/commands.rs` | ✅ Done |
| Session commands (attach-session, detach-client, rename-session, has-session) | `loom-commands/src/commands.rs` | ✅ Done |
| Options commands (set-option, show-options, list-commands) | `loom-commands/src/commands.rs` | ✅ Done |
| Target resolution (-t flag parsing) | `loom-commands/src/commands.rs` | ✅ Done |
| Config file parser (nom-based) | `loom-commands/src/config.rs` | ✅ Done |
| Command queue (stateful sequential execution) | `loom-commands/src/queue.rs` | ✅ Done |
| Key binding tables | — | 📋 Pending |
| Status line, prompts, menus, popups | — | 📋 Pending |

**Tests:** 13 passing.

**Key C sources translated:** `cmd.c` (command table), `cmd-new-session.c`, `cmd-new-window.c`, `cmd-kill-session.c`, `cmd-list-sessions.c`, `cmd-split-window.c`, `cmd-select-pane.c`, `cmd-send-keys.c`, `cmd-resize-pane.c`, `cmd-kill-pane.c`, `cmd-swap-pane.c`, `cmd-switch-client.c`, `cmd-attach-session.c`, `cmd-detach-client.c`, `cmd-set-option.c`, `cmd-list-windows.c`, `cmd-list-panes.c`, `arguments.c` (basic), `cmd-parse.y` (basic nom replacement).

## Phase 7: Interactive I/O Loop (🔄 In Progress)

**Goal:** Make `loom attach` work — client TTY raw mode, bidirectional I/O with pane PTY, live redraw.

**New IPC messages:**
- `KeyPress { key: String }` — client → server: forward keystroke to pane
- `ScreenUpdate { data: Vec<u8> }` — server → client: ANSI redraw data for client stdout
- `AttachSession` — client → server: request to enter interactive attach mode

| Module | File | Status |
|--------|------|--------|
| IPC message types (KeyPress, ScreenUpdate, AttachSession) | `loom-ipc/src/message.rs` | 📋 Pending |
| Client run_attached (raw mode, mio stdin/stdout loop) | `loom/src/main.rs` | 📋 Pending |
| Server AttachSession handler (subscribe client to pane I/O) | `loom-server/src/server.rs` | 📋 Pending |
| Server PTY read → InputCtx parse → redraw → ScreenUpdate | `loom-server/src/server.rs` | 📋 Pending |
| Client ScreenUpdate → write stdout → flush | `loom/src/main.rs` | 📋 Pending |
| SIGWINCH → Resize message | `loom/src/main.rs` | 📋 Pending |

### Data Flow (attach mode)

```
Client stdin  → [mio] → KeyPress msg → Server → write(PTY master)
                                                      │
                                               [InputCtx::parse_buf]
                                                      │
                                              pane.screen updated
                                                      │
                                               redraw_window()
                                                      │
Client stdout ← [mio] ← ScreenUpdate msg ←── Server ←┘
```

## Architecture

```
loom/                  # Binary entry point (phase 5) ✅
├── loom-core/         # Core types (phase 1) ✅
├── loom-ipc/          # IPC + event loop (phase 2) ✅
├── loom-tty/          # TTY I/O (phase 3) ✅
├── loom-input/        # Terminal emulation (phase 4) ✅
├── loom-server/       # Server main loop (phase 5) ✅
├── loom-commands/     # Command definitions + config parser (phase 6) ✅
```

## Data Flow

```
                    ┌─────────────────────────────────────────┐
                    │              loom server                 │
                    │  ┌──────────┐   ┌──────────┐            │
Client stdin ───────┼─▶│ PTY I/O  │──▶│ InputCtx │──▶ Grid   │
                    │  │ (mio)    │   │ (VT100)  │    update  │
                    │  └──────────┘   └──────────┘            │
                    │       │              │                  │
                    │       │              ▼                  │
                    │  ┌──────────┐   ┌──────────┐            │
Client stdout ◀─────┼──│ IPC send │◀──│ redraw() │           │
                    │  │ msg      │   │ (ANSI)   │            │
                    │  └──────────┘   └──────────┘            │
                    │       ▲                                 │
                    │  ┌──────────┐                            │
                    │  │ Attach   │                            │
                    │  │ handler  │                            │
                    │  └──────────┘                            │
                    └─────────────────────────────────────────┘
```

## Notes

- Phase dependencies are strict: each phase builds on the previous.
- Testing strategy: unit tests per module, integration tests for IPC and full command execution.
- The grid cell inline/extended optimization can be deferred; start with simple `Vec<GridCell>`.
