//! The startup screen's main view: a header, tabs, the selected tab's content
//! and a status bar. Tab changes and the way in and out are animated, on the
//! terminal side with tachyonfx and in tron with the startup shader.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table, Wrap};
use ratatui::{DefaultTerminal, Frame};
use tachyonfx::{Effect, Interpolation, fx};

use crate::catalog::Catalog;
use crate::link::Link;
use crate::motion::{lerp, pulse, smooth};
use crate::pickers::{self, Choices, Item, Picker};
use crate::splash;
use crate::ui::{CYAN, DARK, DIM, MAGENTA, TEXT};

const FRAME: Duration = Duration::from_millis(16);
/// Shader scene number of the tabs.
pub const SCENE: u32 = 2;
/// How long the view takes to settle after the splash.
const INTRO: Duration = Duration::from_millis(600);
const SWITCH_MS: u32 = 260;
/// Length of the goodbye animation.
const EXIT: Duration = Duration::from_millis(450);
/// Grid and bloom strength behind the tabs: quieter than the splash.
const GRID: f32 = 0.35;
const BLOOM: f32 = 0.25;
const SMALL_LOGO: [&str; 2] = ["▀█▀ █▀█ █▀█ █▄ █", " █  █▀▄ █▄█ █ ▀█"];

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Tab {
    Overview,
    Settings,
    Themes,
    Shaders,
    Keys,
    Tour,
    About,
}

impl Tab {
    pub const ALL: [Tab; 7] =
        [Tab::Overview, Tab::Settings, Tab::Themes, Tab::Shaders, Tab::Keys, Tab::Tour, Tab::About];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Settings => "Settings",
            Tab::Themes => "Themes",
            Tab::Shaders => "Shaders",
            Tab::Keys => "Keys",
            Tab::Tour => "Tour",
            Tab::About => "About",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|&tab| tab == self).unwrap_or(0)
    }

    fn offset(self, by: isize) -> Tab {
        let count = Tab::ALL.len() as isize;
        Tab::ALL[(self.index() as isize + by).rem_euclid(count) as usize]
    }
}

struct Areas {
    header: Rect,
    tabs: Rect,
    content: Rect,
    status: Rect,
}

impl Areas {
    fn new(screen: Rect) -> Self {
        let screen = screen.inner(Margin::new(2, 1));
        let [header, tabs, _, content, status] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(screen);
        Self { header, tabs, content, status }
    }
}

/// Something on screen that reacts to the mouse.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Target {
    Tab(Tab),
    StartTerminal,
    /// A row of the Themes or Shaders list, by index.
    Item(usize),
}

/// Wait before previewing, so holding an arrow key does not reload the config for every row.
const PREVIEW_DELAY: Duration = Duration::from_millis(90);

/// Length of the hover fade in and out.
const HOVER_MS: u32 = 160;

/// What a key or click asks for.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Command {
    None,
    Select(Tab),
    /// Move the list selection.
    Move(isize),
    /// A list row was clicked.
    Pick(usize),
    /// Choose the highlighted theme, or turn the highlighted shader on or off.
    Activate,
    /// Leave the startup screen and start the shell.
    StartTerminal,
}

pub struct App {
    animations: bool,
    started: Instant,
    tab: Tab,
    catalog: Catalog,
    shell: String,
    effects: Vec<Effect>,
    /// Screen size the effects were made for.
    screen: Rect,
    /// Screen areas of clickable things, rebuilt every frame.
    hits: Vec<(Rect, Target)>,
    /// What the pointer is over.
    hover: Option<Target>,
    switched: Option<Instant>,
    exit: Option<Instant>,
    themes: Picker,
    shaders: Picker,
    /// What the user picked so far, and what the saved configuration has.
    choices: Choices,
    saved: Choices,
    /// Preview to send once its delay passed, and the last one sent.
    preview_due: Option<(Instant, String)>,
    previewed: String,
}

impl App {
    pub fn new(animations: bool, catalog: Catalog, shell: &[String]) -> Self {
        let saved = Choices {
            theme: catalog.config.theme.clone().unwrap_or_else(|| "tron".to_owned()),
            shaders: catalog.config.shader.files.clone(),
        };
        let theme_index = catalog.themes.iter().position(|t| t.name == saved.theme).unwrap_or(0);
        let previewed = pickers::overlay(&saved.theme, &saved.shaders);
        Self {
            themes: Picker::new(theme_index),
            shaders: Picker::new(0),
            choices: saved.clone(),
            saved,
            preview_due: None,
            previewed,
            animations,
            started: Instant::now(),
            tab: Tab::Overview,
            catalog,
            shell: shell.join(" "),
            effects: Vec::new(),
            screen: Rect::default(),
            hits: Vec::new(),
            hover: None,
            switched: None,
            exit: None,
        }
    }

    fn intro_effects(areas: &Areas) -> Vec<Effect> {
        let fade = |delay: u32, ms: u32| fx::prolong_start(delay, fx::fade_from_fg(DARK, (ms, Interpolation::QuadOut)));
        vec![
            fx::parallel(&[fade(0, 400), fx::coalesce((400, Interpolation::QuadOut))]).with_area(areas.header),
            fade(120, 380).with_area(areas.tabs),
            fx::parallel(&[fade(220, 420), fx::prolong_start(220, fx::coalesce((420, Interpolation::CubicOut)))])
                .with_area(areas.content),
            fade(420, 300).with_area(areas.status),
        ]
    }

    fn select(&mut self, tab: Tab, content: Rect) {
        if tab == self.tab {
            return;
        }
        let lists = [Tab::Themes, Tab::Shaders];
        let preview_changes = lists.contains(&tab) || lists.contains(&self.tab);
        self.tab = tab;
        self.hover = None;
        if preview_changes {
            self.schedule_preview();
        }
        self.switched = Some(Instant::now());
        if self.animations {
            let effect = fx::parallel(&[
                fx::coalesce((SWITCH_MS, Interpolation::QuadOut)),
                fx::fade_from_fg(DARK, (SWITCH_MS, Interpolation::QuadOut)),
            ]);
            self.effects.push(effect.with_area(content));
        }
    }

    fn start_exit(&mut self) {
        if self.exit.is_some() {
            return;
        }
        self.exit = Some(Instant::now());
        if self.animations {
            let millis = EXIT.as_millis() as u32;
            self.effects.push(fx::parallel(&[
                fx::dissolve((millis, Interpolation::QuadIn)),
                fx::fade_to_fg(DARK, (millis, Interpolation::QuadIn)),
            ]));
        }
    }

    /// Carries out a command. `content` is the tab content area, for effects.
    fn apply(&mut self, command: Command, content: Rect) {
        match command {
            Command::None => {}
            Command::Select(tab) => self.select(tab, content),
            Command::StartTerminal => self.start_exit(),
            Command::Move(delta) => {
                let moved = match self.tab {
                    Tab::Themes => self.themes.move_by(delta, self.catalog.themes.len()),
                    Tab::Shaders => self.shaders.move_by(delta, self.catalog.shaders.len()),
                    _ => false,
                };
                if moved {
                    self.schedule_preview();
                }
            }
            Command::Pick(index) => {
                let picker = if self.tab == Tab::Themes { &mut self.themes } else { &mut self.shaders };
                if picker.selected == index {
                    self.apply(Command::Activate, content);
                } else {
                    let len =
                        if self.tab == Tab::Themes { self.catalog.themes.len() } else { self.catalog.shaders.len() };
                    picker.set(index, len);
                    self.schedule_preview();
                }
            }
            Command::Activate => match self.tab {
                Tab::Themes => {
                    if let Some(theme) = self.catalog.themes.get(self.themes.selected) {
                        self.choices.theme = theme.name.clone();
                    }
                }
                Tab::Shaders => {
                    if let Some(shader) = self.catalog.shaders.get(self.shaders.selected) {
                        match self.choices.shaders.iter().position(|file| *file == shader.file) {
                            Some(index) => {
                                self.choices.shaders.remove(index);
                            }
                            None => self.choices.shaders.push(shader.file.clone()),
                        }
                        self.schedule_preview();
                    }
                }
                _ => {}
            },
        }
    }

    /// The configuration to show: the choices, plus the highlighted entry on its tab.
    fn preview_overlay(&self) -> String {
        let theme = match self.tab {
            Tab::Themes => self.catalog.themes.get(self.themes.selected).map_or(&self.choices.theme, |t| &t.name),
            _ => &self.choices.theme,
        };
        let mut shaders = self.choices.shaders.clone();
        if self.tab == Tab::Shaders
            && let Some(shader) = self.catalog.shaders.get(self.shaders.selected)
            && !shaders.contains(&shader.file)
        {
            shaders.push(shader.file.clone());
        }
        pickers::overlay(theme, &shaders)
    }

    fn schedule_preview(&mut self) {
        self.preview_due = Some((Instant::now() + PREVIEW_DELAY, self.preview_overlay()));
    }

    /// A preview to send now, when one is due and differs from the last.
    fn take_preview(&mut self, now: Instant) -> Option<String> {
        match &self.preview_due {
            Some((due, _)) if *due <= now => {
                let (_, overlay) = self.preview_due.take()?;
                (overlay != self.previewed).then(|| {
                    self.previewed = overlay.clone();
                    overlay
                })
            }
            _ => None,
        }
    }

    /// The startup effects pause on the Shaders tab so shaders show on their own,
    /// and come back for the goodbye animation.
    fn wants_startup_shader(&self) -> bool {
        self.tab != Tab::Shaders || self.exit.is_some()
    }

    fn exit_done(&self, now: Instant) -> bool {
        self.exit.is_some_and(|since| !self.animations || now.saturating_duration_since(since) >= EXIT)
    }

    fn on_key(&self, key: KeyEvent) -> Command {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if matches!(self.tab, Tab::Themes | Tab::Shaders) {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => return Command::Move(-1),
                KeyCode::Down | KeyCode::Char('j') => return Command::Move(1),
                KeyCode::PageUp => return Command::Move(-10),
                KeyCode::PageDown => return Command::Move(10),
                KeyCode::Home => return Command::Move(isize::MIN / 2),
                KeyCode::End => return Command::Move(isize::MAX / 2),
                KeyCode::Enter | KeyCode::Char(' ') => return Command::Activate,
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => Command::StartTerminal,
            KeyCode::Char('c' | 'q') if ctrl => Command::StartTerminal,
            KeyCode::Enter if self.tab == Tab::Overview => Command::StartTerminal,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => Command::Select(self.tab.offset(1)),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => Command::Select(self.tab.offset(-1)),
            KeyCode::Char(digit @ '1'..='7') => Command::Select(Tab::ALL[digit as usize - '1' as usize]),
            _ => Command::None,
        }
    }

    fn target_at(&self, column: u16, row: u16) -> Option<Target> {
        let point = ratatui::layout::Position::new(column, row);
        self.hits.iter().find(|(area, _)| area.contains(point)).map(|(_, target)| *target)
    }

    fn on_click(&self, column: u16, row: u16) -> Command {
        match self.target_at(column, row) {
            Some(Target::Tab(tab)) => Command::Select(tab),
            Some(Target::StartTerminal) => Command::StartTerminal,
            Some(Target::Item(index)) => Command::Pick(index),
            None => Command::None,
        }
    }

    /// Updates what the pointer is over, fading the old target out and the new one in.
    fn on_move(&mut self, column: u16, row: u16) {
        let target = self.target_at(column, row);
        if target == self.hover {
            return;
        }
        if self.animations {
            let area_of =
                |target: Option<Target>| self.hits.iter().find(|(_, t)| Some(*t) == target).map(|(area, _)| *area);
            let (old, new) = (area_of(self.hover), area_of(target));
            if let Some(area) = old {
                self.effects.push(fx::fade_from_fg(TEXT, (HOVER_MS, Interpolation::QuadOut)).with_area(area));
            }
            if let Some(area) = new {
                self.effects.push(fx::fade_from_fg(DIM, (HOVER_MS, Interpolation::QuadOut)).with_area(area));
            }
        }
        self.hover = target;
    }

    /// Startup shader parameters at `now`: power, grid, glitch, bloom.
    pub fn shader_params(&self, now: Instant) -> [f32; 4] {
        if !self.animations {
            return [if self.exit_done(now) { 0.0 } else { 1.0 }, GRID, 0.0, BLOOM];
        }
        let settle = smooth(now.saturating_duration_since(self.started).as_secs_f32() / INTRO.as_secs_f32());
        let mut power = 1.0;
        let mut grid = lerp(splash::GRID, GRID, settle);
        let bloom = lerp(splash::BLOOM, BLOOM, settle);
        let mut glitch =
            self.switched.map_or(0.0, |at| pulse(now.saturating_duration_since(at).as_secs_f32(), 0.07, 0.14) * 0.35);
        if let Some(since) = self.exit {
            let e = (now.saturating_duration_since(since).as_secs_f32() / EXIT.as_secs_f32()).min(1.0);
            power = 1.0 - smooth(e);
            grid *= 1.0 - e;
            glitch = glitch.max(0.6 * (1.0 - e) * smooth(e * 4.0));
        }
        [power, grid, glitch, bloom]
    }

    pub fn draw(&mut self, frame: &mut Frame, elapsed: Duration) {
        let screen = frame.area();
        let areas = Areas::new(screen);
        self.hits.clear();
        self.draw_header(frame, areas.header);
        self.draw_tabs(frame, areas.tabs);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(DIM))
            .title(Line::styled(format!(" {} ", self.tab.title()), Style::new().fg(CYAN).add_modifier(Modifier::BOLD)));
        let inner = block.inner(areas.content).inner(Margin::new(2, 1));
        frame.render_widget(block, areas.content);
        match self.tab {
            Tab::Overview => self.draw_overview(frame, inner),
            Tab::Themes => self.draw_themes(frame, inner),
            Tab::Shaders => self.draw_shaders(frame, inner),
            Tab::Keys => draw_keys(frame, inner),
            Tab::About => draw_about(frame, inner),
            tab => draw_upcoming(frame, inner, tab),
        }
        self.draw_status(frame, areas.status);

        if !self.animations {
            return;
        }
        if self.screen != screen {
            // Effects are tied to areas; after a resize, start from the settled view.
            let first = self.screen == Rect::default();
            self.effects.clear();
            if first {
                self.effects = Self::intro_effects(&areas);
            }
            self.screen = screen;
        }
        let buffer = frame.buffer_mut();
        for effect in &mut self.effects {
            effect.process(elapsed, buffer, screen);
        }
        let exiting = self.exit.is_some();
        self.effects.retain(|effect| effect.running() || exiting);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let logo = Text::from(
            SMALL_LOGO.map(|row| Line::styled(row, Style::new().fg(CYAN).add_modifier(Modifier::BOLD))).to_vec(),
        );
        frame.render_widget(Paragraph::new(logo), area);
        let right = Text::from(vec![
            Line::styled("GPU accelerated terminal", Style::new().fg(MAGENTA)),
            Line::styled(format!("version {}", env!("CARGO_PKG_VERSION")), Style::new().fg(DIM)),
        ]);
        frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
    }

    fn draw_tabs(&mut self, frame: &mut Frame, area: Rect) {
        let mut spans = Vec::new();
        let mut column = area.x;
        for (index, tab) in Tab::ALL.into_iter().enumerate() {
            let label = format!(" {} {} ", index + 1, tab.title());
            let width = label.chars().count() as u16;
            let style = if tab == self.tab {
                Style::new().fg(CYAN).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else if self.hover == Some(Target::Tab(tab)) {
                Style::new().fg(TEXT).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(DIM)
            };
            self.hits.push((Rect::new(column, area.y, width, 1).intersection(area), Target::Tab(tab)));
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));
            column += width + 1;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_status(&mut self, frame: &mut Frame, area: Rect) {
        let key = |text: &'static str| Span::styled(text, Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD));
        let label = |text: &'static str| Span::styled(text, Style::new().fg(DIM));
        let hints = match self.tab {
            Tab::Themes => {
                vec![key("↑/↓"), label(" browse  "), key("Enter"), label(" choose  "), key("←/→"), label(" tabs")]
            }
            Tab::Shaders => {
                vec![key("↑/↓"), label(" browse  "), key("Space"), label(" on/off  "), key("←/→"), label(" tabs")]
            }
            _ => vec![key("←/→"), label(" switch  "), key("1-7"), label(" jump  "), key("click"), label(" select")],
        };
        frame.render_widget(Paragraph::new(Line::from(hints)), area);
        if self.choices != self.saved {
            let changed = Line::styled("● changed, not saved yet", Style::new().fg(MAGENTA));
            frame.render_widget(Paragraph::new(changed).centered(), area);
        }
        let hovered = self.hover == Some(Target::StartTerminal);
        let start_label = if hovered {
            Span::styled(" start terminal", Style::new().fg(TEXT).add_modifier(Modifier::UNDERLINED))
        } else {
            label(" start terminal")
        };
        let start = Line::from(vec![key("Esc"), start_label]);
        let width = start.width() as u16;
        let start_area = Rect::new(area.right().saturating_sub(width), area.y, width, 1).intersection(area);
        self.hits.push((start_area, Target::StartTerminal));
        frame.render_widget(Paragraph::new(start), start_area);
    }

    fn draw_themes(&mut self, frame: &mut Frame, area: Rect) {
        let [list_area, _, preview_area] =
            Layout::horizontal([Constraint::Length(28), Constraint::Length(2), Constraint::Fill(1)]).areas(area);
        let items: Vec<Item> = self
            .catalog
            .themes
            .iter()
            .map(|theme| Item {
                name: &theme.name,
                tag: if theme.builtin { "" } else { "custom" },
                chosen: theme.name == self.choices.theme,
            })
            .collect();
        let hover = match self.hover {
            Some(Target::Item(index)) => Some(index),
            _ => None,
        };
        let rows = pickers::draw_list(frame, list_area, &items, &mut self.themes, hover, true);
        self.hits.extend(rows.into_iter().map(|(rect, index)| (rect, Target::Item(index))));
        if let Some(theme) = self.catalog.themes.get(self.themes.selected) {
            pickers::draw_theme_preview(frame, preview_area, theme, theme.name == self.choices.theme);
        }
    }

    fn draw_shaders(&mut self, frame: &mut Frame, area: Rect) {
        let [list_area, _, details_area] =
            Layout::horizontal([Constraint::Length(28), Constraint::Length(2), Constraint::Fill(1)]).areas(area);
        let items: Vec<Item> = self
            .catalog
            .shaders
            .iter()
            .map(|shader| Item {
                name: &shader.file,
                tag: if shader.builtin { "" } else { "custom" },
                chosen: self.choices.shaders.contains(&shader.file),
            })
            .collect();
        let hover = match self.hover {
            Some(Target::Item(index)) => Some(index),
            _ => None,
        };
        let rows = pickers::draw_list(frame, list_area, &items, &mut self.shaders, hover, true);
        self.hits.extend(rows.into_iter().map(|(rect, index)| (rect, Target::Item(index))));
        if let Some(shader) = self.catalog.shaders.get(self.shaders.selected) {
            let position = self.choices.shaders.iter().position(|file| *file == shader.file);
            pickers::draw_shader_details(frame, details_area, shader, position, &self.choices.shaders);
        }
    }

    fn draw_overview(&mut self, frame: &mut Frame, area: Rect) {
        let heading = Style::new().fg(CYAN).add_modifier(Modifier::BOLD);
        let label = Style::new().fg(DIM);
        let value = Style::new().fg(TEXT);
        let config = self
            .catalog
            .paths
            .as_ref()
            .map_or_else(|| "defaults (no config directory)".to_owned(), |p| tron_config::display_path(&p.config_file));
        let value_width = usize::from(area.width).saturating_sub(12);
        let field = |name: &'static str, text: String| {
            Line::from(vec![Span::styled(format!("  {name:<10}"), label), Span::styled(fit(&text, value_width), value)])
        };
        let mut lines = vec![
            Line::styled("Welcome to tron", heading),
            Line::raw(""),
            Line::styled(
                "A GPU accelerated terminal with themes, shaders and images. Look around, pick a look, then start the terminal.",
                value,
            ),
            Line::raw(""),
            field("Shell", self.shell.clone()),
            field("Config", config),
            field("Themes", format!("{} available", self.catalog.themes.len())),
            field("Shaders", format!("{} available", self.catalog.shaders.len())),
        ];
        let [text_area, hints_area] = Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(area);
        frame.render_widget(Paragraph::new(std::mem::take(&mut lines)).wrap(Wrap { trim: false }), text_area);

        let hovered = self.hover == Some(Target::StartTerminal);
        let start_label = if hovered { Style::new().fg(TEXT).add_modifier(Modifier::UNDERLINED) } else { label };
        let start = Line::from(vec![
            Span::styled("Enter", Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD)),
            Span::styled("  start the terminal", start_label),
        ]);
        let start_area = Rect::new(hints_area.x, hints_area.y, start.width() as u16, 1).intersection(hints_area);
        self.hits.push((start_area, Target::StartTerminal));
        frame.render_widget(Paragraph::new(start), start_area);
        let again = Line::from(vec![
            Span::styled("tron --startup", Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD)),
            Span::styled("  opens this screen again", label),
        ]);
        let again_area = Rect { y: hints_area.y + 1, height: 1, ..hints_area }.intersection(hints_area);
        frame.render_widget(Paragraph::new(again), again_area);
    }
}

fn draw_keys(frame: &mut Frame, area: Rect) {
    use tron_config::Action;
    // Grouped the way people look for them: clipboard, search, scrolling, prompts, fonts, window.
    let order = |action: &Action| match action {
        Action::Copy => 0,
        Action::Paste => 1,
        Action::PasteSelection => 2,
        Action::Search => 3,
        Action::ScrollPageUp => 4,
        Action::ScrollPageDown => 5,
        Action::ScrollToTop => 6,
        Action::ScrollToBottom => 7,
        Action::ScrollToPreviousPrompt => 8,
        Action::ScrollToNextPrompt => 9,
        Action::SelectCommandOutput => 10,
        Action::IncreaseFontSize => 11,
        Action::DecreaseFontSize => 12,
        Action::ResetFontSize => 13,
        Action::NewWindow => 14,
        Action::ReloadConfig => 15,
        _ => 16,
    };
    let (mut bindings, _) = tron_config::Config::default().bindings();
    bindings.sort_by_key(|binding| order(&binding.action));
    // Two columns when the list would not fit, as long as there is room for them.
    let columns = if bindings.len() + 1 > usize::from(area.height) && area.width >= 80 { 2 } else { 1 };
    let per_column = bindings.len().div_ceil(columns);
    let areas = Layout::horizontal(vec![Constraint::Fill(1); columns]).spacing(4).split(area);
    for (column, chunk) in bindings.chunks(per_column.max(1)).enumerate() {
        let rows = chunk.iter().map(|binding| {
            Row::new(vec![
                Cell::from(Span::styled(
                    combo_text(&binding.combo),
                    Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD),
                )),
                Cell::from(Span::styled(action_text(&binding.action), Style::new().fg(TEXT))),
            ])
        });
        let header = Row::new(vec!["Keys", "Action"]).style(Style::new().fg(CYAN).add_modifier(Modifier::BOLD));
        let table = Table::new(rows, [Constraint::Length(16), Constraint::Fill(1)]).header(header).column_spacing(2);
        frame.render_widget(table, areas[column]);
    }
}

/// `text` shortened to `width` characters with an ellipsis in the middle.
fn fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width || width < 5 {
        return text.to_owned();
    }
    let keep = width - 1;
    let head: String = text.chars().take(keep / 2).collect();
    let tail: String = text.chars().skip(count - (keep - keep / 2)).collect();
    format!("{head}…{tail}")
}

fn draw_about(frame: &mut Frame, area: Rect) {
    let label = Style::new().fg(DIM);
    let value = Style::new().fg(TEXT);
    let field = |name: &'static str, text: &'static str| {
        Line::from(vec![Span::styled(format!("{name:<10}"), label), Span::styled(text, value)])
    };
    let lines = vec![
        Line::styled(format!("tron {}", env!("CARGO_PKG_VERSION")), Style::new().fg(CYAN).add_modifier(Modifier::BOLD)),
        Line::styled("GPU accelerated terminal emulator, written in Rust.", value),
        Line::raw(""),
        field("License", "MIT OR Apache-2.0"),
        field("Source", "https://github.com/skyline69/tron-terminal"),
        field("Built on", "wgpu, harfrust, swash, fontique, winit, ratatui, tachyonfx"),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_upcoming(frame: &mut Frame, area: Rect, tab: Tab) {
    let text = match tab {
        Tab::Settings => "Font, size, opacity, cursor and more, previewed live.",
        _ => "Ligatures, emoji, right-to-left text, images and scaled text.",
    };
    let lines = vec![
        Line::styled(text, Style::new().fg(TEXT)),
        Line::raw(""),
        Line::styled("Coming soon.", Style::new().fg(DIM)),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

/// `ctrl+shift+c` style text for a key combination.
fn combo_text(combo: &tron_config::KeyCombo) -> String {
    let mut parts = Vec::new();
    for (held, name) in [(combo.ctrl, "Ctrl"), (combo.alt, "Alt"), (combo.shift, "Shift"), (combo.super_key, "Super")] {
        if held {
            parts.push(name.to_owned());
        }
    }
    parts.push(match &combo.key {
        tron_config::BindKey::Char(' ') => "Space".to_owned(),
        tron_config::BindKey::Char(c) => c.to_uppercase().collect(),
        tron_config::BindKey::Named(name) => {
            name.split('_').map(|word| word[..1].to_uppercase() + &word[1..]).collect::<Vec<_>>().join(" ")
        }
    });
    parts.join("+")
}

/// "Scroll to previous prompt" for `ScrollToPreviousPrompt`.
fn action_text(action: &tron_config::Action) -> String {
    let name = format!("{action:?}");
    let name = name.split('(').next().unwrap_or(&name);
    let mut text = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            text.push(' ');
            text.extend(ch.to_lowercase());
        } else {
            text.push(ch);
        }
    }
    text
}

/// Runs the tabs until the user starts the terminal, then plays the goodbye animation.
pub fn run<W: Write>(
    terminal: &mut DefaultTerminal,
    animations: bool,
    link: &mut Link<W>,
    shell: &[String],
    tab: Option<Tab>,
) -> io::Result<()> {
    let mut app = App::new(animations, Catalog::load(), shell);
    app.tab = tab.unwrap_or(Tab::Overview);
    if matches!(app.tab, Tab::Themes | Tab::Shaders) {
        app.schedule_preview();
    }
    ratatui::crossterm::execute!(io::stdout(), EnableMouseCapture)?;
    link.scene(SCENE);
    let mut last = Instant::now();
    let mut pointer_hand = false;
    let mut shader_running = true;
    let result = loop {
        let now = Instant::now();
        let elapsed = now - last;
        last = now;
        if let Err(error) = terminal.draw(|frame| app.draw(frame, elapsed)) {
            break Err(error);
        }
        link.params(app.shader_params(now));
        if app.exit_done(now) {
            break Ok(());
        }
        let content = Areas::new(terminal.get_frame().area()).content;
        match event::poll(FRAME) {
            Ok(true) if app.exit.is_none() => {
                let command = match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => app.on_click(mouse.column, mouse.row),
                        MouseEventKind::ScrollUp => Command::Move(-1),
                        MouseEventKind::ScrollDown => Command::Move(1),
                        MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                            app.on_move(mouse.column, mouse.row);
                            Command::None
                        }
                        _ => Command::None,
                    },
                    _ => Command::None,
                };
                // A hand pointer over clickable things (OSC 22, supported by tron and xterm).
                let hand = app.hover.is_some();
                if hand != pointer_hand {
                    pointer_hand = hand;
                    set_pointer(if hand { "pointer" } else { "default" });
                }
                app.apply(command, content);
            }
            Ok(_) => {}
            Err(error) => break Err(error),
        }
        if let Some(overlay) = app.take_preview(Instant::now()) {
            link.preview(&overlay);
        }
        let wants_shader = app.wants_startup_shader();
        if wants_shader != shader_running {
            shader_running = wants_shader;
            if wants_shader {
                link.shader_on(SCENE);
            } else {
                link.shader_off();
            }
        }
    };
    if pointer_hand {
        set_pointer("");
    }
    let _ = ratatui::crossterm::execute!(io::stdout(), DisableMouseCapture);
    result
}

/// Asks the terminal for a mouse pointer shape by CSS name, or the default for "".
fn set_pointer(name: &str) {
    let mut out = io::stdout();
    let _ = write!(out, "\x1b]22;{name}\x07");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app() -> App {
        App::new(true, Catalog::for_paths(None), &["fish".into(), "-l".into()])
    }

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame, Duration::from_secs(2))).unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content().chunks(100).map(|row| row.iter().map(|c| c.symbol()).collect::<String>() + "\n").collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn keys_move_between_tabs_and_start_the_terminal() {
        let mut app = app();
        assert_eq!(app.on_key(key(KeyCode::Left)), Command::Select(Tab::About), "wraps around");
        assert_eq!(app.on_key(key(KeyCode::Char('3'))), Command::Select(Tab::Themes));
        assert_eq!(app.on_key(key(KeyCode::Enter)), Command::StartTerminal);
        app.select(Tab::Keys, Rect::default());
        assert_eq!(app.on_key(key(KeyCode::Tab)), Command::Select(Tab::Tour));
        assert_eq!(app.on_key(key(KeyCode::Enter)), Command::None, "Enter only starts from the overview");
        assert_eq!(app.on_key(key(KeyCode::Esc)), Command::StartTerminal);
    }

    #[test]
    fn every_tab_renders_and_titles_are_clickable() {
        let mut app = app();
        let overview = screen(&mut app);
        assert!(overview.contains("1 Overview") && overview.contains("Welcome to tron"), "{overview}");
        assert!(overview.contains("fish -l"), "{overview}");
        let (area, target) = app.hits[4];
        assert_eq!(target, Target::Tab(Tab::Keys));
        assert_eq!(app.on_click(area.x + 1, 99), Command::None);
        assert_eq!(app.on_click(area.x + 1, area.y), Command::Select(Tab::Keys));
        let (start, _) = *app.hits.iter().find(|(_, t)| *t == Target::StartTerminal).unwrap();
        assert_eq!(app.on_click(start.x, start.y), Command::StartTerminal);
        for tab in Tab::ALL {
            app.tab = tab;
            let text = screen(&mut app);
            assert!(text.contains(&format!(" {} ", tab.title())), "{tab:?}: {text}");
        }
        app.tab = Tab::Keys;
        assert!(screen(&mut app).contains("Scroll to previous prompt"));
    }

    #[test]
    fn hovering_highlights_and_fades() {
        let mut app = app();
        screen(&mut app);
        let effects = app.effects.len();
        let (area, _) = app.hits[2];
        app.on_move(area.x + 1, area.y);
        assert_eq!(app.hover, Some(Target::Tab(Tab::Themes)));
        assert_eq!(app.effects.len(), effects + 1, "fade in");
        app.on_move(area.x + 1, area.y);
        assert_eq!(app.effects.len(), effects + 1, "no new effect without a change");
        app.on_move(0, 0);
        assert_eq!(app.hover, None);
        assert_eq!(app.effects.len(), effects + 2, "fade out");
    }

    fn catalog() -> Catalog {
        Catalog::for_paths(None)
    }

    #[test]
    fn themes_preview_while_browsing_and_enter_chooses() {
        let mut app = App::new(true, catalog(), &[]);
        app.apply(Command::Select(Tab::Themes), Rect::default());
        assert_eq!(app.themes.selected, 0, "starts at the saved theme");
        assert_eq!(app.on_key(key(KeyCode::Down)), Command::Move(1));
        app.apply(Command::Move(1), Rect::default());
        let later = Instant::now() + PREVIEW_DELAY;
        let overlay = app.take_preview(later).expect("a preview is due");
        let name = app.catalog.themes[1].name.clone();
        assert!(overlay.contains(&format!("theme = \"{name}\"")), "{overlay}");
        assert_eq!(app.take_preview(later), None, "sent once");
        assert_eq!(app.choices, app.saved, "browsing alone changes nothing");
        app.apply(Command::Activate, Rect::default());
        assert_eq!(app.choices.theme, name);
        let text = screen(&mut app);
        assert!(text.contains("● chosen") && text.contains("cargo build"), "{text}");
        assert!(text.contains("changed, not saved yet"), "{text}");
    }

    #[test]
    fn shaders_toggle_and_pause_the_startup_effects() {
        let mut app = App::new(true, catalog(), &[]);
        assert!(app.wants_startup_shader());
        app.apply(Command::Select(Tab::Shaders), Rect::default());
        assert!(!app.wants_startup_shader());
        let first = app.catalog.shaders[0].file.clone();
        let preview = app.take_preview(Instant::now() + PREVIEW_DELAY).unwrap();
        assert!(preview.contains(&first), "the highlighted shader is previewed: {preview}");
        app.apply(Command::Pick(0), Rect::default());
        assert_eq!(app.choices.shaders, vec![first.clone()]);
        app.apply(Command::Move(1), Rect::default());
        app.apply(Command::Activate, Rect::default());
        assert_eq!(app.choices.shaders.len(), 2);
        let text = screen(&mut app);
        assert!(text.contains("runs 2nd"), "{text}");
        app.apply(Command::Select(Tab::Keys), Rect::default());
        assert!(app.wants_startup_shader());
        let back = app.take_preview(Instant::now() + PREVIEW_DELAY).unwrap();
        assert!(!back.contains(&app.catalog.shaders[2].file), "leaving drops the highlight: {back}");
    }

    #[test]
    fn leaving_powers_the_screen_down() {
        let mut app = app();
        let now = app.started + INTRO;
        assert_eq!(app.shader_params(now)[1], GRID);
        app.start_exit();
        let end = app.exit.unwrap() + EXIT;
        assert!(app.exit_done(end));
        assert_eq!(app.shader_params(end)[0], 0.0);
    }

    #[test]
    fn long_values_are_shortened_in_the_middle() {
        assert_eq!(fit("/home/user/.config/tron/config.toml", 20), "/home/use…onfig.toml");
        assert_eq!(fit("short", 20), "short");
    }

    #[test]
    fn key_and_action_names_read_well() {
        let combo = tron_config::KeyCombo::parse("ctrl+shift+page_up").unwrap();
        assert_eq!(combo_text(&combo), "Ctrl+Shift+Page Up");
        assert_eq!(action_text(&tron_config::Action::ScrollToPreviousPrompt), "Scroll to previous prompt");
    }
}
