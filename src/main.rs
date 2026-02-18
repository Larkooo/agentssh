mod agents;
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
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Tabs, Wrap},
};
use std::{
    env,
    io::{self, Stdout},
    time::{Duration, Instant},
};

#[derive(Parser, Debug)]
#[command(author, version, about = "Agent-first SSH interface with tabbed TUI")]
struct Cli {
    #[arg(long, default_value_t = 3, help = "Auto refresh interval in seconds")]
    refresh_seconds: u64,
}

#[derive(Debug, Clone)]
struct AgentInstance {
    agent: AgentDefinition,
    session: tmux::Session,
    managed: bool,
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

#[derive(Debug, Clone, Copy)]
struct UiTheme {
    bg: Color,
    chrome_bg: Color,
    panel_bg: Color,
    border: Color,
    text: Color,
    muted: Color,
    accent: Color,
    highlight_bg: Color,
    red: Color,
    yellow: Color,
    green: Color,
}

impl UiTheme {
    fn new() -> Self {
        Self {
            bg: Color::Rgb(5, 6, 9),
            chrome_bg: Color::Rgb(14, 16, 20),
            panel_bg: Color::Rgb(17, 20, 24),
            border: Color::Rgb(58, 64, 74),
            text: Color::Rgb(220, 226, 235),
            muted: Color::Rgb(138, 146, 160),
            accent: Color::Rgb(41, 227, 223),
            highlight_bg: Color::Rgb(41, 227, 223),
            red: Color::Rgb(255, 95, 86),
            yellow: Color::Rgb(255, 189, 46),
            green: Color::Rgb(39, 201, 63),
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
}

impl App {
    fn new(refresh_interval: Duration) -> Self {
        Self {
            available_agents: Vec::new(),
            instances: Vec::new(),
            selected_row: 0,
            selected_tab: 0,
            modal: None,
            last_refresh: Instant::now() - refresh_interval,
            refresh_interval,
            should_quit: false,
            status_line: "Select New Instance and press Enter".to_owned(),
            theme: UiTheme::new(),
        }
    }

    fn refresh(&mut self) {
        self.available_agents = agents::detect_available_agents();

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
                        Some(AgentInstance {
                            agent,
                            session,
                            managed,
                        })
                    })
                    .collect();

                self.instances
                    .sort_by(|a, b| a.session.name.cmp(&b.session.name));
                self.clamp_selection();

                self.status_line = format!(
                    "{} running | {} agent CLIs detected",
                    self.instances.len(),
                    self.available_agents.len()
                );
            }
            Err(err) => {
                self.instances.clear();
                self.selected_row = 0;
                self.selected_tab = 0;
                self.status_line = format!("Refresh failed: {err}");
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

    fn tab_titles(&self) -> Vec<String> {
        let mut tabs = Vec::with_capacity(self.instances.len() + 1);
        tabs.push(" d dashboard ".to_owned());
        for instance in &self.instances {
            let short = truncate(&agents::short_instance_name(&instance.session.name), 18);
            tabs.push(format!(" {} {} ", instance.agent.id, short));
        }
        tabs
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

        let start = env::current_dir().unwrap_or_else(|_| "/".into());
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

        let launch_command = agents::build_launch_command(&working_dir, &agent.launch);
        let session_name = agents::build_managed_session_name(&agent.id);

        match tmux::create_session(&session_name, &launch_command) {
            Ok(()) => {
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
    run(cli)
}

fn run(cli: Cli) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let mut app = App::new(Duration::from_secs(cli.refresh_seconds.max(1)));
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
                    if app.modal.is_some() {
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

    attach_result
}

fn draw_ui(frame: &mut ratatui::Frame<'_>, app: &App) {
    let theme = app.theme;

    frame.render_widget(
        Block::default()
            .style(Style::default().bg(theme.bg))
            .borders(Borders::NONE),
        frame.area(),
    );

    let app_rect = centered_rect(88, 94, frame.area());
    let chrome = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(theme.chrome_bg))
        .border_style(Style::default().fg(theme.border));
    let inner = chrome.inner(app_rect);
    frame.render_widget(chrome, app_rect);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(inner);

    draw_title_bar(frame, sections[0], app);
    draw_tabs(frame, sections[1], app);

    if app.selected_tab == 0 {
        draw_dashboard(frame, sections[2], app);
    } else {
        draw_instance_tab(frame, sections[2], app);
    }

    draw_status_line(frame, sections[3], app);
    draw_footer(frame, sections[4], app);

    if app.modal.is_some() {
        draw_spawn_modal(frame, app);
    }
}

fn draw_title_bar(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    let line = Line::from(vec![
        Span::styled("o", Style::default().fg(t.red)),
        Span::raw(" "),
        Span::styled("o", Style::default().fg(t.yellow)),
        Span::raw(" "),
        Span::styled("o", Style::default().fg(t.green)),
        Span::styled("  [dir] ssh agentssh", Style::default().fg(t.muted)),
    ]);

    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(t.chrome_bg)),
        area,
    );
}

fn draw_tabs(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    let tabs = Tabs::new(app.tab_titles())
        .select(app.selected_tab)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().bg(t.panel_bg))
                .border_style(Style::default().fg(t.border)),
        )
        .style(Style::default().fg(t.muted).bg(t.panel_bg))
        .highlight_style(
            Style::default()
                .fg(t.text)
                .bg(t.panel_bg)
                .add_modifier(Modifier::BOLD),
        )
        .divider("|");

    frame.render_widget(tabs, area);
}

fn draw_dashboard(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(area);

    draw_instance_list(frame, chunks[0], app);
    draw_summary_panel(frame, chunks[1], app);
}

fn draw_instance_list(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    let mut lines = vec![Line::from(Span::styled(
        "~ instances ~",
        Style::default().fg(t.text).add_modifier(Modifier::BOLD),
    ))];

    let total = app.dashboard_row_count();
    let capacity = area.height.saturating_sub(4) as usize;
    let (start, end) = visible_range(total, app.selected_row, capacity.max(1));

    if start > 0 {
        lines.push(Line::from(Span::styled(
            "...",
            Style::default().fg(t.muted),
        )));
    }

    for index in start..end {
        let selected = index == app.selected_row;
        let label = if index < app.instances.len() {
            let instance = &app.instances[index];
            format!(
                "{} {}",
                instance.agent.id,
                truncate(&agents::short_instance_name(&instance.session.name), 24)
            )
        } else {
            "New Instance".to_owned()
        };

        let style = if selected {
            Style::default()
                .fg(t.bg)
                .bg(t.highlight_bg)
                .add_modifier(Modifier::BOLD)
        } else if index == app.instances.len() {
            Style::default().fg(t.accent)
        } else {
            Style::default().fg(t.text)
        };

        lines.push(Line::from(Span::styled(format!("{}", label), style)));
    }

    if end < total {
        lines.push(Line::from(Span::styled(
            "...",
            Style::default().fg(t.muted),
        )));
    }

    let panel = Paragraph::new(Text::from(lines))
        .style(Style::default().bg(t.panel_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.border))
                .style(Style::default().bg(t.panel_bg)),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(panel, area);
}

fn draw_summary_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;

    let lines = if app.is_action_row_selected() || app.instances.is_empty() {
        let available = if app.available_agents.is_empty() {
            "none".to_owned()
        } else {
            app.available_agents
                .iter()
                .map(|a| a.label.clone())
                .collect::<Vec<String>>()
                .join(", ")
        };

        vec![
            Line::from(Span::styled(
                "create new agent instance",
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("1. select New Instance on the left"),
            Line::from("2. choose agent"),
            Line::from("3. navigate folder and select Use <path>"),
            Line::from("4. attach when ready"),
            Line::from(""),
            Line::from(format!("detected: {available}")),
        ]
    } else if let Some(instance) = app.selected_instance() {
        let mut lines = vec![
            Line::from(Span::styled(
                instance.agent.label.clone(),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("session: {}", instance.session.name)),
            Line::from(format!("created: {}", instance.session.created)),
            Line::from(format!(
                "state: {}",
                if instance.session.attached {
                    "attached"
                } else {
                    "idle"
                }
            )),
            Line::from(format!(
                "kind: {}",
                if instance.managed {
                    "managed"
                } else {
                    "external"
                }
            )),
            Line::from(format!("cmd: {}", instance.session.current_command)),
            Line::from(""),
        ];

        let preview_space = area.height.saturating_sub(lines.len() as u16 + 3) as usize;
        let preview_take = preview_space.max(4);
        let preview = instance
            .session
            .preview
            .iter()
            .rev()
            .take(preview_take)
            .cloned()
            .collect::<Vec<String>>()
            .into_iter()
            .rev()
            .collect::<Vec<String>>();

        if preview.is_empty() {
            lines.push(Line::from("(no output captured)"));
        } else {
            for line in preview {
                lines.push(Line::from(line));
            }
        }

        lines
    } else {
        vec![Line::from("select an instance")]
    };

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(t.text).bg(t.panel_bg))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(t.border))
                    .style(Style::default().bg(t.panel_bg)),
            )
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

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(8)])
        .split(area);

    let details = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            format!("{} ({})", instance.agent.label, instance.agent.binary),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("session: {}", instance.session.name)),
        Line::from(format!("created: {}", instance.session.created)),
        Line::from(format!("windows: {}", instance.session.windows)),
        Line::from(format!("command: {}", instance.session.current_command)),
    ]))
    .style(Style::default().fg(t.text).bg(t.panel_bg))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border))
            .style(Style::default().bg(t.panel_bg)),
    );

    let preview_take = sections[1].height.saturating_sub(2) as usize;
    let preview = instance
        .session
        .preview
        .iter()
        .rev()
        .take(preview_take.max(4))
        .cloned()
        .collect::<Vec<String>>()
        .into_iter()
        .rev()
        .collect::<Vec<String>>();

    let preview_lines = if preview.is_empty() {
        vec![Line::from("(no output captured)")]
    } else {
        preview
            .into_iter()
            .map(Line::from)
            .collect::<Vec<Line<'_>>>()
    };

    let preview_panel = Paragraph::new(Text::from(preview_lines))
        .style(Style::default().fg(t.text).bg(t.panel_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" live buffer ")
                .border_style(Style::default().fg(t.border))
                .style(Style::default().bg(t.panel_bg)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(details, sections[0]);
    frame.render_widget(preview_panel, sections[1]);
}

fn draw_status_line(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("status: ", Style::default().fg(t.muted)),
            Span::styled(app.status_line.clone(), Style::default().fg(t.text)),
        ]))
        .style(Style::default().bg(t.chrome_bg)),
        area,
    );
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    let commands = Line::from(vec![
        Span::styled(
            "up/down",
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" select   ", Style::default().fg(t.muted)),
        Span::styled(
            "enter",
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" open/attach   ", Style::default().fg(t.muted)),
        Span::styled(
            "left/right",
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" tabs   ", Style::default().fg(t.muted)),
        Span::styled(
            "x",
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" stop   ", Style::default().fg(t.muted)),
        Span::styled(
            "q",
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" quit", Style::default().fg(t.muted)),
    ]);

    frame.render_widget(
        Paragraph::new(Text::from(vec![commands]))
            .style(Style::default().bg(t.chrome_bg))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(t.border))
                    .style(Style::default().bg(t.chrome_bg)),
            ),
        area,
    );
}

fn draw_spawn_modal(frame: &mut ratatui::Frame<'_>, app: &App) {
    let t = app.theme;
    let Some(modal) = app.modal.as_ref() else {
        return;
    };

    let area = centered_rect(74, 78, frame.area());
    frame.render_widget(Clear, area);

    let selected_agent = app
        .available_agents
        .get(modal.selected_agent)
        .map(|a| format!("{} ({})", a.label, a.binary))
        .unwrap_or_else(|| "none".to_owned());

    let mut lines = vec![
        Line::from(Span::styled(
            "new instance wizard",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!(
            "1) agent [{}]",
            if modal.step == SpawnStep::Agent {
                "active"
            } else {
                "done"
            }
        )),
        Line::from(format!("   {}", selected_agent)),
        Line::from(""),
        Line::from(format!(
            "2) path [{}]",
            if modal.step == SpawnStep::Path {
                "active"
            } else {
                "pending"
            }
        )),
    ];

    match modal.step {
        SpawnStep::Agent => {
            let capacity = area.height.saturating_sub(11) as usize;
            let (start, end) = visible_range(
                app.available_agents.len(),
                modal.selected_agent,
                capacity.max(1),
            );
            if start > 0 {
                lines.push(Line::from("..."));
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
                    format!("{} ({})", agent.label, agent.binary),
                    style,
                )));
            }

            if end < app.available_agents.len() {
                lines.push(Line::from("..."));
            }

            lines.push(Line::from(""));
            lines.push(Line::from("enter next   esc cancel   up/down move"));
        }
        SpawnStep::Path => {
            lines.push(Line::from(format!(
                "   cwd: {}",
                modal.browser.cwd().display()
            )));
            lines.push(Line::from(""));

            let entries = modal.browser.entries();
            let capacity = area.height.saturating_sub(12) as usize;
            let (start, end) =
                visible_range(entries.len(), modal.browser.selected(), capacity.max(1));

            if start > 0 {
                lines.push(Line::from(Span::styled(
                    "...",
                    Style::default().fg(t.muted),
                )));
            }

            for (i, entry) in entries.iter().enumerate().skip(start).take(end - start) {
                let icon = match entry.kind {
                    EntryKind::SelectCurrent => "[use]",
                    EntryKind::CreateDirectory => "[new]",
                    EntryKind::Parent => "[..]",
                    EntryKind::Directory => "[dir]",
                };

                let style = if i == modal.browser.selected() {
                    Style::default()
                        .fg(t.bg)
                        .bg(t.highlight_bg)
                        .add_modifier(Modifier::BOLD)
                } else if matches!(entry.kind, EntryKind::CreateDirectory) {
                    Style::default().fg(t.accent)
                } else {
                    Style::default().fg(t.text)
                };

                lines.push(Line::from(Span::styled(
                    format!("{} {}", icon, entry.label),
                    style,
                )));
            }

            if end < entries.len() {
                lines.push(Line::from(Span::styled(
                    "...",
                    Style::default().fg(t.muted),
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(
                "enter open/select   pgup/pgdn scroll   h back   esc cancel",
            ));
        }
        SpawnStep::NewDirectoryName => {
            lines.push(Line::from(format!(
                "   cwd: {}",
                modal.browser.cwd().display()
            )));
            lines.push(Line::from(""));
            lines.push(Line::from("new directory name:"));
            lines.push(Line::from(Span::styled(
                if modal.new_dir_name.is_empty() {
                    "_".to_owned()
                } else {
                    format!("{}_", modal.new_dir_name)
                },
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from("enter create   esc back   type to set name"));
        }
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(t.text).bg(t.panel_bg))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" spawn ")
                    .border_style(Style::default().fg(t.accent))
                    .style(Style::default().bg(t.panel_bg)),
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
