//! Drives an interactive program headlessly through tron-core: sends scripted
//! input steps and prints the 80x24 screen after each one.
//!
//! `cargo run -p tron-pty --example vttest_driver -- /path/to/vttest '1\r|\r|\r'`
//! Steps are separated by `|`; `\r` and `\e` are expanded.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tron_core::{Flags, Parser, Terminal};
use tron_pty::{Pty, SpawnOptions, WindowSize};

fn main() {
    let mut args = std::env::args().skip(1);
    let program = args.next().expect("program path");
    let script = args.next().unwrap_or_default();
    let (cols, rows) = (80usize, 24usize);
    let options = SpawnOptions { program: Some(program), term: "vt100".into(), ..Default::default() };
    let size = WindowSize { cols: cols as u16, rows: rows as u16, cell_width: 10, cell_height: 20 };
    let pty = Pty::spawn(&options, size).expect("spawn");
    let mut reader = pty.reader().unwrap();
    let mut writer = pty.writer().unwrap();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut term = Terminal::new(cols, rows, 0);
    let mut parser = Parser::new();
    let mut pump = |term: &mut Terminal, writer: &mut std::fs::File, wait: Duration| {
        let deadline = Instant::now() + wait;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok(bytes) = rx.recv_timeout(left) else { break };
            parser.advance(term, &bytes);
            if let Some(reply) = term.take_responses() {
                writer.write_all(&reply).unwrap();
            }
        }
    };
    let dump = |term: &Terminal, label: &str| {
        let cursor = term.cursor();
        println!("=== {label} (cursor {},{}) ===", cursor.row + 1, cursor.col + 1);
        for row in 0..rows {
            let line = term.grid().row(row);
            let mut text = String::new();
            for col in 0..cols {
                if !line.cells[col].flags.contains(Flags::WIDE_SPACER) {
                    line.push_cell_text(col, &mut text);
                }
            }
            println!("{:2}|{}", row + 1, text.trim_end());
        }
    };

    pump(&mut term, &mut writer, Duration::from_millis(800));
    dump(&term, "start");
    for (i, step) in script.split('|').enumerate() {
        let bytes = step.replace("\\r", "\r").replace("\\e", "\x1b");
        writer.write_all(bytes.as_bytes()).unwrap();
        pump(&mut term, &mut writer, Duration::from_millis(600));
        dump(&term, &format!("step {} {:?}", i + 1, step));
    }
}
