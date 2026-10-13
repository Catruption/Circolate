//! Square-spiral playbar geometry.  One cell per measure (bundled when
//! the song would otherwise overflow the available footer height),
//! walking outward from the centre right, down, left, up.

use ratatui::layout::Rect;

/// Maximum spiral grid height, in terminal rows.  Past this we bundle
/// multiple measures into a single spiral cell so the footer doesn't
/// swallow the tracker.
pub const SPIRAL_MAX_H: usize = 15;

/// Horizontal stretch of the spiral, in grid cells per vertical cell.
/// 1 = square spiral (looks tall on a terminal because cells are ~2:1
/// tall).  2 ≈ visually square.  Higher = wider than tall.  Bump this
/// up if the spiral is still eating too many rows.
pub const SPIRAL_ASPECT_X: usize = 2;

/// First `n` cells of an outward spiral starting at (0, 0), proceeding
/// right, down, left, up.  Horizontal legs are `aspect_x` times longer
/// than vertical legs, so the bounding box is wider than it is tall.
pub fn spiral_path(n: usize, aspect_x: usize) -> Vec<(i32, i32)> {
    let mut cells = Vec::with_capacity(n);
    if n == 0 {
        return cells;
    }
    let aspect_x = aspect_x.max(1) as i32;
    cells.push((0, 0));
    let dirs = [(1i32, 0i32), (0, 1), (-1, 0), (0, -1)];
    let mut dir = 0usize;
    let mut leg_index = 0usize;
    let (mut x, mut y) = (0i32, 0i32);
    while cells.len() < n {
        // Leg lengths: 1, 1, 2, 2, 3, 3, ... with the horizontal legs
        // (even indices) multiplied by `aspect_x`.
        let base = (leg_index / 2 + 1) as i32;
        let len = if dir % 2 == 0 { base * aspect_x } else { base };
        for _ in 0..len {
            x += dirs[dir].0;
            y += dirs[dir].1;
            cells.push((x, y));
            if cells.len() >= n {
                return cells;
            }
        }
        dir = (dir + 1) % 4;
        leg_index += 1;
    }
    cells
}

/// Filled-in spiral layout for the current song.  Recomputed each frame
/// — cheap, and it tracks song length changes without cache invalidation
/// bookkeeping.
#[derive(Clone)]
pub struct SpiralLayout {
    /// Measures per cell.  Grows above 1 only when the song can't fit
    /// at full resolution within `SPIRAL_MAX_H` rows.
    pub measures_per_cell: usize,
    /// Bounding box, in spiral coordinates.
    pub min_x: i32,
    pub min_y: i32,
    pub grid_w: u16,
    pub grid_h: u16,
    /// Measure index per grid cell, row-major within the bounding box.
    /// `None` for cells not on the spiral path.
    pub map: Vec<Option<usize>>,
}

impl SpiralLayout {
    /// True if `(gx, gy)` is the innermost cell of the spiral — the
    /// measure-0 cell the path starts from.  The renderer consults this
    /// to add a phantom "down" edge, turning what would be a bare
    /// horizontal segment into a `╔` corner so the centre reads as a
    /// closed curl instead of an open line.
    ///
    /// The check is against measure index, not path position, so it
    /// still fires when `measures_per_cell > 1` — measure 0 is always
    /// the innermost cell.
    pub fn is_center(&self, gx: usize, gy: usize) -> bool {
        self.map
            .get(gy * self.grid_w as usize + gx)
            .copied()
            .flatten()
            .map_or(false, |m| m == 0)
    }
}

pub fn build_spiral(total_measures: usize, max_grid_h: usize) -> SpiralLayout {
    let total = total_measures.max(1);
    let mut mpcell = 1usize;
    let (path, min_x, min_y, grid_w, grid_h) = loop {
        let cells = (total + mpcell - 1) / mpcell;
        let path = spiral_path(cells, SPIRAL_ASPECT_X);

        let (mut min_x, mut max_x) = (i32::MAX, i32::MIN);
        let (mut min_y, mut max_y) = (i32::MAX, i32::MIN);
        for &(x, y) in &path {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        let grid_w = (max_x - min_x + 1) as u16;
        let grid_h = (max_y - min_y + 1) as u16;

        if (grid_h as usize) <= max_grid_h || mpcell >= total {
            break (path, min_x, min_y, grid_w, grid_h);
        }
        mpcell *= 2;
    };

    let mut map = vec![None; grid_w as usize * grid_h as usize];
    for (i, &(x, y)) in path.iter().enumerate() {
        let measure = i * mpcell;
        if measure < total_measures {
            let gx = (x - min_x) as usize;
            let gy = (y - min_y) as usize;
            map[gy * grid_w as usize + gx] = Some(measure);
        }
    }

    SpiralLayout { measures_per_cell: mpcell, min_x, min_y, grid_w, grid_h, map }
}

pub struct SpiralState {
    pub layout: SpiralLayout,
    /// Screen rect occupied by the spiral grid, filled in during draw
    /// and consumed by the mouse handler.
    pub rect: Rect,
}