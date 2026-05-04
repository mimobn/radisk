//! Interactive partition-style mount-point picker.
//!
//! Inspired by the "select a disk" first screen of EaseUS Partition
//! Master, MiniTool Partition Wizard, NIUBI Partition Editor, and
//! AOMEI Partition Assistant. Runs *before* the App is constructed,
//! reuses the same crossterm/ratatui terminal we already set up, and
//! returns the user's chosen mount point so the rest of `main` can
//! treat it like the positional `path` argument.
//!
//! Visually it's a list of `[████████░░] 80%   /  used / total
//! ext4  /dev/sda1` rows sorted fullest-first — the partition tools
//! all front-load the disk that needs attention.

use crate::mounts::{discover, MountInfo};
use crate::tree::format_size;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};
use std::path::PathBuf;
use std::time::Duration;

/// Outcome of running the picker.
pub enum PickerOutcome {
    /// User pressed Enter on a mount.
    Picked(PathBuf),
    /// User pressed q / Esc / Ctrl-C.
    Cancelled,
}

/// Run the picker on the supplied terminal. Blocks until the user
/// either picks a mount or cancels.
pub fn run<B: Backend>(terminal: &mut Terminal<B>) -> Result<PickerOutcome>
where
    <B as Backend>::Error: std::error::Error + Send + Sync + 'static,
{
    let mounts = discover();
    let mut state = ListState::default();
    if !mounts.is_empty() {
        state.select(Some(0));
    }

    loop {
        terminal
            .draw(|f| draw(f, &mounts, &mut state))
            .context("failed to draw picker frame")?;

        if !event::poll(Duration::from_millis(150)).context("failed to poll for picker events")? {
            continue;
        }
        let Event::Key(key) = event::read().context("failed to read picker event")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(PickerOutcome::Cancelled),
            KeyCode::Up | KeyCode::Char('k') => move_selection(&mut state, mounts.len(), -1),
            KeyCode::Down | KeyCode::Char('j') => move_selection(&mut state, mounts.len(), 1),
            KeyCode::Home | KeyCode::Char('g') if !mounts.is_empty() => {
                state.select(Some(0));
            }
            KeyCode::End | KeyCode::Char('G') if !mounts.is_empty() => {
                state.select(Some(mounts.len() - 1));
            }
            KeyCode::Enter => {
                if let Some(i) = state.selected() {
                    if let Some(m) = mounts.get(i) {
                        return Ok(PickerOutcome::Picked(m.mount_point.clone()));
                    }
                }
            }
            _ => {}
        }
    }
}

fn move_selection(state: &mut ListState, len: usize, delta: i32) {
    if len == 0 {
        return;
    }
    let cur = state.selected().unwrap_or(0) as i32;
    let next = (cur + delta).rem_euclid(len as i32);
    state.select(Some(next as usize));
}

fn draw(f: &mut Frame, mounts: &[MountInfo], state: &mut ListState) {
    let area = f.area();

    // Layout: title bar, list, footer hint.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    render_title(f, chunks[0]);

    if mounts.is_empty() {
        let placeholder = Paragraph::new(
            "No mounted filesystems discovered.\n\n\
             radisk currently parses /proc/mounts on Linux only — \
             on other platforms the picker is empty.\n\n\
             Press q to exit.",
        )
        .block(Block::default().borders(Borders::ALL).title(" Mounts "))
        .style(Style::default().fg(Color::White));
        f.render_widget(placeholder, chunks[1]);
    } else {
        let items: Vec<ListItem> = mounts.iter().map(render_row).collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Mounts "))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, chunks[1], state);
    }

    render_footer(f, chunks[2]);
}

fn render_title(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "radisk",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  —  pick a mount to scan"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, area);
}

fn render_footer(f: &mut Frame, area: Rect) {
    let hint =
        Paragraph::new("[j/k or ↑/↓] move    [Enter] scan    [g/G] first/last    [q/Esc] quit")
            .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, area);
}

/// Width of the [###...] used-bar in cells. Matches the tree-view
/// bar width so the visual rhythm is consistent across screens.
const BAR_WIDTH: usize = 20;

fn render_row(m: &MountInfo) -> ListItem<'_> {
    let frac = m.used_fraction();
    let filled = ((frac * BAR_WIDTH as f32).round() as usize).min(BAR_WIDTH);
    let mut bar = String::with_capacity(BAR_WIDTH);
    for _ in 0..filled {
        bar.push('█');
    }
    for _ in filled..BAR_WIDTH {
        bar.push('░');
    }

    let percent = (frac * 100.0).round() as u32;
    let bar_color = match percent {
        0..=70 => Color::Green,
        71..=89 => Color::Yellow,
        _ => Color::Red,
    };

    let used_label = format_size(m.used);
    let total_label = format_size(m.total);
    let mount_label = m.mount_point.display().to_string();

    let line = Line::from(vec![
        Span::styled(format!("[{}] ", bar), Style::default().fg(bar_color)),
        Span::styled(
            format!("{:>3}%  ", percent),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!("{:<20} ", mount_label),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>10} / {:<10}  ", used_label, total_label),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!("{:<8} ", m.fstype),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(m.device.clone(), Style::default().fg(Color::DarkGray)),
    ]);
    ListItem::new(line)
}
