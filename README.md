# Loom

A terminal multiplexer written in Rust, inspired by [tmux](https://github.com/tmux/tmux).

Loom aims to be a faithful Rust reimplementation of tmux's architecture and feature set, built from the ground up with safety, clarity, and modern tooling in mind. It is **not** a drop-in replacement — it is a module-level rewrite that preserves tmux's proven design while leveraging Rust's type system and ecosystem.

> **Status:** Early development. Core data types (Grid, Screen, Colour, Style, Options, UTF-8) are complete. See [ROADMAP.md](ROADMAP.md).

## Planned Features

- [ ] Session, window, and pane management
- [ ] Flexible pane layouts (horizontal/vertical splits, grids, custom)
- [ ] VT100/xterm escape sequence emulation
- [ ] Full 256-color and true colour (24-bit RGB) support
- [ ] UTF-8 and wide character (CJK, emoji) support
- [ ] Mouse support (select, resize, scroll)
- [ ] Copy mode with vi/emacs keybindings
- [ ] Customizable status line with `#{}` format strings
- [ ] Key binding tables with multiple prefix modes
- [ ] Configurable via `~/.loom.conf` (tmux-compatible syntax)
- [ ] Control mode (`-CC`) for IDE integration
- [ ] Sixel image support
- [ ] Popups, menus, and interactive prompts

## Architecture

```
loom/                # Binary entry point
├── loom-core/       # Core types: Grid, Screen, Colour, Style, Options ✅
├── loom-ipc/        # Client-server IPC protocol + event loop
├── loom-tty/        # TTY I/O, terminfo, keyboard input
├── loom-input/      # VT100/xterm escape sequence parser
├── loom-server/     # Server main loop, session/window/pane lifecycle
├── loom-client/     # Client connection to server
├── loom-commands/   # Command definitions and dispatch
└── loom-config/     # Configuration file parser
```

The runtime is a **single-threaded event loop** (via `mio`) with a **client-server process model**:
- `loom` (client) connects to a background `loom` (server) over a Unix domain socket
- The server manages sessions, windows, panes, and terminal emulation
- The client handles TTY I/O and forwards input to the server

## Building

```sh
cargo build --release
```

Requires Rust 1.70+ and a Unix-like operating system (Linux, macOS, BSD).

## Comparison with tmux

| Area | tmux | Loom |
|------|------|------|
| Language | C (~97k LOC) | Rust |
| Event loop | libevent | mio |
| IPC | imsg (OpenBSD) | serde + bincode |
| Config parser | yacc | nom |
| Terminfo | libtermcap/ncurses | terminfo crate |
| Memory safety | Manual | Compiler-enforced |

The goal is **functional parity**, not API compatibility. Loom's config syntax will be similar but not identical to tmux's.

## Related Work

- [tmux](https://github.com/tmux/tmux) — the original
- [zellij](https://zellij.dev/) — a Rust terminal multiplexer with a different architecture
- [tmux-rs](https://github.com/tmux-rs/tmux) — Rust bindings to tmux's control mode

## License

MIT
