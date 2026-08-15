//! Reorder herdr workspaces from the keyboard.
//!
//! Sidebar order is the order of the `workspaces` array in `session.json`, i.e.
//! workspace creation order, and under `agent_panel_sort = "spaces"` that order
//! is what `alt+1..9` and `alt+j/k` aim at. herdr 0.8.0 can change it —
//! `workspace.move { workspace_id, insert_index }` — but exposes no keybinding
//! and no CLI subcommand for it, so dragging rows with the mouse was the only
//! way. This closes that gap.
//!
//! `j`/`k` move the cursor, `J`/`K` move the selected workspace, `Enter` focuses
//! it and exits, `q` exits.

use std::io::{IsTerminal, Write, stdout};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, cursor};
use herdr_api::{Client, Status, Workspace};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

/// Destination index when shifting the item at `from` by `delta`, or `None` when
/// that would run off either end.
///
/// Split out from the UI so the index arithmetic — the part that silently
/// corrupts an ordering when it is wrong — is unit-testable without a server.
fn shift_target(from: usize, delta: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let to = from as isize + delta;
    if to < 0 || to >= len as isize {
        return None;
    }
    Some(to as usize)
}

fn color(s: Status) -> Color {
    match s {
        Status::Blocked => Color::Rgb(247, 118, 142),
        Status::Done => Color::Rgb(255, 158, 100),
        Status::Working => Color::Rgb(224, 175, 104),
        Status::Idle => Color::Rgb(66, 190, 101),
        Status::Unknown => Color::Rgb(124, 124, 124),
    }
}

fn draw(f: &mut Frame, rows: &[Workspace], cursor: usize, err: Option<&str>) {
    let area = f.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" workspace order ")
        .border_style(Style::default().fg(Color::Rgb(90, 90, 90)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if rows.is_empty() {
        f.render_widget(
            Paragraph::new("no workspaces").fg(Color::Rgb(150, 150, 150)),
            inner,
        );
        return;
    }

    let list = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let footer = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let st = Status::parse(&w.agent_status);
            let selected = i == cursor;
            let base = if selected {
                Style::default()
                    .fg(Color::Rgb(224, 224, 224))
                    .bg(Color::Rgb(58, 58, 58))
            } else {
                Style::default().fg(Color::Rgb(208, 208, 208))
            };
            Line::from(vec![
                Span::styled(if selected { " ▸ " } else { "   " }, base),
                Span::styled(format!("{} ", st.glyph()), base.fg(color(st))),
                Span::styled(format!("{:<28}", w.label), base),
                Span::styled(
                    format!(" {:<4} {} tab(s)", w.workspace_id, w.tab_count),
                    base.fg(Color::Rgb(124, 124, 124)),
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list);

    let footer_text = match err {
        Some(e) => Line::from(Span::styled(
            format!(" {e}"),
            Style::default().fg(Color::Rgb(247, 118, 142)),
        )),
        None => Line::from(Span::styled(
            " j/k move cursor · J/K move workspace · ⏎ focus · q quit",
            Style::default().fg(Color::Rgb(110, 110, 110)),
        )),
    };
    f.render_widget(Paragraph::new(footer_text), footer);
}

fn main() -> Result<()> {
    let client = Client::from_env()?;
    let mut rows = client.workspaces()?;

    if !stdout().is_terminal() {
        for (i, w) in rows.iter().enumerate() {
            println!("{i:>2}  {:<6} {}", w.workspace_id, w.label);
        }
        return Ok(());
    }

    enable_raw_mode()?;
    stdout()
        .execute(EnterAlternateScreen)?
        .execute(cursor::Hide)?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut cur = rows.iter().position(|w| w.focused).unwrap_or(0);
    let mut err: Option<String> = None;

    let result = (|| -> Result<()> {
        loop {
            term.draw(|f| draw(f, &rows, cur, err.as_deref()))?;
            let Event::Key(k) = event::read()? else {
                continue;
            };
            if k.kind != KeyEventKind::Press {
                continue;
            }
            let shift = k.modifiers.contains(KeyModifiers::SHIFT);
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('j') | KeyCode::Down if !shift => {
                    cur = (cur + 1).min(rows.len().saturating_sub(1));
                }
                KeyCode::Char('k') | KeyCode::Up if !shift => {
                    cur = cur.saturating_sub(1);
                }
                KeyCode::Char('J') | KeyCode::Char('K') => {
                    let delta = if k.code == KeyCode::Char('J') { 1 } else { -1 };
                    if let Some(to) = shift_target(cur, delta, rows.len()) {
                        match client.move_workspace(&rows[cur].workspace_id, to) {
                            Ok(()) => {
                                // Re-read rather than mutating locally: the server
                                // is the authority on the resulting order.
                                rows = client.workspaces()?;
                                cur = to;
                                err = None;
                            }
                            Err(e) => err = Some(e.to_string()),
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(w) = rows.get(cur) {
                        client.focus_workspace(&w.workspace_id)?;
                    }
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    stdout()
        .execute(LeaveAlternateScreen)?
        .execute(cursor::Show)?;
    stdout().flush()?;
    result
}

#[cfg(test)]
mod tests {
    use super::shift_target;

    #[test]
    fn moves_within_bounds() {
        assert_eq!(shift_target(0, 1, 4), Some(1));
        assert_eq!(shift_target(3, -1, 4), Some(2));
        assert_eq!(shift_target(2, 1, 4), Some(3));
    }

    #[test]
    fn refuses_to_run_off_either_end() {
        assert_eq!(shift_target(0, -1, 4), None, "already at the top");
        assert_eq!(shift_target(3, 1, 4), None, "already at the bottom");
    }

    #[test]
    fn handles_degenerate_lists() {
        assert_eq!(shift_target(0, 1, 0), None, "empty list");
        assert_eq!(shift_target(0, 1, 1), None, "single item cannot move");
        assert_eq!(shift_target(0, -1, 1), None);
    }
}
