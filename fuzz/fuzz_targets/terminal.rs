//! Feeds arbitrary bytes to the parser and terminal, with resizes, snapshots,
//! selections, link lookups and searches mixed in. Any panic is a bug.
#![no_main]

use libfuzzer_sys::fuzz_target;
use tron_core::{Parser, Point, Selection, SelectionKind, Snapshot, Terminal};

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let cols = 1 + usize::from(data[0] % 40);
    let rows = 1 + usize::from(data[1] % 20);
    let mut term = Terminal::new(cols, rows, usize::from(data[2]));
    term.set_cell_pixels(8, 16);
    let mut parser = Parser::new();
    let mut snapshot = Snapshot::default();
    for chunk in data[3..].chunks(37) {
        parser.advance(&mut term, chunk);
        let last = chunk[chunk.len() - 1];
        match chunk[0] % 16 {
            0 => term.resize(1 + usize::from(last % 50), 1 + usize::from(chunk[0] % 25)),
            1 => term.snapshot(&mut snapshot),
            2 => {
                let grid = term.grid();
                let anchor = Point::new(grid.oldest_line(), usize::from(chunk[0]) % grid.cols());
                let head = Point::new(grid.last_line(), usize::from(last) % grid.cols());
                let kind = [SelectionKind::Simple, SelectionKind::Word, SelectionKind::Line, SelectionKind::Block]
                    [usize::from(chunk[0]) % 4];
                term.set_selection(Some(Selection { kind, anchor, head }));
                let _ = term.selection_text();
            }
            3 => {
                let point = term.viewport_point(usize::from(chunk[0]), usize::from(last));
                let _ = term.link_at(point);
                let query = String::from_utf8_lossy(&chunk[1..chunk.len().min(4)]).into_owned();
                let _ = term.search(&query, Some(point), chunk[0] & 1 == 0);
            }
            4 => term.scroll_display(isize::from(chunk[0] as i8)),
            _ => {}
        }
        let _ = term.take_responses();
        let _ = term.take_events();
    }
    term.snapshot(&mut snapshot);
});
