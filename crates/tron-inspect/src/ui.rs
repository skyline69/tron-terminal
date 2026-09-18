//! The inspector's interface: tabs of dense tables, drawn from one report.

use egui::{Align, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke, pos2, vec2};

use crate::report::{Colors, Report};

/// Tabs, in the order the rail shows them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Terminal,
    Renderer,
    Io,
    Input,
    Theme,
}

impl Tab {
    pub const ALL: [Self; 5] = [Self::Terminal, Self::Renderer, Self::Io, Self::Input, Self::Theme];

    /// What the tab holds, shown when the pointer rests on it.
    fn help(self) -> &'static str {
        match self {
            Self::Terminal => "Grid, cursor, history and the modes applications have set",
            Self::Renderer => "GPU, frame pacing, the quads of a frame and the glyph caches",
            Self::Io => "Bytes between the shell and the window, and what the parser made of them",
            Self::Input => "Keys, what they sent, and where the pointer is",
            Self::Theme => "Colors, font and the shader chain",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Terminal => "TERMINAL",
            Self::Renderer => "RENDERER",
            Self::Io => "I/O",
            Self::Input => "INPUT",
            Self::Theme => "THEME",
        }
    }
}

/// What the window keeps between frames.
pub struct State {
    pub tab: Tab,
    /// Readings are frozen, so a moment can be read at leisure.
    pub paused: bool,
    /// The report last drawn, kept so pausing has something to show.
    pub frozen: Option<Report>,
    /// Colors the style was built from, rebuilt when the terminal's theme changes.
    styled: Option<Colors>,
}

impl Default for State {
    fn default() -> Self {
        Self { tab: Tab::Terminal, paused: false, frozen: None, styled: None }
    }
}

/// Colors of the interface, taken from the terminal's own palette so the
/// inspector looks like the window it follows.
#[derive(Clone, Copy)]
struct Skin {
    background: Color32,
    panel: Color32,
    card: Color32,
    border: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    good: Color32,
    warn: Color32,
}

impl Skin {
    fn new(colors: &Colors) -> Self {
        let rgb = |[r, g, b]: [u8; 3]| Color32::from_rgb(r, g, b);
        let background = rgb(colors.background);
        let foreground = rgb(colors.foreground);
        let panel = mix(background, foreground, 0.06);
        let card = mix(background, foreground, 0.04);
        // WCAG 2.1: 4.5:1 for text (1.4.3), 3:1 for borders and other parts of the
        // interface that carry meaning (1.4.11). Terminal themes rarely reach that
        // on their own, so every color is pushed until it does.
        Self {
            background,
            panel,
            card,
            border: readable_against(mix(background, foreground, 0.2), card, 3.0),
            text: readable_against(foreground, card, 7.0),
            muted: readable_against(mix(background, foreground, 0.62), card, 4.5),
            accent: readable_against(rgb(colors.cursor), card, 4.5),
            good: readable_against(rgb(colors.ansi[10]), card, 4.5),
            warn: readable_against(rgb(colors.ansi[11]), card, 4.5),
        }
    }
}

/// Relative luminance of a color, as WCAG 2.1 defines it.
fn luminance(color: Color32) -> f32 {
    let channel = |value: u8| {
        let value = f32::from(value) / 255.0;
        match value <= 0.03928 {
            true => value / 12.92,
            false => ((value + 0.055) / 1.055).powf(2.4),
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// Contrast ratio between two colors, from 1.0 to 21.0.
fn contrast(a: Color32, b: Color32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `color` moved away from `background` until it reaches `ratio`, or as far as
/// black or white if it cannot.
fn readable_against(color: Color32, background: Color32, ratio: f32) -> Color32 {
    let away = match luminance(background) > 0.18 {
        true => Color32::BLACK,
        false => Color32::WHITE,
    };
    let mut adjusted = color;
    for step in 0..=20 {
        if contrast(adjusted, background) >= ratio {
            return adjusted;
        }
        adjusted = mix(color, away, step as f32 / 20.0);
    }
    adjusted
}

/// `color` on `fill` when it reads there, otherwise black or white, whichever does.
fn text_on(color: Color32, fill: Color32) -> Color32 {
    match contrast(color, fill) >= 4.5 {
        true => color,
        false => readable_against(on_color(fill), fill, 4.5),
    }
}

/// Whichever of black and white reads better on `background`, as 1.4.11 asks of
/// text drawn on a colored fill.
fn on_color(background: Color32) -> Color32 {
    match contrast(Color32::BLACK, background) >= contrast(Color32::WHITE, background) {
        true => Color32::BLACK,
        false => Color32::WHITE,
    }
}

/// Blends `amount` of `b` into `a`.
fn mix(a: Color32, b: Color32, amount: f32) -> Color32 {
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    Color32::from_rgb(channel(a.r(), b.r()), channel(a.g(), b.g()), channel(a.b(), b.b()))
}

const LABEL: f32 = 12.5;
const VALUE: f32 = 12.5;
/// Width of the label column, so values line up down a section.
const LABEL_WIDTH: f32 = 140.0;
/// Height of one table row.
const ROW_HEIGHT: f32 = 17.0;

/// Applies the terminal's colors to the interface. Everything is square edged.
fn style(ctx: &egui::Context, skin: Skin) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = skin.background;
    visuals.window_fill = skin.background;
    visuals.extreme_bg_color = mix(skin.background, Color32::BLACK, 0.4);
    visuals.faint_bg_color = skin.panel;
    visuals.override_text_color = Some(skin.text);
    visuals.selection.bg_fill = skin.accent.gamma_multiply(0.3);
    visuals.selection.stroke = Stroke::new(1.0, skin.accent);
    visuals.widgets.noninteractive.bg_fill = skin.card;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, skin.border);
    visuals.widgets.inactive.bg_fill = skin.panel;
    visuals.widgets.inactive.weak_bg_fill = skin.panel;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, skin.border);
    visuals.widgets.hovered.bg_fill = mix(skin.panel, skin.accent, 0.3);
    visuals.widgets.hovered.weak_bg_fill = mix(skin.panel, skin.accent, 0.3);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, skin.accent);
    visuals.widgets.active.bg_fill = mix(skin.panel, skin.accent, 0.45);
    visuals.widgets.active.weak_bg_fill = mix(skin.panel, skin.accent, 0.45);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::ZERO;
    }
    visuals.window_corner_radius = CornerRadius::ZERO;
    visuals.menu_corner_radius = CornerRadius::ZERO;
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(6.0, 3.0);
        style.spacing.button_padding = vec2(8.0, 3.0);
        style.spacing.interact_size.y = 18.0;
        style.spacing.scroll.bar_width = 8.0;
    });
}

/// Draws a frame of the inspector.
pub fn draw(ui: &mut egui::Ui, state: &mut State, report: &Report) {
    let ctx = ui.ctx().clone();
    let skin = Skin::new(&report.colors);
    let restyle = state.styled.as_ref().is_none_or(|colors| {
        colors.ansi != report.colors.ansi
            || colors.background != report.colors.background
            || colors.cursor != report.colors.cursor
    });
    if restyle {
        style(&ctx, skin);
        state.styled = Some(report.colors.clone());
    }
    header(ui, state, report, skin);
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(skin.background).inner_margin(Margin::symmetric(8, 6)))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match state.tab {
                Tab::Terminal => terminal_tab(ui, report, skin),
                Tab::Renderer => renderer_tab(ui, report, skin),
                Tab::Io => io_tab(ui, report, skin),
                Tab::Input => input_tab(ui, report, skin),
                Tab::Theme => theme_tab(ui, report, skin),
            });
        });
}

/// One line of window facts, then the tabs.
fn header(ui: &mut egui::Ui, state: &mut State, report: &Report, skin: Skin) {
    egui::Panel::top("header")
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(skin.panel).inner_margin(Margin { left: 8, right: 8, top: 5, bottom: 0 }))
        .show(ui, |ui| {
            // The facts shrink, so the buttons keep their place in a narrow window.
            egui::Sides::new().shrink_left().truncate().show(
                ui,
                |ui| {
                    let facts = format!(
                        "{}  {}×{}  {}  in {}  out {}  {} scrollback  {}",
                        shorten(&report.session.shell, 46),
                        report.grid.cols,
                        report.grid.rows,
                        frame_rate(&report.frames),
                        rate(report.io.read_rate),
                        rate(report.io.write_rate),
                        report.grid.scrollback,
                        match report.grid.alt_screen {
                            true => "alternate",
                            false => "primary",
                        },
                    );
                    ui.add(
                        egui::Label::new(RichText::new(facts).font(FontId::monospace(12.0)).color(skin.text))
                            .truncate(),
                    );
                },
                |ui| {
                    let label = match state.paused {
                        true => "RESUME",
                        false => "PAUSE",
                    };
                    let color = match state.paused {
                        true => skin.warn,
                        false => skin.text,
                    };
                    if flat_button(ui, skin, label, 11.5, color, false).clicked() {
                        state.paused = !state.paused;
                    }
                    if report.session.exited {
                        ui.label(RichText::new("exited").size(11.0).color(skin.warn));
                    }
                    ui.label(
                        RichText::new(format!("up {:.0}s", report.session.uptime.as_secs_f32()))
                            .size(11.0)
                            .color(skin.muted),
                    );
                },
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                for tab in Tab::ALL {
                    let selected = state.tab == tab;
                    let color = match selected {
                        true => on_color(skin.accent),
                        false => skin.muted,
                    };
                    let response = flat_button(ui, skin, tab.title(), 12.0, color, selected);
                    if response.on_hover_text(tab.help()).clicked() {
                        state.tab = tab;
                    }
                }
            });
        });
}

/// A square button that lights up under the pointer, filled while it is selected.
fn flat_button(ui: &mut egui::Ui, skin: Skin, text: &str, size: f32, color: Color32, selected: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(size), color);
    let padding = ui.spacing().button_padding;
    let (rect, response) = ui.allocate_exact_size(galley.size() + padding * 2.0, Sense::click());
    let fill = match (selected, response.hovered()) {
        (true, true) => mix(skin.accent, on_color(skin.accent), 0.2),
        (true, false) => skin.accent,
        (false, true) => mix(skin.panel, skin.accent, 0.3),
        (false, false) => Color32::TRANSPARENT,
    };
    // Text keeps 4.5:1 on whatever it ends up drawn on.
    let color = match fill == Color32::TRANSPARENT {
        true => text_on(color, skin.panel),
        false => text_on(
            match selected {
                true => color,
                false => skin.text,
            },
            fill,
        ),
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A titled table of readings.
fn section<R>(ui: &mut egui::Ui, skin: Skin, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) {
    egui::Frame::new()
        .fill(skin.card)
        .stroke(Stroke::new(1.0, skin.border))
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(Margin { left: 8, right: 8, top: 5, bottom: 6 })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(title).size(11.5).strong().color(skin.accent));
            let rule = ui.available_rect_before_wrap();
            ui.painter().hline(rule.x_range(), rule.top() + 2.0, Stroke::new(1.0, skin.border));
            ui.add_space(4.0);
            add(ui);
        });
    ui.add_space(6.0);
}

/// Rows of a table. Kept as a scope so rows line up down the section.
fn table(ui: &mut egui::Ui, _id: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        add(ui);
    });
}

/// Draws one row and lights it while the pointer is over it.
fn row(ui: &mut egui::Ui, skin: Skin, add: impl FnOnce(&mut egui::Ui)) {
    // The highlight is painted behind the row, which is only measured afterwards.
    let background = ui.painter().add(egui::Shape::Noop);
    let response = ui
        .horizontal(|ui| {
            ui.set_min_width(ui.available_width());
            add(ui);
        })
        .response;
    if response.hovered() {
        let rect = response.rect.expand2(vec2(4.0, 1.0));
        let fill = mix(skin.panel, skin.accent, 0.18);
        ui.painter().set(background, egui::Shape::rect_filled(rect, CornerRadius::ZERO, fill));
    }
}

/// The label column of a row, left aligned and always the same width, so values
/// line up down a section.
fn label_cell(ui: &mut egui::Ui, skin: Skin, label: &str) {
    // In a narrow window the label gives way, so the value still has room.
    let width = LABEL_WIDTH.min(ui.available_width() * 0.5);
    text_cell(ui, width, label, FontId::proportional(LABEL), skin.muted);
}

/// Text in a cell of exactly `width`, whatever the text measures.
fn text_cell(ui: &mut egui::Ui, width: f32, text: &str, font: FontId, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::hover());
    let galley = ui.painter().layout(text.to_owned(), font, color, width);
    ui.painter().galley(pos2(rect.left(), rect.center().y - galley.size().y / 2.0), galley, color);
}

/// A label and its value, the value in the monospace font.
fn kv(ui: &mut egui::Ui, skin: Skin, label: &str, value: impl Into<String>) {
    let value = value.into();
    row(ui, skin, |ui| {
        label_cell(ui, skin, label);
        ui.add(
            egui::Label::new(RichText::new(value).font(FontId::monospace(VALUE)).color(skin.text))
                .truncate()
                .selectable(false),
        );
    });
}

/// A label and a value colored by whether it is on.
fn kv_flag(ui: &mut egui::Ui, skin: Skin, label: &str, on: bool, on_text: &str, off_text: &str) {
    let (text, color) = match on {
        true => (on_text, skin.good),
        false => (off_text, skin.muted),
    };
    row(ui, skin, |ui| {
        label_cell(ui, skin, label);
        ui.label(RichText::new(text).font(FontId::monospace(VALUE)).color(color));
    });
}

/// Lays sections out in two columns while the window is wide enough.
fn columns(ui: &mut egui::Ui, left: impl FnOnce(&mut egui::Ui), right: impl FnOnce(&mut egui::Ui)) {
    if ui.available_width() < 680.0 {
        left(ui);
        right(ui);
        return;
    }
    ui.columns(2, |columns| {
        left(&mut columns[0]);
        right(&mut columns[1]);
    });
}

/// A square tag, filled while it is on.
fn pill(ui: &mut egui::Ui, skin: Skin, text: &str, on: bool, tooltip: &str) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(11.5), skin.text);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(10.0, 4.0), Sense::hover());
    let (fill, stroke, color) = match on {
        true => (mix(skin.card, skin.accent, 0.25), Stroke::new(1.0, skin.accent), skin.text),
        false => (Color32::TRANSPARENT, Stroke::new(1.0, skin.border), skin.muted),
    };
    let fill = match response.hovered() {
        true => mix(skin.card, skin.accent, 0.5),
        false => fill,
    };
    let color = text_on(color, if fill == Color32::TRANSPARENT { skin.card } else { fill });
    ui.painter().rect(rect, CornerRadius::ZERO, fill, stroke, egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    response.on_hover_text(tooltip);
}

/// A bar per sample, newest at the right, under a line naming what it counts.
fn graph(ui: &mut egui::Ui, skin: Skin, values: &[f32], height: f32, caption: &str, format: impl Fn(f32) -> String) {
    let peak = values.iter().copied().fold(1e-6_f32, f32::max);
    let last = values.last().copied().unwrap_or(0.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(caption).size(11.5).color(skin.muted));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("now {}   peak {}", format(last), format(peak)))
                    .font(FontId::monospace(11.5))
                    .color(skin.text),
            );
        });
    });
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, mix(skin.card, Color32::BLACK, 0.35));
    painter.rect_stroke(rect, CornerRadius::ZERO, Stroke::new(1.0, skin.border), egui::StrokeKind::Inside);
    for step in 1..4 {
        let y = rect.top() + rect.height() * step as f32 / 4.0;
        painter.hline(rect.x_range(), y, Stroke::new(1.0, skin.border.gamma_multiply(0.4)));
    }
    if !values.is_empty() {
        let width = (rect.width() / values.len() as f32).max(1.0);
        for (index, &value) in values.iter().enumerate() {
            let filled = (value / peak).clamp(0.0, 1.0) * (rect.height() - 2.0);
            let left = rect.left() + index as f32 * rect.width() / values.len() as f32;
            let bar = egui::Rect::from_min_size(
                pos2(left, rect.bottom() - filled - 1.0),
                vec2((width - 1.0).max(1.0), filled),
            );
            painter.rect_filled(bar, CornerRadius::ZERO, skin.accent);
        }
    }
    response.on_hover_text(format!("{} samples", values.len()));
    ui.add_space(4.0);
}

/// The frame rate, or idle while the window has nothing to draw.
fn frame_rate(frames: &crate::report::Frames) -> String {
    match frames.fps {
        Some(fps) if frames.since_last < 1.0 => format!("{fps:.0} fps"),
        _ => "idle".to_owned(),
    }
}

/// `text` cut to `width` characters, with an ellipsis when it was longer.
fn shorten(text: &str, width: usize) -> String {
    match text.chars().count() > width {
        true => format!("{}…", text.chars().take(width - 1).collect::<String>()),
        false => text.to_owned(),
    }
}

fn bytes(count: u64) -> String {
    let count = count as f64;
    match count {
        _ if count >= 1e9 => format!("{:.2} GB", count / 1e9),
        _ if count >= 1e6 => format!("{:.2} MB", count / 1e6),
        _ if count >= 1e3 => format!("{:.1} kB", count / 1e3),
        _ => format!("{count:.0} B"),
    }
}

fn rate(per_second: f64) -> String {
    format!("{}/s", bytes(per_second.max(0.0) as u64))
}

fn count(value: u64) -> String {
    match value {
        _ if value >= 1_000_000 => format!("{:.2}M", value as f64 / 1e6),
        _ if value >= 10_000 => format!("{:.1}k", value as f64 / 1e3),
        _ => value.to_string(),
    }
}

fn terminal_tab(ui: &mut egui::Ui, report: &Report, skin: Skin) {
    columns(
        ui,
        |ui| {
            let grid = &report.grid;
            section(ui, skin, "SCREEN", |ui| {
                table(ui, "screen", |ui| {
                    kv(ui, skin, "Grid", format!("{} × {} cells", grid.cols, grid.rows));
                    kv(ui, skin, "Cell", format!("{} × {} px", grid.cell.0, grid.cell.1));
                    kv(ui, skin, "Surface", format!("{} × {} px", grid.surface.0, grid.surface.1));
                    kv(ui, skin, "Scale", format!("{:.2}×", grid.scale));
                    kv(ui, skin, "Padding", format!("{:.0} × {:.0} px", grid.padding.0, grid.padding.1));
                    kv_flag(ui, skin, "Buffer", grid.alt_screen, "alternate", "primary");
                    kv(ui, skin, "Lines", format!("{} … {}", grid.top_line, grid.bottom_line));
                    kv(ui, skin, "Images", grid.images.to_string());
                });
            });
            section(ui, skin, "HISTORY", |ui| {
                table(ui, "history", |ui| {
                    kv(ui, skin, "Scrollback", format!("{} of {} lines", grid.scrollback, grid.scrollback_limit));
                    kv(ui, skin, "Viewport", format!("{:.2} lines up", grid.offset));
                    kv(ui, skin, "Command marks", grid.marks.to_string());
                    kv(ui, skin, "Selection", grid.selection.clone().unwrap_or_else(|| "none".to_owned()));
                });
            });
        },
        |ui| {
            let cursor = &report.cursor;
            section(ui, skin, "CURSOR", |ui| {
                table(ui, "cursor", |ui| {
                    kv(ui, skin, "Position", format!("row {} col {}", cursor.row, cursor.col));
                    kv(ui, skin, "Shape", cursor.shape);
                    kv_flag(ui, skin, "Visible", cursor.visible, "yes", "hidden");
                    kv_flag(ui, skin, "Blinking", cursor.blinking, "yes", "no");
                    kv(ui, skin, "Kitty keyboard", format!("0b{:05b}", cursor.keyboard_flags));
                    let [x, y, w, h] = report.cursor_rect;
                    kv(ui, skin, "Rectangle", format!("{x:.0}, {y:.0}  {w:.0} × {h:.0} px"));
                });
            });
            section(ui, skin, "SESSION", |ui| {
                table(ui, "session", |ui| {
                    kv(ui, skin, "Foreground", report.session.shell.clone());
                    kv(ui, skin, "Process", report.session.pid.map_or_else(|| "—".to_owned(), |pid| pid.to_string()));
                    kv(ui, skin, "Terminal", report.session.tty.clone().unwrap_or_else(|| "—".to_owned()));
                    kv(
                        ui,
                        skin,
                        "Directory",
                        report.session.working_directory.clone().unwrap_or_else(|| "—".to_owned()),
                    );
                    kv(
                        ui,
                        skin,
                        "Window title",
                        report.session.program_title.clone().unwrap_or_else(|| report.session.title.clone()),
                    );
                    kv(ui, skin, "Harness", report.session.harness.clone().unwrap_or_else(|| "none".to_owned()));
                });
            });
        },
    );
    section(ui, skin, "MODES", |ui| {
        ui.horizontal_wrapped(|ui| {
            for mode in &report.modes {
                pill(ui, skin, mode.name, mode.on, mode.help);
            }
        });
    });
}

fn renderer_tab(ui: &mut egui::Ui, report: &Report, skin: Skin) {
    let render = &report.render;
    let frames = &report.frames;
    section(ui, skin, "FRAMES", |ui| {
        graph(ui, skin, &frames.history, 58.0, "ms per frame", |value| format!("{value:.1} ms"));
        columns(
            ui,
            |ui| {
                table(ui, "frames-left", |ui| {
                    kv(ui, skin, "Rate", frame_rate(frames));
                    kv(ui, skin, "Last frame", format!("{:.2} ms", frames.last_ms));
                    kv(ui, skin, "Drawn", format!("{:.1} s ago", frames.since_last));
                    kv(ui, skin, "Cap", format!("{:.2} ms", frames.target_ms));
                });
            },
            |ui| {
                table(ui, "frames-right", |ui| {
                    kv_flag(ui, skin, "Animations", frames.animated, "running", "idle");
                    kv_flag(ui, skin, "Paused", frames.animations_paused, "yes", "no");
                    kv_flag(ui, skin, "Window", frames.occluded, "occluded", "visible");
                    kv_flag(ui, skin, "Focus", frames.focused, "focused", "unfocused");
                });
            },
        );
    });
    columns(
        ui,
        |ui| {
            section(ui, skin, "GPU", |ui| {
                table(ui, "gpu", |ui| {
                    kv(ui, skin, "Adapter", render.adapter.clone());
                    kv(ui, skin, "Backend", render.backend.clone());
                    kv(ui, skin, "Driver", render.driver.clone());
                    kv(ui, skin, "Surface", format!("{} × {}", render.surface.0, render.surface.1));
                    kv(ui, skin, "Format", render.format.clone());
                    kv(ui, skin, "Present mode", render.present_mode.clone());
                    kv(ui, skin, "Alpha mode", render.alpha_mode.clone());
                    kv_flag(ui, skin, "Device", render.device_lost, "lost", "healthy");
                });
            });
            section(ui, skin, "FRAME CONTENTS", |ui| {
                table(ui, "contents", |ui| {
                    kv(ui, skin, "Quads", render.quads.0.to_string());
                    kv(ui, skin, "Backgrounds", render.quads.1.to_string());
                    kv(ui, skin, "Image quads", render.image_quads.to_string());
                    kv(
                        ui,
                        skin,
                        "Shader passes",
                        format!("{} user, {} startup", render.post_passes, render.startup_passes),
                    );
                });
            });
        },
        |ui| {
            section(ui, skin, "CACHES", |ui| {
                let atlas = |(size, used): (u32, u32)| match size {
                    0 => "—".to_owned(),
                    size => format!("{size} px, {:.0}% packed", used as f32 / size as f32 * 100.0),
                };
                table(ui, "caches", |ui| {
                    kv(ui, skin, "Mask atlas", atlas(render.atlases[0]));
                    kv(ui, skin, "Color atlas", atlas(render.atlases[1]));
                    kv(ui, skin, "Glyphs", render.glyphs.to_string());
                    kv(ui, skin, "Shaped runs", render.shaped_runs.to_string());
                    kv(ui, skin, "Cached rows", render.cached_rows.to_string());
                });
            });
            section(ui, skin, "SHADERS", |ui| {
                if report.shaders.is_empty() {
                    ui.label(RichText::new("none").font(FontId::monospace(VALUE)).color(skin.muted));
                    return;
                }
                ui.horizontal_wrapped(|ui| {
                    for shader in &report.shaders {
                        pill(
                            ui,
                            skin,
                            &shader.name,
                            shader.animated,
                            match shader.animated {
                                true => "Animated: the window redraws while it runs",
                                false => "Drawn once per frame of terminal output",
                            },
                        );
                    }
                });
            });
        },
    );
}

fn io_tab(ui: &mut egui::Ui, report: &Report, skin: Skin) {
    let io = &report.io;
    section(ui, skin, "THROUGHPUT", |ui| {
        graph(ui, skin, &io.read_history, 58.0, "bytes read per second", |value| rate(value as f64));
        columns(
            ui,
            |ui| {
                table(ui, "io-left", |ui| {
                    kv(ui, skin, "Reading", rate(io.read_rate));
                    kv(ui, skin, "Read", bytes(io.read));
                });
            },
            |ui| {
                table(ui, "io-right", |ui| {
                    kv(ui, skin, "Writing", rate(io.write_rate));
                    kv(ui, skin, "Written", bytes(io.written));
                });
            },
        );
    });
    columns(
        ui,
        |ui| {
            let parser = &report.parser;
            section(ui, skin, "PARSER", |ui| {
                table(ui, "parser", |ui| {
                    kv(ui, skin, "Printed cells", count(parser.printed));
                    kv(ui, skin, "Control bytes", count(parser.controls));
                    kv(ui, skin, "CSI", count(parser.csi));
                    kv(ui, skin, "ESC", count(parser.esc));
                    kv(ui, skin, "OSC", count(parser.osc));
                    kv(ui, skin, "DCS", count(parser.dcs));
                    kv(ui, skin, "APC", count(parser.apc));
                    kv(ui, skin, "Unhandled", count(parser.unhandled));
                });
            });
        },
        |ui| {
            let parser = &report.parser;
            section(ui, skin, "IGNORED SEQUENCES", |ui| {
                if parser.unhandled_recent.is_empty() {
                    ui.label(
                        RichText::new("none since the inspector opened")
                            .font(FontId::monospace(VALUE))
                            .color(skin.muted),
                    );
                    return;
                }
                table(ui, "ignored", |ui| {
                    for (sequence, times) in parser.unhandled_recent.iter().rev() {
                        row(ui, skin, |ui| {
                            text_cell(ui, 44.0, &format!("×{times}"), FontId::monospace(LABEL), skin.muted);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(sequence).font(FontId::monospace(VALUE)).color(skin.warn),
                                )
                                .truncate()
                                .selectable(false),
                            );
                        });
                    }
                });
            });
        },
    );
}

fn input_tab(ui: &mut egui::Ui, report: &Report, skin: Skin) {
    let mouse = &report.grid.mouse;
    let input = &report.input;
    columns(
        ui,
        |ui| {
            section(ui, skin, "POINTER", |ui| {
                table(ui, "pointer", |ui| {
                    kv(ui, skin, "Position", format!("{:.0}, {:.0} px", mouse.position.0, mouse.position.1));
                    kv(ui, skin, "Cell", format!("row {} col {}", mouse.cell.0, mouse.cell.1));
                    kv(ui, skin, "Reporting", mouse.reporting);
                    kv(ui, skin, "Link", mouse.hovered_link.clone().unwrap_or_else(|| "none".to_owned()));
                });
            });
        },
        |ui| {
            section(ui, skin, "KEYBOARD", |ui| {
                table(ui, "keyboard", |ui| {
                    kv(
                        ui,
                        skin,
                        "Modifiers",
                        match input.modifiers.is_empty() {
                            true => "none".to_owned(),
                            false => input.modifiers.clone(),
                        },
                    );
                    kv(ui, skin, "Preedit", input.preedit.clone().unwrap_or_else(|| "—".to_owned()));
                    kv(ui, skin, "Bindings", input.bindings.to_string());
                    kv_flag(ui, skin, "Search", input.search_open, "open", "closed");
                    kv_flag(ui, skin, "Command palette", input.palette_open, "open", "closed");
                });
            });
        },
    );
    section(ui, skin, "KEYS", |ui| {
        if report.keys.is_empty() {
            ui.label(RichText::new("type in the terminal window").font(FontId::monospace(VALUE)).color(skin.muted));
            return;
        }
        table(ui, "keys", |ui| {
            for key in report.keys.iter().rev() {
                let combination = match key.mods.is_empty() {
                    true => key.key.clone(),
                    false => format!("{}+{}", key.mods, key.key),
                };
                row(ui, skin, |ui| {
                    let width = (LABEL_WIDTH + 40.0).min(ui.available_width() * 0.5);
                    text_cell(ui, width, &combination, FontId::monospace(VALUE), skin.text);
                    match &key.action {
                        Some(action) => {
                            ui.label(RichText::new("action").size(LABEL).color(skin.muted));
                            ui.label(RichText::new(action).font(FontId::monospace(VALUE)).color(skin.accent));
                        }
                        None => {
                            ui.label(RichText::new("sent").size(LABEL).color(skin.muted));
                            ui.label(RichText::new(&key.bytes).font(FontId::monospace(VALUE)).color(skin.text));
                        }
                    }
                });
            }
        });
    });
}

fn theme_tab(ui: &mut egui::Ui, report: &Report, skin: Skin) {
    let colors = &report.colors;
    section(ui, skin, "PALETTE", |ui| {
        let cell = vec2(((ui.available_width() - 7.0 * 4.0) / 8.0).max(14.0), 22.0);
        for half in 0..2 {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for index in half * 8..half * 8 + 8 {
                    let color = colors.ansi[index];
                    let (rect, response) = ui.allocate_exact_size(cell, Sense::hover());
                    let fill = Color32::from_rgb(color[0], color[1], color[2]);
                    ui.painter().rect(
                        rect,
                        CornerRadius::ZERO,
                        fill,
                        Stroke::new(1.0, skin.border),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        rect.left_top() + vec2(4.0, 3.0),
                        egui::Align2::LEFT_TOP,
                        index.to_string(),
                        FontId::monospace(10.0),
                        on_color(fill),
                    );
                    ui.painter().text(
                        rect.right_bottom() + vec2(-4.0, -3.0),
                        egui::Align2::RIGHT_BOTTOM,
                        format!("{:02x}{:02x}{:02x}", color[0], color[1], color[2]),
                        FontId::monospace(10.0),
                        on_color(fill),
                    );
                    if response.hovered() {
                        ui.painter().rect_stroke(
                            rect,
                            CornerRadius::ZERO,
                            Stroke::new(2.0, skin.text),
                            egui::StrokeKind::Inside,
                        );
                    }
                    response.on_hover_text(format!("color {index}"));
                }
            });
            ui.add_space(4.0);
        }
    });
    columns(
        ui,
        |ui| {
            section(ui, skin, "NAMED COLORS", |ui| {
                table(ui, "named", |ui| {
                    swatch(ui, skin, "Background", colors.background);
                    swatch(ui, skin, "Foreground", colors.foreground);
                    swatch(ui, skin, "Cursor", colors.cursor);
                    swatch(ui, skin, "Cursor text", colors.cursor_text);
                    swatch(ui, skin, "Selection", colors.selection);
                    kv(ui, skin, "Theme", colors.theme.clone());
                    kv(ui, skin, "Opacity", format!("{:.0}%", colors.opacity * 100.0));
                });
            });
        },
        |ui| {
            let font = &report.font;
            section(ui, skin, "FONT", |ui| {
                table(ui, "font", |ui| {
                    kv(ui, skin, "Family", font.family.clone());
                    kv(ui, skin, "Size", format!("{:.1} pt", font.size));
                    kv(ui, skin, "Cell", format!("{} × {} px", font.cell.0, font.cell.1));
                    kv(ui, skin, "Baseline", format!("{} px", font.baseline));
                    kv_flag(ui, skin, "Ligatures", font.ligatures, "on", "off");
                    kv_flag(ui, skin, "Bidirectional", font.bidi, "on", "off");
                });
            });
        },
    );
}

/// A table row of a color: its name, a square of it and its hexadecimal value.
fn swatch(ui: &mut egui::Ui, skin: Skin, label: &str, color: [u8; 3]) {
    row(ui, skin, |ui| {
        label_cell(ui, skin, label);
        let (rect, _) = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
        let fill = Color32::from_rgb(color[0], color[1], color[2]);
        ui.painter().rect(rect, CornerRadius::ZERO, fill, Stroke::new(1.0, skin.border), egui::StrokeKind::Inside);
        let hex = format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
        ui.label(RichText::new(hex).font(FontId::monospace(VALUE)).color(skin.text));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tron's own colors, and a light theme, as the terminal would report them.
    fn colors(background: [u8; 3], foreground: [u8; 3], cursor: [u8; 3]) -> Colors {
        Colors {
            theme: "test".to_owned(),
            background,
            foreground,
            cursor,
            cursor_text: background,
            selection: [0x1f, 0x4a, 0x6b],
            ansi: [
                [0x1b, 0x22, 0x30],
                [0xff, 0x5c, 0x75],
                [0x5c, 0xe6, 0xa6],
                [0xff, 0xc8, 0x6b],
                [0x4f, 0x9d, 0xff],
                [0xc3, 0x8b, 0xff],
                [0x4f, 0xd6, 0xff],
                [0xc7, 0xd5, 0xe0],
                [0x4a, 0x55, 0x68],
                [0xff, 0x80, 0x95],
                [0x86, 0xf0, 0xbf],
                [0xff, 0xd9, 0x9a],
                [0x7f, 0xb8, 0xff],
                [0xd6, 0xad, 0xff],
                [0x8a, 0xe6, 0xff],
                [0xee, 0xf4, 0xf8],
            ],
            opacity: 1.0,
        }
    }

    /// WCAG 2.1 asks 4.5:1 of text (1.4.3) and 3:1 of the rest of the interface (1.4.11).
    #[test]
    fn every_color_meets_the_contrast_the_guidelines_ask_for() {
        let themes = [
            colors([0x0a, 0x0e, 0x14], [0xc7, 0xd5, 0xe0], [0x4f, 0xd6, 0xff]),
            colors([0xff, 0xff, 0xff], [0x20, 0x20, 0x20], [0x00, 0x80, 0xa0]),
            // A theme whose colors are all close to its background.
            colors([0x30, 0x30, 0x30], [0x40, 0x40, 0x40], [0x38, 0x38, 0x38]),
        ];
        for theme in themes {
            let skin = Skin::new(&theme);
            for (name, color, least) in [
                ("text", skin.text, 4.5),
                ("muted", skin.muted, 4.5),
                ("accent", skin.accent, 4.5),
                ("good", skin.good, 4.5),
                ("warn", skin.warn, 4.5),
                ("border", skin.border, 3.0),
            ] {
                let ratio = contrast(color, skin.card);
                assert!(ratio >= least, "{name} is {ratio:.2}:1 on the card, wanted {least}:1");
            }
            let on_accent = contrast(on_color(skin.accent), skin.accent);
            assert!(on_accent >= 4.5, "a selected tab's title is {on_accent:.2}:1");
        }
    }

    #[test]
    fn sizes_are_written_in_the_largest_unit_that_fits() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2_048), "2.0 kB");
        assert_eq!(bytes(5_250_000), "5.25 MB");
        assert_eq!(rate(0.0), "0 B/s");
    }

    #[test]
    fn counts_shorten_once_they_stop_being_readable() {
        assert_eq!(count(999), "999");
        assert_eq!(count(9_999), "9999");
        assert_eq!(count(12_500), "12.5k");
        assert_eq!(count(3_400_000), "3.40M");
    }

    #[test]
    fn a_long_command_is_cut_with_an_ellipsis() {
        assert_eq!(shorten("bash", 10), "bash");
        assert_eq!(shorten("0123456789abc", 10), "012345678…");
    }

    #[test]
    fn the_frame_rate_reads_idle_while_the_window_rests() {
        let resting = crate::report::Frames { fps: Some(60.0), since_last: 4.0, ..Default::default() };
        assert_eq!(frame_rate(&resting), "idle");
        let drawing = crate::report::Frames { fps: Some(59.6), since_last: 0.01, ..Default::default() };
        assert_eq!(frame_rate(&drawing), "60 fps");
        assert_eq!(frame_rate(&crate::report::Frames::default()), "idle");
    }
}
