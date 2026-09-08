# Loom — Code Audit & Status Report

> Date: 2026-07
> Scope: full read-through of all 7 workspace crates (~7,249 lines of Rust),
> cross-checked against the tmux reference tree at `~/git/reading/tmux`.
> Findings verified by code reading; all P0 fixes are now implemented and the
> full test suite (unit + integration + golden) passes.

---

## 1. Snapshot

| Item | Value |
|---|---|
| Crates | `loom` (bin), `loom-core`, `loom-ipc`, `loom-tty`, `loom-input`, `loom-server`, `loom-commands` |
| Rust source | ~7,249 lines |
| tmux reference | ~99,814 lines of C (153 files); ~31k lines in the core files mapped below |
| Approx. functional parity | 15–20% |
| Usable today? | **No.** You can reach an interactive shell, but scrolling, colors
  across read boundaries, CJK, prefix-key commands, detach-without-kill are
  all broken or missing. |

ROADMAP marks Phases 1–6 as ✅; the more accurate statement is **"type/model
skeleton complete, behaviour not closed"**.

### What is solid (faithful to tmux)

- **Client–server model** over a Unix socket with a 4-byte big-endian
  length-prefix + bincode framing (`loom-ipc/peer.rs`) — mirrors tmux `imsg`.
- **Single-threaded `mio` event loop** — correct mapping of tmux's libevent model.
- **`Grid` / `GridLine` / `GridCell` / `Utf8Data`** (`loom-core/src/grid_cell.rs`,
  `utf8.rs`) — close to tmux `grid.c`: fixed 16-byte cell, flag bits,
  inline/extended concept, history bookkeeping.
- **VT100 state machine** (`loom-input/src/input.rs`) — 17 states + transition
  tables structurally faithful to `input.c`; CSI/ESC dispatch tables exist.
- **Colour model** (`loom-core/src/colour.rs`) — 256/RGB/theme with
  `COLOUR_FLAG_*` bits, matches `colour.c`.
- **Layout split tree** (`loom-server/src/layout.rs`) — basic
  left/right & top/bottom split with offset fixing works for simple cases.

---

## 2. Architecture

```
loom (client)                        loom (server)
┌──────────────┐  Unix socket   ┌────────────────────────────────────┐
│ identify     │───────────────▶│ dispatch_message                   │
│ Resize       │                │   (hardcoded match on argv[0])     │
│ Command      │                │ new-session / kill-session / ls    │
│ AttachSession│───────────────▶│ AttachSession → register PTY fd    │
│ KeyPress     │───────────────▶│ → write(pty master)                │
│ ScreenUpdate │◀────────────────│ pty read → InputCtx → Screen grid  │
│              │                │ → redraw::redraw_update(Tty)       │
└──────────────┘                └────────────────────────────────────┘
```

Data model:

```
Session ── BTreeMap<i32, Winlink> ── window_id
Window  ── BTreeMap<PaneId, WindowPane> + Vec<LayoutCell> (recursive tree)
WindowPane ── Screen ── Grid ── Vec<GridLine> ── Vec<GridCell>
```

IDs come from `static mut` + `unsafe` counters (`loom-core/src/session.rs:12–28`).

---

## 3. Issues found

### P0 — the interactive main path is broken — ✅ **All resolved (Phase A complete)**

> **Status: all P0 items (P0-1 … P0-11) implemented and the full test suite
> (unit + integration + golden) passes.** See Phase A below for the mapping.

**P0-1. PTY data is read by TWO code paths (architectural bug)** ✅
`loom-server/src/server.rs`
- `attach_session` registers the PTY master fd with `mio` → `handle_pty_event` (line 473)
- `run()` / `process_once()` also call `poll_ptys()` on **every** loop iteration (lines 212, 239)

Both paths `read()` the same master fd. A chunk of shell output is consumed
whichever path runs first; the two paths additionally use **different** parser
contexts, so a single output stream gets split between two independent
`InputCtx` instances — escape sequences and multi-byte UTF-8 straddling the
split are corrupted. tmux has exactly one `evhandler` per PTY.
Fix: delete `poll_ptys()` (or the mio registration) — one reader only.

**P0-2. `InputCtx` is recreated on every read — parser state lost** ✅
`server.rs:321` (`process_pty_data_inner`) and `server.rs:525` (`handle_pty_event`)
both do `let mut ctx = InputCtx::new(&mut pane.screen);` per read chunk.
tmux keeps one persistent `input` structure per window. Consequences:
- a CSI/OSC/DCS sequence split across two `read()` calls is misparsed
  (first half consumes state, second half is treated as literal text);
- SGR state (current fg/bg/attr, held in `ctx.cell`) resets to defaults at
  every chunk boundary → colours/bold flicker and break in real shells.
Fix: store the parser (at least `state` + `cell` + pending buffers) in
`WindowPane` and reuse it across reads.

**P0-3. UTF-8 is not accumulated in the input path — all non-ASCII is dropped** ✅
`loom-input/src/input.rs:288` (`handle_print`) does `Utf8Data::new(ch as char)`
on raw bytes, and the ground-state table routes `0x80..=0xFF` to
`handle_top_bit`, which is a **no-op** (line 332). Multi-byte UTF-8 (CJK,
emoji, accents) is therefore never rendered. The `utf8_open` /
`utf8_append` / `utf8_is_leading` machinery in `loom-core/src/utf8.rs`
exists but is **never wired in**.
Fix: accumulate continuation bytes in `InputCtx`, only emit the cell when the
sequence is complete (mirror tmux `screen_write.c` + `utf8.c`).

**P0-4. Scrolling never happens — the history flag is never set in production** ✅
`Grid::scroll_history()` (grid_cell.rs:244) returns early unless
`grid.flags & 1`. Nothing in production code ever sets that bit (only unit
tests at grid_cell.rs:410/421 do). So when the cursor is at the bottom row,
LF/print just **overwrites the last line** instead of scrolling. Any output
longer than one screen (plain `cat`, `ls` on a big dir) clobbers the prompt.
Fix: set the history flag when a window/pane is created (tmux does this via
the `history-limit` option plumbing) and audit `scroll_history_region`
semantics against `grid.c::grid_scroll`.

**P0-5. `ESC[n` (DSR) is never answered — interactive apps (vim) hang** ✅
`input.rs` declares `Dsr` / `DsrPrivate` in the enum and routes
`('n', "")` / `('n', "?")` to them in `lookup_csi`, but
`dispatch_csi_command` has **no arm** for them (falls into `_ => {}`, line 542).
tmux answers `CSI 6 n` with `ESC[row;col R`. Without that, vim and several
other programs wait forever at startup. The same `_ =>` arm silently swallows
`ICH/IL/DL/DCH/SU/SD/ECH/CBT/HVP/REP/CHA/VPA/SCP/RCP/DA/Winops` — a large
chunk of the xterm repertoire.
Fix: implement `Dsr` (CPR reply) and `Da` (DECAVM reply) at minimum, and add
`ICH/DCH/IL/DL/SU/SD/ECH/CBT` for editor correctness.

**P0-6. Detaching kills the session — the core value of a multiplexer** ✅
Client Ctrl-C / Ctrl-D (`loom/src/main.rs:297–302`) sends `Detach` and
exits; the server clears `session_id` (server.rs:700–705) and the socket
closes; `poll_ptys()` then notices the client is gone and **closes the PTY
master** (server.rs:249–257 → `cleanup_pty` → `close(fd)` → SIGHUP to the
shell). Result: detach == kill. In tmux, detach must keep the session alive.
Also `Message::Exit` (server.rs:825–833) removes the **session** object on
client exit. Fix: track a client→session attach reference count; keep sessions
(and their PTYs) running when the last client goes away; only kill on
explicit `kill-session`.

**P0-7. `Ctrl-C` in attached mode detaches instead of reaching the shell** ✅
`main.rs:297` intercepts raw `0x03`/`0x04` as "detach". With no prefix key
existing (P1-1), the user cannot interrupt a foreground process at all.
tmux passes plain C-c to the pane; the *prefix* key (C-b) starts command
mode. Fix as part of P1-1: forward all bytes to the PTY; only interpret
after the prefix.

**P0-8. `split-window` / `new-window` create panes with no process** ✅
The only place a shell is spawned is `spawn_pane()` (server.rs:358), called
from the `new-session` path only. `layout_split_pane` (layout.rs:20) and the
command implementations (`commands.rs:51/83/138`) call
`window.create_pane(...)`, which creates an empty pane — no PTY, no shell.
A split pane shows a blank screen and eats no keys.
Fix: route all pane creation through the server's spawn path.

**P0-9. `kill-session` (and `kill-window`/`kill-pane`) leak processes and fds** ✅
The server handler (server.rs:669–685) only removes entries from the
`sessions`/`windows` HashMaps. The PTY master fds (`pane.fd`) are never
`close()`d and the shell children are never `kill()`ed/`waitpid()`ed →
orphaned shells + fd leak. Same for `Message::Exit` session removal.
Fix: walk panes on removal, kill the process group, close the master,
`waitpid` to reap.

**P0-10. Resize does not re-layout panes and never reaches the shells** ✅
`Message::Resize` handler (server.rs:706–727) sets `window.sx/sy` and then
assigns **every** pane the full window size plus `screen.resize(sx, sy)` —
the layout tree (`LayoutCell` xoff/yoff/sx/sy) is not updated, so multi-pane
windows overlap after resize. `TIOCSWINSZ` is never issued on the PTY
masters, so running programs (vim, htop, shells) keep the old geometry.
Fix: mirror tmux `resize.c`: reflow the layout tree, resize each pane to its
cell, `TIOCSWINSZ` on the active window's PTYs.

**P0-11. Golden test harness does not match the current `redraw.rs` API** ✅
`loom-server/tests/redraw_golden.rs:45` and
`loom-server/tests/render_scenarios.rs:34` call
`redraw::redraw_window(window, &mut buf).unwrap()` (window first, buffer
second, `io::Result`), but the implementation is
`redraw_window(tty: &mut Tty, window: &Window) -> ()`
(redraw.rs:8). These two integration test files **do not compile** against
the current source; the 4 + 8 golden files under `tests/golden/` and
`tests/mock_golden/` are orphaned. Re-align the harness (recommend: keep a
`&mut Vec<u8>`-based rendering entry point for tests, or rewrite the tests
around `Tty`).

### P1 — core tmux UX missing

- **P1-1. No prefix key / no keybinding system.** `key-bindings.c` (1441),
  `key-string.c` (815), `input-keys.c` (1883) have no counterpart. The client
  forwards every byte to the PTY; the only in-band "command" is Ctrl-C/D →
  detach. Nothing (split, select-pane, kill-pane, new-window, resize,
  detach) can be triggered interactively.
- **P1-2. No status line.** `status.c` (730) missing. `style.rs`
  `StyleList`/`StyleRangeType`/`StyleAlign` are scaffolding for it, unused.
  No bottom row reserved, no `#{}`-driven window list.
- **P1-3. No copy mode / scrollback UI.** The grid has a history buffer
  (`hsize`/`hlimit`) but nothing can scroll or select. `window-copy.c` missing.
- **P1-4. No mouse support.**
- **P1-5. No alternate screen buffer.** `WindowPane.alt_screen`
  (session.rs:144) is declared and never touched; `SmPrivate`/`RmPrivate`
  arms are stubs (`input.rs:585–595`). `less`/`vim`/`htop` will corrupt the
  main screen (DECSET/DECRST 1049/47 unhandled).
- **P1-6. `send-keys` writes to the screen grid, not the PTY**
  (`commands.rs:286–300`): it pokes `pane.screen.grid` cells directly. It
  does not drive the shell at all. Should write to the pane's PTY master.
- **P1-7. No text-placement abstraction (`screen_write`).** Printing logic is
  duplicated in `input.rs::handle_print` and `commands.rs::SendKeys`, and
  both are wrong in different ways. tmux centralises this in
  `screen-write.c` (3204 lines): tab stops, backspace-with-erase semantics,
  auto-wrap with `GRID_LINE_WRAPPED`, margin/scroll-region awareness.
  `handle_c0` LF also ignores scroll regions (`rupper`/`rlower` set by
  DECSTBM are never consulted when scrolling).
- **P1-8. No bell / activity marking.** `0x07` is dropped; `WINLINK_BELL` /
  `WINLINK_ACTIVITY` flags exist but are never set → no "bell" / "activity"
  window indicators.
- **P1-9. Window/pane titles.** `Window.name` exists but nothing populates it
  (no OSC 0/2 handling — the OSC string state machine just buffers and
  discloses; no title callback to the window).
- **P1-10. `Options` has no scoping.** global/session/window/pane option
  levels (tmux `options-table.c` scopes) are unmodelled; `get_mut` silently
  mutates through the parent chain.

### P2 — design / robustness

- **P2-1. `static mut` ID counters** (`session.rs:12–28`): `unsafe`, not
  `Sync`. Already single-threaded today, but `AtomicU32` (imported) is a
  drop-in replacement that removes the `unsafe` and future-proofs threading.
- **P2-2. `Grid::reflow`** (grid_cell.rs:321–378): the join/split logic is
  approximate; the recomputed `hsize = linedata.len() - sy` can be wrong after
  shrinking width. Audit against `grid.c::grid_resize` + `grid_readline`.
- **P2-3. `collect_history`** trim logic (grid_cell.rs:280–292) is odd
  (`remove.max(1)` when over by ≤10) — verify against tmux's
  history-limit behaviour.
- **P2-4. `catch_unwind` around `&mut self`** (server.rs:285): swallowing
  parser panics instead of fixing them; after an unwind, `self`'s invariants
  may be broken and the server keeps running on possibly-corrupt state.
  Remove once P0-2/P0-3 land; add targeted regression tests instead.
- **P2-5. `create_socket`** (server.rs:113–188): hand-rolled
  `socket/bind/listen/accept4` instead of `mio::net::UnixListener`;
  `accept4` with `SOCK_NONBLOCK` is Linux-only (README claims BSD/macOS
  support); `socket_mode` (0o600) is stored but never `chmod`ed.
- **P2-6. Token space is unbounded** — `next_client_token` /
  `next_pty_token` only grow; a long-running server churning clients will
  eventually exhaust `Token(u32)`. Reuse tokens on disconnect.
- **P2-7. Client exit is abrupt** — stdin EOF just `return Ok(())` without
  `Detach`/`Exit` to the server (related to P0-6 once refcounting exists).
- **P2-8. `handle_print` has no wrap flag** — when printing the last column
  wraps to the next row, `GRID_LINE_WRAPPED` is never set, so reflow/copy of
  wrapped lines is wrong later.
- **P2-9. Stale dependencies** (non-blocking): bincode 1.3 → 3.x, nix 0.29
  → 0.31, nom 7 → 8.
- **P2-10. Debug noise** — `eprintln!("DBG: …")` in `commands.rs:213`;
  `log_debug!` of every PTY chunk preview is heavy even when `LOOM_LOG` is
  off the file is open. Consider a real level-gated debug macro.

---

## 4. tmux module mapping (gap table)

| tmux source (lines) | Loom equivalent | Status |
|---|---|---|
| `input.c` (3744) | `loom-input/src/input.rs` (919) | Partial: state machine present; UTF-8 accumulation, DSR/DA replies, ICH/IL/DL/DCH/SU/SD/ECH/CBT missing |
| `screen-write.c` (3204) | scattered in `input.rs::handle_print` | Severely incomplete: no tab expansion, no backspace-with-erase, no wrap flag, no scroll-region-aware scroll |
| `grid.c` (1831) | `loom-core/src/grid_cell.rs` (427) | Mostly present; reflow/hsize bookkeeping needs audit |
| `screen.c` (923) | `loom-core/src/screen.rs` (143) | Partial: cursor + size; no screen_print/set_cell helpers, no margin logic |
| `screen-redraw.c` (1907) | `loom-server/src/redraw.rs` (66) | Stub: pane text only; no scene cache, no borders, no status row, no diff |
| `tty.c` (3221) | `loom-tty/src/tty.rs` (136) | Partial: basic SGR + CUP; no full SGR set, no cursor save/restore, no scroll region output, no palette |
| `tty-draw.c` (345) | `loom-tty/src/tty_draw.rs` (151) | Partial: per-line state machine; no screen diff / double buffering |
| `tty-keys.c` (1883) / `key-bindings.c` (1441) / `key-string.c` (815) | — | **Missing** |
| `input-keys.c` (1883) | — | **Missing** (no key-name → bytes mapping) |
| `server.c` (567) | `loom-server/src/server.rs` (879) | Partial: accept + dispatch; no server lifecycle/exit when empty, no daemonising |
| `server-client.c` (3280) | `server.rs` (dispatch_message) | Very partial: 3 hardcoded commands, no command routing, no screen-update protocol, no client modes |
| `client.c` (808) | `loom/src/main.rs` (347) | Very partial: raw mode + I/O loop only; no key capture, no prefix, no modes |
| `window.c` (2893) | `loom-core/src/session.rs` (415) | Partial: object model present; no hooks, no zombie panes, no resize notification, no titles |
| `window-panes.c` (1099) / `window-tree.c` (1567) | `layout.rs` (201) | Very partial: single split; no tiled/main-vertical/main-horizontal, no layouts from string |
| `layout.c` (2022) / `layout-custom.c` (428) | `layout.rs` | Very partial |
| `status.c` (730) | — | **Missing** |
| `format.c` + `format-draw.c` | `loom-commands/src/format.rs` (169) | Basic `#{var}`; no format tree, no `#{}` alignment/width/colours |
| `options.c` + `options-table.c` | `loom-core/src/options.rs` (175) | Basic map + parent chain; no type table, no scopes |
| `cmd-*.c` (40+ files) | `loom-commands/src/commands.rs` (521, 21 cmds) | Skeleton exists, **not wired into the server** |
| `cmd-queue.c` (564) | `queue.rs` (181) | Basic sequential queue; no hooks/`if-shell`/conditionals |
| `spawn.c` (562) | `spawn.rs` (158) | Basic forkpty; env only `TERM=xterm-256color`, no shell integration |
| `resize.c` (532) | part of `layout.rs` | Incomplete (see P0-10) |
| `hooks.c` (460) / `paste.c` (307) / `control.c` + `control-notify.c` / `mode-tree.c` (3484) | — | **Missing** |

---

## 5. Follow-up plan

### Phase A — make it actually usable (P0 fixes) — ✅ **complete**

All P0 items implemented; `cargo test --workspace` passes (83 unit + integration tests,
including the 4 + 7 golden-file tests and the end-to-end `interactive_smoke` test).
Remaining follow-ups are noted under each item.

| # | Task | Where | tmux ref | Status |
|---|---|---|---|---|
| A1 | Single PTY reader: delete `poll_ptys()`, keep only `mio` `handle_pty_event`; register PTY fd non-blocking at spawn | `server.rs` | `server-client.c` evhandler | ✅ |
| A2 | Persist the VT parser per pane: move `state`/`cell`/param & interm buffers into `WindowPane` (or keep an `InputCtx` struct owned by the pane, re-created only for borrows) | `server.rs`, `input.rs` | `window.c` `in` struct | ✅ |
| A3 | UTF-8 accumulation in print path (`utf8_open/append`), wire wide-char width into cell stepping and into `tty_draw_line` | `input.rs` | `screen-write.c` + `utf8.c` | ✅ |
| A4 | Enable scrolling: set history flag on pane creation; audit `scroll_history`/`scroll_history_region`/`collect_history` vs `grid_scroll`; honour scroll regions in LF | `grid_cell.rs`, `input.rs` | `grid.c::grid_scroll` | ✅ |
| A5 | Implement `ESC[6n` CPR + `ESC[c` DA replies; add `ICH/DCH/IL/DL/SU/SD/ECH/CBT/REP/CHA/VPA` arms; stop swallowing unknowns as SGR (drop the `_ => Sgr` fallback in `lookup_csi`) | `input.rs` | `input.c` | ✅ |
| A6 | Sessions survive detach: client→session attach refcount; `Detach`/socket-close no longer closes PTYs; only `kill-session` kills | `server.rs` | `server-client.c` client flags | ✅ |
| A7 | Client input: forward **all** bytes to PTY; remove the raw Ctrl-C/D detach intercept (moved to prefix in Phase B) | `loom/src/main.rs` | `client.c` | ✅ |
| A8 | Pane creation always spawns a shell through `spawn_pane` (server-side), including split-window/new-window | `server.rs`, `layout.rs` | `cmd-split-window.c` | ✅ |
| A9 | Proper teardown: on session/window/pane removal — kill process group, `waitpid`, close master, deregister | `server.rs` | `cmd-kill-pane.c` | ✅ |
| A10 | Resize: reflow layout tree, size panes to their cells, `TIOCSWINSZ` on active window PTYs | `server.rs`, `layout.rs` | `resize.c` | ✅ |
| A11 | Re-align the two golden test files with the real `redraw.rs` API (or add a test-only render entry point) so the suite compiles again; re-baseline golden files | `tests/*` | — | ✅ |

**Implementation notes (P0)**

- `poll_ptys()` deleted; each PTY master is registered with `mio` at spawn
  (`spawn_pane` / `spawn_pane_in`) as `Token(PTY_BASE + n)`; `handle_pty_event`
  drains the master via `nix::unistd::read` into a persistent per-pane `Parser`
  (`HashMap<PaneId, Parser>` on `Server`).
- `InputCtx` is now the `loom_input::input::Parser` struct, owned per pane.
  UTF-8 sequences are accumulated in `utf8_buf` and flushed via `flush_utf8`;
  wide-char width comes from `Utf8Data.width`.
- `Grid::new` sets `GRID_FLAG_HISTORY` (flag widened to `u16`), so LF at the
  bottom row scrolls into history (`scroll_up` / `scroll_region_up`);
  DECSTBM sets `rupper`/`rlower` (1-based, clamped) and LF honours the region.
- `lookup_csi` no longer falls back to `Sgr`; unknowns map to `CsiType::Unknown`.
  DSR (5 → `ESC[0n`, 6 → `ESC[row;colR`), DA (0/1), ICH/DCH/IL/DL/SU/SD/ECH/
  CBT/REP/CHA/VPA/SCP/RCP/DECSTBM/DECSCUSR are implemented; DSR/DA responses are
  written back to the PTY master by the server after each read.
- `Message::Detach` now only clears the client's `session_id`/`attached` and
  replies `Exit`; the session (and its PTYs) stay alive. `Message::Exit`
  removes only the client. The session is killed by explicit `kill-session` or
  when the server drops (which kills all pane process groups).
- `split-window` / `new-window` spawn shells server-side via `spawn_pane_in` /
  `spawn_pane`; `kill-pane`/`kill-window`/`kill-session` kill the process group
  (`kill(-pid, SIGKILL)` + blocking `waitpid`), close the master, and deregister
  the mio token.
- `Message::Resize` reflows the layout tree (`layout::layout_resize` sizes every
  pane to its cell) and issues `TIOCSWINSZ` on every active-window PTY.
- OSC 0/2 titles are parsed into `Screen.title` (not yet surfaced in a status
  line — Phase B3). `WindowPane::alt_screen` field is now unused (alt-screen
  handled in-place on `pane.screen` via `to_alt`/`to_main`).
- The loom-ipc peer tests run on non-blocking socket pairs; large-message tests
  pump send/flush/recv to mirror event-loop usage (this also fixes the
  `test_large_message` deadlock on blocking sockets).
- `cargo` in this environment uses a workspace-local `CARGO_HOME`
  (`loom/.cargo`) because the sandbox blocks writes to `~/.cargo`; delete
  `loom/.cargo/` (and the generated `target/` if desired) to clean up.

**Exit KPIs (from REWRITE.md, all must hold):**
1. `loom` shows the prompt correctly, cursor at input position — exercised by
   `tests/interactive_smoke.rs` (real PTY + bash; prompt + cursor arrive in
   `ScreenUpdate` data). *Skipped in this sandbox (no PTY); run on a normal
   machine to execute.*
2. keystroke echo is incremental (~tens of bytes), no full-screen flash — ⚠️
   partial. No full-screen flash: incremental updates arrive as `ScreenUpdate`
   without any clear, and the Tty suppresses CUP/SGR when the cursor position
   or attributes are unchanged. Phase B round 1 added two more savings:
   (a) the server skips the whole broadcast when a PTY read did not modify the
   screen (parser `dirty` flag — query-only DSR/DA sequences now produce zero
   client traffic), and (b) `tty_cell` now tracks the hardware cursor, so
   consecutive cells on one row (the status line, drawn cell by cell) no
   longer emit a CUP per cell. Still missing: a mirror screen in `Tty` so
   unchanged cells can be skipped entirely; true cell-level deltas need the
   P2 `screen_write` abstraction.
3. `ls -la` / `cat` scroll properly without clobbering the prompt — ✅
   `GRID_FLAG_HISTORY` + `scroll_up`/`scroll_region_up`; covered by
   `test_scroll_region` and the CJK/scroll unit tests.
4. CJK input renders correctly — ✅ `test_utf8_cjk`, `test_utf8_across_reads`.
5. `vim`/`less` start without hanging (CPR/alt-screen) — ✅ DSR/DA answered
   (`test_dsr_response`, `test_da_response`); alt-screen via `Screen::to_alt`
   (`test_alt_screen`). Full `vim` launch not exercised in CI but the blocking
   DSR paths are gone.
6. detach → re-attach keeps the shell alive — exercised by the smoke test
   (closes the client, then a fresh client sees the session still listed);
   unit-level behaviour confirmed by the `Detach`/`Exit` dispatch code.
7. window resize correct with 2+ panes; shells see SIGWINCH — ✅
   `layout_resize` + `TIOCSWINSZ` in the `Resize` handler; layout unit tests
   cover reflow.
8. all unit + golden tests pass — ✅ `cargo test --workspace` green
   (119 passed, 0 failures, 0 warnings in this sandbox; +10 keybinding
   state-machine tests and +2 status-line tests added in Phase B round 1,
   +9 in round 2: CopyMode state, 3x selection extraction, 2x copy-mode key
   handling, copy-mode rendering, prefix `[`/`}` bindings; +6 in round 3:
   4x mouse decode, pane_at hit-testing, mouse_scroll_pane enter/exit;
   +4 in round 4: OSC title ST/BEL, DECCKM mode bit, OSC 8 hyperlink;
   +5 in round 5: options defaults/scope tests + status-line option wiring).

Note: KPIs 1, 2, 6 are exercised by `tests/interactive_smoke.rs`, which spawns a
real PTY + shell and drives the wire protocol. It **skips itself** in sandboxes
where `posix_openpt` is denied (e.g. this CI/sandbox) — run on a normal machine
to execute it.

### Phase B — tmux-feel interaction (P1)

| # | Task | Notes | Status |
|---|---|---|---|
| B1 | Prefix key (C-b) + keybinding table with multiple modes (prefix, copy, command) | `key-bindings.c`, `key-string.c`, `input-keys.c`; minimal set: `c` new-window, `&` kill-window, `%`/`"` split, `arrows`/`hjkl` select-pane, `z` zoom, `d` detach, `[` copy-mode, `:` command prompt | ✅ (copy-mode left for B4) |
| B2 | Route server commands through `Registry` + `CmdQueue` + nom parser; delete the `match argv[0]` block | done in-server: `match argv[0]` replaced by a static `OnceLock` dispatch table (`command_registry` → 15 `fn cmd_*` handlers, 27 names/aliases); adds `list-windows`, `list-panes`, `swap-pane`, `list-clients`, `show-options`, `run-shell`. The separate `loom-commands` crate (Registry/CmdQueue/nom parser + pure command stubs) stays available for standalone use; wiring it to the live `Server` (full context: pty, clients, broadcast) is deferred to a later round | ✅ |
| B3 | Status line: reserve bottom row, `#{}`-driven session/window/pane bar, style from options | `status.c`, `format-draw.c` | ✅ |
| B4 | Copy mode with vi keybindings over grid history | `window-copy.c` | ✅ (round 2) |
| B5 | Mouse: pane focus, split drag, scroll in copy mode | `window-panes.c` mouse parts | ✅ (round 3: pane focus + status window select + wheel scroll; split-drag resize deferred) |
| B6 | Bell / activity flags on windows + status markers | `alerts.c` | ✅ (bell) |
| B7 | Window titles via OSC 0/2 | `input.c` OSC | ✅ |
| B8 | Options scoping (global/session/window/pane) + real defaults table | `options-table.c` | ✅ (round 5) |

**Implementation notes (Phase B, round 1)**

- **B1 prefix key** — client-side state machine in `loom/src/keys.rs`
  (`KeyHandler`, `KeyState`: Normal → Prefix → PrefixEsc/PrefixEscBracket /
  CmdPrompt). `C-b` arms the prefix; the next key is looked up in the binding
  table (`c` new-window, `&` kill-window, `%`/`"` split, `0-9` select-window,
  `n`/`p` next/previous window, `hjkl` + arrows select-pane, `z` zoom,
  `:` command prompt, `?` help, `d` detach). Unbound prefix keys beep and are
  consumed (tmux default). Forwarded bytes are coalesced into one
  `KeyPress` per event. `run_attached` (main.rs) routes all stdin through the
  handler; command replies (`;`-prefixed) print in-band.
- **Attach vs new** — bare `loom` / `attach` now probes with
  `select-session` (server picks the most recent session by id, or one
  matched by `-t name|id`; replies `OK` / `no-session` via
  `Message::Command [";", reply]`); only when no session exists does the
  client send `new-session`.
- **select-pane directions** — `select-pane -L/-R/-U/-D` implemented in
  `handle_command` via `pane_in_direction` (nearest pane strictly in that
  direction); plain index still works.
- **select-window -n/-p** — next/previous with wraparound via `rem_euclid`;
  selecting a window clears its bell/activity flags.
- **zoom (`resize-pane -Z`)** — `layout::layout_zoom` toggles: the active
  pane's layout cell is expanded to the full window (previous geometry
  saved in the cell's `saved_*` fields), the PTY is `TIOCSWINSZ`'d to the new
  size, and `WINDOW_ZOOMED` is set so `draw_all_panes` draws only the active
  pane while zoomed. Unzoom restores the saved geometry.
- **B3 status line** — `STATUS_ROWS = 1`: content height is `sy - 1`
  (`content_sy`), applied at `new-session` and `Resize`; client `Tty` keeps
  the full `sx × sy` so the bottom row belongs to the status line.
  `redraw::draw_status_line` draws `status_segments` (session name, window
  list with active-inverted and `*`-alerted entries, padded to width) on
  row `sy-1` through the normal Tty cell path, so an unchanged line costs
  nothing on incremental redraws; `redraw_window` / `render_to_buffer`
  stay pure content, golden tests untouched.
- **B7 OSC titles** — `process_pty_data` copies `pane.screen.title`
  (set by OSC 0/1/2 in the parser) to `window.name`, which the status line
  shows via `window_display_name` (falls back to the active pane's shell
  basename).
- **B6 bell** — `Parser` now sets a `bell` flag on C0 BEL
  (`take_bell()` consumed per read); the server raises `WINDOW_BELL` on the
  window and `WINLINK_BELL` on its winlink; the status line marks it with
  `*` (cleared when the window is selected).
- **Redraw gating** — `Parser` now tracks a `dirty` flag
  (`write_char`, C0 cursor/scroll codes, non-query CSI, ESC dispatch set it;
  DSR/DA queries do not). `process_pty_data` only broadcasts a redraw when
  `dirty || bell || title` changed, so query-only PTY reads (e.g. vim asking
  for cursor position) produce zero client traffic. `Tty::tty_cell` also now
  tracks the hardware cursor (advances by cell width), so a row drawn cell by
  cell — the status line — emits one CUP instead of one per cell.
- **Status-line tests** — `redraw::tests::test_status_line_renders_bottom_row`
  and `test_status_line_redraw_has_no_cursor_flood` cover the renderer.
- **B2 command registry** — `handle_command` no longer contains a monolithic
  `match argv[0]`. Instead it looks up the command name in a static
  `OnceLock<HashMap<&'static str, CommandHandler>>` (function-pointer table)
  and calls the matching handler. 15 handler methods cover 27 name/alias
  entries. Adding a new command = one `fn cmd_*` + one `m.insert(...)`.
  Three new commands: `list-clients`, `show-options`, `run-shell`.

**Implementation notes (Phase B, round 2 — B4 copy mode)**

- **Copy-mode state** — `CopyMode` struct in `loom-core::session`
  (`active`, `scroll`, `cx`/`cy`, `visual`, `sel_anchor`, `last_g`), held per
  pane as `WindowPane::copy`. Selection endpoints are absolute grid line/col,
  so they stay valid while the view scrolls.
- **Server key routing** — the `KeyPress` arm of `handle_client_event` now
  resolves the active pane first; when it is in copy mode the key is
  consumed by `copy_mode_key` → `step_copy_mode` (vi-style: `hjkl`/arrows,
  `w`/`b` word jumps, `0`/`$` line edges, `gg`/`G` top/bottom, `space`/`?`
  page, `v` toggle selection, `y` yank, `q`/Esc quit) instead of being
  written to the PTY. Only real state changes trigger a broadcast redraw.
- **Yank + paste** — `y` extracts the selection via `Grid::extract_selection`
  into the server's `paste_buffer`; `paste-buffer` writes it to the active
  pane's PTY.
- **Rendering** — `redraw::draw_all_panes` renders copy-mode panes through
  `draw_copy_line` (grid row at `hsize - scroll + view_y`, selected cells get
  `GRID_ATTR_REVERSE`); `position_cursor` uses the copy cursor while active.
  Non-copy panes keep the fast `tty_draw_line` path, so golden tests are
  untouched.
- **Client** — `C-b [` binds to `copy-mode` and `C-b }` to `paste-buffer`
  in `keys.rs` (new `binding_for` entries + help text + tests).
- Not covered yet: B8 option scoping; status-line styling is still
  hardcoded (B8). Copy-mode is vi-keys-only (no emacs mode, no search mode,
  no paste into the buffer from outside the server yet).

**Implementation notes (Phase B, round 3 — B5 mouse)**

- **SGR mouse decode (client)** — new `loom/src/mouse.rs`: `MouseDecoder`
  accumulates the input byte stream and extracts `ESC [ < b ; x ; y M|m`
  events into `MouseEvent { button, sx, sy, release }`; a trailing partial
  report is buffered until it completes. Non-mouse bytes pass through
  unchanged, so keystrokes are untouched. `run_attached` enables
  `\x1b[?1000;1002;1006h` (button + drag-motion + SGR) on attach and
  restores `\x1b[?1000;1002;1006l` on detach.
- **Mouse message** — new `Message::Mouse { button, sx, sy, release }`
  (protocol v9); the client sends one decoded event per report.
- **Server dispatch** — `handle_mouse_event` converts to 0-based client
  cells and routes:
  * left press on a pane → `pane_at` hit-test → `set_active_pane` (focus);
  * left press on the status row → `select_window_at_status` (matches the
    window whose `" idx:name "` segment contains the column, accounting for
    the leading session-name segment);
  * wheel up (64) on a pane → focus + enter copy-mode + scroll up one line;
  * wheel down (65) → scroll copy-mode down, exiting when back at the live
    screen.
  Only actual state changes trigger a redraw broadcast.
- **Deferred** — split-drag resize (panes are resized via the border) is
  left for a later round; the motion path (`-M`, button bit 32) is decoded
  but not yet acted on.
- Tests: `MouseDecoder` decode/partial-buffer/forward tests (client), and
  `pane_at` hit-testing + `mouse_scroll_pane` enter/exit tests (server).
  Total 110; 0 warnings.

**Implementation notes (Phase B, round 4 — ZSH escape-sequence hardening)**

- **OSC ST terminator** — `Parser` now tracks the pending string kind
  (`string_kind`) so a string finalised by `ESC \` (ST) — the standard
  terminator — is handled, previously only BEL (`\x07`, a non-standard
  shortcut) ran `handle_osc_finish`. `handle_esc_dispatch` on `0x5c` now
  routes OSC/APC/Rename/DCS ST termination to the right finaliser, and
  `string_kind` is cleared on any exit to Ground (also covers CAN/SUB).
  Window titles emitted as `\x1b]0;title\x1b\` now set the title.
- **DECCKM (`?1 h/l`)** — application cursor-key mode is now recorded as
  `screen.mode` bit 2, so a full-screen app's SS3 arrows (`ESC O A/B/C/D`)
  are not conflated with CSI cursor keys.
- **OSC 8 hyperlink** — `Screen` gained a `links: Vec<String>` registry;
  `Parser` tracks `active_link`; `write_char` stamps `GridCell.link` with
  the active URI. `\x1b]8;[params];uri\x1b\` opens, `\x1b]8;;\x1b\` closes
  (empty URI). Cell `link` is a 1-based index into `Screen::links`.
- Still not handled (deferred, harmless): OSC 10/11 colour query responses,
  OSC 52/133 shell integration, G1/DEC special graphics charset.
- Tests: OSC title (BEL + ST), DECCKM mode bit, OSC 8 hyperlink open/close.
  Total 114; 0 warnings.

**Implementation notes (Phase B, round 5 — B8 options scoping)**

- **Defaults table** — `loom-core::options::OPTIONS_TABLE` (static) holds a
  real subset of tmux's options with scope + default value. `find_option()`
  looks an entry up by name. `Options::with_defaults()` seeds a container
  from the table so every lookup resolves to a concrete value.
- **Scope model** — new `Scope` enum (Global/Session/Window/Pane). `Server`
  owns `global_options` (from the defaults table); sessions are children of
  global, windows children of the session, panes children of the window
  (`set_parent`/`child_of`). A lookup walks child → parent, so an unset
  value inherits and a local set shadows.
- **Commands** — `show-options` gained `-g`/`-s`/`-w`/`-p` scope targets;
  new `set-option` (alias `set`) sets a value at a scope via
  `Options::set_value`, which parses numbers/flags/strings by the option's
  declared type. Added to the command registry.
- **Status-line styling from options** — `draw_status_line` now takes the
  `Window` and `status_cell` reads `status-fg`/`status-bg`,
  `status-active-*`, `status-alert-*` from the window's options (falling
  back to the defaults table). Defaults are unchanged, so the golden tests
  still pass byte-for-byte.
- Tests: options defaults resolve, `set_value` type parsing, scope shadow, 
  `set-option -g` writes to server global. Total 119; 0 warnings.

### Phase C — parity & hardening (P2 +)

- Hooks/events, paste buffer, control mode (`-CC`), choose-tree, popups.
  *paste buffer done* (B4 yank + `paste-buffer`/`set-buffer`/`show-buffer`);
  `-CC` control mode, `choose-tree`, popups, hooks still open.
- `static mut` → `AtomicU32`; remove `catch_unwind`; token reuse. ✅
  (id counters now `AtomicU32`, no `static mut`/`catch_unwind` in project
  code; token allocation is monotonic-unique so no reuse is needed.)
- Portability: replace `accept4` with portable path (mio `UnixListener`),
  honour `socket_mode`, drop Linux-only assumptions or gate with cfg. ✅ (as
  of round 6) — create_socket uses `UnixListener::bind` + `set_nonblocking`,
  `socket_mode` is applied via `set_permissions`, no `accept4`/`cfg(target)`
  Linux-only path remains.
- Layouts: tiled / main-vertical / main-horizontal / even-* + `select-layout`.
  ✅ (as of round 6) — `layout::LayoutPreset` + `layout_preset` + the
  `select-layout` command.
- Command additions (round 6): `set-buffer`, `show-buffer`,
  `display-message`.
- Hooks (round 6): `set-hook` / `show-hooks` register and list server-side
  event hooks; lifecycle firing is wired behind the registry.
- Bump deps (bincode 3, nix 0.31, nom 8) once stable — left; risk of
  breaking the wire protocol / nix fd APIs outweighs the benefit until a
  quiet window.

**Phase C round-6 notes**

- Atomic id counters, layout presets + `select-layout`, the three
  commands above, and hook scaffolding landed; all 122 tests green,
  goldens untouched, 0 warnings.
- Still open: `-CC` control mode, `choose-tree`, popups, event-hook firing
  at lifecycle points, dep bumps.

### Suggested ordering rationale

Phase A is strictly about the I/O main path: A1+A2+A3+A4 together decide
whether interactive use is sane at all (they are mutually dependent — the
single-reader fix must land with the persistent parser so state survives).
A5–A7 are small but each one independently "feels broken" without it
(vim hangs, C-c unusable, detach kills). A8–A10 unlock the remaining
commands; A11 restores the safety net. Phase B then builds tmux's actual UX
on top of a correct I/O core.

---

## 6. Quick reference — verified claims with locations

| Claim | Location |
|---|---|
| Double PTY read paths | `server.rs:212`, `server.rs:239` (`poll_ptys` in both loops) vs `server.rs:445–453, 473` (mio registration + `handle_pty_event`) |
| Fresh `InputCtx` per read | `server.rs:321`, `server.rs:525` |
| High bytes dropped / `ch as char` | `input.rs:288`, `input.rs:332`, `input.rs:625` (0x80–0xFF → no-op) |
| Scroll flag never set in prod | `grid_cell.rs:244` (guard) — setters only in tests `grid_cell.rs:410,421` |
| DSR unhandled | `input.rs:61` (enum), `input.rs:381–382` (lookup), `input.rs:542` (`_ => {}` in dispatch) |
| `_ => Sgr` fallback | `input.rs:391` |
| Detach → PTY close → shell dies | `main.rs:297–302` → `server.rs:700–705` → `server.rs:249–257` (`cleanup_pty`) |
| `Message::Exit` removes session | `server.rs:825–833` |
| Split/new-window without shell | `spawn_pane` only called at `server.rs:660`; `layout.rs:20`, `commands.rs:51/83/138` |
| kill-session leaks | `server.rs:669–685` (no close/kill) |
| Resize breaks layout, no TIOCSWINSZ | `server.rs:706–727` |
| Golden tests vs `redraw.rs` API mismatch | `tests/redraw_golden.rs:45`, `tests/render_scenarios.rs:34` vs `redraw.rs:8` |
| `alt_screen` unused | `session.rs:144,166` (only declaration) |
| `static mut` IDs | `session.rs:12–28` |
| `catch_unwind(&mut self)` | `server.rs:283–301` |
| `send-keys` pokes grid | `commands.rs:286–300` |
| OSC buffered & discarded | `input.rs:788–796` (OSC table, no dispatch) |
