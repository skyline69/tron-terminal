//! Parser and terminal throughput, without rendering.
//!
//! Run with `cargo run --release -p tron-core --example throughput [plain|sgr|unicode]`.

use std::time::Instant;

use tron_core::{Parser, Terminal};

fn main() {
    let mut plain = Vec::new();
    let mut styled = Vec::new();
    let mut unicode = Vec::new();
    for i in 0..400_000 {
        plain.extend_from_slice(format!("line {i}: the quick brown fox jumps over the lazy dog\r\n").as_bytes());
        styled.extend_from_slice(
            format!("\x1b[1;38;2;{};{};200m{i:>8}\x1b[0m \x1b[4:3mstatus\x1b[24m ok\r\n", i % 256, (i / 3) % 256)
                .as_bytes(),
        );
        unicode.extend_from_slice(format!("{i} → λ 中文字符 ✓ ünïcödé\r\n").as_bytes());
    }

    let only = std::env::args().nth(1);
    for (name, data) in [("plain ascii", &plain), ("sgr heavy", &styled), ("unicode", &unicode)] {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        let repeat: usize = std::env::args().nth(2).and_then(|r| r.parse().ok()).unwrap_or(1);
        let mut term = Terminal::new(200, 60, 10_000);
        let mut parser = Parser::new();
        let start = Instant::now();
        for _ in 0..repeat {
            for chunk in data.chunks(64 * 1024) {
                parser.advance(&mut term, chunk);
            }
        }
        let secs = start.elapsed().as_secs_f64();
        let mib = (data.len() * repeat) as f64 / (1024.0 * 1024.0);
        println!("{name:>12}: {mib:7.1} MiB in {secs:6.3} s = {:8.1} MiB/s", mib / secs);
    }
}
