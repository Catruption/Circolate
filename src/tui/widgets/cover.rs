//! Cover image as a background pass.  `Cover::load` reads the image,
//! cover-fits it to the terminal cell grid, and resamples one dimmed
//! RGB color per cell.  `paint_background` then writes those colors
//! into every buffer cell that doesn't already carry its own explicit
//! background style, so text stays on top of a dimmed image.

use image::GenericImageView;
use ratatui::{buffer::Buffer, style::Color};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};

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

        // Read the file and let the image crate sniff the format from
        // its magic bytes, instead of relying on the file extension.
        // `image::open` picks a decoder from the path suffix; if the
        // path lost its extension somewhere (cache round-trip, manual
        // rename), the decoder lookup fails silently.
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("cover: read {} failed: {:#}", path.display(), e);
                return None;
            }
        };

        let img = match image::load_from_memory(&bytes) {
            Ok(i) => i,
            Err(e) => {
                eprintln!(
                    "cover: decode {} failed: {:#} ({})",
                          path.display(),
                          e,
                          guess_format(&bytes)
                );
                return None;
            }
        };

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

/// Pick a random image file from `dir`, for songs that ship no cover of
/// their own.  Files are recognised by their magic bytes, not their
/// names, so any filename (or none of an extension) works; anything
/// that isn't a png/jpeg/webp/gif is skipped, as are subdirectories.
/// Returns `None` if the directory is missing or holds no images.
///
/// Call this once per song and keep the result: it is random each call,
/// so re-picking on every resize or redraw would swap the image.
pub fn random_image_in(dir: &Path) -> Option<PathBuf> {
    let mut images: Vec<PathBuf> = std::fs::read_dir(dir)
    .ok()?
    .filter_map(|e| e.ok())
    .map(|e| e.path())
    .filter(|p| p.is_file() && looks_like_image(p))
    .collect();
    if images.is_empty() {
        return None;
    }
    // Sort so the choice depends only on the random number, not on
    // directory iteration order.
    images.sort();
    // std-only randomness: RandomState is seeded randomly per instance.
    let r = RandomState::new().build_hasher().finish() as usize;
    Some(images.swap_remove(r % images.len()))
}

fn looks_like_image(path: &Path) -> bool {
    let mut head = [0u8; 16];
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let Ok(n) = f.read(&mut head) else { return false };
    guess_format(&head[..n]) != "unknown"
}

/// Paint the cover as a background over every cell that doesn't carry
/// its own explicit background style.  `Color::Reset` is the "unset"
/// marker ratatui uses; anything else was set by a widget and stays.
pub fn paint_background(buf: &mut Buffer, cover: Option<&Cover>) {
    let Some(cover) = cover else { return };
    let w = buf.area.width;
    let h = buf.area.height;
    for y in 0..h {
        for x in 0..w {
            let Some(cc) = cover.get(y as usize, x as usize) else { continue };
            if let Some(cell) = buf.cell_mut((x, y)) {
                if cell.bg == Color::Reset {
                    cell.bg = cc;
                }
            }
        }
    }
}

/// Best-effort format tag from the magic bytes, used only for the error
/// message above so a failing cover says *what* it failed on.
fn guess_format(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { "png" }
    else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) { "jpeg" }
    else if bytes.starts_with(b"RIFF") && bytes.len() > 12 && &bytes[8..12] == b"WEBP" { "webp" }
    else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { "gif" }
    else { "unknown" }
}
