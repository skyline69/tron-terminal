//! Logical lines (rows joined across soft wraps), with URL detection and search.

use crate::cell::Flags;
use crate::grid::Grid;
use crate::selection::Point;

/// Most rows a logical line may span. Bounds work on pathological output.
const MAX_WRAPPED_ROWS: i64 = 256;

const URL_SCHEMES: [&str; 9] =
    ["https://", "http://", "file://", "ftp://", "sftp://", "ssh://", "git://", "gemini://", "mailto:"];

/// Characters of a logical line and the cell each one came from.
pub struct LogicalLine {
    pub chars: Vec<char>,
    pub points: Vec<Point>,
    pub first: i64,
    pub last: i64,
}

impl LogicalLine {
    /// The logical line containing absolute line `line`, if retained.
    pub fn at(grid: &Grid, line: i64) -> Option<Self> {
        grid.line(line)?;
        let mut first = line;
        while line - first < MAX_WRAPPED_ROWS && grid.line(first - 1).is_some_and(|r| r.wrapped) {
            first -= 1;
        }
        let mut last = line;
        while last - line < MAX_WRAPPED_ROWS && last < grid.last_line() && grid.line(last).is_some_and(|r| r.wrapped) {
            last += 1;
        }
        let mut chars = Vec::new();
        let mut points = Vec::new();
        for number in first..=last {
            let Some(row) = grid.line(number) else { break };
            for (col, cell) in row.cells.iter().enumerate() {
                if cell.flags.contains(Flags::WIDE_SPACER) {
                    continue;
                }
                chars.push(if cell.ch == '\0' { ' ' } else { cell.ch });
                points.push(Point::new(number, col));
            }
        }
        Some(Self { chars, points, first, last })
    }

    /// Index of the character drawn at `point`, including the right half of wide characters.
    fn index_of(&self, point: Point) -> Option<usize> {
        match self.points.binary_search(&point) {
            Ok(i) => Some(i),
            Err(i) => i.checked_sub(1).filter(|&j| self.points[j].line == point.line),
        }
    }

    /// A URL covering `point`, as (first, last) inclusive character indices.
    pub fn url_at(&self, point: Point) -> Option<(usize, usize)> {
        let target = self.index_of(point)?;
        let lower: Vec<char> = self.chars.iter().map(|c| c.to_ascii_lowercase()).collect();
        let mut start = 0;
        while start < lower.len() {
            let Some(scheme) = URL_SCHEMES.iter().find(|s| starts_with(&lower[start..], s)) else {
                start += 1;
                continue;
            };
            let boundary = start == 0 || !lower[start - 1].is_alphanumeric();
            let mut end = start + scheme.len();
            while end < self.chars.len() && is_url_char(self.chars[end]) {
                end += 1;
            }
            let end = trim_url_end(&self.chars[start..end]) + start;
            if boundary && end > start + scheme.len() {
                if (start..end).contains(&target) {
                    return Some((start, end - 1));
                }
                start = end;
            } else {
                start += 1;
            }
        }
        None
    }

    pub fn text(&self, first: usize, last: usize) -> String {
        self.chars[first..=last].iter().collect()
    }
}

fn starts_with(chars: &[char], prefix: &str) -> bool {
    let mut it = chars.iter();
    prefix.chars().all(|p| it.next() == Some(&p))
}

fn is_url_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | '"' | '`' | '\'' | '│')
}

/// Length after dropping trailing punctuation and unbalanced closing brackets.
fn trim_url_end(chars: &[char]) -> usize {
    let mut end = chars.len();
    while end > 0 {
        let c = chars[end - 1];
        let unbalanced = |open: char, close: char| {
            c == close
                && chars[..end].iter().filter(|&&x| x == close).count()
                    > chars[..end].iter().filter(|&&x| x == open).count()
        };
        if matches!(c, '.' | ',' | ':' | ';' | '!' | '?')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
            || unbalanced('{', '}')
        {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// A search hit, inclusive.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub start: Point,
    pub end: Point,
}

/// Finds `query` starting after (or before, when `backwards`) `from`. Wraps
/// around once. Matching ignores case unless the query has uppercase letters.
pub fn search(grid: &Grid, query: &str, from: Point, backwards: bool) -> Option<SearchMatch> {
    let needle: Vec<char> = query.chars().collect();
    if needle.is_empty() {
        return None;
    }
    let ignore_case = !query.chars().any(char::is_uppercase);
    let fold = |c: char| if ignore_case { c.to_lowercase().next().unwrap_or(c) } else { c };
    let needle: Vec<char> = needle.into_iter().map(fold).collect();
    let (oldest, newest) = (grid.oldest_line(), grid.last_line());
    let total = (newest - oldest + 1).max(1);

    let mut line = from.line.clamp(oldest, newest);
    let mut scanned = 0;
    let mut wrapped = false;
    while scanned <= total + MAX_WRAPPED_ROWS {
        let logical = LogicalLine::at(grid, line)?;
        let hay: Vec<char> = logical.chars.iter().map(|&c| fold(c)).collect();
        let mut hits: Vec<usize> = (0..hay.len().saturating_sub(needle.len() - 1))
            .filter(|&i| hay[i..i + needle.len()] == needle[..])
            .collect();
        if backwards {
            hits.reverse();
        }
        for i in hits {
            let start = logical.points[i];
            let after = if backwards { start < from } else { start > from };
            if after || wrapped {
                return Some(SearchMatch { start, end: logical.points[i + needle.len() - 1] });
            }
        }
        scanned += logical.last - logical.first + 1;
        line = if backwards { logical.first - 1 } else { logical.last + 1 };
        if line < oldest || line > newest {
            if wrapped {
                return None;
            }
            wrapped = true;
            line = if backwards { newest } else { oldest };
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Parser, Terminal};

    fn term(cols: usize, rows: usize, input: &str) -> Terminal {
        let mut t = Terminal::new(cols, rows, 100);
        Parser::new().advance(&mut t, input.as_bytes());
        t
    }

    #[test]
    fn finds_urls_across_wraps_and_trims_punctuation() {
        let t = term(20, 4, "see (https://example.com/a_(b)/path?x=1).\r\nno url here");
        let grid = t.grid();
        let line = LogicalLine::at(grid, grid.screen_line(1)).unwrap();
        let (first, last) = line.url_at(Point::new(grid.screen_line(1), 3)).unwrap();
        assert_eq!(line.text(first, last), "https://example.com/a_(b)/path?x=1");
        assert!(line.url_at(Point::new(grid.screen_line(0), 1)).is_none());
        let other = LogicalLine::at(grid, grid.screen_line(2)).unwrap();
        assert!(other.url_at(Point::new(grid.screen_line(2), 1)).is_none());
    }

    #[test]
    fn search_wraps_and_respects_smart_case() {
        let t = term(10, 4, "alpha beta\r\nBeta gamma\r\nbeta");
        let grid = t.grid();
        let bottom = Point::new(grid.last_line(), 9);
        let first = search(grid, "beta", bottom, true).unwrap();
        assert_eq!(first.start, Point::new(grid.screen_line(2), 0));
        let second = search(grid, "beta", first.start, true).unwrap();
        assert_eq!(second.start, Point::new(grid.screen_line(1), 0));
        let exact = search(grid, "Beta", bottom, true).unwrap();
        assert_eq!(exact.start, Point::new(grid.screen_line(1), 0));
        let wrapped = search(grid, "alpha", Point::new(grid.screen_line(0), 0), true).unwrap();
        assert_eq!(wrapped.start, Point::new(grid.screen_line(0), 0));
        assert!(search(grid, "missing", bottom, true).is_none());
    }
}
