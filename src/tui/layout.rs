//! Frame-level layout: computes every rect the frame will draw into.
//! Widgets receive their rects as arguments and never decide their own
//! position or size.
//!
//! Naming note: the "notch" is the small `[? ⇅]` strip at the tracker's
//! bottom-left.  The bottom-right box is the "queue box".  These are
//! unrelated and were previously both called "notch" in some places,
//! which was a bug magnet.

use ratatui::layout::Rect;

use crate::tui::state::VISIBLE_LINES;

/// Minimum content height of the queue box: one transport row plus
/// `VISIBLE_LINES` buffer rows.  The box grows taller than this if the
/// spiral needs more room; it never shrinks below it.
pub const QUEUE_MIN_CONTENT_H: u16 = 1 + VISIBLE_LINES as u16;

/// One row of the queue box's border, described as coordinates so the
/// renderer knows where to place corners and how far the frame's left
/// wall runs before the tape row takes over.
#[derive(Clone)]
pub struct BorderRows {
    pub top_y: u16,
    pub left_x: u16,
    pub right_x: u16,
    pub frame_bottom_y: u16,
}

/// Where the queue box goes and how its interior is divided.  All rects
/// are absolute (terminal coordinates).
#[derive(Clone)]
pub struct QueueLayout {
    /// The whole box, including borders.  Used for click exclusion and
    /// for the notch strip's `max_x` bound.
    pub box_rect: Rect,
    /// Interior of the buffer column.  Used to blank the cover mosaic
    /// out from behind the queue's text.
    pub buffer: Rect,
    /// Where the spiral is drawn.  Bottom-aligned inside the box.
    pub spiral: Rect,
    /// Click target for `◀`.
    pub rewind: Rect,
    /// Click target for `▶` and the speed label.
    pub play: Rect,
    /// Click target for the `[n]song` rows.
    pub lines: Rect,
    pub border: BorderRows,
}

/// Compute the queue box layout given a spiral size.  Returns `None` if
/// the box can't fit in `tracker` (too narrow, too short, or spiral
/// taller than the tracker allows).
pub fn compute_queue(
    tracker: Rect,
    tape_rows: u16,
    spiral: (u16, u16),
                     buf_w: u16,
                     speed_label_w: u16,
) -> Option<QueueLayout> {
    let sw = spiral.0.max(1);
    let sh = spiral.1.max(1);
    let content_h = QUEUE_MIN_CONTENT_H;
    let notch_content_h = sh.max(content_h);
    let notch_h = notch_content_h + 1;
    let nw = 1 + sw + buf_w;
    if tracker.width < nw + 2 || tracker.height < notch_h + 3 {
        return None;
    }

    let right = tracker.right() - 1;
    let bottom = tracker.bottom() - 1;
    let frame_bottom = bottom - tape_rows;
    let nx = right + 1 - nw;
    let ny = tracker.bottom() - notch_h;
    let sx = nx + 1;
    let bx = sx + sw;

    // Buffer content is bottom-anchored to the box's interior.
    let ty = tracker.bottom() - content_h;
    let lines_y = ty + 1;

    // Transport buttons: right-aligned inside the buffer's horizontal span.
    let tx = right - 3 - speed_label_w;

    Some(QueueLayout {
        box_rect: Rect::new(nx, ny, nw, notch_h),
         buffer: Rect::new(bx, ny + 1, buf_w, notch_h - 1),
         spiral: Rect::new(sx, tracker.bottom() - sh, sw, sh),
         rewind: Rect::new(tx, ty, 2, 1),
         play: Rect::new(tx + 2, ty, 1 + speed_label_w, 1),
         lines: Rect::new(bx, lines_y, buf_w, VISIBLE_LINES as u16),
         border: BorderRows {
             top_y: ny,
             left_x: nx,
             right_x: right,
             frame_bottom_y: frame_bottom,
         },
    })
}
