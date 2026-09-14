//! Terminal emulation core: escape sequence parser, screen grid, selection,
//! images and terminal state.
//!
//! This crate has no knowledge of windows, fonts, GPUs or processes, so it can
//! be tested and benchmarked headless.

pub mod cell;
pub mod graphics;
pub mod grid;
pub mod palette;
pub mod parser;
pub mod selection;
pub mod sixel;
pub mod snapshot;
pub mod term;
pub mod text;

pub use cell::{Cell, Color, ColorKind, Flags};
pub use graphics::{Graphics, Image, PLACEHOLDER, PlaceholderCell, Placement, placeholder_cell};
pub use grid::{Grid, Row};
pub use palette::Palette;
pub use parser::{Params, Parser, Perform};
pub use selection::{Point, Selection, SelectionKind, SelectionRange};
pub use snapshot::Snapshot;
pub use term::{CursorShape, CursorState, Hyperlink, LinkMatch, Modes, TermEvent, Terminal};
pub use text::SearchMatch;
