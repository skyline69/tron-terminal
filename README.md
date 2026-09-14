# tron

A GPU accelerated terminal emulator written in Rust. Fast first, then beautiful.

- **Fast.** Custom parser and screen model with SIMD friendly fast paths,
  lock-free frame building and frame pacing. Faster than Alacritty and Ghostty
  in the throughput benchmarks below.
- **Good text.** HarfBuzz-grade shaping through `harfrust` with programming
  ligatures, font fallback through fontconfig, color emoji, and box drawing,
  block and Powerline glyphs drawn to fit every cell exactly.
- **Customizable.** WGSL post-processing shaders, themes, hot reloaded
  configuration and key bindings.
- **Modern protocols.** Kitty graphics (including animation, shared memory
  and Unicode placeholders) and keyboard protocols, Sixel, synchronized output,
  OSC 8 hyperlinks, OSC 52 clipboard, styled underlines, true color.
- **Images and video.** `chafa`, `kitten icat`-style tools, `yazi` previews and
  `mpv --vo=kitty` work out of the box.

Linux and Wayland are the primary platform. Tabs and splits are out of scope:
use your window manager or a multiplexer.

## Building

Requires Rust 1.98 or newer and the development files for Wayland,
xkbcommon and fontconfig. `tic` from ncurses is used at runtime to install the
bundled terminfo entry.

```sh
# Fedora
sudo dnf install wayland-devel libxkbcommon-devel fontconfig-devel ncurses
# Debian / Ubuntu
sudo apt install libwayland-dev libxkbcommon-dev libfontconfig-dev ncurses-bin

cargo build --release
./target/release/tron
```

A desktop entry and icon live in `dist/`.

## Usage

```
tron [options] [-e program [args...]]

  -e, --command <program> [args...]  Run a program instead of the shell
  -d, --working-directory <dir>      Start in this directory
      --config-dir <dir>             Use this configuration directory
```

### Default key bindings

| Keys | Action |
|---|---|
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy / paste |
| `Shift+Insert`, middle click | Paste the primary selection |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Font size bigger / smaller / reset |
| `Shift+PageUp` / `Shift+PageDown` | Scroll a page |
| `Shift+Home` / `Shift+End` | Scroll to top / bottom |
| `Ctrl+Shift+F` | Search scrollback (Enter older, Shift+Enter newer, Esc close) |
| `Ctrl+Shift+N` | New window in the current directory |
| `Ctrl+Shift+,` | Reload configuration |
| `Ctrl+click` | Open a link |

Mouse: drag to select, double click for words, triple click for lines,
`Alt`+drag for a block, `Shift`+click to extend. Hold `Shift` to select text in
applications that use the mouse.

## Configuration

tron reads `~/.config/tron/config.toml` (the XDG config directory) and applies
changes while running. Every key is optional; see
[`examples/config.toml`](examples/config.toml) for all of them.

```
~/.config/tron/
  config.toml           configuration
  themes/<name>.toml    color themes, selected with theme = "<name>"
  shaders/<name>.wgsl   post-processing shaders
```

Key bindings override the defaults:

```toml
[keybindings]
"ctrl+shift+c" = "none"            # remove a default
"alt+enter" = "new_window"
"ctrl+shift+k" = "clear_scrollback"
"super+u" = "text:\u0015"    # send Ctrl+U to the application
```

Actions: `copy`, `paste`, `paste_selection`, `increase_font_size`,
`decrease_font_size`, `reset_font_size`, `scroll_line_up`, `scroll_line_down`,
`scroll_page_up`, `scroll_page_down`, `scroll_to_top`, `scroll_to_bottom`,
`clear_scrollback`, `search`, `new_window`, `reload_config`, `text:...`, `none`.

### Shaders

Shaders run on the rendered terminal, in the order listed in
`[shader] files`. Each file defines one function:

```wgsl
fn shade(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let color = terminal(uv);            // the rendered terminal
    let scanline = 0.9 + 0.1 * sin(frag_coord.y * 3.14159);
    return vec4<f32>(color.rgb * scanline, color.a);
}
```

Available inputs on the `tron` uniform: `resolution`, `time`, `frame`,
`cursor` (x, y, width, height in pixels), `cell_size`, `focused` and
`background`. Shaders that read `tron.time` or `tron.frame` redraw every
frame. Examples: CRT, bloom and cursor glow in
[`examples/shaders`](examples/shaders).

## Shell integration

tron clears and lets the shell redraw its prompt on resize when the shell
marks prompts with OSC 133. fish 4 does this out of the box. For bash and zsh:

```sh
# bash (~/.bashrc)
PS0='\e]133;C\e\\'
PS1='\[\e]133;D\e\\\e]133;A\e\\\]'"$PS1"'\[\e]133;B\e\\\]'

# zsh (~/.zshrc)
precmd() { print -n '\e]133;D\e\\\e]133;A\e\\' }
preexec() { print -n '\e]133;C\e\\' }
```

## Remote hosts

tron sets `TERM=xterm-tron`. Hosts without that terminfo entry need it
copied, or `term = "xterm-256color"` under `[shell]`:

```sh
infocmp -x xterm-tron | ssh host 'mkdir -p ~/.terminfo && tic -x -o ~/.terminfo /dev/stdin'
```

## Performance

`cat` of large files at 100x30 on Fedora, Wayland, RTX 2070 SUPER. Median of
five runs, lower is better.

| Workload | tron | Alacritty 0.17 | Ghostty 1.3 |
|---|---|---|---|
| Plain ASCII, 54 MiB | 707 ms | 882 ms | 1275 ms |
| True color SGR, 44 MiB | 663 ms | 731 ms | 3058 ms |
| Unicode and CJK, 31 MiB | 457 ms | 463 ms | 667 ms |
| Launch until the shell runs | 53 ms | 140 ms | 417 ms |

Headless parser throughput:
`cargo run --release -p tron-core --example throughput`.

## Architecture

| Crate | Purpose |
|---|---|
| `tron-core` | Parser, grid, scrollback, reflow, selection, search, images, terminal state. No GPU or window code. |
| `tron-font` | Font discovery and fallback, shaping, rasterization, generated box drawing glyphs. |
| `tron-render` | wgpu renderer: instanced cells, glyph atlases, images, post-processing. |
| `tron-pty` | Pseudo terminal and child process. |
| `tron-config` | Configuration, themes, key bindings, hot reload. |
| `tron` | The application: window, input, clipboard, terminfo. |

Development: `cargo nextest run --workspace` and
`cargo clippy --workspace --all-targets`.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
