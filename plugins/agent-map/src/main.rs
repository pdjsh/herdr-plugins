//! A radial overview of every agent herdr is running, grouped by repository.
//!
//! This is a *viewer*. It deliberately has no jump-to-agent binding yet: the
//! interaction model was left open, and guessing one would bake in muscle memory
//! that may need unlearning. `q` / `Esc` closes it, `r` forces a refresh.
//!
//! Run with `--once` to print a single frame to stdout and exit, which is how the
//! layout is eyeballed without a TTY.

mod layout;

use std::io::{IsTerminal, Write, stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, cursor};
use herdr_api::{Client, Status};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// herdr pushes no "agent changed" signal a plugin can select() on, so the map
/// polls. 2s is fast enough to feel live without hammering the socket.
const REFRESH: Duration = Duration::from_secs(2);

fn color(s: Status) -> Color {
    // Matches the semantic colours in the user's herdr theme so the map and the
    // sidebar agree on what "blocked" looks like.
    match s {
        Status::Blocked => Color::Rgb(247, 118, 142),
        Status::Done => Color::Rgb(255, 158, 100),
        Status::Working => Color::Rgb(224, 175, 104),
        Status::Idle => Color::Rgb(66, 190, 101),
        Status::Unknown => Color::Rgb(124, 124, 124),
    }
}

fn to_rect(r: layout::Rect) -> Rect {
    Rect::new(r.x, r.y, r.w, r.h)
}

/// Dot-trail from a group to one of its agents, drawn under the boxes.
fn connector(buf: &mut Buffer, from: (u16, u16), to: (u16, u16), area: Rect) {
    let (x0, y0) = (from.0 as i32, from.1 as i32);
    let (x1, y1) = (to.0 as i32, to.1 as i32);
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    for i in 1..steps {
        let x = x0 + (x1 - x0) * i / steps;
        let y = y0 + (y1 - y0) * i / steps;
        if x < area.x as i32
            || y < area.y as i32
            || x >= (area.x + area.width) as i32
            || y >= (area.y + area.height) as i32
        {
            continue;
        }
        let cell = &mut buf[(x as u16, y as u16)];
        // Never paint over a box that is already there.
        if cell.symbol() == " " {
            cell.set_symbol("·")
                .set_style(Style::default().fg(Color::Rgb(70, 70, 70)));
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".into();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn draw(f: &mut Frame, groups: &[layout::Group], last: Instant) {
    let full = f.area();
    if full.height < 6 || full.width < 30 {
        f.render_widget(
            Paragraph::new("pane too small for the map").fg(Color::Rgb(150, 150, 150)),
            full,
        );
        return;
    }

    // Reserve the last row for the legend.
    let canvas = Rect::new(full.x, full.y, full.width, full.height - 1);
    let footer = Rect::new(full.x, full.y + full.height - 1, full.width, 1);

    let total: usize = groups.iter().map(|g| g.agents.len()).sum();
    if total == 0 {
        f.render_widget(
            Paragraph::new(
                "\n  No agents running.\n\n  herdr is up, but nothing is attached to a pane yet.",
            )
            .fg(Color::Rgb(150, 150, 150)),
            canvas,
        );
        return;
    }

    let map = layout::build(
        groups,
        layout::Rect {
            x: canvas.x,
            y: canvas.y,
            w: canvas.width,
            h: canvas.height,
        },
    );

    // Connectors first so the boxes paint over them.
    for a in &map.agents {
        let g = &map.groups[a.group];
        connector(f.buffer_mut(), g.rect.center(), a.rect.center(), canvas);
    }

    for g in &map.groups {
        let c = color(g.status);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(c));
        let label = truncate(&g.label, g.rect.w.saturating_sub(4) as usize);
        // Wipe the connector dots out from under the box before drawing it,
        // otherwise they show through the block's padding.
        f.render_widget(Clear, to_rect(g.rect));
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label, Style::default().fg(Color::Rgb(224, 224, 224)).bold()),
                Span::styled(
                    format!(" {}", g.agent_count),
                    Style::default().fg(Color::Rgb(124, 124, 124)),
                ),
            ]))
            .centered()
            .block(block),
            to_rect(g.rect),
        );
    }

    for a in &map.agents {
        let c = color(a.status);
        // Stale agents fade, echoing ClawTab's opacity ramp. A floor keeps the
        // dimmest node readable rather than invisible.
        let text_shade = 150 + (a.recency * 90.0) as u8;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(if a.focused {
                BorderType::Thick
            } else {
                BorderType::Rounded
            })
            .border_style(Style::default().fg(if a.focused {
                Color::Rgb(224, 224, 224)
            } else {
                c
            }));
        let inner_w = a.rect.w.saturating_sub(2) as usize;
        let lines = vec![
            Line::from(vec![
                Span::styled(format!("{} ", a.status.glyph()), Style::default().fg(c)),
                Span::styled(
                    truncate(&a.workspace, inner_w.saturating_sub(2)),
                    Style::default().fg(Color::Rgb(196, 196, 196)),
                ),
            ]),
            Line::from(Span::styled(
                truncate(&a.title, inner_w),
                Style::default().fg(Color::Rgb(text_shade, text_shade, text_shade)),
            )),
        ];
        f.render_widget(Clear, to_rect(a.rect));
        f.render_widget(Paragraph::new(lines).block(block), to_rect(a.rect));
    }

    let counts = |s: Status| map.agents.iter().filter(|a| a.status == s).count();
    let age = last.elapsed().as_secs();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} agents ", total),
                Style::default().fg(Color::Rgb(224, 224, 224)),
            ),
            Span::styled(
                format!("⚠ {} ", counts(Status::Blocked)),
                Style::default().fg(color(Status::Blocked)),
            ),
            Span::styled(
                format!("● {} ", counts(Status::Done)),
                Style::default().fg(color(Status::Done)),
            ),
            Span::styled(
                format!("✳ {} ", counts(Status::Working)),
                Style::default().fg(color(Status::Working)),
            ),
            Span::styled(
                format!("✓ {} ", counts(Status::Idle)),
                Style::default().fg(color(Status::Idle)),
            ),
            Span::styled(
                format!("· updated {age}s ago — q close, r refresh"),
                Style::default().fg(Color::Rgb(110, 110, 110)),
            ),
        ])),
        footer,
    );
}

/// Plain-text fallback for `--once`, and for when stdout is not a terminal.
fn print_once(groups: &[layout::Group]) {
    let total: usize = groups.iter().map(|g| g.agents.len()).sum();
    println!("{total} agent(s) across {} group(s)\n", groups.len());
    for g in groups {
        println!("{} ({})", g.label, g.agents.len());
        for a in &g.agents {
            let st = Status::parse(&a.agent_status);
            let title = a
                .terminal_title_stripped
                .as_deref()
                .map(herdr_api::clean_title)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| a.agent.clone());
            println!(
                "  {} {:<7} {:<10} {}",
                st.glyph(),
                st.label(),
                a.workspace_id,
                title
            );
        }
        println!();
    }
}

fn fetch(client: &Client) -> Result<Vec<layout::Group>> {
    let workspaces = client.workspaces()?;
    let agents = client.agents()?;
    Ok(layout::group_agents(&agents, &workspaces))
}

fn main() -> Result<()> {
    let once = std::env::args().any(|a| a == "--once");
    let client = Client::from_env()?;

    if once || !stdout().is_terminal() {
        print_once(&fetch(&client)?);
        return Ok(());
    }

    enable_raw_mode()?;
    stdout()
        .execute(EnterAlternateScreen)?
        .execute(cursor::Hide)?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;

    let mut groups = fetch(&client)?;
    let mut last = Instant::now();

    let result = (|| -> Result<()> {
        loop {
            term.draw(|f| draw(f, &groups, last))?;

            // Waking on the shorter of "time to refresh" and "a key is pending"
            // keeps q responsive without spinning.
            let wait = REFRESH
                .checked_sub(last.elapsed())
                .unwrap_or(Duration::ZERO)
                .min(Duration::from_millis(250));
            if event::poll(wait)?
                && let Event::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
            {
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('r') => {
                        groups = fetch(&client)?;
                        last = Instant::now();
                    }
                    _ => {}
                }
            }

            if last.elapsed() >= REFRESH {
                groups = fetch(&client)?;
                last = Instant::now();
            }
        }
        Ok(())
    })();

    // Restore the terminal even if the loop bailed, or the pane is left unusable.
    disable_raw_mode()?;
    stdout()
        .execute(LeaveAlternateScreen)?
        .execute(cursor::Show)?;
    stdout().flush()?;
    result
}
