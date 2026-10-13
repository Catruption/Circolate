//! Cover image resampled to one dimmed RGB color per terminal cell.

use image::GenericImageView;
use ratatui::style::Color;
use std::path::Path;

/// Brightness multiplier applied to the cover image.  0.5 = "half
/// transparency" — the image is visible but text on top stays readable.
/// Raise toward 1.0 for a brighter image behind everything.
pub const COVER_DIM: f32 = 0.4;

/// A cover image resampled to one dimmed RGB color per terminal cell.
/// Sampled at the full terminal size, painted as a background pass over
/// every cell that doesn't carry its own explicit background style.
pub struct Cover {
    cells: Vec<Color>,
    cols: usize,
    rows: usize,
}

impl Cover {
    pub fn load(path: &Path, cols: usize, rows: usize, dim: f32) -> Option<Self> {
        if cols == 0 || rows == 0 {
            return None;
        }
        let img = image::open(path).ok()?;
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            return None;
        }
        let rgb = img.to_rgb8();

        // Terminal cells are roughly twice as tall as they are wide, so
        // the cell grid's pixel aspect is cols : rows*2.  Cover-fit: crop
        // the image to that aspect, then sample one pixel per cell.
        let grid_aspect = cols as f32 / (rows as f32 * 2.0);
        let img_aspect = w as f32 / h as f32;
        let (src_x, src_y, src_w, src_h) = if img_aspect > grid_aspect {
            let new_w = (h as f32 * grid_aspect).round().max(1.0) as u32;
            ((w - new_w) / 2, 0, new_w, h)
        } else {
            let new_h = (w as f32 / grid_aspect).round().max(1.0) as u32;
            (0, (h - new_h) / 2, w, new_h)
        };

        let mut cells = Vec::with_capacity(cols * rows);
        for cy in 0..rows {
            for cx in 0..cols {
                let sx = src_x + ((cx as f32 + 0.5) / cols as f32 * src_w as f32) as u32;
                let sy = src_y + ((cy as f32 + 0.5) / rows as f32 * src_h as f32) as u32;
                let sx = sx.min(rgb.width().saturating_sub(1));
                let sy = sy.min(rgb.height().saturating_sub(1));
                let p = rgb.get_pixel(sx, sy);
                let r = (p[0] as f32 * dim).round().clamp(0.0, 255.0) as u8;
                let g = (p[1] as f32 * dim).round().clamp(0.0, 255.0) as u8;
                let b = (p[2] as f32 * dim).round().clamp(0.0, 255.0) as u8;
                cells.push(Color::Rgb(r, g, b));
            }
        }
        Some(Cover { cells, cols, rows })
    }

    pub fn get(&self, row: usize, col: usize) -> Option<Color> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        Some(self.cells[row * self.cols + col])
    }
}
