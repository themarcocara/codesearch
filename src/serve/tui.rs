//! ratatui-based TUI for `codesearch serve`.
//!
//! Replaces the old `print_dashboard()` eprintln approach with a fullscreen
//! alternate-screen TUI that renders a live status table without flickering.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table, TableState};
use ratatui::Terminal;

use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};

use tokio_util::sync::CancellationToken;

use super::ServeState;
use crate::cli::doctor;
use crate::constants::{DB_DIR_NAME, LANG_CSHARP};
use crate::index::IndexManager;

// ---------------------------------------------------------------------------
// Key actions
// ---------------------------------------------------------------------------

/// Actions returned by key handling, replacing the old `bool` return.
#[derive(Debug)]
enum KeyAction {
    /// No action / key not recognized.
    None,
    /// User pressed `s` — reload repos config.
    Reload,
    /// User pressed `i` — show info overlay for repo at given index.
    ShowInfo(usize),
    /// User pressed `d` — run doctor for repo at given index.
    RunDoctor(usize),
    /// User pressed `f` — force reindex repo at given index.
    ForceReindex(usize),
}

// ---------------------------------------------------------------------------
// Overlay state (modal shown on top of the table)
// ---------------------------------------------------------------------------

/// Modal overlay shown on top of the normal TUI content.
/// `Esc` dismisses it.
enum OverlayState {
    /// Info modal: repo name, chunks, files, db size, model, dims, etc.
    Info {
        alias: String,
        chunks: usize,
        files: usize,
        max_chunk_id: u32,
        db_size_human: String,
        model: String,
        dims: usize,
        lock: String,
        index_age: String,
    },
    /// Doctor is running in background — show spinner.
    DoctorRunning { alias: String },
    /// Doctor results: per-check pass/warn/fail lines.
    Doctor { alias: String, results: Vec<String> },
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the fullscreen TUI.  Spawns as a tokio task from `run_serve`.
///
/// Returns `Ok(())` when the user presses `q`, or when the
/// `cancel_token` is cancelled externally (e.g. Ctrl-C from the main task).
///
/// Terminal restoration is guaranteed on normal exit and on errors.
/// Panics mid-frame are extremely unlikely (ratatui is panic-free in practice)
/// and the OS will restore raw mode on process exit as a last resort.
pub async fn run_tui(
    state: Arc<ServeState>,
    cancel_token: CancellationToken,
    serve_url: String,
) -> io::Result<()> {
    // Setup terminal
    crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Run main loop. Errors (e.g. from terminal.draw) propagate up
    // and are caught below to ensure restoration.
    let result = run_tui_loop(&mut terminal, state, cancel_token, &serve_url).await;

    // Always restore terminal, even on error
    restore_terminal(&mut terminal)?;

    result
}

async fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: Arc<ServeState>,
    cancel_token: CancellationToken,
    serve_url: &str,
) -> io::Result<()> {
    // TUI-local state
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let tick_interval = Duration::from_millis(500);
    let poll_timeout = Duration::from_millis(100);

    // sysinfo System instance — must persist across frames so cpu_usage()
    // can compute a delta between refresh calls (first call always returns 0).
    let mut sys_system: Option<sysinfo::System> = None;

    // Optional modal overlay (dismissed by Esc)
    let mut overlay: Option<OverlayState> = None;

    // Channel to receive doctor results from background task
    let (doctor_tx, mut doctor_rx) = tokio::sync::mpsc::channel::<OverlayState>(1);

    // Main loop
    loop {
        // Draw the UI
        let repos = state.repo_statuses_lightweight();

        // Clamp selection
        if !repos.is_empty() {
            let sel = table_state.selected().unwrap_or(0);
            if sel >= repos.len() {
                table_state.select(Some(repos.len() - 1));
            }
        }

        // Load session count + CPU for footer
        let active = state.active_session_count();
        let cpu = cpu_usage_str(&mut sys_system);

        terminal.draw(|f| {
            let size = f.area();
            let chunks = Layout::vertical([
                Constraint::Length(3), // header
                Constraint::Min(4),    // body (table)
                Constraint::Length(4), // detail panel (selected repo info + optional error)
                Constraint::Length(1), // footer
            ])
            .split(size);

            render_header(f, chunks[0], serve_url);
            render_table(f, chunks[1], &repos, &mut table_state);
            render_detail(f, chunks[2], &repos, &table_state, &state);
            let csharp_helper = state
                .symbol_registry
                .get(LANG_CSHARP)
                .map(|i| i.is_available())
                .unwrap_or(false);
            render_footer(
                f,
                chunks[3],
                &repos,
                &table_state,
                active,
                &cpu,
                csharp_helper,
            );

            // Render overlay on top of everything if active
            if let Some(ref ov) = overlay {
                render_overlay(f, size, ov);
            }
        })?;

        // Check if doctor result arrived from background task
        if let Ok(result) = doctor_rx.try_recv() {
            overlay = Some(result);
        }

        // Poll for key events
        let mut should_quit = false;
        while event::poll(poll_timeout)? {
            if let Event::Key(key) = event::read()? {
                // On Windows, crossterm emits both Press and Release events.
                // Only act on Press to avoid double-stepping (scroll by 2).
                if key.kind != event::KeyEventKind::Press {
                    continue;
                }

                // If overlay is active, Esc dismisses it; no other keys processed
                if overlay.is_some() {
                    if matches!(key.code, KeyCode::Esc) {
                        overlay = None;
                    }
                    continue;
                }

                if is_quit_key(key) {
                    should_quit = true;
                    break;
                }
                match handle_key(key, &mut table_state, repos.len()) {
                    KeyAction::Reload => {
                        // 's' pressed — force reload of repos config
                        // Clear mtime so reload_if_changed actually reloads
                        if let Ok(mut mtime_guard) = state.config_mtime.write() {
                            *mtime_guard = None;
                        }
                        let _ = state.reload_if_changed();
                    }
                    KeyAction::ShowInfo(idx) => {
                        if let Some(ov) = build_info_overlay(idx, &repos, &state) {
                            overlay = Some(ov);
                        }
                    }
                    KeyAction::RunDoctor(idx) => {
                        if idx < repos.len() {
                            let alias = repos[idx].0.clone();
                            // Show "running" overlay immediately
                            overlay = Some(OverlayState::DoctorRunning {
                                alias: alias.clone(),
                            });
                            // Spawn background task to run diagnostics
                            spawn_doctor(alias, state.clone(), doctor_tx.clone());
                        }
                    }
                    KeyAction::ForceReindex(idx) => {
                        if idx < repos.len() {
                            let alias = repos[idx].0.clone();
                            spawn_force_reindex(alias, &state);
                        }
                    }
                    KeyAction::None => {}
                }
            }
        }

        if should_quit {
            // User pressed q — signal shutdown to the whole serve process
            cancel_token.cancel();
            break;
        }

        if cancel_token.is_cancelled() {
            break;
        }

        tokio::time::sleep(tick_interval).await;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Restore helpers
// ---------------------------------------------------------------------------

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    terminal::disable_raw_mode()?;
    crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

fn is_quit_key(key: KeyEvent) -> bool {
    // Ctrl-C is intentionally NOT a quit key here. crossterm's raw mode delivers
    // it as a key event (ENABLE_PROCESSED_INPUT off on Windows / ISIG off on Unix),
    // so the OS-level ctrlc::set_handler in main.rs is bypassed while the TUI runs.
    // Treating Ctrl-C as quit was a foot-gun: a stray Ctrl-C in the wrong terminal
    // would tear down the whole serve process. Use `q` instead.
    matches!(key.code, KeyCode::Char('q'))
}

fn handle_key(key: KeyEvent, table_state: &mut TableState, row_count: usize) -> KeyAction {
    if row_count == 0 {
        return KeyAction::None;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            let i = table_state.selected().unwrap_or(0);
            if i > 0 {
                table_state.select(Some(i - 1));
            }
            KeyAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let i = table_state.selected().unwrap_or(0);
            if i < row_count - 1 {
                table_state.select(Some(i + 1));
            }
            KeyAction::None
        }
        KeyCode::Home => {
            table_state.select(Some(0));
            KeyAction::None
        }
        KeyCode::End => {
            table_state.select(Some(row_count - 1));
            KeyAction::None
        }
        KeyCode::Char('s') => KeyAction::Reload,
        KeyCode::Char('i') => {
            let idx = table_state.selected().unwrap_or(0);
            KeyAction::ShowInfo(idx)
        }
        KeyCode::Char('d') => {
            let idx = table_state.selected().unwrap_or(0);
            KeyAction::RunDoctor(idx)
        }
        KeyCode::Char('f') => {
            let idx = table_state.selected().unwrap_or(0);
            KeyAction::ForceReindex(idx)
        }
        _ => KeyAction::None,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_header(f: &mut ratatui::Frame, area: Rect, serve_url: &str) {
    let now = chrono::Local::now().format("%H:%M:%S").to_string();
    let version = env!("CARGO_PKG_VERSION");

    let title_line = Line::from(vec![
        Span::styled(
            format!(" codesearch serve v{} · ", version),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(serve_url.to_string(), Style::default().fg(Color::White)),
        Span::styled(format!("  {} ", now), Style::default().fg(Color::DarkGray)),
    ]);

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Center the title line vertically (area is 3 rows, title is 1 row)
    let centered = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    f.render_widget(ratatui::widgets::Paragraph::new(title_line), centered[0]);
}

/// Returns true during the "bright" phase of a ~1s pulse cycle (500ms bright, 500ms dim).
/// Used to animate status indicators while indexing is in progress.
fn pulse_bright() -> bool {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        % 1000
        < 500
}

fn render_table(
    f: &mut ratatui::Frame,
    area: Rect,
    repos: &[(String, super::RepoStatusInfo)],
    table_state: &mut TableState,
) {
    let header_cells = [
        "Alias",
        "Status",
        "Changes",
        "Calls",
        "Last Tool Call",
        "Lock",
    ];
    let header = Row::new(
        header_cells
            .iter()
            .map(|h| Cell::from(*h).style(Style::default().add_modifier(Modifier::BOLD))),
    )
    .style(Style::default().fg(Color::White))
    .bottom_margin(1);

    let max_alias_w = repos
        .iter()
        .map(|(a, _)| a.len())
        .max()
        .unwrap_or(10)
        .max(10);

    let rows: Vec<Row> = repos
        .iter()
        .map(|(alias, info)| {
            let status_cell = status_cell(info.status, info.csharp_index);
            // Keep left edge stable: fixed-width, left-aligned value.
            let changes_str = if info.changes > 99999 {
                " 99k+".to_string()
            } else {
                format!("{:>5}", info.changes)
            };
            let changes_cell = Cell::from(changes_str).style(Style::default().fg(Color::White));
            let calls_cell = if info.tool_call_count > 0 {
                Cell::from(format!("{:>5}", info.tool_call_count))
                    .style(Style::default().fg(Color::Cyan))
            } else {
                Cell::from("    -".to_string()).style(Style::default().fg(Color::DarkGray))
            };
            let tool_cell = Cell::from(info.last_tool_call.as_deref().unwrap_or("—").to_string())
                .style(Style::default().fg(Color::DarkGray));
            // We don't have lock info in lightweight status, show status-derived value
            let lock_cell = lock_cell_from_status(info.status);

            // Alias cell — plain name, no C# indicator
            let alias_cell = Cell::from(alias.clone()).style(Style::default().fg(Color::White));

            // Red alias if the repo has errors
            let alias_cell = if matches!(info.status, super::RepoStateLabel::Error) {
                alias_cell.style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
            } else {
                alias_cell
            };

            Row::new(vec![
                alias_cell,
                status_cell,
                changes_cell,
                calls_cell,
                tool_cell,
                lock_cell,
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(max_alias_w as u16 + 2),
            Constraint::Length(14),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Min(24),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::NONE)
            .style(Style::default().bg(Color::Reset)),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(30, 30, 50))
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, table_state);
}

fn render_detail(
    f: &mut ratatui::Frame,
    area: Rect,
    repos: &[(String, super::RepoStatusInfo)],
    table_state: &TableState,
    state: &Arc<ServeState>,
) {
    if repos.is_empty() {
        return;
    }

    let idx = table_state.selected().unwrap_or(0);
    if idx >= repos.len() {
        return;
    }

    let (alias, info) = &repos[idx];
    let config = state.config_snapshot();
    let path = config
        .resolve(alias)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".to_string());

    // Truncate path if too long for the area
    let max_path_len = (area.width as usize).saturating_sub(14);
    let display_path = if path.len() > max_path_len && max_path_len > 3 {
        format!("...{}", &path[path.len() - max_path_len + 3..])
    } else {
        path
    };

    let (status_label, status_color) = match info.status {
        super::RepoStateLabel::Open => match info.csharp_index {
            super::CSharpIndexStatus::Ready => ("Open C#·".to_string(), Color::Green),
            super::CSharpIndexStatus::Indexing => {
                let c = if pulse_bright() {
                    Color::Yellow
                } else {
                    Color::DarkGray
                };
                ("Index C#…".to_string(), c)
            }
            super::CSharpIndexStatus::Error => ("Open C#!".to_string(), Color::Red),
            super::CSharpIndexStatus::None => ("Open".to_string(), Color::Green),
        },
        super::RepoStateLabel::Warm => match info.csharp_index {
            super::CSharpIndexStatus::Ready => ("Warm C#·".to_string(), Color::Yellow),
            super::CSharpIndexStatus::Indexing => {
                let c = if pulse_bright() {
                    Color::Yellow
                } else {
                    Color::DarkGray
                };
                ("Index C#…".to_string(), c)
            }
            super::CSharpIndexStatus::Error => ("Warm C#!".to_string(), Color::Red),
            super::CSharpIndexStatus::None => ("Warm".to_string(), Color::Yellow),
        },
        super::RepoStateLabel::Readonly => ("Readonly".to_string(), Color::Cyan),
        super::RepoStateLabel::Closed => ("Closed".to_string(), Color::Gray),
        super::RepoStateLabel::Indexing => match info.csharp_index {
            super::CSharpIndexStatus::Indexing => {
                let c = if pulse_bright() {
                    Color::Yellow
                } else {
                    Color::DarkGray
                };
                ("Index C#…".to_string(), c)
            }
            _ => {
                let c = if pulse_bright() {
                    Color::Yellow
                } else {
                    Color::DarkGray
                };
                ("Indexing…".to_string(), c)
            }
        },
        super::RepoStateLabel::Error => ("Error".to_string(), Color::Red),
        super::RepoStateLabel::NoIndex => ("No Index".to_string(), Color::Gray),
    };

    let detail_line = Line::from(vec![
        Span::styled(" ▶ ", Style::default().fg(Color::Yellow)),
        Span::styled(
            alias.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(status_label, Style::default().fg(status_color)),
        Span::styled("  ", Style::default()),
        Span::styled(display_path, Style::default().fg(Color::DarkGray)),
    ]);

    // Second line: changes + tool calls + last tool call
    let tool_str = info.last_tool_call.as_deref().unwrap_or("—");
    let info_line = Line::from(vec![
        Span::styled("   changes:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {}  ", info.changes),
            Style::default().fg(Color::White),
        ),
        Span::styled("calls:", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {}  ", info.tool_call_count),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("last:", Style::default().fg(Color::DarkGray)),
        Span::styled(format!(" {}", tool_str), Style::default().fg(Color::White)),
    ]);

    // Error line: shown only when C# index has an error
    let error_line = if matches!(info.csharp_index, super::CSharpIndexStatus::Error) {
        let err_msg = info.csharp_error.as_deref().unwrap_or("Unknown error");
        // Truncate error message to fit the area.
        // Use char-boundary-safe truncation to avoid panics on multi-byte UTF-8
        // (C# helper errors may contain '…', '—', non-ASCII project paths, etc.).
        // The prefix "   ⚠ " occupies 6 visible columns; leave 1 col margin → subtract 7.
        const ERR_PREFIX_COLS: usize = 7;
        let max_err_chars = (area.width as usize).saturating_sub(ERR_PREFIX_COLS);
        let err_chars: Vec<char> = err_msg.chars().collect();
        let display_err = if err_chars.len() > max_err_chars && max_err_chars > 3 {
            let truncated: String = err_chars[..max_err_chars - 3].iter().collect();
            format!("{}...", truncated)
        } else {
            err_msg.to_string()
        };
        Some(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(
                "⚠ ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(display_err, Style::default().fg(Color::Red)),
        ]))
    } else {
        None
    };

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 60)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let constraints = if error_line.is_some() {
        vec![
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else {
        vec![Constraint::Length(1), Constraint::Length(1)]
    };
    let detail_chunks = Layout::vertical(constraints).split(inner);

    f.render_widget(
        ratatui::widgets::Paragraph::new(detail_line),
        detail_chunks[0],
    );
    f.render_widget(
        ratatui::widgets::Paragraph::new(info_line),
        detail_chunks[1],
    );
    if let Some(err_line) = error_line {
        f.render_widget(ratatui::widgets::Paragraph::new(err_line), detail_chunks[2]);
    }
}

fn render_footer(
    f: &mut ratatui::Frame,
    area: Rect,
    repos: &[(String, super::RepoStatusInfo)],
    table_state: &TableState,
    active: u64,
    cpu: &str,
    csharp_helper: bool,
) {
    let selected = table_state.selected().unwrap_or(0);
    let scroll_indicator = if repos.len() > 1 {
        format!("[{}/{}]", selected + 1, repos.len())
    } else {
        String::new()
    };

    let sessions_str = format!("Sessions: {}", active);
    let cpu_str = format!("CPU: {}", cpu);

    // Right side: CPU | Sessions
    let right_len = cpu_str.len() + sessions_str.len() + 3 + "C# │ ".len(); // C# label + " │ "

    let footer_inner = area.inner(Margin {
        vertical: 0,
        horizontal: 1,
    });
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_len as u16 + 2)])
            .areas(footer_inner);

    let left_line = Line::from(vec![
        Span::styled("[q] quit  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[↑↓] scroll  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[i] info  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[d] doctor  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[f] reindex  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[s] reload  ", Style::default().fg(Color::DarkGray)),
        Span::styled(scroll_indicator, Style::default().fg(Color::Yellow)),
    ]);

    let csharp_indicator = if csharp_helper {
        Span::styled("C# │ ", Style::default().fg(Color::Green))
    } else {
        Span::styled("C# │ ", Style::default().fg(Color::DarkGray))
    };

    let right_line = Line::from(vec![
        csharp_indicator,
        Span::styled(cpu_str, Style::default().fg(Color::Green)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(sessions_str, Style::default().fg(Color::Cyan)),
    ]);

    f.render_widget(ratatui::widgets::Paragraph::new(left_line), left);
    f.render_widget(
        ratatui::widgets::Paragraph::new(right_line).right_aligned(),
        right,
    );
}

// ---------------------------------------------------------------------------
// Cell styling helpers
// ---------------------------------------------------------------------------

/// Get current process CPU usage as a human-readable string.
///
/// Uses `sysinfo` crate — fully cross-platform, no platform-specific code.
///
/// **Important:** `sys_system` must be reused across calls so `cpu_usage()` can
/// compute a delta between refresh calls (first call always returns 0%).
fn cpu_usage_str(sys_system: &mut Option<sysinfo::System>) -> String {
    use sysinfo::{ProcessesToUpdate, System};

    let pid = match sysinfo::get_current_pid() {
        Ok(p) => p,
        Err(_) => return "—".into(),
    };

    // Create System instance on first call, reuse on subsequent calls.
    // Refresh CPUs once on creation so sys.cpus().len() is populated.
    let sys = sys_system.get_or_insert_with(|| {
        let mut s = System::new();
        s.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
        s
    });

    // Refresh only our process (cpu)
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);

    match sys.process(pid) {
        Some(proc) => {
            // sysinfo reports cpu_usage() as total across all logical CPUs
            // (e.g. 474% on a 15-core machine). Divide by CPU count to get
            // a 0-100% value that makes sense to humans.
            let num_cpus = sys.cpus().len().max(1) as f32;
            let pct = proc.cpu_usage() / num_cpus;
            format!("{:.0}%", pct)
        }
        None => "—".into(),
    }
}

fn status_cell(status: super::RepoStateLabel, csharp: super::CSharpIndexStatus) -> Cell<'static> {
    use super::CSharpIndexStatus as CS;
    use super::RepoStateLabel::*;
    let bright = pulse_bright();
    match status {
        Open => match csharp {
            CS::Ready => Cell::from("✓ ready C#·  ".to_string()).style(
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            CS::Indexing => {
                if bright {
                    Cell::from("⟳ idx C#…    ".to_string()).style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Cell::from("⟳ idx C#…    ".to_string())
                        .style(Style::default().fg(Color::DarkGray))
                }
            }
            CS::Error => Cell::from("✓ ready C#!  ".to_string())
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            CS::None => Cell::from("✓ ready      ".to_string()).style(
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        },
        Warm => {
            match csharp {
                CS::Ready => Cell::from("◐ warm C#·   ".to_string())
                    .style(Style::default().fg(Color::Yellow)),
                CS::Indexing => {
                    if bright {
                        Cell::from("⟳ idx C#…    ".to_string()).style(
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Cell::from("⟳ idx C#…    ".to_string())
                            .style(Style::default().fg(Color::DarkGray))
                    }
                }
                CS::Error => Cell::from("◐ warm C#!   ".to_string())
                    .style(Style::default().fg(Color::Yellow)),
                CS::None => Cell::from("◐ warm       ".to_string())
                    .style(Style::default().fg(Color::Yellow)),
            }
        }
        Readonly => Cell::from("◑ ro         ".to_string()).style(Style::default().fg(Color::Cyan)),
        Indexing => {
            if bright {
                match csharp {
                    CS::Ready => Cell::from("⟳ idx… C#·   ".to_string()).style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    CS::Indexing => Cell::from("⟳ idx C#…    ".to_string()).style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    CS::Error => Cell::from("⟳ idx… C#!   ".to_string()).style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    CS::None => Cell::from("⟳ indexing…  ".to_string()).style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                }
            } else {
                match csharp {
                    CS::Ready => Cell::from("⟳ idx… C#·   ".to_string())
                        .style(Style::default().fg(Color::DarkGray)),
                    CS::Indexing => Cell::from("⟳ idx C#…    ".to_string())
                        .style(Style::default().fg(Color::DarkGray)),
                    CS::Error => Cell::from("⟳ idx… C#!   ".to_string())
                        .style(Style::default().fg(Color::DarkGray)),
                    CS::None => Cell::from("⟳ indexing…  ".to_string())
                        .style(Style::default().fg(Color::DarkGray)),
                }
            }
        }
        Closed => Cell::from("○ closed     ".to_string()).style(Style::default().fg(Color::Gray)),
        Error => Cell::from("✗ error      ".to_string())
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        NoIndex => Cell::from("— no idx     ".to_string()).style(Style::default().fg(Color::Gray)),
    }
}

/// Derive lock column from RepoStateLabel.
/// TODO: Once RepoStatusInfo gains a real `lock_mode` field, replace this heuristic.
fn lock_cell_from_status(status: super::RepoStateLabel) -> Cell<'static> {
    use super::RepoStateLabel::*;
    match status {
        Open => Cell::from("write".to_string()).style(Style::default().fg(Color::Cyan)),
        Warm => Cell::from("read".to_string()).style(Style::default().fg(Color::White)),
        Readonly => Cell::from("read".to_string()).style(Style::default().fg(Color::White)),
        Indexing => Cell::from("write".to_string()).style(Style::default().fg(Color::Cyan)),
        Closed => Cell::from("—".to_string()).style(Style::default().fg(Color::Gray)),
        Error => Cell::from("—".to_string()).style(Style::default().fg(Color::Red)),
        NoIndex => Cell::from("—".to_string()).style(Style::default().fg(Color::Gray)),
    }
}

// ---------------------------------------------------------------------------
// Overlay rendering
// ---------------------------------------------------------------------------

/// Render the info/doctor overlay as a centered modal on top of the TUI.
fn render_overlay(f: &mut ratatui::Frame, area: Rect, overlay: &OverlayState) {
    // Always draw backdrop first to hide content behind modal
    render_backdrop(f, area);

    match overlay {
        OverlayState::Info {
            alias,
            chunks,
            files,
            max_chunk_id,
            db_size_human,
            model,
            dims,
            lock,
            index_age,
        } => {
            let title = format!(" {} — Index Info ", alias);
            let lines = vec![
                Line::from(vec![
                    Span::styled("  Chunks:      ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}", chunks), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("  Files:       ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}", files), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("  Max chunk ID:", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{}", max_chunk_id),
                        Style::default().fg(Color::White),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  DB size:     ", Style::default().fg(Color::DarkGray)),
                    Span::styled(db_size_human.clone(), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("  Model:       ", Style::default().fg(Color::DarkGray)),
                    Span::styled(model.clone(), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled("  Dimensions:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}", dims), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("  Lock:        ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        lock.clone(),
                        Style::default().fg(if lock == "write" {
                            Color::Cyan
                        } else {
                            Color::White
                        }),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  Index age:   ", Style::default().fg(Color::DarkGray)),
                    Span::styled(index_age.clone(), Style::default().fg(Color::White)),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "  [Esc] close",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            render_centered_modal(f, area, &title, lines);
        }
        OverlayState::Doctor { alias, results } => {
            let title = format!(" {} — Doctor ", alias);
            let mut lines: Vec<Line> = results
                .iter()
                .map(|r| {
                    let color = if r.starts_with("✓") {
                        Color::Green
                    } else if r.starts_with("⚠") {
                        Color::Yellow
                    } else if r.starts_with("✗") {
                        Color::Red
                    } else {
                        Color::White
                    };
                    Line::from(Span::styled(format!("  {}", r), Style::default().fg(color)))
                })
                .collect();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  [Esc] close",
                Style::default().fg(Color::DarkGray),
            )));
            render_centered_modal(f, area, &title, lines);
        }
        OverlayState::DoctorRunning { alias } => {
            let title = format!(" {} — Doctor ", alias);
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  ⏳ Running diagnostics...",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  [Esc] cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            render_centered_modal(f, area, &title, lines);
        }
    }
}

/// Render a semi-transparent dark backdrop over the entire screen.
/// This hides the table/content behind modal overlays.
fn render_backdrop(f: &mut ratatui::Frame, area: Rect) {
    f.render_widget(
        ratatui::widgets::Block::default().style(
            Style::default()
                .bg(Color::Rgb(10, 10, 20))
                .fg(Color::DarkGray),
        ),
        area,
    );
}

/// Render a centered modal with a title and content lines.
fn render_centered_modal(f: &mut ratatui::Frame, area: Rect, title: &str, lines: Vec<Line<'_>>) {
    let content_height = lines.len() as u16 + 2; // +2 for border
                                                 // Compute width from actual content lines to avoid truncation
    let max_line_w = lines.iter().map(|l| l.width() as u16).max().unwrap_or(20);
    let title_w = title.len() as u16;
    let content_width = (max_line_w + 4).max(title_w + 4).max(30);

    // Center the modal
    let modal_area = Rect {
        x: area
            .width
            .saturating_sub(content_width)
            .saturating_div(2)
            .min(area.width.saturating_sub(content_width)),
        y: area
            .height
            .saturating_sub(content_height)
            .saturating_div(2)
            .min(area.height.saturating_sub(content_height)),
        width: content_width.min(area.width),
        height: content_height.min(area.height),
    };

    // Clear the area behind the modal
    f.render_widget(
        ratatui::widgets::Block::default().style(Style::default().bg(Color::Rgb(20, 20, 35))),
        modal_area,
    );

    let block = ratatui::widgets::Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(20, 20, 35)));
    let inner = block.inner(modal_area);
    f.render_widget(block, modal_area);

    let content = ratatui::widgets::Paragraph::new(lines);
    f.render_widget(content, inner);
}

// ---------------------------------------------------------------------------
// Info overlay builder
// ---------------------------------------------------------------------------

/// Build an `OverlayState::Info` by gathering live stats from SharedStores or metadata.
fn build_info_overlay(
    idx: usize,
    repos: &[(String, super::RepoStatusInfo)],
    state: &Arc<ServeState>,
) -> Option<OverlayState> {
    if idx >= repos.len() {
        return None;
    }
    let (alias, _info) = &repos[idx];
    let config = state.config_snapshot();
    let project_path = config.resolve(alias)?;
    let db_path = project_path.join(DB_DIR_NAME);

    // Try to get live stats from opened stores
    let mut chunks = 0usize;
    let mut files = 0usize;
    let mut max_chunk_id = 0u32;
    let mut dims = 0usize;
    let mut model = String::from("unknown");
    let mut lock = String::from("—");
    let mut index_age = String::from("—");

    // Read model + dims from metadata.json
    if let Ok(content) = std::fs::read_to_string(db_path.join("metadata.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            model = json
                .get("model_short_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            dims = json.get("dimensions").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            // Total chunks from metadata (may be 0 if clobbered — Stage 2 fix)
            chunks = json
                .get("total_chunks")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            files = json
                .get("total_files")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;

            // Index age from indexed_at
            if let Some(indexed_at) = json.get("indexed_at").and_then(|v| v.as_str()) {
                index_age = format_age(indexed_at);
            }
        }
    }

    // If stores are open, get live stats (overrides metadata)
    if let Some(stores) = state.get_opened_stores(alias) {
        if let Ok(vs) = stores.vector_store.try_read() {
            if let Ok(live_stats) = vs.stats() {
                chunks = live_stats.total_chunks;
                files = live_stats.total_files;
                max_chunk_id = live_stats.max_chunk_id;
                if dims == 0 {
                    dims = live_stats.dimensions;
                }
            }
        }
        lock = if stores.readonly {
            "read".to_string()
        } else {
            "write".to_string()
        };
    }

    // DB size on disk
    let db_size_human = dir_size_human(&db_path);

    Some(OverlayState::Info {
        alias: alias.clone(),
        chunks,
        files,
        max_chunk_id,
        db_size_human,
        model,
        dims,
        lock,
        index_age,
    })
}

/// Format an ISO 8601 timestamp as a human-readable age string.
fn format_age(iso_ts: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc3339(iso_ts).or_else(|_| {
        // Try without timezone (assume UTC)
        chrono::NaiveDateTime::parse_from_str(iso_ts, "%Y-%m-%dT%H:%M:%S%.f")
            .map(|dt| dt.and_utc().fixed_offset())
    });

    match parsed {
        Ok(dt) => {
            let now = chrono::Utc::now();
            let dur = now.signed_duration_since(dt);
            if dur.num_seconds() < 0 {
                return "just now".to_string();
            }
            let mins = dur.num_minutes();
            if mins < 1 {
                "just now".to_string()
            } else if mins < 60 {
                format!("{}m ago", mins)
            } else {
                let hours = mins / 60;
                if hours < 24 {
                    format!("{}h ago", hours)
                } else {
                    let days = hours / 24;
                    format!("{}d ago", days)
                }
            }
        }
        Err(_) => iso_ts.to_string(),
    }
}

/// Compute total size of a directory on disk, formatted as human-readable string.
fn dir_size_human(path: &std::path::Path) -> String {
    let total_bytes = walkdir_size(path);
    if total_bytes == 0 {
        return "—".to_string();
    }
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if total_bytes >= GB {
        format!("{:.1} GB", total_bytes as f64 / GB as f64)
    } else if total_bytes >= MB {
        format!("{:.1} MB", total_bytes as f64 / MB as f64)
    } else {
        format!("{:.0} KB", total_bytes as f64 / KB as f64)
    }
}

/// Walk a directory and sum file sizes. Returns 0 on any error.
fn walkdir_size(path: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_file() {
                    if let Ok(meta) = entry.metadata() {
                        total += meta.len();
                    }
                } else if file_type.is_dir() {
                    total += walkdir_size(&entry.path());
                }
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Doctor (non-blocking spawn)
// ---------------------------------------------------------------------------

/// Spawn a background task to run doctor diagnostics for the given repo alias.
/// Sends the result overlay back via `tx` when done.
fn spawn_doctor(
    alias: String,
    state: Arc<ServeState>,
    tx: tokio::sync::mpsc::Sender<OverlayState>,
) {
    let resolved = state.config.read().ok().and_then(|c| c.resolve(&alias));
    let project_path = match resolved {
        Some(p) => p,
        None => {
            let _ = tx.try_send(OverlayState::Doctor {
                alias,
                results: vec![
                    "✗ Cannot resolve alias to path".to_string(),
                    String::new(),
                    "  [Esc] close".to_string(),
                ],
            });
            return;
        }
    };

    tokio::spawn(async move {
        let result_overlay = async {
            let stores = state.get_or_open_stores(&alias, true).await;
            match stores {
                Ok(s) => {
                    let vs = s.vector_store.read().await;
                    let report = doctor::diagnose_with_store(&project_path, &vs);
                    drop(vs);
                    match report {
                        Ok(r) => OverlayState::Doctor {
                            alias,
                            results: r.render_tui(),
                        },
                        Err(e) => OverlayState::Doctor {
                            alias,
                            results: vec![
                                format!("✗ Doctor failed: {}", e),
                                String::new(),
                                "  [Esc] close".to_string(),
                            ],
                        },
                    }
                }
                Err(e) => OverlayState::Doctor {
                    alias,
                    results: vec![
                        format!("✗ Cannot open database: {}", e),
                        String::new(),
                        "  [Esc] close".to_string(),
                    ],
                },
            }
        }
        .await;

        // Send result back to TUI loop (non-blocking — if channel closed, just drop it)
        let _ = tx.send(result_overlay).await;
    });
}

// ---------------------------------------------------------------------------
// Force reindex (non-blocking spawn)
// ---------------------------------------------------------------------------

/// Spawn a background force reindex task for the given repo alias.
/// Follows the same flow as the HTTP `reindex_handler`.
fn spawn_force_reindex(alias: String, state: &Arc<ServeState>) {
    // Guard against concurrent reindex
    if !state.active_reindexes.insert(alias.clone()) {
        tracing::warn!(
            "Force reindex already in progress for '{}', skipping TUI request",
            alias
        );
        return;
    }

    let config = match state.config.read() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Config lock poisoned: {}", e);
            state.active_reindexes.remove(&alias);
            return;
        }
    };
    let project_path = match config.resolve(&alias) {
        Some(p) => p,
        None => {
            tracing::error!("Cannot resolve alias '{}' for force reindex", alias);
            state.active_reindexes.remove(&alias);
            return;
        }
    };
    drop(config); // release read lock

    let db_path = project_path.join(DB_DIR_NAME);

    // Stop FSW
    let stores = match state.stop_fsw(&alias) {
        Some(s) => s,
        None => {
            // Try to open stores (allow_create=true for recovery)
            let cancel = CancellationToken::new();
            match state.try_open_stores(&alias, &db_path, true) {
                Ok(super::OpenedStores::Write(s)) => {
                    state.repos.insert(
                        alias.clone(),
                        super::RepoState::Write {
                            stores: s.clone(),
                            index_manager: None,
                            cancel_token: cancel,
                        },
                    );
                    state.touch_access(&alias);
                    s
                }
                Ok(super::OpenedStores::Readonly(_)) => {
                    tracing::error!(
                        "Repo {} opened read-only; cannot force-reindex from TUI",
                        alias
                    );
                    state.active_reindexes.remove(&alias);
                    return;
                }
                Err(e) => {
                    tracing::error!("Cannot open stores for '{}': {}", alias, e);
                    state.active_reindexes.remove(&alias);
                    return;
                }
            }
        }
    };

    let alias_bg = alias.clone();
    let state_bg = state.clone();
    tokio::spawn(async move {
        tracing::info!(
            "TUI: Force reindex for '{}': clearing stores and reindexing",
            alias_bg
        );

        match IndexManager::force_reindex_with_stores(&project_path, &db_path, &stores).await {
            Ok(()) => {
                tracing::info!("TUI: Force reindex complete for '{}'", alias_bg);
            }
            Err(e) => {
                tracing::error!("TUI: Force reindex failed for '{}': {}", alias_bg, e);
            }
        }

        // Restart FSW with fresh IndexManager
        state_bg.restart_fsw(&alias_bg, stores).await;

        // Remove guard
        state_bg.active_reindexes.remove(&alias_bg);
    });
}

// ---------------------------------------------------------------------------
// TTY detection
// ---------------------------------------------------------------------------

/// Check if stdout is connected to a real terminal (TTY).
/// Returns `false` when piped, redirected, or running as a service.
pub fn is_tty() -> bool {
    // crossterm::terminal::size() returns Err when stdout is not a real terminal.
    // This covers piped, redirected, and service scenarios.
    crossterm::terminal::size().is_ok()
}

/// Attempt to start the TUI. Returns None if no TTY is available.
/// Logs a one-line message to stderr in non-TTY mode.
pub fn maybe_spawn_tui(
    state: Arc<ServeState>,
    cancel_token: CancellationToken,
    serve_url: String,
) -> Option<tokio::task::JoinHandle<()>> {
    if !is_tty() {
        return None;
    }
    Some(tokio::spawn(async move {
        if let Err(e) = run_tui(state, cancel_token, serve_url).await {
            tracing::error!("TUI error: {}", e);
        }
    }))
}
