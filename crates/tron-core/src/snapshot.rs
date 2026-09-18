//! A copy of the state the renderer needs, taken under the terminal lock.
//!
//! Building a frame (shaping, rasterizing) is slow compared to copying the few
//! rows that changed. Rendering from a snapshot lets the pty reader keep
//! parsing while the frame is built.

use foldhash::HashMap;

use crate::cell::Extended;
use crate::graphics::{Image, Placement};
use crate::grid::Row;
use crate::palette::Palette;
use crate::selection::SelectionRange;
use crate::term::{CursorState, Modes};

#[derive(Default)]
pub struct Snapshot {
    /// Rows as seen through the viewport. Only damaged rows are refreshed.
    pub rows: Vec<Row>,
    /// Rows that changed since the previous snapshot.
    pub damaged: Vec<bool>,
    pub cols: usize,
    /// Absolute line number of the top viewport row.
    pub top_line: i64,
    pub display_offset: usize,
    /// Asks for the row above the viewport in the next snapshot. Smooth scrolling
    /// sets it while the viewport sits between two lines.
    pub want_overscan: bool,
    /// Row above the top viewport row, copied when `want_overscan` is set and
    /// history holds one.
    pub overscan: Option<Row>,
    pub cursor: Option<CursorState>,
    pub modes: Option<Modes>,
    pub alt_screen: bool,
    pub palette: Palette,
    pub palette_generation: u64,
    /// Copy of the terminal's [`crate::ExtendedTable`] entries.
    pub extended: Vec<Extended>,
    pub selection: Option<SelectionRange>,
    pub graphics_generation: u64,
    pub placements: Vec<Placement>,
    pub images: HashMap<u32, Image>,
    /// When the next animation frame is due, if an image is animating.
    pub next_frame_due: Option<std::time::Instant>,
}

impl Snapshot {
    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    /// Absolute line number of viewport row `row`.
    pub fn line(&self, row: usize) -> i64 {
        self.top_line + row as i64
    }

    /// Absolute line number of the overscan row.
    pub fn overscan_line(&self) -> i64 {
        self.top_line - 1
    }

    /// Extended attributes of a cell.
    #[inline]
    pub fn extended(&self, id: u16) -> &Extended {
        self.extended.get(usize::from(id)).unwrap_or(&Extended::DEFAULT)
    }

    pub fn cursor(&self) -> CursorState {
        self.cursor.unwrap_or(CursorState {
            row: 0,
            col: 0,
            visible: false,
            shape: crate::term::CursorShape::Block,
            blinking: false,
        })
    }

    pub fn modes(&self) -> Modes {
        self.modes.unwrap_or(Modes::empty())
    }
}
