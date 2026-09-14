//! Welcome splash. The terminal side animates with tachyonfx, while tron's
//! startup shader turns the screen on like a CRT, draws a neon grid behind
//! the text and adds bloom and a short glitch.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{DefaultTerminal, Frame};
use tachyonfx::{Effect, Interpolation, fx};

use crate::Outcome;
use crate::link::Link;

const LOGO: [&str; 6] = [
    "████████╗██████╗  ██████╗ ███╗   ██╗",
    "╚══██╔══╝██╔══██╗██╔═══██╗████╗  ██║",
    "   ██║   ██████╔╝██║   ██║██╔██╗ ██║",
    "   ██║   ██╔══██╗██║   ██║██║╚██╗██║",
    "   ██║   ██║  ██║╚██████╔╝██║ ╚████║",
    "   ╚═╝   ╚═╝  ╚═╝ ╚═════╝ ╚═╝  ╚═══╝",
];
const CYAN: Color = Color::Rgb(0x4f, 0xd6, 0xff);
const MAGENTA: Color = Color::Rgb(0xc3, 0x8b, 0xff);
const DIM: Color = Color::Rgb(0x6a, 0x75, 0x88);
/// Color text fades from and to. Close to tron's default background.
const DARK: Color = Color::Rgb(0x0a, 0x0e, 0x14);
const FRAME: Duration = Duration::from_millis(16);
/// Length of the goodbye animation after a key press.
const EXIT: Duration = Duration::from_millis(450);
/// When the logo is fully drawn: the moment of the glitch and bloom flash.
const LOGO_DONE: f32 = 1.35;

/// Shader scene number of the splash.
pub const SCENE: u32 = 1;

struct Areas {
    screen: Rect,
    logo: Rect,
    subtitle: Rect,
    hint: Rect,
}

impl Areas {
    fn new(screen: Rect) -> Self {
        let [_, logo, _, subtitle, _, hint, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(LOGO.len() as u16),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(screen);
        Self { screen, logo: centered(logo, LOGO[0].chars().count()), subtitle, hint }
    }
}

pub struct Splash {
    started: Instant,
    animations: bool,
    /// Effects for the logo, subtitle and hint, created on the first frame.
    effects: Vec<Effect>,
    screen: Rect,
    exit: Option<(Instant, Effect)>,
}

impl Splash {
    pub fn new(animations: bool) -> Self {
        Self { started: Instant::now(), animations, effects: Vec::new(), screen: Rect::default(), exit: None }
    }

    fn intro_effects(areas: &Areas) -> Vec<Effect> {
        let logo = fx::parallel(&[
            fx::prolong_start(300, fx::coalesce((1050, Interpolation::CubicOut))),
            fx::prolong_start(300, fx::fade_from_fg(Color::White, (1300, Interpolation::QuadOut))),
        ]);
        let subtitle = fx::prolong_start(1000, fx::fade_from_fg(DARK, (600, Interpolation::QuadOut)));
        let hint = fx::sequence(&[
            fx::prolong_start(1600, fx::fade_from_fg(DARK, (500, Interpolation::QuadOut))),
            fx::repeating(fx::ping_pong(fx::fade_to_fg(DARK, (1100, Interpolation::SineInOut)))),
        ]);
        vec![logo.with_area(areas.logo), subtitle.with_area(areas.subtitle), hint.with_area(areas.hint)]
    }

    /// Draws the splash and advances its effects by `elapsed`.
    pub fn draw(&mut self, frame: &mut Frame, elapsed: Duration) {
        let areas = Areas::new(frame.area());
        let logo: Vec<Line> = LOGO.iter().map(|row| Line::styled(*row, Style::new().fg(CYAN))).collect();
        frame.render_widget(Paragraph::new(logo), areas.logo);
        let subtitle = Line::from(vec![
            Span::styled("GPU accelerated terminal", Style::new().fg(MAGENTA)),
            Span::styled(format!("  ·  {}", env!("CARGO_PKG_VERSION")), Style::new().fg(DIM)),
        ]);
        frame.render_widget(Paragraph::new(subtitle).alignment(Alignment::Center), areas.subtitle);
        let hint = Line::styled("press any key to continue", Style::new().fg(DIM));
        frame.render_widget(Paragraph::new(hint).alignment(Alignment::Center), areas.hint);

        if !self.animations {
            return;
        }
        if self.effects.is_empty() || self.screen != areas.screen {
            // A resize restarts the effects in the new layout, keeping their progress.
            let restart = !self.effects.is_empty();
            self.effects = Self::intro_effects(&areas);
            if restart {
                let progress = self.started.elapsed();
                for effect in &mut self.effects {
                    effect.process(progress, frame.buffer_mut(), areas.screen);
                }
            }
            self.screen = areas.screen;
        }
        let buffer = frame.buffer_mut();
        for effect in &mut self.effects {
            effect.process(elapsed, buffer, areas.screen);
        }
        if let Some((_, effect)) = &mut self.exit {
            effect.process(elapsed, buffer, areas.screen);
        }
    }

    pub fn start_exit(&mut self) {
        let effect = fx::parallel(&[
            fx::dissolve((EXIT.as_millis() as u32, Interpolation::QuadIn)),
            fx::fade_to_fg(DARK, (EXIT.as_millis() as u32, Interpolation::QuadIn)),
        ]);
        self.exit = Some((Instant::now(), effect));
    }

    pub fn exit_done(&self, now: Instant) -> bool {
        self.exit.as_ref().is_some_and(|(since, _)| now.saturating_duration_since(*since) >= EXIT)
    }

    /// Startup shader parameters at `now`: power, grid, glitch, bloom.
    pub fn shader_params(&self, now: Instant) -> [f32; 4] {
        if !self.animations {
            return [1.0, 0.6, 0.0, 0.5];
        }
        let t = now.saturating_duration_since(self.started).as_secs_f32();
        let power = smooth(t / 0.7);
        let mut grid = smooth((t - 0.5) / 1.5) * 0.8;
        let mut glitch = pulse(t, LOGO_DONE, 0.18) * 0.8;
        let bloom = 0.45 + 0.7 * pulse(t, LOGO_DONE, 0.5);
        let mut power = power;
        if let Some((since, _)) = &self.exit {
            let e = (now.saturating_duration_since(*since).as_secs_f32() / EXIT.as_secs_f32()).min(1.0);
            power *= 1.0 - smooth(e);
            grid *= 1.0 - e;
            glitch = glitch.max(0.6 * (1.0 - e) * smooth(e * 4.0));
        }
        [power, grid, glitch, bloom]
    }
}

/// Smoothstep from 0 to 1 for `x` in 0..1.
fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// A bump that peaks at `at` and lasts `width` seconds.
fn pulse(t: f32, at: f32, width: f32) -> f32 {
    (1.0 - ((t - at).abs() / (width / 2.0))).max(0.0)
}

/// `area` narrowed to `width` columns in the middle.
fn centered(area: Rect, width: usize) -> Rect {
    let width = (width as u16).min(area.width);
    Rect { x: area.x + (area.width - width) / 2, width, ..area }
}

/// Shows the splash until a key is pressed, then plays the goodbye animation.
pub fn run<W: Write>(terminal: &mut DefaultTerminal, animations: bool, link: &mut Link<W>) -> io::Result<Outcome> {
    let mut splash = Splash::new(animations);
    link.shader_on(SCENE);
    let mut last = Instant::now();
    let mut outcome = None;
    loop {
        let now = Instant::now();
        let elapsed = now - last;
        last = now;
        terminal.draw(|frame| splash.draw(frame, elapsed))?;
        link.params(splash.shader_params(now));
        if outcome.is_some() && (!animations || splash.exit_done(now)) {
            break;
        }
        let timeout = if animations { FRAME } else { Duration::from_secs(1) };
        if outcome.is_none()
            && event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            outcome = Some(match key.code {
                KeyCode::Enter => Outcome::Setup,
                KeyCode::Char('t') => Outcome::Tour,
                _ => Outcome::Skip,
            });
            splash.start_exit();
        } else if outcome.is_some() {
            std::thread::sleep(FRAME);
        }
    }
    link.shader_off();
    Ok(outcome.unwrap_or(Outcome::Skip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(splash: &mut Splash, elapsed: Duration) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| splash.draw(frame, elapsed)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content().chunks(60).map(|row| row.iter().map(|c| c.symbol()).collect::<String>() + "\n").collect()
    }

    #[test]
    fn logo_coalesces_then_everything_is_shown() {
        let mut splash = Splash::new(true);
        let start = screen(&mut splash, Duration::ZERO);
        assert!(!start.contains("████████╗██████╗"), "{start}");
        let done = screen(&mut splash, Duration::from_secs(3));
        assert!(done.contains("████████╗██████╗"), "{done}");
        assert!(done.contains("GPU accelerated terminal"), "{done}");
    }

    #[test]
    fn without_animations_everything_shows_at_once() {
        let mut splash = Splash::new(false);
        assert!(screen(&mut splash, Duration::ZERO).contains("press any key to continue"));
        assert_eq!(splash.shader_params(Instant::now())[0], 1.0);
    }

    #[test]
    fn shader_turns_on_and_off_with_the_splash() {
        let mut splash = Splash::new(true);
        let start = splash.started;
        assert_eq!(splash.shader_params(start)[0], 0.0);
        assert_eq!(splash.shader_params(start + Duration::from_secs(1))[0], 1.0);
        assert!(splash.shader_params(start + Duration::from_secs_f32(LOGO_DONE))[2] > 0.7, "glitch at the logo");
        splash.start_exit();
        let (exit, _) = splash.exit.as_ref().unwrap();
        let end = *exit + EXIT;
        assert!(splash.exit_done(end));
        assert_eq!(splash.shader_params(end)[0], 0.0);
    }
}
