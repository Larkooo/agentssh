mod tmux;

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
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
};
use std::{
    io::{self, Stdout},
    time::{Duration, Instant},
};
use tmux::Session;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "SSH TUI for managing tmux-backed agent sessions"
)]
struct Cli {
    #[arg(long, help = "Optional name filter for sessions")]
    filter: Option<String>,

    #[arg(long, default_value_t = 5, help = "Auto refresh interval in seconds")]
    refresh_seconds: u64,
}

struct App {
    sessions: Vec<Session>,
    selected: usize,
    last_refresh: Instant,
    refresh_interval: Duration,
    filter: Option<String>,
    should_quit: bool,
    status_line: String,
}

impl App {
    fn new(filter: Option<String>, refresh_interval: Duration) -> Self {
        Self {
            sessions: Vec::new(),
            selected: 0,
            last_refresh: Instant::now() - refresh_interval,
            refresh_interval,
            filter,
            should_quit: false,
            status_line: "Press r to refresh sessions".to_owned(),
        }
    }

    fn refresh(&mut self) {
        match tmux::list_sessions(self.filter.as_deref()) {
            Ok(sessions) => {
                self.sessions = sessions;
                if self.sessions.is_empty() {
                    self.selected = 0;
                    self.status_line = "No tmux sessions found".to_owned();
                } else if self.selected >= self.sessions.len() {
                    self.selected = self.sessions.len() - 1;
                    self.status_line = format!("Loaded {} sessions", self.sessions.len());
                } else {
                    self.status_line = format!("Loaded {} sessions", self.sessions.len());
                }
            }
            Err(err) => {
                self.sessions.clear();
                self.selected = 0;
                self.status_line = format!("Refresh failed: {err}");
            }
        }
        self.last_refresh = Instant::now();
    }

    fn next(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.sessions.len();
    }

    fn previous(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.sessions.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.selected)
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

    let mut app = App::new(cli.filter, Duration::from_secs(cli.refresh_seconds.max(1)));
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
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                    KeyCode::Char('j') | KeyCode::Down => app.next(),
                    KeyCode::Char('k') | KeyCode::Up => app.previous(),
                    KeyCode::Char('r') => app.refresh(),
                    KeyCode::Enter => {
                        if let Some(session) = app.selected_session() {
                            let attach_result = attach_into_session(terminal, &session.name);
                            match attach_result {
                                Ok(()) => {
                                    app.status_line = format!("Detached from {}", session.name)
                                }
                                Err(err) => {
                                    app.status_line =
                                        format!("Attach failed for {}: {err}", session.name)
                                }
                            }
                            app.refresh();
                        }
                    }
                    _ => {}
                },
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
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, areas[0], app);
    draw_body(frame, areas[1], app);
    draw_footer(frame, areas[2], app);
}

fn draw_header(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let title = format!(
        "Agent SSH - tmux sessions{}",
        app.filter
            .as_ref()
            .map(|f| format!(" (filter: {f})"))
            .unwrap_or_default()
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    frame.render_widget(block, area);
}

fn draw_body(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(area);

    draw_sessions_table(frame, chunks[0], app);
    draw_preview(frame, chunks[1], app);
}

fn draw_sessions_table(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let rows: Vec<Row<'_>> = app
        .sessions
        .iter()
        .map(|session| {
            let state = if session.attached {
                "attached"
            } else {
                "running"
            };
            Row::new(vec![
                Cell::from(session.name.clone()),
                Cell::from(state),
                Cell::from(session.current_command.clone()),
                Cell::from(session.last_line.clone()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(18),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec!["Session", "State", "Command", "Last Output"]).style(
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
    .block(Block::default().borders(Borders::ALL).title("Sessions"));

    let mut state = TableState::default();
    if !app.sessions.is_empty() {
        state.select(Some(app.selected));
    }

    if app.sessions.is_empty() {
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from("No sessions found."),
                Line::from(""),
                Line::from("Start an agent in tmux, for example:"),
                Line::from("tmux new-session -d -s codex 'codex'"),
            ]))
            .block(Block::default().borders(Borders::ALL).title("Sessions"))
            .wrap(Wrap { trim: false }),
            area,
        );
    } else {
        frame.render_stateful_widget(table, area, &mut state);
    }
}

fn draw_preview(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let preview_lines = if let Some(session) = app.selected_session() {
        if session.preview.is_empty() {
            vec![Line::from("(no output captured)")]
        } else {
            session
                .preview
                .iter()
                .map(|line| Line::from(line.clone()))
                .collect::<Vec<Line<'_>>>()
        }
    } else {
        vec![Line::from("Select a session")]
    };

    let title = app
        .selected_session()
        .map(|s| format!("Preview - {}", s.name))
        .unwrap_or_else(|| "Preview".to_owned());

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Text::from(preview_lines))
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let help = Line::from("j/k or arrows: move   enter: attach   r: refresh   q: quit");
    let status = Line::from(app.status_line.clone());
    let paragraph = Paragraph::new(Text::from(vec![help, status])).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Controls / Status"),
    );
    frame.render_widget(paragraph, area);
}
