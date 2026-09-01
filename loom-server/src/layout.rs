use loom_core::session::{LayoutCell, LayoutCellIdx, LayoutType, PaneId, Window, WINDOW_ZOOMED};

/// Split a pane horizontally (left/right) or vertically (top/bottom).
pub fn layout_split_pane(
    window: &mut Window,
    pane_id: PaneId,
    vertical: bool,
) -> Option<PaneId> {
    let pane_cell_idx = *window.panes.get(&pane_id)?.layout_cell.as_ref()?;

    // Extract needed info from pane_cell before any mutations
    let (pane_sx, pane_sy, pane_xoff, pane_yoff, parent_idx) = {
        let pc = window.cells.get(pane_cell_idx)?;
        (pc.sx, pc.sy, pc.xoff, pc.yoff, pc.parent)
    };

    let cell_type = if vertical { LayoutType::TopBottom } else { LayoutType::LeftRight };

    // Create new pane
    let new_pane_id = window.create_pane(window.sx, window.sy);
    let new_cell_idx = window.cells.len() - 1; // create_pane pushed one

    // Set new cell properties
    if let Some(cell) = window.cells.get_mut(new_cell_idx) {
        cell.sx = pane_sx;
        cell.sy = pane_sy;
        cell.xoff = pane_xoff;
        cell.yoff = pane_yoff;
        cell.parent = None;
        cell.pane_id = Some(new_pane_id);
    }
    if let Some(p) = window.panes.get_mut(&new_pane_id) {
        p.layout_cell = Some(new_cell_idx);
    }

    let new_root_idx = window.cells.len(); // after push of new parent

    if parent_idx.is_none() && vertical {
        let half = pane_sy / 2;
        if let Some(cell) = window.cells.get_mut(pane_cell_idx) {
            cell.parent = Some(new_root_idx);
            cell.sy = half;
        }
        if let Some(cell) = window.cells.get_mut(new_cell_idx) {
            cell.parent = Some(new_root_idx);
            cell.sy = pane_sy - half;
            cell.yoff = pane_yoff + half as i32;
        }
    } else if parent_idx.is_none() {
        let half = pane_sx / 2;
        if let Some(cell) = window.cells.get_mut(pane_cell_idx) {
            cell.parent = Some(new_root_idx);
            cell.sx = half;
        }
        if let Some(cell) = window.cells.get_mut(new_cell_idx) {
            cell.parent = Some(new_root_idx);
            cell.sx = pane_sx - half;
            cell.xoff = pane_xoff + half as i32;
        }
    } else if let Some(pidx) = parent_idx {
        // Add sibling under existing parent
        let pos = window.cells.get(pidx)
            .and_then(|c| c.children.iter().position(|&ci| ci == pane_cell_idx))
            .unwrap_or(0);

        if vertical {
            let half = pane_sy / 2;
            if let Some(cell) = window.cells.get_mut(pane_cell_idx) {
                cell.sy = half;
            }
            if let Some(cell) = window.cells.get_mut(new_cell_idx) {
                cell.parent = Some(pidx);
                cell.sy = pane_sy - half;
                cell.yoff = pane_yoff + half as i32;
                cell.sx = pane_sx;
                cell.xoff = pane_xoff;
            }
            if let Some(pc) = window.cells.get_mut(pidx) {
                pc.children.insert(pos + 1, new_cell_idx);
            }
        } else {
            let half = pane_sx / 2;
            if let Some(cell) = window.cells.get_mut(pane_cell_idx) {
                cell.sx = half;
            }
            if let Some(cell) = window.cells.get_mut(new_cell_idx) {
                cell.parent = Some(pidx);
                cell.sx = pane_sx - half;
                cell.xoff = pane_xoff + half as i32;
                cell.sy = pane_sy;
                cell.yoff = pane_yoff;
            }
            if let Some(pc) = window.cells.get_mut(pidx) {
                pc.children.insert(pos + 1, new_cell_idx);
            }
        }
        fix_layout_offsets(window);
        fix_layout_panes(window);
        return Some(new_pane_id);
    }

    // Create parent for root case
    let parent = LayoutCell {
        cell_type,
        sx: if vertical { pane_sx } else { pane_sx },
        sy: if vertical { pane_sy } else { pane_sy },
        xoff: pane_xoff,
        yoff: pane_yoff,
        parent: None,
        children: vec![pane_cell_idx, new_cell_idx],
        ..LayoutCell::new_node(LayoutType::WindowPane)
    };
    window.cells.push(parent);
    window.layout_root = Some(new_root_idx);

    fix_layout_offsets(window);
    fix_layout_panes(window);
    Some(new_pane_id)
}

pub fn fix_layout_offsets(window: &mut Window) {
    let root_idx = match window.layout_root {
        Some(idx) => idx,
        None => return,
    };
    // Snapshot cell count to avoid borrow issues
    let len = window.cells.len();
    fix_offsets_rec(&mut window.cells[..], root_idx, 0, 0, len);
}

fn fix_offsets_rec(
    cells: &mut [LayoutCell],
    idx: LayoutCellIdx,
    xoff: i32,
    yoff: i32,
    _len: usize,
) {
    if idx >= cells.len() {
        return;
    }
    let cell_type;
    let children;
    {
        let cell = &cells[idx];
        if cell.is_leaf() {
            return;
        }
        cell_type = cell.cell_type;
        children = cell.children.clone();
    }

    match cell_type {
        LayoutType::LeftRight => {
            let mut cx = xoff;
            for &child in &children {
                if child < cells.len() {
                    cells[child].xoff = cx;
                    cells[child].yoff = yoff;
                    cx += cells[child].sx as i32;
                }
            }
            for &child in &children {
                if child < cells.len() {
                    let (cx, cy) = (cells[child].xoff, cells[child].yoff);
                    fix_offsets_rec(cells, child, cx, cy, _len);
                }
            }
        }
        LayoutType::TopBottom => {
            let mut cy = yoff;
            for &child in &children {
                if child < cells.len() {
                    cells[child].xoff = xoff;
                    cells[child].yoff = cy;
                    cy += cells[child].sy as i32;
                }
            }
            for &child in &children {
                if child < cells.len() {
                    let (cx, cy) = (cells[child].xoff, cells[child].yoff);
                    fix_offsets_rec(cells, child, cx, cy, _len);
                }
            }
        }
        _ => {}
    }
}

pub fn fix_layout_panes(window: &mut Window) {
    for (_, pane) in &mut window.panes {
        if let Some(cell_idx) = pane.layout_cell {
            if let Some(cell) = window.cells.get(cell_idx) {
                pane.sx = cell.sx;
                pane.sy = cell.sy;
                pane.xoff = cell.xoff;
                pane.yoff = cell.yoff;
                pane.screen.resize(cell.sx, cell.sy);
            }
        }
    }
}

/// Resize a window to a new size, redistributing space across the layout tree.
/// (P0-10) The layout cells are recomputed from the root and panes follow.
pub fn layout_resize(window: &mut Window, sx: u32, sy: u32) {
    window.sx = sx;
    window.sy = sy;

    let root = match window.layout_root {
        Some(r) => r,
        None => {
            // No layout tree: put every pane at full size (shouldn't normally happen).
            for pane in window.panes.values_mut() {
                pane.sx = sx;
                pane.sy = sy;
                pane.xoff = 0;
                pane.yoff = 0;
                pane.screen.resize(sx, sy);
            }
            return;
        }
    };

    resize_cell(window, root, 0, 0, sx, sy);
    fix_layout_panes(window);
}

/// Toggle zoom for `pane_id` (tmux `resize-pane -Z`): when zoomed the active
/// pane cell covers the whole window; its previous geometry is saved in the
/// cell's `saved_*` fields and restored on unzoom.
pub fn layout_zoom(window: &mut Window, pane_id: PaneId) -> bool {
    if window.panes.len() < 2 {
        return false;
    }
    let cell_idx = match window.panes.get(&pane_id).and_then(|p| p.layout_cell) {
        Some(i) => i,
        None => return false,
    };
    {
        let cell = &mut window.cells[cell_idx];
        if window.flags & WINDOW_ZOOMED != 0 {
            // Unzoom: restore the saved geometry.
            cell.sx = cell.saved_sx.max(1);
            cell.sy = cell.saved_sy.max(1);
            cell.xoff = cell.saved_xoff;
            cell.yoff = cell.saved_yoff;
            window.flags &= !WINDOW_ZOOMED;
        } else {
            // Zoom: save the current geometry and expand to the full window.
            cell.saved_sx = cell.sx;
            cell.saved_sy = cell.sy;
            cell.saved_xoff = cell.xoff;
            cell.saved_yoff = cell.yoff;
            cell.sx = window.sx;
            cell.sy = window.sy;
            cell.xoff = 0;
            cell.yoff = 0;
            window.flags |= WINDOW_ZOOMED;
        }
    }
    fix_layout_panes(window);
    true
}

/// Recursively assign (x, y, w, h) to a cell and distribute space to its children.
fn resize_cell(window: &mut Window, idx: usize, x: i32, y: i32, w: u32, h: u32) {
    let (cell_type, children) = {
        let cell = &window.cells[idx];
        (cell.cell_type, cell.children.clone())
    };

    // Set this cell's position and size.
    {
        let cell = &mut window.cells[idx];
        cell.xoff = x;
        cell.yoff = y;
        cell.sx = w;
        cell.sy = h;
    }

    // Leaf: nothing to distribute.
    if cell_type == LayoutType::WindowPane || children.is_empty() {
        return;
    }

    match cell_type {
        LayoutType::LeftRight => {
            let total_w: u32 = children
                .iter()
                .filter(|&&ci| ci < window.cells.len())
                .map(|&ci| window.cells[ci].sx)
                .sum();
            let n = children.len().max(1) as u32;
            let mut cx = x;
            for &ci in &children {
                if ci >= window.cells.len() {
                    continue;
                }
                let cw = if total_w > 0 {
                    w.saturating_mul(window.cells[ci].sx).saturating_div(total_w)
                } else {
                    w / n
                };
                resize_cell(window, ci, cx, y, cw, h);
                cx += cw as i32;
            }
        }
        LayoutType::TopBottom => {
            let total_h: u32 = children
                .iter()
                .filter(|&&ci| ci < window.cells.len())
                .map(|&ci| window.cells[ci].sy)
                .sum();
            let n = children.len().max(1) as u32;
            let mut cy = y;
            for &ci in &children {
                if ci >= window.cells.len() {
                    continue;
                }
                let ch = if total_h > 0 {
                    h.saturating_mul(window.cells[ci].sy).saturating_div(total_h)
                } else {
                    h / n
                };
                resize_cell(window, ci, x, cy, w, ch);
                cy += ch as i32;
            }
        }
        _ => {}
    }
}
