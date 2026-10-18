use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Tabs},
    Terminal,
};
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::app::{TuiApp, TuiTab};

/// Enter the alternate-screen TUI at `initial_path` and run until quit.
/// `log` pins the Logs tab to a specific file instead of auto-detection.
pub fn run(initial_path: &Path, log: Option<&Path>) -> io::Result<()> {
    let mut app = TuiApp::new(initial_path, log.map(Path::to_path_buf));

    // Restore the terminal before the default panic report runs so a panic
    // does not leave the tty in raw mode on the alternate screen.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut TuiApp,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    loop {
        // Pick up a finished background scan and advance the spinner.
        app.poll_scan();
        if app.scanning {
            app.spinner_frame = app.spinner_frame.wrapping_add(1);
        }

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // Tabs
                    Constraint::Min(10),   // Content
                    Constraint::Length(3), // Status footer
                ])
                .split(f.area());

            let titles = TuiTab::all()
                .iter()
                .map(|t| Line::from(t.title()))
                .collect::<Vec<_>>();
            let tabs = Tabs::new(titles)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Lens Platform — Views [Tab / 1-4] "),
                )
                .select(match app.active_tab {
                    TuiTab::Storage => 0,
                    TuiTab::Services => 1,
                    TuiTab::Logs => 2,
                    TuiTab::Network => 3,
                })
                .style(Style::default().fg(Color::Cyan))
                .highlight_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                );
            f.render_widget(tabs, chunks[0]);

            match app.active_tab {
                TuiTab::Storage => {
                    let body_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(chunks[1]);
                    app.page_size = body_chunks[0].height.saturating_sub(2) as usize;

                    let list_items: Vec<ListItem> = app
                        .items
                        .iter()
                        .enumerate()
                        .map(|(idx, item)| {
                            let prefix = if item.is_dir { "📁 " } else { "📄 " };
                            let style = if idx == app.selected_index {
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            };
                            let content = format!(
                                "{}{}  ({:.2} MB)",
                                prefix,
                                item.name,
                                item.size as f64 / 1_048_576.0
                            );
                            ListItem::new(Line::from(content)).style(style)
                        })
                        .collect();

                    let dir_title = if app.scanning {
                        format!(" Directory: {:?} {} scanning… ", app.current_path, app.spinner())
                    } else if app.scan_incomplete {
                        format!(
                            " Directory: {:?} (INCOMPLETE — {} error(s)) ",
                            app.current_path,
                            app.scan_errors.len()
                        )
                    } else {
                        format!(" Directory: {:?} (j/k) ", app.current_path)
                    };
                    let list = List::new(list_items).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(dir_title),
                    );
                    // Stateful render so the viewport follows the
                    // selection past the first page.
                    let mut state = ListState::default();
                    state.select(Some(app.selected_index));
                    f.render_stateful_widget(list, body_chunks[0], &mut state);

                    let right_chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Length(8), Constraint::Min(4)])
                        .split(body_chunks[1]);

                    if let Some(selected) = app.items.get(app.selected_index) {
                        let pct = if app.total_size > 0 {
                            (selected.size as f64 / app.total_size as f64).min(1.0)
                        } else {
                            0.0
                        };

                        let detail_text = vec![
                            Line::from(vec![
                                Span::raw("Name: "),
                                Span::styled(
                                    &selected.name,
                                    Style::default().add_modifier(Modifier::BOLD),
                                ),
                            ]),
                            Line::from(format!(
                                "Type: {}",
                                if selected.is_dir {
                                    "Directory"
                                } else {
                                    "Regular File"
                                }
                            )),
                            Line::from(format!(
                                "Logical Size: {} bytes ({:.2} MB)",
                                selected.size,
                                selected.size as f64 / 1_048_576.0
                            )),
                            Line::from(format!(
                                "Allocated Size: {} bytes",
                                selected.allocated_size
                            )),
                            Line::from(format!("Relative Share: {:.1}%", pct * 100.0)),
                        ];
                        let detail = Paragraph::new(detail_text).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(" Storage Node Details "),
                        );
                        f.render_widget(detail, right_chunks[0]);

                        let gauge = Gauge::default()
                            .block(
                                Block::default()
                                    .borders(Borders::ALL)
                                    .title(" Footprint Share "),
                            )
                            .gauge_style(Style::default().fg(Color::Green))
                            .ratio(pct);
                        f.render_widget(gauge, right_chunks[1]);
                    } else {
                        let empty = Paragraph::new("No items in directory.").block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(" Storage Node Details "),
                        );
                        f.render_widget(empty, right_chunks[0]);
                    }
                }
                TuiTab::Network => {
                    let body_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                        .split(chunks[1]);
                    app.page_size = body_chunks[0].height.saturating_sub(2) as usize;

                    let listening = app.net_report.as_ref().map(|r| &r.listening);
                    let list_items: Vec<ListItem> = listening
                        .map(|entries| {
                            entries
                                .iter()
                                .enumerate()
                                .map(|(idx, l)| {
                                    let owner = if let Some(p) = &l.process {
                                        format!("{} (PID: {})", p.name, p.pid)
                                    } else {
                                        match l.owner_state {
                                            Some(lens_net::OwnerState::OwnerUnknown) => {
                                                "<permission-denied>".to_string()
                                            }
                                            Some(lens_net::OwnerState::Orphan) => {
                                                "<orphan>".to_string()
                                            }
                                            None => "<kernel>".to_string(),
                                        }
                                    };
                                    let line = format!(
                                        "{:<6} {:<20} {}",
                                        format!("{:?}", l.kind),
                                        format!("{}:{}", l.local_address, l.local_port),
                                        owner
                                    );
                                    let style = if idx == app.net_selected {
                                        Style::default()
                                            .fg(Color::Yellow)
                                            .add_modifier(Modifier::BOLD)
                                    } else {
                                        Style::default()
                                    };
                                    ListItem::new(Line::from(line)).style(style)
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    let net_list = List::new(list_items).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Active Listening Ports & Sockets "),
                    );
                    let mut state = ListState::default();
                    state.select(Some(app.net_selected));
                    f.render_stateful_widget(net_list, body_chunks[0], &mut state);

                    let summary_text = if let Some(rep) = &app.net_report {
                        vec![
                            Line::from(format!(
                                "Total Sockets:           {}",
                                rep.summary.total_sockets
                            )),
                            Line::from(format!(
                                "Listening Ports:         {}",
                                rep.summary.listening_ports
                            )),
                            Line::from(format!(
                                "Established Connections: {}",
                                rep.summary.established_connections
                            )),
                            Line::from(format!(
                                "TIME_WAIT Sockets:       {}",
                                rep.summary.time_wait_sockets
                            )),
                            Line::from(format!(
                                "Orphan Sockets:          {}",
                                rep.summary.orphan_sockets
                            )),
                            Line::from(format!(
                                "UNIX Domain Sockets:     {}",
                                rep.summary.unix_domain_sockets
                            )),
                        ]
                    } else {
                        vec![Line::from("Network inspection unavailable (/proc not mounted)")]
                    };

                    let detail = Paragraph::new(summary_text).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Network Summary "),
                    );
                    f.render_widget(detail, body_chunks[1]);
                }
                TuiTab::Services => {
                    let body_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                        .split(chunks[1]);
                    app.page_size = body_chunks[0].height.saturating_sub(2) as usize;

                    let list_items: Vec<ListItem> = app
                        .sys_units
                        .iter()
                        .enumerate()
                        .map(|(idx, u)| {
                            let diag = if u.diagnostics.is_empty() {
                                String::new()
                            } else {
                                format!("  ⚠{}", u.diagnostics.len())
                            };
                            let style = if idx == app.sys_selected {
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            };
                            ListItem::new(Line::from(format!("{}{}", u.name, diag))).style(style)
                        })
                        .collect();

                    let list = List::new(list_items).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(format!(
                                " Systemd Units ({}) ",
                                app.sys_units.len()
                            )),
                    );
                    let mut state = ListState::default();
                    state.select(Some(app.sys_selected));
                    f.render_stateful_widget(list, body_chunks[0], &mut state);

                    let detail_text = if let Some(u) = app.sys_units.get(app.sys_selected) {
                        let mut lines = vec![
                            Line::from(format!("Unit:      {}", u.name)),
                            Line::from(format!(
                                "ExecStart: {}",
                                u.exec_start.as_deref().unwrap_or("<none>")
                            )),
                            Line::from(format!(
                                "Wants: {} | Requires: {} | Before: {} | After: {}",
                                u.wants.len(),
                                u.requires.len(),
                                u.before.len(),
                                u.after.len()
                            )),
                            Line::from(format!("Drop-ins:  {}", u.drop_ins.len())),
                        ];
                        for d in u.drop_ins.iter().take(5) {
                            lines.push(Line::from(format!("  ↳ {}", d)));
                        }
                        for d in u.diagnostics.iter().take(5) {
                            lines.push(Line::from(format!("  ! {} {}", d.code, d.message)));
                        }
                        lines
                    } else {
                        vec![Line::from(
                            "No units loaded (/etc/systemd/system missing or empty)",
                        )]
                    };

                    let detail = Paragraph::new(detail_text).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Unit Details "),
                    );
                    f.render_widget(detail, body_chunks[1]);
                }
                TuiTab::Logs => {
                    let visible = app.visible_log_indices();
                    let title = match (&app.log_path, &app.log_search) {
                        (Some(p), Some(q)) => format!(
                            " {} (tail) — /{} [{} lines] ",
                            p.display(),
                            q,
                            visible.len()
                        ),
                        (Some(p), None) => format!(" {} (tail) ", p.display()),
                        _ => " Logs (no log file found) ".to_string(),
                    };
                    let height = chunks[1].height.saturating_sub(2) as usize;
                    app.page_size = height;
                    let end = app.log_scroll.min(visible.len());
                    let start = end.saturating_sub(height);
                    let items: Vec<ListItem> = visible[start..end]
                        .iter()
                        .map(|&i| {
                            let l = &app.log_lines[i];
                            let lvl = lens_log::detect_level(l);
                            let color = match lvl {
                                lens_log::LogLevel::Error | lens_log::LogLevel::Fatal => {
                                    Color::Red
                                }
                                lens_log::LogLevel::Warn => Color::Yellow,
                                lens_log::LogLevel::Debug | lens_log::LogLevel::Trace => {
                                    Color::DarkGray
                                }
                                _ => Color::White,
                            };
                            ListItem::new(Line::from(l.clone())).style(Style::default().fg(color))
                        })
                        .collect();
                    let list = List::new(items)
                        .block(Block::default().borders(Borders::ALL).title(title));
                    f.render_widget(list, chunks[1]);
                }
            }

            if app.show_help {
                f.render_widget(ratatui::widgets::Clear, chunks[1]);
                let help = Paragraph::new(vec![
                    Line::from("Keys:"),
                    Line::from("  Tab / 1-4   switch views"),
                    Line::from("  j / k       move selection (wraps)"),
                    Line::from("  PgUp/PgDn   page up/down"),
                    Line::from("  g / G       first / last"),
                    Line::from("  Enter / l   open directory"),
                    Line::from("  Backspace/h parent directory"),
                    Line::from("  /           search log lines (Enter applies, Esc clears)"),
                    Line::from("  r           rescan current directory"),
                    Line::from("  ?           toggle this help"),
                    Line::from("  q / Esc     quit"),
                ])
                .block(Block::default().borders(Borders::ALL).title(" Help "));
                f.render_widget(help, chunks[1]);
            }

            // Footer — doubles as the `/` search prompt on the Logs tab.
            let footer_text = if app.search_active {
                format!(" /{}", app.search_buf)
            } else {
                format!(
                    " {} | Hotkeys: [Tab/1-4] Views  [q] Quit  [j/k] Navigate  [PgUp/PgDn/g/G] Page  [Enter] Open  [Backspace/h] Parent  [/] Search  [?] Help  [r] Reload",
                    app.status_message
                )
            };
            let footer = Paragraph::new(footer_text)
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(footer, chunks[2]);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // `/` search mode captures keystrokes until Enter or Esc.
                if app.search_active {
                    match key.code {
                        KeyCode::Enter => {
                            app.log_search = Some(app.search_buf.clone());
                            app.search_active = false;
                            app.log_scroll = app.visible_log_indices().len();
                        }
                        KeyCode::Esc => {
                            app.search_active = false;
                            app.search_buf.clear();
                            app.log_search = None;
                        }
                        KeyCode::Backspace => {
                            app.search_buf.pop();
                        }
                        KeyCode::Char(c) => app.search_buf.push(c),
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Esc => {
                        if app.show_help {
                            app.show_help = false;
                        } else {
                            return Ok(());
                        }
                    }
                    KeyCode::Tab => app.next_tab(),
                    KeyCode::Char('?') => app.show_help = !app.show_help,
                    KeyCode::Char('1') => app.set_tab(TuiTab::Storage),
                    KeyCode::Char('2') => app.set_tab(TuiTab::Services),
                    KeyCode::Char('3') => app.set_tab(TuiTab::Logs),
                    KeyCode::Char('4') => app.set_tab(TuiTab::Network),
                    KeyCode::Char('j') | KeyCode::Down => app.next(),
                    KeyCode::Char('k') | KeyCode::Up => app.previous(),
                    KeyCode::PageDown => app.page_down(),
                    KeyCode::PageUp => app.page_up(),
                    KeyCode::Char('g') | KeyCode::Home => app.jump_first(),
                    KeyCode::Char('G') | KeyCode::End => app.jump_last(),
                    KeyCode::Char('/') => {
                        if app.active_tab == TuiTab::Logs {
                            app.search_active = true;
                            app.search_buf.clear();
                        }
                    }
                    KeyCode::Enter | KeyCode::Char('l') => app.enter(),
                    KeyCode::Backspace | KeyCode::Char('h') => app.parent(),
                    KeyCode::Char('r') => {
                        app.reload();
                        app.reload_network();
                        app.reload_services();
                        app.reload_logs();
                    }
                    _ => {}
                }
            }
        }
    }
}
