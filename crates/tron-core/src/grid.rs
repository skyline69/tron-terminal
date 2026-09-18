//! Screen storage.
//!
//! Scrollback and the visible screen share one ring buffer, so scrolling a full
//! screen is O(1): the top row moves into history and a recycled row is appended.
//! Rows track how many cells were written, so clearing a recycled row only
//! touches that prefix.
//!
//! Lines have absolute numbers that stay stable while content scrolls. Selections
//! and image placements are anchored to them.

use std::collections::VecDeque;

use crate::cell::{Cell, Flags};

/// DEC line size attributes: DECSWL, DECDWL and DECDHL.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum LineSize {
    #[default]
    Single,
    /// Every cell is twice as wide; half as many columns fit.
    DoubleWidth,
    /// Top half of double width, double height text.
    DoubleHeightTop,
    /// Bottom half of double width, double height text.
    DoubleHeightBottom,
}

/// One line of cells.
#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The line continues on the next row because of auto-wrap.
    pub wrapped: bool,
    pub line_size: LineSize,
    /// Cells at or after this index are default blanks.
    occupied: usize,
    /// Combining characters, keyed by column. Usually empty.
    extras: Vec<(u16, String)>,
}

impl Row {
    pub fn new(cols: usize) -> Self {
        Self {
            cells: vec![Cell::BLANK; cols],
            wrapped: false,
            line_size: LineSize::Single,
            occupied: 0,
            extras: Vec::new(),
        }
    }

    fn filled(cols: usize, blank: Cell) -> Self {
        let mut row = Self::new(0);
        row.reset(cols, blank);
        row
    }

    pub fn reset(&mut self, cols: usize, blank: Cell) {
        if blank == Cell::BLANK && self.cells.len() == cols {
            let end = self.occupied.min(cols);
            // SAFETY: an all-zero `Cell` is `Cell::BLANK` (NUL char, default
            // colors, no flags), checked by `blank_cell_is_all_zero_bits`.
            unsafe { std::ptr::write_bytes(self.cells.as_mut_ptr(), 0, end) };
        } else {
            self.cells.clear();
            self.cells.resize(cols, blank);
        }
        self.occupied = if blank == Cell::BLANK { 0 } else { cols };
        self.wrapped = false;
        self.line_size = LineSize::Single;
        self.extras.clear();
    }

    /// Records a write to `start..end`: updates the occupied prefix and drops
    /// combining characters of overwritten cells.
    #[inline]
    pub fn touch(&mut self, start: usize, end: usize) {
        if end > self.occupied {
            self.occupied = end;
        }
        if !self.extras.is_empty() {
            self.extras.retain(|(col, _)| !(start..end).contains(&usize::from(*col)));
        }
    }

    /// Combining characters stored for `col`, without the base character.
    pub fn combining(&self, col: usize) -> Option<&str> {
        self.extras.iter().find(|(c, _)| usize::from(*c) == col).map(|(_, s)| s.as_str())
    }

    pub fn push_combining(&mut self, col: usize, ch: char) {
        match self.extras.iter_mut().find(|(c, _)| usize::from(*c) == col) {
            Some((_, text)) => {
                if text.len() < 64 {
                    text.push(ch);
                }
            }
            None => self.extras.push((col as u16, ch.to_string())),
        }
        self.cells[col].flags.insert(Flags::GRAPHEME);
    }

    /// Appends the full text of the cell at `col` (base plus combining characters).
    pub fn push_cell_text(&self, col: usize, out: &mut String) {
        let cell = &self.cells[col];
        out.push(if cell.ch == '\0' { ' ' } else { cell.ch });
        if cell.flags.contains(Flags::GRAPHEME)
            && let Some(extra) = self.combining(col)
        {
            out.push_str(extra);
        }
    }

    /// Inserts `n` blank cells at `col`, shifting the rest right.
    pub fn insert_cells(&mut self, col: usize, n: usize, blank: Cell) {
        let len = self.cells.len();
        let tail = &mut self.cells[col..];
        let n = n.min(tail.len());
        tail.rotate_right(n);
        tail[..n].fill(blank);
        if let Some(last) = self.cells.last_mut()
            && last.flags.contains(Flags::WIDE)
        {
            *last = blank;
        }
        for (c, _) in &mut self.extras {
            if usize::from(*c) >= col {
                *c += n as u16;
            }
        }
        self.extras.retain(|(c, _)| usize::from(*c) < len);
        self.occupied = (self.occupied + n).min(len);
    }

    /// Deletes `n` cells at `col`, shifting the rest left and filling with `blank`.
    pub fn delete_cells(&mut self, col: usize, n: usize, blank: Cell) {
        let tail = &mut self.cells[col..];
        let n = n.min(tail.len());
        tail.rotate_left(n);
        let len = tail.len();
        tail[len - n..].fill(blank);
        self.extras.retain(|(c, _)| !(col..col + n).contains(&usize::from(*c)));
        for (c, _) in &mut self.extras {
            if usize::from(*c) >= col + n {
                *c -= n as u16;
            }
        }
        if blank != Cell::BLANK {
            self.occupied = self.cells.len();
        }
    }

    /// Makes this row a copy of `other`, reusing allocations.
    pub fn copy_from(&mut self, other: &Row) {
        self.cells.clear();
        self.cells.extend_from_slice(&other.cells);
        self.wrapped = other.wrapped;
        self.line_size = other.line_size;
        self.occupied = other.occupied;
        self.extras.clone_from(&other.extras);
    }

    /// Length up to the last cell that is not a default blank.
    pub fn content_len(&self) -> usize {
        let end = self.occupied.min(self.cells.len());
        self.cells[..end].iter().rposition(|c| *c != Cell::BLANK).map_or(0, |i| i + 1)
    }

    pub fn is_blank(&self) -> bool {
        !self.wrapped && self.content_len() == 0
    }

    fn truncate_to(&mut self, cols: usize) {
        self.cells.resize(cols, Cell::BLANK);
        if let Some(last) = self.cells.last_mut()
            && last.flags.contains(Flags::WIDE)
        {
            *last = Cell::BLANK;
        }
        self.occupied = self.occupied.min(cols);
        self.extras.retain(|(c, _)| usize::from(*c) < cols);
    }
}

/// How many rows ahead of the recycled one [`Grid::scroll_up`] prefetches.
const PREFETCH_DISTANCE: usize = 4;

/// Asks the CPU to load a row's written cells into cache.
#[inline]
fn prefetch_row(row: &Row) {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
        let bytes = row.occupied.min(row.cells.len()) * size_of::<Cell>();
        let start = row.cells.as_ptr().cast::<i8>();
        for offset in (0..bytes).step_by(64) {
            // SAFETY: prefetching is a hint that never faults; the address is inside the allocation.
            unsafe { _mm_prefetch(start.add(offset), _MM_HINT_T0) };
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = row;
}

/// Where rows moved during a reflow, in absolute line numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMap {
    old_oldest: i64,
    cols: usize,
    /// New position (absolute line, column) of the first cell of each old row,
    /// `None` when the row was dropped.
    rows: Vec<Option<(i64, usize)>>,
}

impl LineMap {
    /// New absolute line of the row that was at `line` before the reflow.
    pub fn map(&self, line: i64) -> Option<i64> {
        self.start(line).map(|(line, _)| line)
    }

    /// New position of the cell that was at `line`, `col`. Wide characters that
    /// wrapped early can shift the result by a column.
    pub fn map_point(&self, line: i64, col: usize) -> Option<(i64, usize)> {
        let (start_line, start_col) = self.start(line)?;
        let offset = start_col + col;
        Some((start_line + (offset / self.cols) as i64, offset % self.cols))
    }

    fn start(&self, line: i64) -> Option<(i64, usize)> {
        let index = usize::try_from(line - self.old_oldest).ok()?;
        self.rows.get(index).copied().flatten()
    }
}

/// Visible screen plus scrollback.
pub struct Grid {
    cols: usize,
    rows: usize,
    /// Scrollback followed by the visible rows. Always at least `rows` long.
    lines: VecDeque<Row>,
    max_scrollback: usize,
    /// Absolute number of the top visible row.
    history: i64,
    /// Number of scrollback lines the viewport is scrolled up by.
    display_offset: usize,
    damage: Vec<bool>,
    full_damage: bool,
    /// Set by the last reflow, taken by [`Grid::take_line_map`].
    line_map: Option<LineMap>,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, max_scrollback: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            lines: (0..rows).map(|_| Row::new(cols)).collect(),
            max_scrollback,
            history: 0,
            display_offset: 0,
            damage: vec![true; rows],
            full_damage: true,
            line_map: None,
        }
    }

    /// Row movements of the last reflow, so anchored content can follow its text.
    pub fn take_line_map(&mut self) -> Option<LineMap> {
        self.line_map.take()
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.cols
    }

    #[inline]
    pub fn rows(&self) -> usize {
        self.rows
    }

    #[inline]
    fn base(&self) -> usize {
        self.lines.len() - self.rows
    }

    #[inline]
    pub fn row(&self, row: usize) -> &Row {
        &self.lines[self.base() + row]
    }

    /// Mutable access to a visible row. Marks it damaged.
    #[inline]
    pub fn row_mut(&mut self, row: usize) -> &mut Row {
        self.damage[row] = true;
        let base = self.base();
        &mut self.lines[base + row]
    }

    pub fn scrollback_len(&self) -> usize {
        self.base()
    }

    pub fn max_scrollback(&self) -> usize {
        self.max_scrollback
    }

    pub fn set_max_scrollback(&mut self, lines: usize) {
        self.max_scrollback = lines;
        self.trim_scrollback();
    }

    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

    /// Scrolls the viewport. Positive `delta` moves into history.
    pub fn scroll_display(&mut self, delta: isize) {
        let max = self.base() as isize;
        let new = (self.display_offset as isize + delta).clamp(0, max) as usize;
        if new != self.display_offset {
            self.display_offset = new;
            self.full_damage = true;
        }
    }

    /// Scrolls the viewport so absolute line `line` is its top row, as far as history allows.
    pub fn scroll_to_top(&mut self, line: i64) {
        let offset = (self.history - line).clamp(0, self.base() as i64) as usize;
        if offset != self.display_offset {
            self.display_offset = offset;
            self.full_damage = true;
        }
    }

    /// Scrolls the viewport so absolute line `line` is visible, centering it when it was not.
    pub fn scroll_to_line(&mut self, line: i64) {
        let top = self.viewport_line(0);
        if (top..top + self.rows as i64).contains(&line) {
            return;
        }
        let wanted_top = line - self.rows as i64 / 2;
        let offset = (self.history - wanted_top).clamp(0, self.base() as i64) as usize;
        if offset != self.display_offset {
            self.display_offset = offset;
            self.full_damage = true;
        }
    }

    pub fn reset_display_offset(&mut self) {
        self.scroll_display(-(self.display_offset as isize));
    }

    /// Row as seen through the viewport, `0` being the top of the window.
    pub fn visible_row(&self, row: usize) -> &Row {
        &self.lines[self.base() - self.display_offset + row]
    }

    /// Row just above the viewport, if history still holds one. Smooth scrolling
    /// draws part of it while the viewport sits between two lines.
    pub fn overscan_row(&self) -> Option<&Row> {
        let index = (self.base() - self.display_offset).checked_sub(1)?;
        self.lines.get(index)
    }

    /// Absolute line number of viewport row `row`.
    pub fn viewport_line(&self, row: usize) -> i64 {
        self.history - self.display_offset as i64 + row as i64
    }

    /// Absolute line number of screen row `row`.
    pub fn screen_line(&self, row: usize) -> i64 {
        self.history + row as i64
    }

    /// Absolute number of the oldest retained line.
    pub fn oldest_line(&self) -> i64 {
        self.history - self.base() as i64
    }

    /// Absolute number of the bottom screen row.
    pub fn last_line(&self) -> i64 {
        self.history + self.rows as i64 - 1
    }

    /// Row by absolute line number, if still retained.
    pub fn line(&self, line: i64) -> Option<&Row> {
        let index = line - self.oldest_line();
        (0..self.lines.len() as i64).contains(&index).then(|| &self.lines[index as usize])
    }

    /// Whether viewport row `row` must be redrawn.
    pub fn is_damaged(&self, row: usize) -> bool {
        self.full_damage || self.display_offset > 0 || self.damage[row]
    }

    pub fn damage_all(&mut self) {
        self.full_damage = true;
    }

    pub fn clear_damage(&mut self) {
        self.full_damage = false;
        self.damage.fill(false);
    }

    fn trim_scrollback(&mut self) {
        while self.base() > self.max_scrollback {
            self.lines.pop_front();
        }
        self.display_offset = self.display_offset.min(self.base());
    }

    /// Scrolls rows `top..=bottom` up by `n`. With `save` and `top == 0`, lines
    /// leaving the screen go to scrollback.
    pub fn scroll_up(&mut self, top: usize, bottom: usize, n: usize, blank: Cell, save: bool) {
        let n = n.min(bottom + 1 - top);
        if n == 0 {
            return;
        }
        let cols = self.cols;
        if save && top == 0 && self.max_scrollback > 0 {
            for _ in 0..n {
                // The recycled row is the oldest in history and cold in cache. Start
                // loading rows a few scrolls ahead so clearing them does not wait.
                if self.base() >= self.max_scrollback
                    && let Some(ahead) = self.lines.get(PREFETCH_DISTANCE)
                {
                    prefetch_row(ahead);
                }
                let recycled = if self.base() >= self.max_scrollback { self.lines.pop_front() } else { None };
                let row = match recycled {
                    Some(mut row) => {
                        row.reset(cols, blank);
                        row
                    }
                    None => Row::filled(cols, blank),
                };
                let at = self.base() + bottom + 1;
                if at == self.lines.len() {
                    self.lines.push_back(row);
                } else {
                    self.lines.insert(at, row);
                }
                self.history += 1;
                if self.display_offset > 0 {
                    self.display_offset = (self.display_offset + 1).min(self.base());
                }
            }
        } else {
            let base = self.base();
            for _ in 0..n {
                let mut row = self.lines.remove(base + top).expect("row in range");
                row.reset(cols, blank);
                self.lines.insert(base + bottom, row);
            }
            // Without scrollback, full screen scrolls still advance line numbers
            // so anchored content (images) moves with the text.
            if self.max_scrollback == 0 && top == 0 && bottom + 1 == self.rows {
                self.history += n as i64;
            }
        }
        if top == 0 && bottom + 1 == self.rows {
            self.full_damage = true;
        } else {
            self.damage[top..=bottom].fill(true);
        }
    }

    /// Scrolls rows `top..=bottom` down by `n`.
    pub fn scroll_down(&mut self, top: usize, bottom: usize, n: usize, blank: Cell) {
        let n = n.min(bottom + 1 - top);
        let cols = self.cols;
        let base = self.base();
        for _ in 0..n {
            let mut row = self.lines.remove(base + bottom).expect("row in range");
            row.reset(cols, blank);
            self.lines.insert(base + top, row);
        }
        self.damage[top..=bottom].fill(true);
    }

    /// Fills `cols` of `row` with `blank`, repairing split wide characters.
    pub fn erase(&mut self, row: usize, cols: std::ops::Range<usize>, blank: Cell) {
        let width = self.cols;
        let start = cols.start.min(width);
        let end = cols.end.min(width);
        if start >= end {
            return;
        }
        let line = self.row_mut(row);
        if start > 0
            && line.cells[start].flags.contains(Flags::WIDE_SPACER)
            && line.cells[start - 1].flags.contains(Flags::WIDE)
        {
            let cell = &mut line.cells[start - 1];
            cell.ch = '\0';
            cell.flags.remove(Flags::CONTENT_MASK);
        }
        if end < width && line.cells[end - 1].flags.contains(Flags::WIDE) {
            let cell = &mut line.cells[end];
            cell.ch = '\0';
            cell.flags.remove(Flags::CONTENT_MASK);
        }
        line.cells[start..end].fill(blank);
        if blank == Cell::BLANK {
            if !line.extras.is_empty() {
                line.extras.retain(|(c, _)| !(start..end).contains(&usize::from(*c)));
            }
        } else {
            line.touch(start, end);
        }
        if end == width {
            line.wrapped = false;
        }
    }

    /// Erases a whole row and makes it single width.
    pub fn erase_row(&mut self, row: usize, blank: Cell) {
        let cols = self.cols;
        self.erase(row, 0..cols, blank);
        self.row_mut(row).line_size = LineSize::Single;
    }

    pub fn clear_scrollback(&mut self) {
        let base = self.base();
        self.lines.drain(..base);
        self.display_offset = 0;
        self.full_damage = true;
    }

    /// Resizes the grid. With `reflow`, wrapped lines are rewrapped to the new
    /// width. Returns the new cursor position.
    pub fn resize(&mut self, cols: usize, rows: usize, cursor: (usize, usize), reflow: bool) -> (usize, usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let (mut cursor_row, mut cursor_col) = cursor;

        if cols != self.cols {
            if reflow {
                (cursor_row, cursor_col) = self.reflow(cols, (cursor_row, cursor_col));
            } else {
                for row in &mut self.lines {
                    row.truncate_to(cols);
                }
                self.cols = cols;
            }
        }

        if rows < self.rows {
            let excess = self.rows - rows;
            // Drop blank rows below the cursor first.
            let mut dropped = 0;
            while dropped < excess
                && self.rows - 1 - dropped > cursor_row
                && self.row(self.rows - 1 - dropped).is_blank()
            {
                dropped += 1;
            }
            for _ in 0..dropped {
                self.lines.pop_back();
            }
            let pushed = excess - dropped;
            self.rows = rows;
            self.history += pushed as i64;
            cursor_row = cursor_row.saturating_sub(pushed);
        } else if rows > self.rows {
            let extra = rows - self.rows;
            let pulled = extra.min(self.base());
            self.history -= pulled as i64;
            cursor_row += pulled;
            for _ in pulled..extra {
                self.lines.push_back(Row::new(cols));
            }
            self.rows = rows;
        }

        self.trim_scrollback();
        self.damage = vec![true; rows];
        self.full_damage = true;
        (cursor_row.min(rows - 1), cursor_col.min(cols - 1))
    }

    fn reflow(&mut self, cols: usize, cursor: (usize, usize)) -> (usize, usize) {
        let old_base = self.base();
        let old_oldest = self.oldest_line();
        let old_len = self.lines.len();
        let cursor_index = old_base + cursor.0;
        // Blank rows below the cursor would otherwise push content into history.
        while self.lines.len() > cursor_index + 1 && self.lines.back().is_some_and(Row::is_blank) {
            self.lines.pop_back();
        }

        let old: Vec<Row> = self.lines.drain(..).collect();
        let mut out =
            Rewrap { cols, lines: VecDeque::with_capacity(old.len()), cursor: None, row_map: vec![None; old_len] };
        let mut cells: Vec<Cell> = Vec::new();
        let mut extras: Vec<(usize, String)> = Vec::new();
        let mut cursor_offset = None;
        // Old rows of the logical line being collected, with their offset into it.
        let mut line_rows: Vec<(usize, usize)> = Vec::new();
        let mut line_size = LineSize::Single;

        for (index, mut row) in old.into_iter().enumerate() {
            let offset = cells.len();
            if line_rows.is_empty() {
                line_size = row.line_size;
            }
            line_rows.push((index, offset));
            if index == cursor_index {
                cursor_offset = Some(offset + cursor.1);
            }
            let mut take = if row.wrapped { row.cells.len() } else { row.content_len() };
            // A leading spacer left by a wrapped wide character is not content.
            if row.wrapped
                && take > 0
                && row.cells[take - 1].flags.contains(Flags::WIDE_SPACER)
                && !(take >= 2 && row.cells[take - 2].flags.contains(Flags::WIDE))
            {
                take -= 1;
            }
            for (col, text) in row.extras.drain(..) {
                if usize::from(col) < take {
                    extras.push((offset + usize::from(col), text));
                }
            }
            cells.extend_from_slice(&row.cells[..take]);
            if !row.wrapped {
                extras.sort_by_key(|(c, _)| *c);
                out.line(&cells, &extras, cursor_offset.take(), &line_rows, line_size);
                cells.clear();
                extras.clear();
                line_rows.clear();
            }
        }
        if !cells.is_empty() || cursor_offset.is_some() || !line_rows.is_empty() {
            extras.sort_by_key(|(c, _)| *c);
            out.line(&cells, &extras, cursor_offset.take(), &line_rows, line_size);
        }

        let (cursor_index, cursor_col) = out.cursor.unwrap_or((out.lines.len().saturating_sub(1), 0));
        let row_map = std::mem::take(&mut out.row_map);
        self.lines = out.lines;
        self.cols = cols;
        while self.lines.len() < self.rows {
            self.lines.push_back(Row::new(cols));
        }
        let new_base = self.base();
        self.history = (self.history - old_base as i64 + new_base as i64).max(new_base as i64);
        self.display_offset = 0;
        let new_oldest = self.oldest_line();
        self.line_map = Some(LineMap {
            old_oldest,
            cols,
            rows: row_map.into_iter().map(|row| row.map(|(index, col)| (new_oldest + index as i64, col))).collect(),
        });
        (cursor_index.saturating_sub(new_base), cursor_col)
    }
}

/// Accumulates rewrapped rows during reflow.
struct Rewrap {
    cols: usize,
    lines: VecDeque<Row>,
    cursor: Option<(usize, usize)>,
    /// New row index and column of the first cell of each old row.
    row_map: Vec<Option<(usize, usize)>>,
}

impl Rewrap {
    fn line(
        &mut self,
        cells: &[Cell],
        extras: &[(usize, String)],
        cursor: Option<usize>,
        rows: &[(usize, usize)],
        line_size: LineSize,
    ) {
        let cols = self.cols;
        let new_row = || Row { line_size, ..Row::new(cols) };
        let mut row = new_row();
        let mut col: usize = 0;
        let mut extra = 0;
        // New row and column of every cell, to map old rows through the rewrap.
        let mut cell_positions: Vec<(usize, usize)> = Vec::with_capacity(cells.len());
        for (i, cell) in cells.iter().enumerate() {
            let wide = cell.flags.contains(Flags::WIDE);
            if cell.flags.contains(Flags::WIDE_SPACER) && i > 0 && cells[i - 1].flags.contains(Flags::WIDE) {
                cell_positions.push((self.lines.len(), col.saturating_sub(1)));
                continue;
            }
            let width = if wide && cols > 1 { 2 } else { 1 };
            if col + width > cols {
                if width == 2 && col < cols {
                    row.cells[col] = Cell { flags: Flags::WIDE_SPACER, ..Cell::BLANK };
                }
                row.wrapped = true;
                row.occupied = cols;
                self.lines.push_back(std::mem::replace(&mut row, new_row()));
                col = 0;
            }
            cell_positions.push((self.lines.len(), col));
            if cursor == Some(i) {
                self.cursor = Some((self.lines.len(), col));
            }
            if width == 2 {
                row.cells[col] = *cell;
                row.cells[col + 1] = cells.get(i + 1).copied().unwrap_or(Cell { flags: Flags::WIDE_SPACER, ..*cell });
                if cursor == Some(i + 1) {
                    self.cursor = Some((self.lines.len(), col + 1));
                }
            } else {
                row.cells[col] = Cell { flags: cell.flags - Flags::WIDE, ..*cell };
            }
            while extra < extras.len() && extras[extra].0 < i {
                extra += 1;
            }
            if extra < extras.len() && extras[extra].0 == i {
                row.extras.push((col as u16, extras[extra].1.clone()));
            }
            col += width;
            row.occupied = col;
        }
        let last_row = self.lines.len();
        for &(old, offset) in rows {
            let past_end = (last_row, (col + offset.saturating_sub(cells.len())).min(cols - 1));
            self.row_map[old] = Some(cell_positions.get(offset).copied().unwrap_or(past_end));
        }
        if let Some(offset) = cursor
            && offset >= cells.len()
        {
            self.cursor = Some((self.lines.len(), (col + offset - cells.len()).min(cols - 1)));
        }
        self.lines.push_back(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(row: &Row) -> String {
        let mut out = String::new();
        for col in 0..row.cells.len() {
            if !row.cells[col].flags.contains(Flags::WIDE_SPACER) {
                row.push_cell_text(col, &mut out);
            }
        }
        out.trim_end().to_string()
    }

    fn write(grid: &mut Grid, row: usize, s: &str) {
        let line = grid.row_mut(row);
        for (i, c) in s.chars().enumerate() {
            line.cells[i].ch = c;
        }
        line.touch(0, s.chars().count());
    }

    #[test]
    fn blank_cell_is_all_zero_bits() {
        // SAFETY: Cell has no invalid all-zero representation: '\0' is a valid char.
        let zeroed: Cell = unsafe { std::mem::zeroed() };
        assert_eq!(zeroed, Cell::BLANK);
    }

    #[test]
    fn scrolling_keeps_line_numbers_stable() {
        let mut grid = Grid::new(5, 2, 10);
        write(&mut grid, 0, "a");
        let line = grid.screen_line(0);
        grid.scroll_up(0, 1, 1, Cell::BLANK, true);
        assert_eq!(text(grid.line(line).unwrap()), "a");
        assert_eq!(grid.scrollback_len(), 1);
    }

    #[test]
    fn scrollback_is_bounded_and_recycled() {
        let mut grid = Grid::new(5, 2, 3);
        for i in 0..10 {
            write(&mut grid, 1, &i.to_string());
            grid.scroll_up(0, 1, 1, Cell::BLANK, true);
        }
        assert_eq!(grid.scrollback_len(), 3);
        assert_eq!(text(grid.line(grid.oldest_line()).unwrap()), "6");
        assert!(grid.line(grid.oldest_line() - 1).is_none());
    }

    #[test]
    fn reflow_joins_and_splits_wrapped_lines() {
        let mut grid = Grid::new(4, 3, 10);
        write(&mut grid, 0, "abcd");
        grid.row_mut(0).wrapped = true;
        write(&mut grid, 1, "ef");
        let cursor = grid.resize(8, 3, (1, 2), true);
        assert_eq!(text(grid.row(0)), "abcdef");
        assert_eq!(cursor, (0, 6));
        let cursor = grid.resize(3, 3, cursor, true);
        assert_eq!(text(grid.row(0)), "abc");
        assert_eq!(text(grid.row(1)), "def");
        assert!(grid.row(0).wrapped);
        assert_eq!(cursor, (1, 2));
    }

    #[test]
    fn reflow_reports_where_rows_moved() {
        let mut grid = Grid::new(4, 4, 10);
        write(&mut grid, 0, "abcd");
        grid.row_mut(0).wrapped = true;
        write(&mut grid, 1, "ef");
        write(&mut grid, 2, "g");
        let (row0, row1, row2) = (grid.screen_line(0), grid.screen_line(1), grid.screen_line(2));
        grid.resize(8, 4, (2, 1), true);
        let map = grid.take_line_map().unwrap();
        assert_eq!(map.map(row0), Some(grid.screen_line(0)));
        assert_eq!(map.map(row1), Some(grid.screen_line(0)));
        assert_eq!(map.map(row2), Some(grid.screen_line(1)));
        assert_eq!(text(grid.line(map.map(row2).unwrap()).unwrap()), "g");
        assert_eq!(map.map_point(row1, 1), Some((grid.screen_line(0), 5)));
    }

    #[test]
    fn shrinking_rows_moves_lines_to_history() {
        let mut grid = Grid::new(4, 4, 10);
        for (i, s) in ["1", "2", "3", "4"].iter().enumerate() {
            write(&mut grid, i, s);
        }
        let cursor = grid.resize(4, 2, (3, 1), false);
        assert_eq!(cursor, (1, 1));
        assert_eq!(text(grid.row(0)), "3");
        assert_eq!(grid.scrollback_len(), 2);
        let cursor = grid.resize(4, 4, cursor, false);
        assert_eq!(text(grid.row(0)), "1");
        assert_eq!(cursor, (3, 1));
    }
}
