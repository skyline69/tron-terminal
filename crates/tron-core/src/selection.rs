//! Text selection in absolute line coordinates.

use crate::cell::Flags;
use crate::grid::Grid;

/// A cell position. `line` is an absolute line number (see [`Grid::line`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: i64,
    pub col: usize,
}

impl Point {
    pub fn new(line: i64, col: usize) -> Self {
        Self { line, col }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Character by character.
    Simple,
    /// Expands to whole words (double click).
    Word,
    /// Expands to whole logical lines (triple click).
    Line,
    /// Rectangular block.
    Block,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub kind: SelectionKind,
    pub anchor: Point,
    pub head: Point,
}

/// Resolved, inclusive selection bounds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SelectionRange {
    pub start: Point,
    pub end: Point,
    pub block: bool,
}

impl SelectionRange {
    pub fn contains(&self, line: i64, col: usize) -> bool {
        if line < self.start.line || line > self.end.line {
            return false;
        }
        if self.block {
            let (left, right) = min_max(self.start.col, self.end.col);
            return (left..=right).contains(&col);
        }
        (line > self.start.line || col >= self.start.col) && (line < self.end.line || col <= self.end.col)
    }

    /// Selected text. Wrapped lines are joined, trailing blanks trimmed.
    pub fn text(&self, grid: &Grid) -> String {
        let mut out = String::new();
        let (left, right) = min_max(self.start.col, self.end.col);
        for line in self.start.line..=self.end.line {
            let Some(row) = grid.line(line) else { continue };
            let last = row.cells.len() - 1;
            let (from, to) = if self.block {
                (left.min(last), right.min(last))
            } else {
                (
                    if line == self.start.line { self.start.col.min(last) } else { 0 },
                    if line == self.end.line { self.end.col.min(last) } else { last },
                )
            };
            let start = out.len();
            for col in from..=to {
                if !row.cells[col].flags.contains(Flags::WIDE_SPACER) {
                    row.push_cell_text(col, &mut out);
                }
            }
            let joined = !self.block && row.wrapped && line != self.end.line && to == last;
            if !joined {
                let trimmed = out[start..].trim_end_matches(' ').len();
                out.truncate(start + trimmed);
                if line != self.end.line {
                    out.push('\n');
                }
            }
        }
        out
    }
}

impl Selection {
    pub fn new(kind: SelectionKind, point: Point) -> Self {
        Self { kind, anchor: point, head: point }
    }

    pub fn update(&mut self, point: Point) {
        self.head = point;
    }

    /// Resolves the selection against grid content. `separators` are the
    /// characters, besides whitespace, that end a word.
    pub fn range(&self, grid: &Grid, separators: &str) -> Option<SelectionRange> {
        let (mut start, mut end) =
            if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) };
        let (oldest, newest) = (grid.oldest_line(), grid.last_line());
        if end.line < oldest || start.line > newest {
            return None;
        }
        if start.line < oldest {
            start = Point::new(oldest, 0);
        }
        if end.line > newest {
            end = Point::new(newest, grid.cols() - 1);
        }
        let last_col = grid.cols() - 1;
        start.col = start.col.min(last_col);
        end.col = end.col.min(last_col);

        let range = match self.kind {
            SelectionKind::Simple => SelectionRange { start, end, block: false },
            SelectionKind::Block => {
                let (left, right) = min_max(self.anchor.col.min(last_col), self.head.col.min(last_col));
                SelectionRange { start: Point::new(start.line, left), end: Point::new(end.line, right), block: true }
            }
            SelectionKind::Word => SelectionRange {
                start: word_start(grid, start, separators),
                end: word_end(grid, end, separators),
                block: false,
            },
            SelectionKind::Line => {
                let mut first = start.line;
                while grid.line(first - 1).is_some_and(|r| r.wrapped) {
                    first -= 1;
                }
                let mut last = end.line;
                while last < newest && grid.line(last).is_some_and(|r| r.wrapped) {
                    last += 1;
                }
                SelectionRange { start: Point::new(first, 0), end: Point::new(last, last_col), block: false }
            }
        };
        Some(range)
    }
}

fn min_max(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

fn char_at(grid: &Grid, point: Point) -> Option<char> {
    let row = grid.line(point.line)?;
    let cell = row.cells.get(point.col)?;
    if cell.flags.contains(Flags::WIDE_SPACER) {
        return match point.col.checked_sub(1).map(|c| row.cells[c]) {
            Some(prev) if prev.flags.contains(Flags::WIDE) => Some(prev.ch),
            _ => Some(' '),
        };
    }
    Some(if cell.ch == '\0' { ' ' } else { cell.ch })
}

fn is_separator(c: char, separators: &str) -> bool {
    c.is_whitespace() || separators.contains(c)
}

fn word_start(grid: &Grid, mut point: Point, separators: &str) -> Point {
    if char_at(grid, point).is_none_or(|c| is_separator(c, separators)) {
        return point;
    }
    let last_col = grid.cols() - 1;
    loop {
        let prev = if point.col > 0 {
            Point::new(point.line, point.col - 1)
        } else if grid.line(point.line - 1).is_some_and(|r| r.wrapped) {
            Point::new(point.line - 1, last_col)
        } else {
            break;
        };
        match char_at(grid, prev) {
            Some(c) if !is_separator(c, separators) => point = prev,
            _ => break,
        }
    }
    point
}

fn word_end(grid: &Grid, mut point: Point, separators: &str) -> Point {
    if char_at(grid, point).is_none_or(|c| is_separator(c, separators)) {
        return point;
    }
    let last_col = grid.cols() - 1;
    loop {
        let next = if point.col < last_col {
            Point::new(point.line, point.col + 1)
        } else if grid.line(point.line).is_some_and(|r| r.wrapped) {
            Point::new(point.line + 1, 0)
        } else {
            break;
        };
        match char_at(grid, next) {
            Some(c) if !is_separator(c, separators) => point = next,
            _ => break,
        }
    }
    point
}
