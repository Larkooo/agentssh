mod agents;
mod tmux;

use agents::AgentDefinition;
use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap},
};
use std::{
    env,
    io::{self, Stdout},
    path::PathBuf,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathInputMode {
    Presets,
    Custom,
}

#[derive(Debug, Clone)]
struct SpawnModal {
    step: SpawnStep,
    selected_agent: usize,
    selected_path: usize,
    path_input_mode: PathInputMode,
    custom_path: String,
    path_options: Vec<String>,
}

impl SpawnModal {
    fn selected_working_dir(&self) -> Option<String> {
        if self.path_input_mode == PathInputMode::Custom {
            let custom = self.custom_path.trim();
            if !custom.is_empty() {
                return Some(custom.to_owned());
            }
        }

        self.path_options.get(self.selected_path).cloned()
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
            status_line: "Select [+ New Instance] and press Enter".to_owned(),
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
                    "{} instance(s) running | {} agent CLI(s) detected",
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
        tabs.push(" Dashboard ".to_owned());
        for instance in &self.instances {
            let short = agents::short_instance_name(&instance.session.name);
            tabs.push(format!(" {}:{} ", instance.agent.id, short));
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

    fn open_new_modal(&mut self) {
        if self.available_agents.is_empty() {
            self.status_line = "No supported agent CLIs found in PATH".to_owned();
            return;
        }

        self.modal = Some(SpawnModal {
            step: SpawnStep::Agent,
            selected_agent: 0,
            selected_path: 0,
            path_input_mode: PathInputMode::Presets,
            custom_path: String::new(),
            path_options: default_working_paths(),
        });
    }

    fn create_instance_from_modal(&mut self) {
        let Some(modal) = self.modal.as_ref() else {
            return;
        };

        let Some(agent) = self.available_agents.get(modal.selected_agent).cloned() else {
            self.status_line = "Invalid agent selection".to_owned();
            self.modal = None;
            return;
        };

        let Some(working_dir) = modal.selected_working_dir() else {
            self.status_line = "Select or type a working directory first".to_owned();
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
        Create,
    }

    let mut action = Action::None;

    if let Some(modal) = app.modal.as_mut() {
        match modal.step {
            SpawnStep::Agent => match code {
                KeyCode::Esc => action = Action::Close,
                KeyCode::Char('j') | KeyCode::Down => {
                    if app.available_agents.is_empty() {
                        return;
                    }
                    modal.selected_agent = (modal.selected_agent + 1) % app.available_agents.len();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if app.available_agents.is_empty() {
                        return;
                    }
                    if modal.selected_agent == 0 {
                        modal.selected_agent = app.available_agents.len() - 1;
                    } else {
                        modal.selected_agent -= 1;
                    }
                }
                KeyCode::Enter => modal.step = SpawnStep::Path,
                _ => {}
            },
            SpawnStep::Path => match code {
                KeyCode::Esc => action = Action::Close,
                KeyCode::Left | KeyCode::Char('h') => modal.step = SpawnStep::Agent,
                KeyCode::Tab | KeyCode::BackTab => {
                    modal.path_input_mode = match modal.path_input_mode {
                        PathInputMode::Presets => PathInputMode::Custom,
                        PathInputMode::Custom => PathInputMode::Presets,
                    }
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if modal.path_input_mode == PathInputMode::Presets
                        && !modal.path_options.is_empty()
                    {
                        modal.selected_path = (modal.selected_path + 1) % modal.path_options.len();
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if modal.path_input_mode == PathInputMode::Presets
                        && !modal.path_options.is_empty()
                    {
                        if modal.selected_path == 0 {
                            modal.selected_path = modal.path_options.len() - 1;
                        } else {
                            modal.selected_path -= 1;
                        }
                    }
                }
                KeyCode::Backspace => {
                    if modal.path_input_mode == PathInputMode::Custom {
                        modal.custom_path.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if modal.path_input_mode == PathInputMode::Custom {
                        modal.custom_path.push(c);
                    }
                }
                KeyCode::Enter => action = Action::Create,
                _ => {}
            },
        }
    }

    match action {
        Action::None => {}
        Action::Close => app.modal = None,
        Action::Create => app.create_instance_from_modal(),
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
        KeyCode::Char('n') => app.open_new_modal(),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Enter => {
            if app.selected_tab == 0 && app.is_action_row_selected() {
                app.open_new_modal();
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
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(4),
        ])
        .split(frame.area());

    draw_tabs(frame, areas[0], app);

    if app.selected_tab == 0 {
        draw_dashboard(frame, areas[1], app);
    } else {
        draw_instance_tab(frame, areas[1], app);
    }

    draw_footer(frame, areas[2], app);

    if app.modal.is_some() {
        draw_spawn_modal(frame, app);
    }
}

fn draw_tabs(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let titles = app.tab_titles();
    let tabs = Tabs::new(titles)
        .select(app.selected_tab)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" agentssh ")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .divider("|");

    frame.render_widget(tabs, area);
}

fn draw_dashboard(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(area);

    draw_instance_table(frame, chunks[0], app);
    draw_summary_panel(frame, chunks[1], app);
}

fn draw_instance_table(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let mut rows: Vec<Row<'_>> = app
        .instances
        .iter()
        .enumerate()
        .map(|(index, instance)| {
            let state = if instance.session.attached {
                "attached"
            } else {
                "idle"
            };
            let marker = if instance.managed {
                "managed"
            } else {
                "external"
            };
            Row::new(vec![
                Cell::from(format!("{}", index + 1)),
                Cell::from(instance.agent.id.clone()),
                Cell::from(agents::short_instance_name(&instance.session.name)),
                Cell::from(state),
                Cell::from(marker),
                Cell::from(instance.session.last_line.clone()),
            ])
        })
        .collect();

    rows.push(
        Row::new(vec![
            Cell::from("+"),
            Cell::from("action"),
            Cell::from("[+ New Instance]"),
            Cell::from(""),
            Cell::from(""),
            Cell::from("Start a new agent session"),
        ])
        .style(Style::default().fg(Color::Green)),
    );

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(10),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Min(18),
        ],
    )
    .header(
        Row::new(vec![
            "#",
            "Agent",
            "Session",
            "State",
            "Kind",
            "Last Output / Action",
        ])
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .row_highlight_style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Instances ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );

    let mut state = TableState::default();
    state.select(Some(app.selected_row));

    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_summary_panel(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
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
            Line::from("Create new instance"),
            Line::from(""),
            Line::from("Select [+ New Instance] in the list and press Enter."),
            Line::from(""),
            Line::from("Detected CLIs:"),
            Line::from(available),
        ]
    } else if let Some(instance) = app.selected_instance() {
        let mut lines = vec![
            Line::from(format!("Agent: {}", instance.agent.label)),
            Line::from(format!("Binary: {}", instance.agent.binary)),
            Line::from(format!("Session: {}", instance.session.name)),
            Line::from(format!("Created: {}", instance.session.created)),
            Line::from(format!("Command: {}", instance.session.current_command)),
            Line::from(""),
            Line::from("Recent output:"),
        ];

        if instance.session.preview.is_empty() {
            lines.push(Line::from("(no output captured)"));
        } else {
            let tail = instance
                .session
                .preview
                .iter()
                .rev()
                .take(12)
                .cloned()
                .collect::<Vec<String>>()
                .into_iter()
                .rev()
                .collect::<Vec<String>>();

            for line in tail {
                lines.push(Line::from(line));
            }
        }

        lines
    } else {
        vec![Line::from("Select an instance")]
    };

    let panel = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Summary ")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(panel, area);
}

fn draw_instance_tab(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let Some(instance) = app.current_tab_instance() else {
        draw_dashboard(frame, area, app);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(8)])
        .split(area);

    let details = Paragraph::new(Text::from(vec![
        Line::from(format!(
            "Agent: {} ({})",
            instance.agent.label, instance.agent.binary
        )),
        Line::from(format!("Session: {}", instance.session.name)),
        Line::from(format!("Created: {}", instance.session.created)),
        Line::from(format!(
            "State: {} | Windows: {} | Kind: {}",
            if instance.session.attached {
                "attached"
            } else {
                "idle"
            },
            instance.session.windows,
            if instance.managed {
                "managed"
            } else {
                "external"
            }
        )),
        Line::from(format!("Command: {}", instance.session.current_command)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Instance ")
            .border_style(Style::default().fg(Color::DarkGray)),
    )
    .wrap(Wrap { trim: false });

    let preview = if instance.session.preview.is_empty() {
        vec![Line::from("(no output captured)")]
    } else {
        instance
            .session
            .preview
            .iter()
            .map(|line| Line::from(line.clone()))
            .collect::<Vec<Line<'_>>>()
    };

    let preview_panel = Paragraph::new(Text::from(preview))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Live Buffer ")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(details, chunks[0]);
    frame.render_widget(preview_panel, chunks[1]);
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let commands = "Use arrows + Enter. New instance is an action row in the list.";
    let shortcuts = "Shortcuts: Tab/←/→ tabs  x stop  r refresh  q quit";
    let panel = Paragraph::new(Text::from(vec![
        Line::from(commands),
        Line::from(shortcuts),
        Line::from(app.status_line.clone()),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Controls ")
            .border_style(Style::default().fg(Color::DarkGray)),
    )
    .wrap(Wrap { trim: false });

    frame.render_widget(panel, area);
}

fn draw_spawn_modal(frame: &mut ratatui::Frame<'_>, app: &App) {
    let Some(modal) = app.modal.as_ref() else {
        return;
    };

    let area = centered_rect(70, 65, frame.area());
    frame.render_widget(Clear, area);

    let selected_agent = app
        .available_agents
        .get(modal.selected_agent)
        .map(|a| format!("{} ({})", a.label, a.binary))
        .unwrap_or_else(|| "none".to_owned());

    let mut lines = vec![
        Line::from("Create a new agent instance"),
        Line::from(""),
        Line::from(format!(
            "1) Agent  [{}]",
            if modal.step == SpawnStep::Agent {
                "ACTIVE"
            } else {
                "done"
            }
        )),
        Line::from(format!("   Selected: {}", selected_agent)),
        Line::from(""),
        Line::from(format!(
            "2) Working Directory  [{}]",
            if modal.step == SpawnStep::Path {
                "ACTIVE"
            } else {
                "pending"
            }
        )),
    ];

    match modal.step {
        SpawnStep::Agent => {
            for (i, agent) in app.available_agents.iter().enumerate() {
                let marker = if i == modal.selected_agent { ">" } else { " " };
                lines.push(Line::from(format!(
                    "{} {} ({})",
                    marker, agent.label, agent.binary
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("enter next   esc cancel   ↑/↓ move"));
        }
        SpawnStep::Path => {
            lines.push(Line::from("   Presets:"));
            for (i, path) in modal.path_options.iter().enumerate() {
                let marker = if modal.path_input_mode == PathInputMode::Presets
                    && i == modal.selected_path
                {
                    ">"
                } else {
                    " "
                };
                lines.push(Line::from(format!("{} {}", marker, path)));
            }

            lines.push(Line::from(""));
            let custom_prefix = if modal.path_input_mode == PathInputMode::Custom {
                ">"
            } else {
                " "
            };
            let custom_value = if modal.custom_path.is_empty() {
                "(type a path)".to_owned()
            } else {
                modal.custom_path.clone()
            };
            lines.push(Line::from(format!(
                "{} Custom: {}",
                custom_prefix, custom_value
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(
                "enter create   tab toggle field   h back   esc cancel",
            ));
        }
    }

    let panel = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" New Instance Wizard ")
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(panel, area);
}

fn default_working_paths() -> Vec<String> {
    let mut paths = Vec::<String>::new();

    if let Ok(cwd) = env::current_dir() {
        push_unique_path(&mut paths, cwd);
    }

    if let Some(home) = env::var_os("HOME") {
        push_unique_path(&mut paths, PathBuf::from(home));
    }

    push_unique_path(&mut paths, PathBuf::from("/tmp"));
    push_unique_path(&mut paths, PathBuf::from("/"));

    if paths.is_empty() {
        paths.push(".".to_owned());
    }

    paths
}

fn push_unique_path(paths: &mut Vec<String>, path: PathBuf) {
    let as_str = path.to_string_lossy().to_string();
    if !paths.contains(&as_str) {
        paths.push(as_str);
    }
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
