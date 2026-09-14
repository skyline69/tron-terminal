//! The startup screen's main view: a header, tabs, the selected tab's content
//! and a status bar. Tab changes and the way in and out are animated, on the
//! terminal side with tachyonfx and in tron with the startup shader.

use std::io::{self, Write};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, Wrap};
use ratatui::{DefaultTerminal, Frame};
use tachyonfx::{Effect, Interpolation, fx};

use crate::catalog::Catalog;
use crate::link::Link;
use crate::motion::{lerp, pulse, smooth};
use crate::pickers::{self, Item, Picker};
use crate::settings::{self, Choices, Setting};
use crate::splash;
use crate::tour::{self, Page};
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
    /// A row of the Themes, Shaders or Settings list, by index.
    Item(usize),
    /// The unsaved changes note in the status bar.
    SaveButton,
    DialogButton(DialogButton),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum DialogButton {
    Save,
    Discard,
    Cancel,
}

/// A question over the tabs.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Dialog {
    /// Confirm saving, showing what changes.
    Save,
    /// Unsaved changes when starting the terminal.
    Leave,
}

/// How long a message stays in the status bar.
const TOAST: Duration = Duration::from_secs(4);

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
    /// Change the highlighted setting by steps.
    Change(isize),
    OpenSave,
    OpenLeave,
    Dialog(DialogButton),
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
    settings: Vec<Setting>,
    settings_picker: Picker,
    /// Installed fonts, listed on a background thread.
    fonts: Option<Receiver<Vec<String>>>,
    dialog: Option<Dialog>,
    /// The dialog opened this frame, so it gets its entrance effect.
    dialog_fresh: bool,
    /// A short message in the status bar and when it appeared.
    toast: Option<(Instant, String)>,
    /// A save finished; tron must stop previewing and reload the file.
    commit_pending: bool,
    tour: Picker,
    /// Where the tour page draws its demo, as of the last frame.
    tour_demo: Option<Rect>,
    /// The screen must be cleared, removing a painted tour page.
    tour_clear: bool,
    /// When to paint the tour page, once transition effects are done.
    tour_paint_at: Option<Instant>,
    tour_image_sent: bool,
    /// Screen size of the last frame, to repaint the tour after a resize.
    last_screen: Rect,
}

impl App {
    pub fn new(animations: bool, catalog: Catalog, shell: &[String]) -> Self {
        let saved = Choices::from_config(&catalog.config);
        let theme_index = catalog.themes.iter().position(|t| t.name == saved.theme).unwrap_or(0);
        let previewed = saved.overlay(&saved.theme, &saved.shaders);
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let _ = sender.send(crate::catalog::monospace_families());
        });
        Self {
            settings: settings::list(std::slice::from_ref(&catalog.config.font.family)),
            settings_picker: Picker::new(0),
            fonts: Some(receiver),
            dialog: None,
            dialog_fresh: false,
            toast: None,
            commit_pending: false,
            tour: Picker::new(0),
            tour_demo: None,
            tour_clear: false,
            tour_paint_at: None,
            tour_image_sent: false,
            last_screen: Rect::default(),
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
        if tab == Tab::Tour || self.tab == Tab::Tour {
            self.repaint_tour();
        }
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
        if self.tab == Tab::Tour {
            self.repaint_tour();
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
        // A dialog blocks everything else, including the mouse wheel.
        if self.dialog.is_some() && !matches!(command, Command::Dialog(_) | Command::None) {
            return;
        }
        match command {
            Command::None => {}
            Command::Select(tab) => self.select(tab, content),
            Command::StartTerminal => self.start_exit(),
            Command::Move(delta) => {
                let moved = match self.tab {
                    Tab::Settings => {
                        self.settings_picker.move_by(delta, self.settings.len());
                        false
                    }
                    Tab::Tour => {
                        if self.tour.move_by(delta, Page::ALL.len()) {
                            self.repaint_tour();
                        }
                        false
                    }
                    Tab::Themes => self.themes.move_by(delta, self.catalog.themes.len()),
                    Tab::Shaders => self.shaders.move_by(delta, self.catalog.shaders.len()),
                    _ => false,
                };
                if moved {
                    self.schedule_preview();
                }
            }
            Command::Pick(index) if self.tab == Tab::Tour => {
                if self.tour.set(index, Page::ALL.len()) {
                    self.repaint_tour();
                }
            }
            Command::Pick(index) if self.tab == Tab::Settings => {
                if self.settings_picker.selected == index {
                    self.apply(Command::Change(1), content);
                } else {
                    self.settings_picker.set(index, self.settings.len());
                }
            }
            Command::Change(delta) => {
                if let Some(setting) = self.settings.get(self.settings_picker.selected)
                    && let Some(current) = self.choices.settings.get(setting.key)
                {
                    let next = settings::step(setting, current, delta);
                    self.choices.settings.insert(setting.key, next);
                    self.schedule_preview();
                }
            }
            Command::OpenSave => {
                if self.choices == self.saved {
                    self.show_toast("No changes to save");
                } else {
                    self.open_dialog(Dialog::Save);
                }
            }
            Command::OpenLeave => self.open_dialog(Dialog::Leave),
            Command::Dialog(button) => {
                let dialog = self.dialog.take();
                match button {
                    DialogButton::Cancel => {}
                    DialogButton::Discard => self.start_exit(),
                    DialogButton::Save => {
                        if self.save() && dialog == Some(Dialog::Leave) {
                            self.start_exit();
                        }
                    }
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
        self.choices.overlay(theme, &shaders)
    }

    /// Clears the tour page from the screen and paints the current one after the
    /// transition, when effects no longer touch the demo area.
    fn repaint_tour(&mut self) {
        self.tour_clear = true;
        let delay = if self.animations { Duration::from_millis(SWITCH_MS as u64 + 80) } else { Duration::ZERO };
        let intro = if self.animations { self.started + INTRO + Duration::from_millis(100) } else { self.started };
        self.tour_paint_at = Some((Instant::now() + delay).max(intro));
    }

    /// The tour page to paint now, with its area.
    fn tour_paint(&mut self, now: Instant) -> Option<(Page, Rect)> {
        let due = self.tour_paint_at.is_some_and(|at| at <= now);
        if !due || self.tab != Tab::Tour || self.exit.is_some() || self.dialog.is_some() {
            return None;
        }
        self.tour_paint_at = None;
        Some((Page::ALL[self.tour.selected], self.tour_demo?))
    }

    fn open_dialog(&mut self, dialog: Dialog) {
        self.dialog = Some(dialog);
        self.dialog_fresh = true;
        self.hover = None;
    }

    fn show_toast(&mut self, text: impl Into<String>) {
        self.toast = Some((Instant::now(), text.into()));
    }

    /// Writes the choices to `config.toml`. Returns whether that worked.
    fn save(&mut self) -> bool {
        let Some(paths) = self.catalog.paths.clone() else {
            self.show_toast("No configuration directory to save to");
            return false;
        };
        match self.choices.save(&self.saved, &paths) {
            Ok(()) => {
                self.saved = self.choices.clone();
                self.previewed = self.saved.overlay(&self.saved.theme, &self.saved.shaders);
                self.commit_pending = true;
                // A highlighted list entry that is not the choice is previewed again after the reload.
                self.schedule_preview();
                self.show_toast(format!("Saved to {}", tron_config::display_path(&paths.config_file)));
                true
            }
            Err(error) => {
                self.show_toast(format!("Could not save: {error}"));
                false
            }
        }
    }

    /// Starting the terminal asks first when there are unsaved changes.
    fn leave(&self) -> Command {
        if self.choices == self.saved { Command::StartTerminal } else { Command::OpenLeave }
    }

    /// Takes the font list once the background thread has it.
    fn poll_fonts(&mut self) {
        let Some(mut families) = self.fonts.as_ref().and_then(|receiver| receiver.try_recv().ok()) else { return };
        for choices in [&self.saved, &self.choices] {
            if let Some(toml::Value::String(family)) = choices.settings.get("font.family")
                && !families.contains(family)
            {
                families.insert(1.min(families.len()), family.clone());
            }
        }
        self.settings = settings::list(&families);
        self.fonts = None;
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
        if let Some(dialog) = self.dialog {
            return match key.code {
                KeyCode::Enter | KeyCode::Char('y' | 's') => Command::Dialog(DialogButton::Save),
                KeyCode::Char('d') if dialog == Dialog::Leave => Command::Dialog(DialogButton::Discard),
                KeyCode::Esc | KeyCode::Char('n') => Command::Dialog(DialogButton::Cancel),
                _ => Command::None,
            };
        }
        if ctrl && key.code == KeyCode::Char('s') {
            return Command::OpenSave;
        }
        if self.tab == Tab::Tour {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => return Command::Move(-1),
                KeyCode::Down | KeyCode::Char('j') => return Command::Move(1),
                _ => {}
            }
        }
        if self.tab == Tab::Settings {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => return Command::Move(-1),
                KeyCode::Down | KeyCode::Char('j') => return Command::Move(1),
                KeyCode::PageUp => return Command::Move(-10),
                KeyCode::PageDown => return Command::Move(10),
                KeyCode::Left | KeyCode::Char('h') => return Command::Change(-1),
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter | KeyCode::Char(' ') => {
                    return Command::Change(1);
                }
                _ => {}
            }
        }
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
            KeyCode::Esc => self.leave(),
            KeyCode::Char('c' | 'q') if ctrl => Command::StartTerminal,
            KeyCode::Enter if self.tab == Tab::Overview => self.leave(),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => Command::Select(self.tab.offset(1)),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => Command::Select(self.tab.offset(-1)),
            KeyCode::Char(digit @ '1'..='7') => Command::Select(Tab::ALL[digit as usize - '1' as usize]),
            _ => Command::None,
        }
    }

    /// What is under the pointer. While a dialog is open, only its buttons count,
    /// so nothing behind it reacts.
    fn target_at(&self, column: u16, row: u16) -> Option<Target> {
        let point = ratatui::layout::Position::new(column, row);
        let dialog_open = self.dialog.is_some();
        self.hits
            .iter()
            .filter(|(_, target)| !dialog_open || matches!(target, Target::DialogButton(_)))
            .find(|(area, _)| area.contains(point))
            .map(|(_, target)| *target)
    }

    fn on_click(&self, column: u16, row: u16) -> Command {
        match (self.target_at(column, row), self.dialog) {
            (Some(Target::DialogButton(button)), _) => Command::Dialog(button),
            (_, Some(_)) => Command::None,
            (Some(Target::Tab(tab)), None) => Command::Select(tab),
            (Some(Target::StartTerminal), None) => self.leave(),
            (Some(Target::Item(index)), None) => Command::Pick(index),
            (Some(Target::SaveButton), None) => Command::OpenSave,
            (None, None) => Command::None,
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
        self.poll_fonts();
        let screen = frame.area();
        if screen != self.last_screen {
            self.last_screen = screen;
            if self.tab == Tab::Tour {
                self.repaint_tour();
            }
        }
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
            Tab::Settings => self.draw_settings(frame, inner),
            Tab::Themes => self.draw_themes(frame, inner),
            Tab::Shaders => self.draw_shaders(frame, inner),
            Tab::Keys => draw_keys(frame, inner),
            Tab::About => draw_about(frame, inner),
            Tab::Tour => self.draw_tour(frame, inner),
        }
        self.draw_status(frame, areas.status);
        if let Some(dialog) = self.dialog {
            self.draw_dialog(frame, screen, dialog);
        }

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
                Style::new().fg(CYAN).add_modifier(Modifier::BOLD)
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
            Tab::Settings => {
                vec![key("↑/↓"), label(" select  "), key("←/→"), label(" change  "), key("Tab"), label(" tabs")]
            }
            Tab::Tour => {
                vec![key("↑/↓"), label(" pages  "), key("←/→"), label(" tabs  "), key("click"), label(" select")]
            }
            _ => vec![key("←/→"), label(" switch  "), key("1-7"), label(" jump  "), key("click"), label(" select")],
        };
        frame.render_widget(Paragraph::new(Line::from(hints)), area);
        let toast = self.toast.as_ref().filter(|(since, _)| since.elapsed() < TOAST).map(|(_, text)| text.clone());
        if let Some(text) = toast {
            frame.render_widget(Paragraph::new(Line::styled(text, Style::new().fg(CYAN))).centered(), area);
        } else if self.choices != self.saved {
            let hovered = self.hover == Some(Target::SaveButton);
            let note = Line::from(vec![
                Span::styled("● unsaved changes  ", Style::new().fg(MAGENTA)),
                Span::styled("Ctrl+S", Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(
                    "save",
                    if hovered {
                        Style::new().fg(TEXT).add_modifier(Modifier::UNDERLINED)
                    } else {
                        Style::new().fg(DIM)
                    },
                ),
            ]);
            let width = note.width() as u16;
            let note_area =
                Rect::new(area.x + area.width.saturating_sub(width) / 2, area.y, width, 1).intersection(area);
            self.hits.push((note_area, Target::SaveButton));
            frame.render_widget(Paragraph::new(note), note_area);
        }
        let hovered = self.hover == Some(Target::StartTerminal);
        let start_label = if hovered {
            Span::styled("start terminal", Style::new().fg(TEXT).add_modifier(Modifier::UNDERLINED))
        } else {
            label("start terminal")
        };
        // The space stays outside the underlined label.
        let start = Line::from(vec![key("Esc"), Span::raw(" "), start_label]);
        let width = start.width() as u16;
        let start_area = Rect::new(area.right().saturating_sub(width), area.y, width, 1).intersection(area);
        self.hits.push((start_area, Target::StartTerminal));
        frame.render_widget(Paragraph::new(start), start_area);
    }

    fn draw_tour(&mut self, frame: &mut Frame, area: Rect) {
        let [list_area, _, page_area] =
            Layout::horizontal([Constraint::Length(24), Constraint::Length(2), Constraint::Fill(1)]).areas(area);
        let items: Vec<Item> =
            Page::ALL.iter().map(|page| Item { name: page.title(), tag: "", chosen: false }).collect();
        let hover = match self.hover {
            Some(Target::Item(index)) => Some(index),
            _ => None,
        };
        let rows = pickers::draw_list(frame, list_area, &items, &mut self.tour, hover, true);
        self.hits.extend(rows.into_iter().map(|(rect, index)| (rect, Target::Item(index))));
        let page = Page::ALL[self.tour.selected];
        let [title_area, description_area, _, demo_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(page_area);
        frame.render_widget(
            Paragraph::new(Line::styled(page.title(), Style::new().fg(CYAN).add_modifier(Modifier::BOLD))),
            title_area,
        );
        frame.render_widget(
            Paragraph::new(Line::styled(page.description(), Style::new().fg(DIM))).wrap(Wrap { trim: true }),
            description_area,
        );
        // Left blank: the page is written into this area directly.
        self.tour_demo = Some(demo_area);
    }

    fn draw_settings(&mut self, frame: &mut Frame, area: Rect) {
        let [list_area, _, help_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Length(2)]).areas(area);
        let height = usize::from(list_area.height);
        let first = self.settings_picker.scroll(height);
        let hover = match self.hover {
            Some(Target::Item(index)) => Some(index),
            _ => None,
        };
        for (row, index) in (first..self.settings.len()).take(height).enumerate() {
            let setting = &self.settings[index];
            let selected = index == self.settings_picker.selected;
            let value = self.choices.settings.get(setting.key).map(settings::display).unwrap_or_default();
            let changed = self.choices.settings.get(setting.key) != self.saved.settings.get(setting.key);
            let label_style = match (selected, hover == Some(index)) {
                (true, _) => Style::new().fg(CYAN).add_modifier(Modifier::BOLD),
                (false, true) => Style::new().fg(TEXT).add_modifier(Modifier::UNDERLINED),
                (false, false) => Style::new().fg(TEXT),
            };
            let (value_text, value_style) = if selected {
                (format!("‹ {value} ›"), Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD))
            } else {
                (format!("  {value}"), Style::new().fg(DIM))
            };
            let line = Line::from(vec![
                Span::styled(if selected { "› " } else { "  " }, Style::new().fg(CYAN)),
                // Padding in its own span, so a hover underline covers only the label.
                Span::styled(setting.label, label_style),
                Span::raw(" ".repeat(26usize.saturating_sub(setting.label.chars().count()))),
                Span::styled(value_text, value_style),
                Span::styled(if changed { "  ●" } else { "" }, Style::new().fg(MAGENTA)),
            ]);
            let rect = Rect::new(list_area.x, list_area.y + row as u16, list_area.width, 1);
            frame.render_widget(Paragraph::new(line), rect);
            self.hits.push((rect, Target::Item(index)));
        }
        // Arrows show that the list continues.
        let arrow = Style::new().fg(DIM);
        if first > 0 {
            frame.render_widget(
                Paragraph::new(Line::styled("↑ more", arrow)).right_aligned(),
                Rect { height: 1, ..list_area },
            );
        }
        if first + height < self.settings.len() {
            let bottom = Rect { y: list_area.bottom().saturating_sub(1), height: 1, ..list_area };
            frame.render_widget(Paragraph::new(Line::styled("↓ more", arrow)).right_aligned(), bottom);
        }
        if let Some(setting) = self.settings.get(self.settings_picker.selected) {
            let help = Paragraph::new(Line::styled(setting.help, Style::new().fg(DIM))).wrap(Wrap { trim: true });
            frame.render_widget(help, help_area);
        }
    }

    fn draw_dialog(&mut self, frame: &mut Frame, screen: Rect, dialog: Dialog) {
        const SHOWN: usize = 10;
        let changes = self.choices.changes(&self.saved, &self.settings);
        let path = self
            .catalog
            .paths
            .as_ref()
            .map_or_else(|| "config.toml".to_owned(), |p| tron_config::display_path(&p.config_file));
        let mut lines = vec![match dialog {
            Dialog::Save => Line::styled(format!("Write these changes to {path}?"), Style::new().fg(TEXT)),
            Dialog::Leave => Line::styled("Save your changes before starting the terminal?", Style::new().fg(TEXT)),
        }];
        lines.push(Line::raw(""));
        for (label, old, new) in changes.iter().take(SHOWN) {
            lines.push(Line::from(vec![
                Span::styled(format!("{label:<22}"), Style::new().fg(DIM)),
                Span::styled(old.clone(), Style::new().fg(TEXT).add_modifier(Modifier::CROSSED_OUT)),
                Span::styled("  →  ", Style::new().fg(DIM)),
                Span::styled(new.clone(), Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD)),
            ]));
        }
        if changes.len() > SHOWN {
            lines.push(Line::styled(format!("… and {} more", changes.len() - SHOWN), Style::new().fg(DIM)));
        }
        let height = (lines.len() as u16 + 6).min(screen.height);
        let width = 72.min(screen.width.saturating_sub(4));
        let area = Rect::new(
            screen.x + screen.width.saturating_sub(width) / 2,
            screen.y + screen.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, area);
        let title = match dialog {
            Dialog::Save => " Save changes ",
            Dialog::Leave => " Unsaved changes ",
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(CYAN))
            .title(Line::styled(title, Style::new().fg(CYAN).add_modifier(Modifier::BOLD)));
        let inner = block.inner(area).inner(Margin::new(2, 1));
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines), inner);

        let mut buttons = vec![(DialogButton::Save, "Enter", "save")];
        if dialog == Dialog::Leave {
            buttons.push((DialogButton::Discard, "d", "discard"));
        }
        buttons.push((DialogButton::Cancel, "Esc", "cancel"));
        let mut column = inner.x;
        let row = inner.bottom().saturating_sub(1);
        for (button, key, text) in buttons {
            let hovered = self.hover == Some(Target::DialogButton(button));
            let text_style =
                if hovered { Style::new().fg(TEXT).add_modifier(Modifier::UNDERLINED) } else { Style::new().fg(DIM) };
            let line = Line::from(vec![
                Span::styled(key, Style::new().fg(MAGENTA).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(text, text_style),
            ]);
            let width = line.width() as u16;
            let rect = Rect::new(column, row, width, 1).intersection(inner);
            frame.render_widget(Paragraph::new(line), rect);
            self.hits.push((rect, Target::DialogButton(button)));
            column += width + 4;
        }
        if std::mem::take(&mut self.dialog_fresh) && self.animations {
            self.effects.push(
                fx::parallel(&[
                    fx::coalesce((220, Interpolation::QuadOut)),
                    fx::fade_from_fg(DARK, (220, Interpolation::QuadOut)),
                ])
                .with_area(area),
            );
        }
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
            Span::raw("  "),
            Span::styled("start the terminal", start_label),
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
    if app.tab == Tab::Tour {
        if let Some(page) = std::env::var(crate::DEBUG_PAGE_ENV).ok().and_then(|p| p.parse::<usize>().ok()) {
            app.tour.set(page.saturating_sub(1), Page::ALL.len());
        }
        app.repaint_tour();
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
        if std::mem::take(&mut app.tour_clear) {
            tour::clear(&mut io::stdout());
            if let Err(error) = terminal.clear() {
                break Err(error);
            }
        }
        if let Err(error) = terminal.draw(|frame| app.draw(frame, elapsed)) {
            break Err(error);
        }
        if let Some((page, area)) = app.tour_paint(now) {
            tour::paint(page, area, &mut io::stdout(), &mut app.tour_image_sent);
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
                if std::mem::take(&mut app.commit_pending) {
                    link.commit();
                }
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
        assert!(text.contains("unsaved changes"), "{text}");
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
    fn settings_change_values_and_preview_them() {
        let mut app = App::new(true, catalog(), &[]);
        app.apply(Command::Select(Tab::Settings), Rect::default());
        assert_eq!(app.on_key(key(KeyCode::Right)), Command::Change(1));
        assert_eq!(app.on_key(key(KeyCode::Tab)), Command::Select(Tab::Themes), "Tab still switches tabs");
        app.apply(Command::Move(1), Rect::default());
        assert_eq!(app.settings[app.settings_picker.selected].key, "font.size");
        app.apply(Command::Change(2), Rect::default());
        assert_eq!(app.choices.settings["font.size"], toml::Value::Float(13.0));
        let preview = app.take_preview(Instant::now() + PREVIEW_DELAY).unwrap();
        assert!(preview.contains("size = 13.0"), "{preview}");
        let text = screen(&mut app);
        assert!(text.contains("‹ 13 ›") && text.contains("Font size"), "{text}");
    }

    #[test]
    fn saving_asks_first_and_writes_the_file() {
        let dir = std::env::temp_dir().join(format!("tron-startup-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = tron_config::Paths::with_dirs(dir.clone(), dir.join("data"));
        let mut app = App::new(true, Catalog::for_paths(Some(paths.clone())), &[]);
        app.apply(Command::Select(Tab::Settings), Rect::default());
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        app.apply(app.on_key(ctrl_s), Rect::default());
        assert_eq!(app.dialog, None, "nothing to save");
        assert!(app.toast.is_some());

        app.choices.theme = "nord".into();
        assert_eq!(app.on_key(key(KeyCode::Esc)), Command::OpenLeave, "unsaved changes ask before leaving");
        app.apply(app.on_key(ctrl_s), Rect::default());
        assert_eq!(app.dialog, Some(Dialog::Save));
        let text = screen(&mut app);
        assert!(text.contains("Save changes") && text.contains("tron  →  nord"), "{text}");
        // The buttons sit over list rows; clicking one must reach the dialog, not the row.
        let (button, _) = *app.hits.iter().find(|(_, t)| *t == Target::DialogButton(DialogButton::Save)).unwrap();
        assert!(app.hits.iter().any(|(area, t)| matches!(t, Target::Item(_)) && area.intersects(button)));
        app.on_move(button.x, button.y);
        assert_eq!(app.hover, Some(Target::DialogButton(DialogButton::Save)));
        app.apply(Command::Move(1), Rect::default());
        assert!(app.dialog.is_some(), "the wheel does not reach the list");
        app.apply(app.on_click(button.x, button.y), Rect::default());
        assert!(app.commit_pending && app.dialog.is_none());
        assert_eq!(tron_config::Config::load(&paths).unwrap().theme.as_deref(), Some("nord"));
        assert_eq!(app.on_key(key(KeyCode::Esc)), Command::StartTerminal, "saved, so no question");

        app.choices.theme = "dracula".into();
        app.apply(Command::OpenLeave, Rect::default());
        app.apply(app.on_key(key(KeyCode::Char('d'))), Rect::default());
        assert!(app.exit.is_some(), "discard starts the terminal");
        assert_eq!(tron_config::Config::load(&paths).unwrap().theme.as_deref(), Some("nord"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tour_pages_repaint_after_transitions() {
        let mut app = App::new(false, catalog(), &[]);
        app.apply(Command::Select(Tab::Tour), Rect::default());
        assert!(app.tour_clear, "entering clears the screen");
        let text = screen(&mut app);
        assert!(text.contains("Text and styles") && text.contains("Ligatures from your font"), "{text}");
        let (page, _) = app.tour_paint(Instant::now()).expect("paint without animations");
        assert_eq!(page, Page::Text);
        assert_eq!(app.tour_paint(Instant::now()), None, "painted once");
        assert_eq!(app.on_key(key(KeyCode::Down)), Command::Move(1));
        app.apply(Command::Move(1), Rect::default());
        assert_eq!(app.tour_paint(Instant::now()).map(|(page, _)| page), Some(Page::Unicode));
        app.apply(Command::Select(Tab::About), Rect::default());
        assert!(app.tour_clear, "leaving clears the painted page");
        assert_eq!(app.tour_paint(Instant::now()), None);

        let mut animated = App::new(true, catalog(), &[]);
        animated.apply(Command::Select(Tab::Tour), Rect::default());
        screen(&mut animated);
        assert_eq!(animated.tour_paint(Instant::now()), None, "waits for the transition");
        assert!(animated.tour_paint(Instant::now() + Duration::from_secs(2)).is_some());
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
