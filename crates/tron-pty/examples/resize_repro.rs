//! Runs a shell headlessly through tron-core and resizes it repeatedly,
//! printing what the shell sends and the resulting screen.
//!
//! `cargo run -p tron-pty --example resize_repro -- fish -i`

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tron_core::{Parser, Terminal};
use tron_pty::{Pty, SpawnOptions, WindowSize};

fn main() {
    let mut args = std::env::args().skip(1);
    let program = args.next().unwrap_or_else(|| "fish".into());
    let options = SpawnOptions {
        remove_env: Vec::new(),
        program: Some(program),
        args: args.collect(),
        term: "xterm-256color".into(),
        ..Default::default()
    };
    // RESIZES="80x12,120x30" overrides the default column sweep.
    let resizes: Vec<(usize, usize)> = std::env::var("RESIZES")
        .ok()
        .map(|spec| {
            spec.split(',')
                .filter_map(|s| s.split_once('x'))
                .map(|(c, r)| (c.parse().unwrap(), r.parse().unwrap()))
                .collect()
        })
        .unwrap_or_else(|| [50, 80, 35, 90, 40, 85].into_iter().map(|c| (c, 12)).collect());
    let rows = 12;
    let size = |cols: u16, rows: u16| WindowSize { cols, rows, cell_width: 10, cell_height: 20 };
    let pty = Pty::spawn(&options, size(80, rows as u16)).expect("spawn");
    let mut reader = pty.reader().unwrap();
    let mut writer = pty.writer().unwrap();
    let mut writer_input = pty.writer().unwrap();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut term = Terminal::new(80, rows, 1000);
    term.set_cell_pixels(10, 20);
    let mut parser = Parser::new();
    let mut pump = |term: &mut Terminal, wait: Duration, show: bool| {
        let deadline = Instant::now() + wait;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok(bytes) = rx.recv_timeout(left) else { break };
            if show {
                println!("  shell sent: {:?}", String::from_utf8_lossy(&bytes));
            }
            parser.advance(term, &bytes);
            if let Some(reply) = term.take_responses() {
                writer.write_all(&reply).unwrap();
            }
        }
    };
    let dump = |term: &Terminal, label: &str| {
        let grid = term.grid();
        println!(
            "--- {label}: {}x{}, cursor {:?}, scrollback {}",
            grid.cols(),
            grid.rows(),
            term.cursor(),
            grid.scrollback_len()
        );
        for line in grid.oldest_line().max(grid.screen_line(0) - 3)..=grid.last_line() {
            let row = grid.line(line).unwrap();
            let mut text = String::new();
            for col in 0..row.cells.len() {
                if !row.cells[col].flags.contains(tron_core::Flags::WIDE_SPACER) {
                    row.push_cell_text(col, &mut text);
                }
            }
            let marker =
                if line < grid.screen_line(0) { "h".to_string() } else { format!("{:2}", line - grid.screen_line(0)) };
            println!("{marker}|{}|{}", text.trim_end(), if row.wrapped { " (wrapped)" } else { "" });
        }
    };

    pump(&mut term, Duration::from_millis(2500), false);
    dump(&term, "start");
    if let Ok(input) = std::env::var("TYPE_INPUT") {
        writer_input.write_all(input.as_bytes()).unwrap();
        pump(&mut term, Duration::from_millis(1500), true);
        dump(&term, "after input");
        if std::env::var_os("ONLY_INPUT").is_some() {
            return;
        }
    }
    for (cols, rows) in resizes {
        term.resize(cols, rows);
        pty.resize(size(cols as u16, rows as u16)).unwrap();
        pump(&mut term, Duration::from_millis(700), std::env::var_os("QUIET").is_none());
        dump(&term, &format!("after resize to {cols}x{rows}"));
        let top = term.grid().screen_line(0);
        for p in term.graphics().placements() {
            let first = p.line - top;
            println!(
                "  placement image {} on screen rows {}..={} (line {}), cursor row {}",
                p.image_id,
                first,
                first + i64::from(p.rows) - 1,
                p.line,
                term.cursor().row
            );
        }
    }
}
