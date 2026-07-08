# Loom Render Engine Rewrite

## Background

Phase 7 implemented a functional I/O loop (PTY → InputCtx → ScreenUpdate → Client TTY), but
the rendering path has fundamental architectural flaws that cannot be fixed with incremental patches.

## Diagnosis: What Went Wrong

| Problem | Root Cause |
|---------|------------|
| 146KB ScreenUpdate per keystroke | `redraw_window()` redraws ALL 15494 cells every time |
| Cursor at line end, not at prompt | `cx` from InputCtx is at right-prompt position (253), not command-input position |
| Output overwrites prompt | No scene caching — every keystroke triggers a full redraw |
| Borders / status line missing | Current `redraw.rs` only handles pane interiors, skips the full client scene |

## Fixing Strategy

Phase 7 attempted to patch `redraw.rs` incrementally (Tty incremental SGR, `redraw_rows`,
cursor-after-prompt). These were the right ideas but insufficient: the architecture lacks
**scene caching** and **span-based incremental drawing**, which are the core of tmux's
renderer.

The correct approach: **reimplement tmux's rendering pipeline verbatim**:

```
tmux file                →  loom file
──────────────────────────────────────────────────
screen-redraw.c          →  scene.rs + redraw.rs
tty.c                    →  tty.rs
tty-draw.c               →  tty_draw.rs
```

## Architecture (New)

```
InputCtx → Screen.grid → redraw_build_cells() → redraw_make_scene() → RedrawScene (cached)
                                                                          │
                                                                     redraw_draw()
                                                                          │
                                                                     Tty (persistent per client)
                                                                          │
                                                                     ScreenUpdate { data: Vec<u8> }
                                                                          │
                                                                     Client → stdout
```

### Key Differences from Current

| Aspect | Current (broken) | New (tmux design) |
|--------|-----------------|-------------------|
| Redraw scope | Full window (15494 cells) every time | **Scene cached**, only rebuilt on structural change |
| Per-key redraw | Full 146KB ANSI | **Incremental** via `tty_draw_line()` — single line only |
| Tty persistence | Created/destroyed per redraw | **Persistent** per client — tracks last cursor/SGR state |
| Cell drawing | Every cell gets full SGR | **tty_attributes()** compares with `last_cell`, only outputs changes |
| Borders | Not drawn | Build into scene: `REDRAW_SPAN_BORDER` |

## Scene Model

```rust
// ── Phase 1: Build cells ──
//
// For every visible cell (sx × sy), determine its type:
//
//   REDRAW_SPAN_PANE       — inside a pane, draw pane content
//   REDRAW_SPAN_BORDER     — pane border (top/bottom/left/right/connection)
//   REDRAW_SPAN_EMPTY      — inside window, no pane (gap area)
//   REDRAW_SPAN_OUTSIDE    — outside window (terminal wider than window)
//   REDRAW_SPAN_STATUS     — pane status line
//   REDRAW_SPAN_SCROLLBAR  — pane scrollbar
//
// Panes are iterated in reverse z-order (floating first).

// ── Phase 2: Make scene ──
//
// Adjacent cells of the same type with compatible data are merged into spans.
// Each span: { x, width, type, union { pane_ref, border_info, ... } }
//
// Scene is cached per-client and invalidated when:
//   - `window.redraw_scene_generation` counter changes
//   - Window offset/size changes
//   - Window changes

// ── Phase 3: Draw ──
//
// For each visible line, iterate span types in order:
//   1. REDRAW_SPAN_PANE      → tty_draw_line(screen, px, py, nx, atx, aty)
//   2. REDRAW_SPAN_BORDER    → tty_cursor + tty_cell per cell
//   3. REDRAW_SPAN_STATUS    → tty_draw_line()
//   4. REDRAW_SPAN_SCROLLBAR → tty_cell()
//   5. REDRAW_SPAN_EMPTY     → tty_cursor + spaces
//
// tty_draw_line() uses a state machine to skip identical adjacent cells
// and only output changes.
```

## Tty Model

```rust
pub struct Tty {
    // Cached terminal state (only output when it changes)
    pub cx: i32,                 // -1 = unknown
    pub cy: i32,
    last_cell: GridCell,         // last-rendered attributes for diff
    pub out: Vec<u8>,            // output buffer

    // Output methods (from tmux's tty.c):
    //   tty_cursor(x, y)        — only emit CUP/CUF/CUB etc. if position changed
    //   tty_cell(gc, style)     — cursor + attributes + character
    //   tty_attributes(gc)      — only emit SGR if fg/bg/attr changed vs last_cell
    //   tty_draw_line(...)      — row-based state machine with cell diff
}
```

Key invariant: **`Tty` is persistent across redraws**. It tracks what was last sent
to the terminal, so it can skip redundant sequences.

## Implementation Plan

### Phase 1: Tty Persistence + `tty_draw_line` (2–3 days)

| File | Lines | What |
|------|-------|------|
| `loom-tty/src/tty.rs` | 250 | Rewrite: persistent state, `tty_cursor()`, `tty_attributes()`, `tty_cell()` |
| `loom-tty/src/tty_draw.rs` | 180 | `tty_draw_line()` with state machine (FIRST/NEW1/NEW2/SAME/EMPTY/FLUSH/DONE) |
| `loom-server/src/server.rs` | 20 | Store `Tty<Vec<u8>>` in `ClientState` (persistent per client) |
| `loom-server/src/redraw.rs` | 50 | Strip down to just draw pane content through ClientState's Tty |

**Deliverable:** Keystroke echo renders incrementally (~50 bytes per key), no full redraw.

### Phase 2: Scene Cache (2 days)

| File | Lines | What |
|------|-------|------|
| `loom-server/src/scene.rs` | 200 | `RedrawBuildCell`, `RedrawSpan`, `RedrawScene` structs |
| `loom-server/src/redraw.rs` | 150 | `redraw_build_cells()`, `redraw_make_scene()`, `redraw_get_scene()` |

**Deliverable:** Full rendering through cached scene. Border cells marked but not yet styled.

### Phase 3: Border + Scrollbar Drawing (2 days)

| File | Lines | What |
|------|-------|------|
| `loom-server/src/draw.rs` | 250 | `redraw_draw()`, `draw_border_span()`, `draw_scrollbar_span()` |
| `loom-tty/src/terminfo.rs` | 20 | Border character lookup (ACS) |

**Deliverable:** Full tmux-compatible client scene with borders, scrollbar.

### Phase 4: Scene Invalidation + Incremental Redraw (1 day)

| File | Lines | What |
|------|-------|------|
| `loom-server/src/server.rs` | 30 | Invalidation hooks in layout/resize functions |
| `loom-server/src/redraw.rs` | 20 | Generation counter management |

**Deliverable:** Full golden test suite (10+ scenes) passing.

## Current Code That Remains

| Module | File | Reason |
|--------|------|--------|
| Grid | `loom-core/src/grid_cell.rs` | Correct, tmux-aligned data model |
| Screen | `loom-core/src/screen.rs` | Virtual terminal abstraction |
| InputCtx | `loom-input/src/input.rs` | VT100 parser — basic sequences work |
| Session/Window/Pane | `loom-core/src/session.rs` | Session model |
| IPC | `loom-ipc/src/*` | Peer/Message framing |
| Commands | `loom-commands/src/*` | Command trait + dispatch |

## Code That Gets Replaced or Significantly Rewritten

| Current File | Lines | Action |
|-------------|-------|--------|
| `loom-server/src/redraw.rs` | 80 | Replace with scene-based pipeline |
| `loom-tty/src/tty.rs` | 200 | Rewrite with persistent state |

## Code That Gets Removed

None — the rendering code is being rewritten in place, not deleted and recreated.

## KPI: Success Criteria

1. `cargo run --release -- new-session` shows correct prompt, cursor at input position
2. Keystroke echo is immediate (not full-screen flash)
3. `ls -la` output scrolls properly without overwriting prompt
4. Window resize works correctly
5. All golden tests pass
6. No panic on any PTY input (Powerlevel10k, etc.)
