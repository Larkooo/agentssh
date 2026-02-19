mod agents;
mod config;
mod pathnav;
mod tmux;

use agents::AgentDefinition;
use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use pathnav::{ActivateResult, Browser, EntryKind};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::{
    collections::HashMap,
    env,
    io::{self, Stdout},
    time::{Duration, Instant},
};

#[derive(Parser, Debug)]
#[command(author, version, about = "Agent-first SSH interface with tabbed TUI")]
struct Cli {
    #[arg(long, help = "Auto refresh interval in seconds")]
    refresh_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
struct AgentInstance {
    agent: AgentDefinition,
    session: tmux::Session,
    managed: bool,
    title_override: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnStep {
    Agent,
    Path,
    NewDirectoryName,
}

#[derive(Debug, Clone)]
struct SpawnModal {
    step: SpawnStep,
    selected_agent: usize,
    browser: Browser,
    new_dir_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppScreen {
    Warning,
    Main,
}

#[derive(Debug, Clone)]
struct Warning {
    title: String,
    message: String,
    details: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct UiTheme {
    bg: Color,
    border: Color,
    text: Color,
    muted: Color,
    accent: Color,
    highlight_bg: Color,
    yellow: Color,
    green: Color,
}

impl UiTheme {
    fn new() -> Self {
        Self {
            bg: Color::Rgb(0, 0, 0),
            border: Color::Rgb(70, 60, 55),
            text: Color::Rgb(215, 205, 195),
            muted: Color::Rgb(130, 120, 110),
            accent: Color::Rgb(207, 144, 89),     // claude terracotta/clay
            highlight_bg: Color::Rgb(191, 111, 74), // warm sienna
            yellow: Color::Rgb(228, 175, 105),    // warm amber
            green: Color::Rgb(169, 195, 140),     // sage green
        }
    }
}

struct App {
    available_agents: Vec<AgentDefinition>,
    instances: Vec<AgentInstance>,
    selected_row: usize,
    selected_tab: usize,
    modal: Option<SpawnModal>,
    last_refresh: Instant,
    refresh_interval: Duration,
    should_quit: bool,
    status_line: String,
    theme: UiTheme,
    screen: AppScreen,
    warning: Option<Warning>,
    tmux_available: bool,
    config: config::AppConfig,
    previous_commands: HashMap<String, String>,
}

impl App {
    fn new(cfg: config::AppConfig) -> Self {
        let tmux_available = tmux::is_tmux_available();
        let refresh_interval = Duration::from_secs(cfg.refresh_interval.max(1));

        Self {
            available_agents: Vec::new(),
            instances: Vec::new(),
            selected_row: 0,
            selected_tab: 0,
            modal: None,
            last_refresh: Instant::now() - refresh_interval,
            refresh_interval,
            should_quit: false,
            status_line: String::new(),
            theme: UiTheme::new(),
            screen: AppScreen::Main,
            warning: None,
            tmux_available,
            config: cfg,
            previous_commands: HashMap::new(),
        }
    }

    fn check_warnings(&mut self) {
        if !self.tmux_available {
            self.warning = Some(Warning {
                title: "tmux not found".to_owned(),
                message: "agentssh requires tmux to manage agent sessions.".to_owned(),
                details: vec![
                    "install via your package manager:".to_owned(),
                    "  brew install tmux".to_owned(),
                    "  apt install tmux".to_owned(),
                    "  pacman -S tmux".to_owned(),
                ],
            });
            self.screen = AppScreen::Warning;
            return;
        }

        if self.available_agents.is_empty() {
            self.warning = Some(Warning {
                title: "no agent CLIs found".to_owned(),
                message: "agentssh needs at least one supported agent CLI in PATH.".to_owned(),
                details: vec![
                    "supported agents:".to_owned(),
                    "  claude    - Claude Code".to_owned(),
                    "  codex     - Codex CLI".to_owned(),
                    "  aider     - Aider".to_owned(),
                    "  gemini    - Gemini CLI".to_owned(),
                    "  opencode  - OpenCode".to_owned(),
                ],
            });
            self.screen = AppScreen::Warning;
            return;
        }

        self.warning = None;
        self.screen = AppScreen::Main;
    }

    fn refresh(&mut self) {
        self.tmux_available = tmux::is_tmux_available();
        self.available_agents = agents::detect_available_agents(&self.config.custom_agents);
        self.check_warnings();

        if !self.tmux_available {
            self.last_refresh = Instant::now();
            return;
        }

        match tmux::list_sessions() {
            Ok(sessions) => {
                self.instances = sessions
                    .into_iter()
                    .filter_map(|session| {
                        let agent = agents::classify_agent_from_session(
                            &session.name,
                            &session.current_command,
                            &self.available_agents,
                        )?;
                        let managed = agents::managed_session_agent_id(&session.name).is_some();
                        let title_override = agents::read_title_file(&session.name);
                        Some(AgentInstance {
                            agent,
                            session,
                            managed,
                            title_override,
                        })
                    })
                    .collect();

                self.instances
                    .sort_by(|a, b| a.session.name.cmp(&b.session.name));
                self.clamp_selection();

                // Completion detection: agent binary → shell means agent finished
                let current_cmds: Vec<(String, String)> = self
                    .instances
                    .iter()
                    .map(|i| {
                        (
                            i.session.name.clone(),
                            i.session.current_command.clone(),
                        )
                    })
                    .collect();
                config::detect_completions(
                    &mut self.previous_commands,
                    &current_cmds,
                    &self.available_agents,
                    &self.config,
                );

                self.status_line = format!(
                    "{} running  {}  {} agents detected",
                    self.instances.len(),
                    "\u{2502}",
                    self.available_agents.len()
                );
            }
            Err(err) => {
                self.instances.clear();
                self.selected_row = 0;
                self.selected_tab = 0;
                self.status_line = format!("refresh failed: {err}");
            }
        }

        self.last_refresh = Instant::now();
    }

    fn dashboard_row_count(&self) -> usize {
        self.instances.len() + 1
    }

    fn clamp_selection(&mut self) {
        if self.selected_row >= self.dashboard_row_count() {
            self.selected_row = self.dashboard_row_count().saturating_sub(1);
        }

        if self.selected_tab > self.instances.len() {
            self.selected_tab = 0;
        }

        if self.selected_tab > 0 {
            self.selected_row = self.selected_tab - 1;
        }
    }

    fn selected_instance(&self) -> Option<&AgentInstance> {
        if self.selected_row < self.instances.len() {
            self.instances.get(self.selected_row)
        } else {
            None
        }
    }

    fn current_tab_instance(&self) -> Option<&AgentInstance> {
        if self.selected_tab == 0 {
            return None;
        }
        self.instances.get(self.selected_tab - 1)
    }

    fn is_action_row_selected(&self) -> bool {
        self.selected_tab == 0 && self.selected_row == self.instances.len()
    }

    fn next_row(&mut self) {
        let count = self.dashboard_row_count();
        self.selected_row = (self.selected_row + 1) % count;
    }

    fn previous_row(&mut self) {
        let count = self.dashboard_row_count();
        if self.selected_row == 0 {
            self.selected_row = count.saturating_sub(1);
        } else {
            self.selected_row -= 1;
        }
    }

    fn next_tab(&mut self) {
        let max = self.instances.len();
        self.selected_tab = if self.selected_tab >= max {
            0
        } else {
            self.selected_tab + 1
        };
        if self.selected_tab > 0 {
            self.selected_row = self.selected_tab - 1;
        }
    }

    fn previous_tab(&mut self) {
        let max = self.instances.len();
        self.selected_tab = if self.selected_tab == 0 {
            max
        } else {
            self.selected_tab - 1
        };
        if self.selected_tab > 0 {
            self.selected_row = self.selected_tab - 1;
        }
    }

    fn open_spawn_modal(&mut self) {
        if self.available_agents.is_empty() {
            self.status_line = "No supported agent CLIs found in PATH".to_owned();
            return;
        }

        let start = self
            .config
            .default_spawn_dir
            .as_ref()
            .map(|s| std::path::PathBuf::from(s))
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| "/".into()));
        match Browser::new(start) {
            Ok(browser) => {
                self.modal = Some(SpawnModal {
                    step: SpawnStep::Agent,
                    selected_agent: 0,
                    browser,
                    new_dir_name: String::new(),
                });
            }
            Err(err) => {
                self.status_line = format!("Cannot open path browser: {err}");
            }
        }
    }

    fn create_instance(&mut self, agent_index: usize, working_dir: String) {
        let Some(agent) = self.available_agents.get(agent_index).cloned() else {
            self.status_line = "Invalid agent selection".to_owned();
            self.modal = None;
            return;
        };

        let session_name = agents::build_managed_session_name(&agent.id);
        let title_enabled = self.config.title_injection_enabled;

        let launch_cmd = agents::build_launch_command(&agent, title_enabled);

        match tmux::create_session(&session_name, &working_dir, &launch_cmd) {
            Ok(()) => {
                // For agents without a system-prompt flag, inject a first
                // message asking them to write task titles to a temp file.
                // Delay gives TUI-based agents time to boot.
                if title_enabled && agents::needs_title_injection(&agent) {
                    let msg = agents::build_title_injection(&session_name);
                    let delay = self.config.title_injection_delay;
                    let _ = tmux::send_keys_delayed(&session_name, &msg, delay);
                }

                self.status_line = format!("Started {} in {}", agent.label, working_dir);
                self.modal = None;
                self.refresh();

                if let Some(pos) = self
                    .instances
                    .iter()
                    .position(|x| x.session.name == session_name)
                {
                    self.selected_row = pos;
                    self.selected_tab = pos + 1;
                }
            }
            Err(err) => {
                self.status_line = format!("Failed to start {}: {err}", agent.label);
                self.modal = None;
            }
        }
    }

    fn kill_selected_instance(&mut self) {
        let Some(instance) = self.active_instance_ref().cloned() else {
            self.status_line = "Select an instance row first".to_owned();
            return;
        };

        match tmux::kill_session(&instance.session.name) {
            Ok(()) => {
                self.status_line = format!("Stopped {}", instance.session.name);
                self.refresh();
            }
            Err(err) => {
                self.status_line = format!("Failed to stop {}: {err}", instance.session.name);
            }
        }
    }

    fn active_instance_ref(&self) -> Option<&AgentInstance> {
        if self.selected_tab == 0 {
            self.selected_instance()
        } else {
            self.current_tab_instance()
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = config::load_config();
    config::apply_cli_overrides(&mut cfg, cli.refresh_seconds);
    run(cfg)
}

fn run(cfg: config::AppConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let mut app = App::new(cfg);
    app.refresh();

    let loop_result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    loop_result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| draw_ui(frame, app))?;

        let until_refresh = app
            .refresh_interval
            .saturating_sub(app.last_refresh.elapsed())
            .min(Duration::from_millis(250));

        if event::poll(until_refresh)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.screen == AppScreen::Warning {
                        handle_warning_key(app, key.code);
                    } else if app.modal.is_some() {
                        handle_modal_key(app, key.code);
                    } else {
                        handle_main_key(terminal, app, key.code)?;
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        if app.last_refresh.elapsed() >= app.refresh_interval {
            app.refresh();
        }
    }

    Ok(())
}

fn handle_warning_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('r') => app.refresh(),
        _ => {}
    }
}

fn handle_modal_key(app: &mut App, code: KeyCode) {
    enum Action {
        None,
        Close,
        CreateInstance {
            agent_index: usize,
            working_dir: String,
        },
        CreateDirectory {
            name: String,
        },
    }

    let mut action = Action::None;
    let mut status_override: Option<String> = None;

    if let Some(modal) = app.modal.as_mut() {
        match modal.step {
            SpawnStep::Agent => match code {
                KeyCode::Esc => action = Action::Close,
                KeyCode::Char('j') | KeyCode::Down => {
                    if !app.available_agents.is_empty() {
                        modal.selected_agent =
                            (modal.selected_agent + 1) % app.available_agents.len();
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if !app.available_agents.is_empty() {
                        if modal.selected_agent == 0 {
                            modal.selected_agent = app.available_agents.len() - 1;
                        } else {
                            modal.selected_agent -= 1;
                        }
                    }
                }
                KeyCode::Enter => modal.step = SpawnStep::Path,
                _ => {}
            },
            SpawnStep::Path => match code {
                KeyCode::Esc => action = Action::Close,
                KeyCode::Left | KeyCode::Char('h') => modal.step = SpawnStep::Agent,
                KeyCode::Char('j') | KeyCode::Down => modal.browser.next(),
                KeyCode::Char('k') | KeyCode::Up => modal.browser.previous(),
                KeyCode::PageDown => {
                    for _ in 0..10 {
                        modal.browser.next();
                    }
                }
                KeyCode::PageUp => {
                    for _ in 0..10 {
                        modal.browser.previous();
                    }
                }
                KeyCode::Enter => match modal.browser.activate_selected() {
                    Ok(ActivateResult::Selected(path)) => {
                        action = Action::CreateInstance {
                            agent_index: modal.selected_agent,
                            working_dir: path.to_string_lossy().to_string(),
                        }
                    }
                    Ok(ActivateResult::ChangedDirectory) => {}
                    Ok(ActivateResult::StartCreateDirectory) => {
                        modal.step = SpawnStep::NewDirectoryName;
                        modal.new_dir_name.clear();
                    }
                    Err(err) => {
                        status_override = Some(format!("Path navigation failed: {err}"));
                    }
                },
                _ => {}
            },
            SpawnStep::NewDirectoryName => match code {
                KeyCode::Esc => {
                    modal.step = SpawnStep::Path;
                    modal.new_dir_name.clear();
                }
                KeyCode::Enter => {
                    action = Action::CreateDirectory {
                        name: modal.new_dir_name.clone(),
                    }
                }
                KeyCode::Backspace => {
                    modal.new_dir_name.pop();
                }
                KeyCode::Char(c) => {
                    if !c.is_control() {
                        modal.new_dir_name.push(c);
                    }
                }
                _ => {}
            },
        }
    }

    if let Some(status) = status_override {
        app.status_line = status;
    }

    match action {
        Action::None => {}
        Action::Close => app.modal = None,
        Action::CreateInstance {
            agent_index,
            working_dir,
        } => app.create_instance(agent_index, working_dir),
        Action::CreateDirectory { name } => {
            if let Some(modal) = app.modal.as_mut() {
                match modal.browser.create_directory(&name) {
                    Ok(path) => {
                        modal.step = SpawnStep::Path;
                        modal.new_dir_name.clear();
                        app.status_line = format!("Created {}", path.display());
                    }
                    Err(err) => {
                        app.status_line = format!("Create directory failed: {err}");
                    }
                }
            }
        }
    }
}

fn handle_main_key(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    code: KeyCode,
) -> Result<()> {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => {
            if app.selected_tab == 0 {
                app.next_row();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.selected_tab == 0 {
                app.previous_row();
            }
        }
        KeyCode::Char('h') | KeyCode::Left => app.previous_tab(),
        KeyCode::Char('l') | KeyCode::Right | KeyCode::Tab => app.next_tab(),
        KeyCode::Char('d') => app.selected_tab = 0,
        KeyCode::Char('x') => app.kill_selected_instance(),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Enter => {
            if app.selected_tab == 0 && app.is_action_row_selected() {
                app.open_spawn_modal();
            } else if let Some(instance) = app.active_instance_ref() {
                let attach_result = attach_into_session(terminal, &instance.session.name);
                match attach_result {
                    Ok(()) => app.status_line = format!("Detached from {}", instance.session.name),
                    Err(err) => {
                        app.status_line =
                            format!("Attach failed for {}: {err}", instance.session.name)
                    }
                }
                app.refresh();
            }
        }
        _ => {}
    }

    Ok(())
}

fn attach_into_session(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    name: &str,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    let attach_result = tmux::attach_session(name);

    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    enable_raw_mode()?;
    terminal.hide_cursor()?;
    terminal.clear()?;

    attach_result
}

fn draw_ui(frame: &mut ratatui::Frame<'_>, app: &App) {
    let t = app.theme;

    frame.render_widget(
        Block::default().style(Style::default().bg(t.bg)),
        frame.area(),
    );

    match app.screen {
        AppScreen::Warning => draw_warning_screen(frame, app),
        AppScreen::Main => draw_main_screen(frame, app),
    }
}

fn draw_warning_screen(frame: &mut ratatui::Frame<'_>, app: &App) {
    let t = app.theme;
    let container = centered_rect(60, 96, frame.area());

    let Some(warning) = &app.warning else { return };

    // Center the warning vertically
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Min(12),
            Constraint::Percentage(40),
        ])
        .split(container);

    let area = vert[1];

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  ! {}", warning.title),
            Style::default().fg(t.yellow).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", warning.message),
            Style::default().fg(t.text),
        )),
        Line::from(""),
    ];

    for detail in &warning.details {
        lines.push(Line::from(Span::styled(
            format!("  {detail}"),
            Style::default().fg(t.muted),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  press ", Style::default().fg(t.muted)),
        Span::styled("r", Style::default().fg(t.text).add_modifier(Modifier::BOLD)),
        Span::styled(" to retry    ", Style::default().fg(t.muted)),
        Span::styled("q", Style::default().fg(t.text).add_modifier(Modifier::BOLD)),
        Span::styled(" to quit", Style::default().fg(t.muted)),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.yellow))
        .style(Style::default().bg(t.bg))
        .title(Line::from(vec![
            Span::styled(
                " agentssh ",
                Style::default().fg(t.yellow).add_modifier(Modifier::BOLD),
            ),
        ]));

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(t.bg))
            .block(block),
        area,
    );
}

fn draw_main_screen(frame: &mut ratatui::Frame<'_>, app: &App) {
    let container = centered_rect(80, 96, frame.area());

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header tabs
            Constraint::Length(1), // spacer
            Constraint::Min(6),   // content
            Constraint::Length(1), // spacer
            Constraint::Length(1), // status message
            Constraint::Length(1), // horizontal rule
            Constraint::Length(1), // keybindings
        ])
        .split(container);

    draw_header(frame, sections[0], app);

    if app.selected_tab == 0 {
        draw_dashboard(frame, sections[2], app);
    } else {
        draw_instance_tab(frame, sections[2], app);
    }

    draw_status_line(frame, sections[4], app);
    draw_footer_rule(frame, sections[5], app);
    draw_footer(frame, sections[6], app);

    if app.modal.is_some() {
        draw_spawn_modal(frame, app);
    }
}

/// Renders the header as a connected bordered table row:
/// ┌──────────┬──────────┬──────────┐
/// │ agentssh │  s shop  │  a acct  │
/// └──────────┴──────────┴──────────┘
fn draw_header(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    let w = area.width as usize;

    // Cell 0 = "agentssh" brand (maps to dashboard / tab 0)
    // Cell 1 = "s sessions" shortcut
    // Cell 2+ = instance tabs
    struct TabCell {
        label: String,
        is_selected: bool,
    }

    let mut cells: Vec<TabCell> = Vec::new();
    cells.push(TabCell {
        label: "agentssh".to_owned(),
        is_selected: app.selected_tab == 0,
    });
    cells.push(TabCell {
        label: "s sessions".to_owned(),
        is_selected: app.selected_tab == 0,
    });
    for (i, instance) in app.instances.iter().enumerate() {
        let title = agents::derive_display_title(
            &instance.session.name,
            &instance.session.pane_title,
            &instance.session.pane_current_path,
            &instance.title_override,
        );
        let display = truncate(&title, 14);
        cells.push(TabCell {
            label: format!("{} {}", instance.agent.id, display),
            is_selected: app.selected_tab == i + 1,
        });
    }

    let n = cells.len();
    if n == 0 || w < n + 1 {
        return;
    }

    // Calculate column widths (content only, not including border chars)
    let available = w.saturating_sub(n + 1);
    let base = available / n;
    let extra = available % n;
    let mut col_widths: Vec<usize> = vec![base; n];
    for i in 0..extra {
        col_widths[i] += 1;
    }

    let border_style = Style::default().fg(t.border);

    // Top border: ┌───┬───┬───┐
    let mut top_spans: Vec<Span> = vec![Span::styled("\u{250c}", border_style)];
    for (i, &cw) in col_widths.iter().enumerate() {
        top_spans.push(Span::styled("\u{2500}".repeat(cw), border_style));
        if i < n - 1 {
            top_spans.push(Span::styled("\u{252c}", border_style));
        } else {
            top_spans.push(Span::styled("\u{2510}", border_style));
        }
    }

    // Content: │ label │ label │
    let mut mid_spans: Vec<Span> = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        mid_spans.push(Span::styled("\u{2502}", border_style));

        let cw = col_widths[i];
        let display_label = if cell.label.len() > cw {
            truncate(&cell.label, cw)
        } else {
            cell.label.clone()
        };
        let label_len = display_label.len();
        let pad_total = cw.saturating_sub(label_len);
        let pad_left = pad_total / 2;
        let pad_right = pad_total - pad_left;

        let text_style = if cell.is_selected {
            Style::default().fg(t.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.muted)
        };

        mid_spans.push(Span::styled(" ".repeat(pad_left), Style::default()));
        mid_spans.push(Span::styled(display_label, text_style));
        mid_spans.push(Span::styled(" ".repeat(pad_right), Style::default()));
    }
    mid_spans.push(Span::styled("\u{2502}", border_style));

    // Bottom border: └───┴───┴───┘
    let mut bot_spans: Vec<Span> = vec![Span::styled("\u{2514}", border_style)];
    for (i, &cw) in col_widths.iter().enumerate() {
        bot_spans.push(Span::styled("\u{2500}".repeat(cw), border_style));
        if i < n - 1 {
            bot_spans.push(Span::styled("\u{2534}", border_style));
        } else {
            bot_spans.push(Span::styled("\u{2518}", border_style));
        }
    }

    let text = Text::from(vec![
        Line::from(top_spans),
        Line::from(mid_spans),
        Line::from(bot_spans),
    ]);

    frame.render_widget(
        Paragraph::new(text).style(Style::default().bg(t.bg)),
        area,
    );
}

fn draw_dashboard(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(28),
            Constraint::Length(2),
            Constraint::Percentage(70),
        ])
        .split(area);

    draw_instance_list(frame, chunks[0], app);
    draw_summary_panel(frame, chunks[2], app);
}

fn draw_instance_list(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    let has_managed = app.instances.iter().any(|i| i.managed);
    let has_external = app.instances.iter().any(|i| !i.managed);

    let mut lines: Vec<Line> = Vec::new();

    if has_managed {
        lines.push(Line::from(Span::styled(
            "~ managed ~",
            Style::default().fg(t.accent),
        )));
    } else if !app.instances.is_empty() {
        lines.push(Line::from(Span::styled(
            "~ sessions ~",
            Style::default().fg(t.accent),
        )));
    }

    let total = app.dashboard_row_count();
    let capacity = area.height.saturating_sub(4) as usize;
    let (start, end) = visible_range(total, app.selected_row, capacity.max(1));

    if start > 0 {
        lines.push(Line::from(Span::styled(
            "...",
            Style::default().fg(t.muted),
        )));
    }

    let mut shown_external_header = false;

    for index in start..end {
        let selected = index == app.selected_row;

        if index < app.instances.len() {
            let instance = &app.instances[index];

            if !instance.managed && !shown_external_header && has_managed && has_external {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "~ external ~",
                    Style::default().fg(t.accent),
                )));
                shown_external_header = true;
            }

            let title = agents::derive_display_title(
                &instance.session.name,
                &instance.session.pane_title,
                &instance.session.pane_current_path,
                &instance.title_override,
            );
            let label = truncate(&title, 28);

            let style = if selected {
                Style::default()
                    .fg(t.bg)
                    .bg(t.highlight_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.text)
            };

            lines.push(Line::from(Span::styled(label, style)));
        } else {
            // "New Instance" action row
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            let style = if selected {
                Style::default()
                    .fg(t.bg)
                    .bg(t.highlight_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.accent)
            };
            lines.push(Line::from(Span::styled("+ new instance", style)));
        }
    }

    if end < total {
        lines.push(Line::from(Span::styled(
            "...",
            Style::default().fg(t.muted),
        )));
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(t.bg))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_summary_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;

    let lines = if app.is_action_row_selected() || app.instances.is_empty() {
        let mut l = vec![
            Line::from(Span::styled(
                "new instance",
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press enter to launch the spawn wizard.",
                Style::default().fg(t.text),
            )),
            Line::from(Span::styled(
                "Select an agent CLI, pick a working directory,",
                Style::default().fg(t.text),
            )),
            Line::from(Span::styled(
                "and a new tmux session will be created.",
                Style::default().fg(t.text),
            )),
            Line::from(""),
        ];

        if !app.available_agents.is_empty() {
            l.push(Line::from(Span::styled(
                "~ detected agents ~",
                Style::default().fg(t.accent),
            )));
            for agent in &app.available_agents {
                l.push(Line::from(vec![
                    Span::styled(
                        format!("{}", agent.id),
                        Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", agent.label),
                        Style::default().fg(t.muted),
                    ),
                ]));
            }
        }
        l
    } else if let Some(instance) = app.selected_instance() {
        let state_style = if instance.session.attached {
            Style::default().fg(t.green)
        } else {
            Style::default().fg(t.muted)
        };

        let mut lines = vec![
            Line::from(Span::styled(
                instance.agent.label.clone(),
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("session  ", Style::default().fg(t.muted)),
                Span::styled(instance.session.name.clone(), Style::default().fg(t.text)),
            ]),
            Line::from(vec![
                Span::styled("created  ", Style::default().fg(t.muted)),
                Span::styled(instance.session.created.clone(), Style::default().fg(t.text)),
            ]),
            Line::from(vec![
                Span::styled("state    ", Style::default().fg(t.muted)),
                Span::styled(
                    if instance.session.attached { "attached" } else { "idle" },
                    state_style,
                ),
            ]),
            Line::from(vec![
                Span::styled("kind     ", Style::default().fg(t.muted)),
                Span::styled(
                    if instance.managed { "managed" } else { "external" },
                    Style::default().fg(t.text),
                ),
            ]),
            Line::from(vec![
                Span::styled("command  ", Style::default().fg(t.muted)),
                Span::styled(
                    instance.session.current_command.clone(),
                    Style::default().fg(t.text),
                ),
            ]),
            Line::from(vec![
                Span::styled("path     ", Style::default().fg(t.muted)),
                Span::styled(
                    if instance.session.pane_current_path.is_empty() {
                        "—".to_owned()
                    } else {
                        instance.session.pane_current_path.clone()
                    },
                    Style::default().fg(t.text),
                ),
            ]),
            Line::from(""),
        ];

        let preview_space = area.height.saturating_sub(lines.len() as u16 + 1) as usize;
        let preview_take = preview_space.max(4);
        let preview: Vec<String> = instance
            .session
            .preview
            .iter()
            .rev()
            .take(preview_take)
            .cloned()
            .collect::<Vec<String>>()
            .into_iter()
            .rev()
            .collect();

        if preview.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no output captured)",
                Style::default().fg(t.muted),
            )));
        } else {
            for line in preview {
                lines.push(Line::from(Span::styled(line, Style::default().fg(t.muted))));
            }
        }

        lines
    } else {
        vec![Line::from(Span::styled(
            "select an instance",
            Style::default().fg(t.muted),
        ))]
    };

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(t.text).bg(t.bg))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_instance_tab(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    let Some(instance) = app.current_tab_instance() else {
        draw_dashboard(frame, area, app);
        return;
    };

    let state_style = if instance.session.attached {
        Style::default().fg(t.green)
    } else {
        Style::default().fg(t.muted)
    };

    let mut lines = vec![
        Line::from(Span::styled(
            instance.agent.label.clone(),
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("session  ", Style::default().fg(t.muted)),
            Span::styled(instance.session.name.clone(), Style::default().fg(t.text)),
        ]),
        Line::from(vec![
            Span::styled("created  ", Style::default().fg(t.muted)),
            Span::styled(instance.session.created.clone(), Style::default().fg(t.text)),
        ]),
        Line::from(vec![
            Span::styled("state    ", Style::default().fg(t.muted)),
            Span::styled(
                if instance.session.attached { "attached" } else { "idle" },
                state_style,
            ),
        ]),
        Line::from(vec![
            Span::styled("windows  ", Style::default().fg(t.muted)),
            Span::styled(
                format!("{}", instance.session.windows),
                Style::default().fg(t.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("command  ", Style::default().fg(t.muted)),
            Span::styled(
                instance.session.current_command.clone(),
                Style::default().fg(t.text),
            ),
        ]),
        Line::from(vec![
            Span::styled("path     ", Style::default().fg(t.muted)),
            Span::styled(
                if instance.session.pane_current_path.is_empty() {
                    "\u{2014}".to_owned()
                } else {
                    instance.session.pane_current_path.clone()
                },
                Style::default().fg(t.text),
            ),
        ]),
        Line::from(""),
    ];

    let preview_take = area.height.saturating_sub(lines.len() as u16 + 1) as usize;
    let preview: Vec<String> = instance
        .session
        .preview
        .iter()
        .rev()
        .take(preview_take.max(4))
        .cloned()
        .collect::<Vec<String>>()
        .into_iter()
        .rev()
        .collect();

    if preview.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no output captured)",
            Style::default().fg(t.muted),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "~ live buffer ~",
            Style::default().fg(t.accent),
        )));
        for line in preview {
            lines.push(Line::from(Span::styled(line, Style::default().fg(t.text))));
        }
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(t.text).bg(t.bg))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_status_line(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;

    if !app.status_line.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                app.status_line.clone(),
                Style::default().fg(t.muted),
            )))
            .alignment(Alignment::Center)
            .style(Style::default().bg(t.bg)),
            area,
        );
    }
}

fn draw_footer_rule(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    let w = area.width as usize;
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2500}".repeat(w),
            Style::default().fg(t.border),
        )))
        .style(Style::default().bg(t.bg)),
        area,
    );
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;

    let key_style = Style::default().fg(t.text).add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(t.muted);

    let commands = Line::from(vec![
        Span::styled("r", key_style),
        Span::styled(" refresh   ", desc_style),
        Span::styled("\u{2191}/\u{2193}", key_style),
        Span::styled(" select   ", desc_style),
        Span::styled("enter", key_style),
        Span::styled(" attach   ", desc_style),
        Span::styled("\u{2190}/\u{2192}", key_style),
        Span::styled(" tabs   ", desc_style),
        Span::styled("x", key_style),
        Span::styled(" stop   ", desc_style),
        Span::styled("q", key_style),
        Span::styled(" quit", desc_style),
    ]);

    frame.render_widget(
        Paragraph::new(commands)
            .alignment(Alignment::Center)
            .style(Style::default().bg(t.bg)),
        area,
    );
}

fn draw_spawn_modal(frame: &mut ratatui::Frame<'_>, app: &App) {
    let t = app.theme;
    let Some(modal) = app.modal.as_ref() else {
        return;
    };

    let area = centered_rect(70, 75, frame.area());
    frame.render_widget(Clear, area);

    let selected_agent = app
        .available_agents
        .get(modal.selected_agent)
        .map(|a| a.label.clone())
        .unwrap_or_else(|| "none".to_owned());

    let agent_step_style = if modal.step == SpawnStep::Agent {
        Style::default().fg(t.accent)
    } else {
        Style::default().fg(t.green)
    };
    let path_step_style = if modal.step == SpawnStep::Path
        || modal.step == SpawnStep::NewDirectoryName
    {
        Style::default().fg(t.accent)
    } else {
        Style::default().fg(t.muted)
    };

    let mut lines = vec![
        Line::from(Span::styled(
            "spawn new instance",
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  1 ", agent_step_style.add_modifier(Modifier::BOLD)),
            Span::styled("agent", agent_step_style),
            Span::styled("  ", Style::default()),
            Span::styled(selected_agent.clone(), Style::default().fg(t.muted)),
        ]),
        Line::from(vec![
            Span::styled("  2 ", path_step_style.add_modifier(Modifier::BOLD)),
            Span::styled("path", path_step_style),
        ]),
        Line::from(""),
    ];

    match modal.step {
        SpawnStep::Agent => {
            lines.push(Line::from(Span::styled(
                "  ~ select agent ~",
                Style::default().fg(t.accent),
            )));

            let capacity = area.height.saturating_sub(12) as usize;
            let (start, end) = visible_range(
                app.available_agents.len(),
                modal.selected_agent,
                capacity.max(1),
            );
            if start > 0 {
                lines.push(Line::from(Span::styled(
                    "  ...",
                    Style::default().fg(t.muted),
                )));
            }

            for i in start..end {
                let agent = &app.available_agents[i];
                let selected = i == modal.selected_agent;
                let style = if selected {
                    Style::default()
                        .fg(t.bg)
                        .bg(t.highlight_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.text)
                };
                lines.push(Line::from(Span::styled(
                    format!("  {}", agent.label),
                    style,
                )));
            }

            if end < app.available_agents.len() {
                lines.push(Line::from(Span::styled(
                    "  ...",
                    Style::default().fg(t.muted),
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "  enter",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" next   ", Style::default().fg(t.muted)),
                Span::styled(
                    "esc",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" cancel   ", Style::default().fg(t.muted)),
                Span::styled(
                    "\u{2191}/\u{2193}",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" move", Style::default().fg(t.muted)),
            ]));
        }
        SpawnStep::Path => {
            lines.push(Line::from(vec![
                Span::styled("  cwd ", Style::default().fg(t.muted)),
                Span::styled(
                    format!("{}", modal.browser.cwd().display()),
                    Style::default().fg(t.text),
                ),
            ]));
            lines.push(Line::from(""));

            let entries = modal.browser.entries();
            let capacity = area.height.saturating_sub(13) as usize;
            let (start, end) =
                visible_range(entries.len(), modal.browser.selected(), capacity.max(1));

            if start > 0 {
                lines.push(Line::from(Span::styled(
                    "  ...",
                    Style::default().fg(t.muted),
                )));
            }

            for (i, entry) in entries.iter().enumerate().skip(start).take(end - start) {
                let icon = match entry.kind {
                    EntryKind::SelectCurrent => "\u{2192}",
                    EntryKind::CreateDirectory => "+",
                    EntryKind::Parent => "\u{2190}",
                    EntryKind::Directory => " ",
                };

                let style = if i == modal.browser.selected() {
                    Style::default()
                        .fg(t.bg)
                        .bg(t.highlight_bg)
                        .add_modifier(Modifier::BOLD)
                } else if matches!(entry.kind, EntryKind::CreateDirectory) {
                    Style::default().fg(t.accent)
                } else if matches!(entry.kind, EntryKind::SelectCurrent) {
                    Style::default().fg(t.green)
                } else {
                    Style::default().fg(t.text)
                };

                lines.push(Line::from(Span::styled(
                    format!("  {} {}", icon, entry.label),
                    style,
                )));
            }

            if end < entries.len() {
                lines.push(Line::from(Span::styled(
                    "  ...",
                    Style::default().fg(t.muted),
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "  enter",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" select   ", Style::default().fg(t.muted)),
                Span::styled(
                    "h",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" back   ", Style::default().fg(t.muted)),
                Span::styled(
                    "esc",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" cancel", Style::default().fg(t.muted)),
            ]));
        }
        SpawnStep::NewDirectoryName => {
            lines.push(Line::from(vec![
                Span::styled("  cwd ", Style::default().fg(t.muted)),
                Span::styled(
                    format!("{}", modal.browser.cwd().display()),
                    Style::default().fg(t.text),
                ),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  directory name",
                Style::default().fg(t.muted),
            )));
            lines.push(Line::from(Span::styled(
                if modal.new_dir_name.is_empty() {
                    "  _".to_owned()
                } else {
                    format!("  {}_", modal.new_dir_name)
                },
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "  enter",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" create   ", Style::default().fg(t.muted)),
                Span::styled(
                    "esc",
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" back", Style::default().fg(t.muted)),
            ]));
        }
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(t.text).bg(t.bg))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Line::from(vec![Span::styled(
                        " spawn ",
                        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                    )]))
                    .border_style(Style::default().fg(t.accent))
                    .style(Style::default().bg(t.bg)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn visible_range(total: usize, selected: usize, capacity: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    if total <= capacity {
        return (0, total);
    }

    let half = capacity / 2;
    let mut start = selected.saturating_sub(half);
    let max_start = total.saturating_sub(capacity);
    if start > max_start {
        start = max_start;
    }

    (start, (start + capacity).min(total))
}

fn truncate(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        return input.to_owned();
    }

    let mut out = input
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    out.push('~');
    out
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
