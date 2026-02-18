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

#[derive(Debug, Clone)]
struct NewInstanceModal {
    selected_agent: usize,
}

struct App {
    available_agents: Vec<AgentDefinition>,
    instances: Vec<AgentInstance>,
    selected_row: usize,
    selected_tab: usize,
    modal: Option<NewInstanceModal>,
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
            status_line: "Press n to start a new agent instance".to_owned(),
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

    fn clamp_selection(&mut self) {
        if self.instances.is_empty() {
            self.selected_row = 0;
            self.selected_tab = 0;
            return;
        }

        if self.selected_row >= self.instances.len() {
            self.selected_row = self.instances.len() - 1;
        }

        let max_tab = self.instances.len();
        if self.selected_tab > max_tab {
            self.selected_tab = 0;
        }
    }

    fn selected_instance(&self) -> Option<&AgentInstance> {
        self.instances.get(self.selected_row)
    }

    fn current_tab_instance(&self) -> Option<&AgentInstance> {
        if self.selected_tab == 0 {
            return None;
        }
        self.instances.get(self.selected_tab - 1)
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
        if self.instances.is_empty() {
            return;
        }
        self.selected_row = (self.selected_row + 1) % self.instances.len();
    }

    fn previous_row(&mut self) {
        if self.instances.is_empty() {
            return;
        }
        if self.selected_row == 0 {
            self.selected_row = self.instances.len() - 1;
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
        self.modal = Some(NewInstanceModal { selected_agent: 0 });
    }

    fn create_instance_from_modal(&mut self) {
        let Some(modal) = self.modal.as_ref() else {
            return;
        };

        let Some(agent) = self.available_agents.get(modal.selected_agent) else {
            self.status_line = "Invalid agent selection".to_owned();
            self.modal = None;
            return;
        };

        let session_name = agents::build_managed_session_name(&agent.id);
        match tmux::create_session(&session_name, &agent.launch) {
            Ok(()) => {
                self.status_line = format!("Started {} in {}", agent.label, session_name);
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
            self.status_line = "No instance selected".to_owned();
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
    let Some(modal) = app.modal.as_mut() else {
        return;
    };

    match code {
        KeyCode::Esc => app.modal = None,
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
        KeyCode::Enter => app.create_instance_from_modal(),
        _ => {}
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
        KeyCode::Char('n') => app.open_new_modal(),
        KeyCode::Char('x') => app.kill_selected_instance(),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Enter => {
            if let Some(instance) = app.active_instance_ref() {
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
        draw_new_instance_modal(frame, app);
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
    if app.instances.is_empty() {
        let available = if app.available_agents.is_empty() {
            "none".to_owned()
        } else {
            app.available_agents
                .iter()
                .map(|a| a.label.clone())
                .collect::<Vec<String>>()
                .join(", ")
        };

        let panel = Paragraph::new(Text::from(vec![
            Line::from("No running agent instances."),
            Line::from(""),
            Line::from("Detected agent CLIs:"),
            Line::from(available),
            Line::from(""),
            Line::from("Press n to start a new instance."),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Instances ")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false });

        frame.render_widget(panel, area);
        return;
    }

    let rows: Vec<Row<'_>> = app
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

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(10),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec![
            "#",
            "Agent",
            "Session",
            "State",
            "Kind",
            "Last Output",
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
    let lines = if let Some(instance) = app.selected_instance() {
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
    let commands =
        "h/l tabs  j/k list  enter attach  n new  x stop  r refresh  d dashboard  q quit";
    let panel = Paragraph::new(Text::from(vec![
        Line::from(commands),
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

fn draw_new_instance_modal(frame: &mut ratatui::Frame<'_>, app: &App) {
    let Some(modal) = app.modal.as_ref() else {
        return;
    };

    let area = centered_rect(60, 58, frame.area());
    frame.render_widget(Clear, area);

    let mut lines = vec![Line::from("Start a new agent instance"), Line::from("")];

    for (i, agent) in app.available_agents.iter().enumerate() {
        let prefix = if i == modal.selected_agent { ">" } else { " " };
        lines.push(Line::from(format!(
            "{} {} ({})",
            prefix, agent.label, agent.binary
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("enter create   esc cancel   j/k move"));

    let panel = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" New Instance ")
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(panel, area);
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
