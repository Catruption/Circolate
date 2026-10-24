//! Spiral playbar geometry.
//!
//! The song is a tape measure lying along the bottom row of the tracker
//! (measure 0 at the far left).  Every measure is exactly ONE cell: the tape
//! holds measures `0..coil_start` one per column, and the coil holds the rest.
//! As the screen narrows the tape has fewer columns, so more measures curl
//! into the coil; as it widens the coil unravels back onto the tape.  The
//! song's END is the centre of the coil.
//!
//! The coil is an inward (counter-clockwise) spiral whose outer end is its
//! bottom-right corner, where the tape joins and runs left along the bottom
//! row.  Valid coil sizes are the sizes at which the outward walk from the
//! centre finishes a downward leg (`coil_sizes`); 0 means no coil at all.
//!
//! `tape_span` / `tape_cell_measures` are shared by the renderer and the
//! click handler so they can't drift apart.

/// Default maximum spiral grid height, in terminal rows.
pub const SPIRAL_MAX_H: usize = 15;

/// Default horizontal stretch of the spiral, in grid cells per vertical cell.
pub const SPIRAL_ASPECT_X: usize = 2;

/// First `n` cells of an outward spiral starting at (0, 0), proceeding
/// right, down, left, up.
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

#[derive(Clone)]
pub struct SpiralLayout {
    /// Measures per coil cell.  Always 1.
    pub measures_per_cell: usize,
    /// Tight size of the coil.  Its bottom row is the tape's row, and its
    /// bottom-right cell (`path[0]`) is where the tape joins.  A song with no
    /// coil gets a 1x1 placeholder grid whose single cell is empty.
    pub grid_w: u16,
    pub grid_h: u16,
    /// Measure index per coil cell, row-major (`grid_w` wide).
    pub map: Vec<Option<usize>>,
    /// Number of cells in the coil (0 = the whole song is on the tape).
    pub coil_cells: usize,
    /// First measure that lives in the coil.  Measures `0..coil_start` are on
    /// the tape, one per column.
    pub coil_start: usize,
    /// Coil cells in playback order: outer end first, centre last.
    pub path: Vec<(u16, u16)>,
    /// Screen column where the tape starts (set by the renderer each frame so
    /// the click handler can find the tape).
    pub tape_left: u16,
}

impl SpiralLayout {
    /// The centre of the coil is the last cell: the end of the song.
    pub fn is_center(&self, gx: usize, gy: usize) -> bool {
        self.path
        .last()
        .map_or(false, |&(cx, cy)| cx as usize == gx && cy as usize == gy)
    }

    /// Columns the tape can use when the coil's rect starts at `rect_x`:
    /// everything from `tape_left` up to the coil's bottom-right corner.  With
    /// no coil the corner column is free too.
    pub fn tape_room(&self, rect_x: u16) -> usize {
        let corner_x = rect_x.saturating_add(self.grid_w.saturating_sub(1));
        corner_x.saturating_sub(self.tape_left) as usize + usize::from(self.coil_cells == 0)
    }

    /// True when every tape measure gets its own column.
    pub fn tape_fits(&self, rect_x: u16) -> bool {
        self.coil_start <= self.tape_room(rect_x)
    }

    /// Where the tape sits on screen when the coil's rect starts at
    /// (`rect_x`, `rect_y`): `(first column, number of columns, row)`.
    /// One column per measure.  With a coil the tape is right-aligned so it
    /// always touches the coil's corner (any slack is on the left); with no
    /// coil it starts at `tape_left`.  `None` when there is no tape.
    pub fn tape_span(&self, rect_x: u16, rect_y: u16) -> Option<(u16, usize, u16)> {
        if self.coil_start == 0 {
            return None;
        }
        let room = self.tape_room(rect_x);
        let cols = self.coil_start.min(room);
        if cols == 0 {
            return None;
        }
        let row = rect_y.saturating_add(self.grid_h.saturating_sub(1));
        let x0 = if self.coil_cells == 0 {
            self.tape_left
        } else {
            let corner_x = rect_x.saturating_add(self.grid_w.saturating_sub(1));
            corner_x - cols as u16
        };
        Some((x0, cols, row))
    }

    /// Measures covered by tape column `j` of `cols`: `(first, count)`.
    /// Normally exactly measure `j`.  Only when the tape can't hold every
    /// measure (biggest coil still too small) are several measures grouped.
    pub fn tape_cell_measures(&self, cols: usize, j: usize) -> (usize, usize) {
        if cols >= self.coil_start {
            return (j, 1);
        }
        let m0 = j * self.coil_start / cols;
        let m1 = ((j + 1) * self.coil_start / cols).max(m0 + 1);
        (m0, m1 - m0)
    }
}

/// Length of leg `i` of the outward walk from the centre: horizontal legs
/// (even `i`) are stretched by `aspect_x`, vertical legs are 1, 1, 2, 2, ...
fn leg_len(i: usize, aspect_x: usize) -> usize {
    let base = i / 2 + 1;
    if i % 2 == 0 {
        (base * aspect_x * 3 + 3) / 4
    } else {
        base
    }
}

/// Valid coil sizes in ascending order: 0 (no coil), then every size at which
/// the walk has just finished a downward leg, up to `total_measures`.
pub fn coil_sizes(total_measures: usize, aspect_x: usize) -> Vec<usize> {
    let aspect_x = aspect_x.max(1);
    let mut sizes = vec![0];
    let (mut n, mut leg) = (1usize, 0usize);
    while n <= total_measures {
        n += leg_len(leg, aspect_x);
        leg += 1;
        if leg % 2 == 0 && n <= total_measures {
            sizes.push(n);
        }
    }
    sizes
}

/// Build the layout for a coil of `coil_cells` cells (a value from
/// `coil_sizes`; anything else is rounded up to the next valid size).
pub fn build_coil(total_measures: usize, coil_cells: usize, aspect_x: usize) -> SpiralLayout {
    let total = total_measures.max(1);
    let aspect_x = aspect_x.max(1);

    if coil_cells == 0 {
        return SpiralLayout {
            measures_per_cell: 1,
            grid_w: 1,
            grid_h: 1,
            map: vec![None],
            coil_cells: 0,
            coil_start: total,
            path: Vec::new(),
            tape_left: 0,
        };
    }

    // Number of legs: an even count (the walk always ends on a down leg).
    let (mut n, mut legs) = (1usize, 0usize);
    loop {
        n += leg_len(legs, aspect_x);
        legs += 1;
        if legs % 2 == 0 && n >= coil_cells {
            break;
        }
    }

    // Outward walk from the centre, clockwise: right, down, left, up.  The
    // first direction is chosen so the LAST leg is a downward one, which
    // leaves the outer end in the bottom-right corner of the bounding box.
    const DIRS: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];
    let first = if legs % 4 == 0 { 2 } else { 0 };
    let mut cells: Vec<(i32, i32)> = vec![(0, 0)];
    let (mut x, mut y) = (0i32, 0i32);
    for leg in 0..legs {
        let d = DIRS[(first + leg) % 4];
        for _ in 0..leg_len(leg, aspect_x) {
            x += d.0;
            y += d.1;
            cells.push((x, y));
        }
    }

    let min_x = cells.iter().map(|c| c.0).min().unwrap_or(0);
    let max_x = cells.iter().map(|c| c.0).max().unwrap_or(0);
    let min_y = cells.iter().map(|c| c.1).min().unwrap_or(0);
    let max_y = cells.iter().map(|c| c.1).max().unwrap_or(0);
    let gw = (max_x - min_x + 1) as usize;
    let gh = (max_y - min_y + 1) as usize;

    // Path runs outer end -> centre.
    let path: Vec<(u16, u16)> = cells
    .iter()
    .rev()
    .map(|&(cx, cy)| ((cx - min_x) as u16, (cy - min_y) as u16))
    .collect();

    let coil_start = total.saturating_sub(n);
    let mut map: Vec<Option<usize>> = vec![None; gw * gh];
    for (i, &(gx, gy)) in path.iter().enumerate() {
        map[gy as usize * gw + gx as usize] = Some(coil_start + i);
    }

    SpiralLayout {
        measures_per_cell: 1,
        grid_w: gw as u16,
        grid_h: gh as u16,
        map,
        coil_cells: n,
        coil_start,
        path,
        tape_left: 0,
    }
}

/// Pure version of the choice `views/mod.rs` makes: the smallest coil (within
/// the `max_w` x `max_h` budget) whose tape fits in `tape_cols` columns, or
/// the biggest coil that fits the budget if none does.
#[allow(dead_code)]
pub fn build_tape_measure(
    total_measures: usize,
    max_w: usize,
    max_h: usize,
    aspect_x: usize,
    tape_cols: usize,
) -> SpiralLayout {
    let total = total_measures.max(1);
    let (max_w, max_h) = (max_w.max(1), max_h.max(1));
    let mut best = build_coil(total, 0, aspect_x);
    for n in coil_sizes(total, aspect_x) {
        let l = build_coil(total, n, aspect_x);
        if l.grid_w as usize > max_w || l.grid_h as usize > max_h {
            break;
        }
        let fits = l.coil_start <= tape_cols + usize::from(l.coil_cells == 0);
        best = l;
        if fits {
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coil_is_contiguous_and_ends_bottom_right() {
        for total in [1usize, 2, 5, 9, 20, 28, 30, 60, 400, 5000] {
            for aspect in [1usize, 2, 3] {
                for tape_cols in [0usize, 5, 20, 1000] {
                    let l = build_tape_measure(total, 40, 12, aspect, tape_cols);
                    assert!(l.grid_w as usize <= 40 && l.grid_h as usize <= 12);
                    assert_eq!(l.coil_start + l.coil_cells, total);
                    assert_eq!(l.path.len(), l.coil_cells);
                    assert_eq!(l.map.iter().flatten().count(), l.coil_cells);
                    for w in l.path.windows(2) {
                        let d = (w[0].0 as i32 - w[1].0 as i32).abs()
                        + (w[0].1 as i32 - w[1].1 as i32).abs();
                        assert_eq!(d, 1);
                    }
                    if let Some(&p0) = l.path.first() {
                        assert_eq!(p0, (l.grid_w - 1, l.grid_h - 1));
                    }
                }
            }
        }
    }

    #[test]
    fn coil_is_the_smallest_that_lets_the_tape_fit() {
        let l = build_tape_measure(30, 100, 30, 2, 26);
        assert_eq!((l.coil_cells, l.coil_start), (4, 26));
        let l = build_tape_measure(30, 100, 30, 2, 21);
        assert_eq!((l.coil_cells, l.coil_start), (9, 21));
        let l = build_tape_measure(30, 100, 30, 2, 40);
        assert_eq!((l.coil_cells, l.coil_start), (0, 30));
    }

    #[test]
    fn tape_touches_the_coil_and_is_one_measure_per_column() {
        let mut l = build_tape_measure(30, 100, 30, 2, 21);
        l.tape_left = 0;
        let rect_x = 40;
        let (x0, cols, _) = l.tape_span(rect_x, 5).unwrap();
        assert_eq!(cols, l.coil_start);
        // Last tape column is directly left of the coil's corner column.
        assert_eq!(x0 as usize + cols, rect_x as usize + l.grid_w as usize - 1);
        for j in 0..cols {
            assert_eq!(l.tape_cell_measures(cols, j), (j, 1));
        }
    }

    #[test]
    fn grouped_tape_covers_every_measure() {
        for (cs, cols) in [(40usize, 40usize), (373, 38), (7, 1), (5, 5)] {
            let mut l = build_tape_measure(cs + 30, 20, 6, 2, 20);
            l.coil_start = cs;
            let mut seen = vec![false; cs];
            for j in 0..cols {
                let (m0, n) = l.tape_cell_measures(cols, j);
                for m in m0..m0 + n {
                    seen[m] = true;
                }
            }
            assert!(seen.iter().all(|&s| s));
        }
    }
}
