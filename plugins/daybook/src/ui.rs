//! Rendering. One screen, three panels, no navigation beyond a cursor.
//!
//! The layout answers two questions in a fixed order: *what is stuck* on the
//! left, taking the majority of the width because it is the only part that
//! asks anything of you, and *what moved* on the right. Hints for the selected
//! row are expanded in place rather than for every row — a list where every
//! entry is two lines tall stops being scannable at about six entries.

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

use crate::App;
use crate::model::{Doc, Severity, truncate};

// Palette lifted from agent-map, which took it from the user's herdr theme, so
// a blocked agent is the same red in the sidebar, the map, and here.
const RED: Color = Color::Rgb(247, 118, 142);
const ORANGE: Color = Color::Rgb(255, 158, 100);
const YELLOW: Color = Color::Rgb(224, 175, 104);
const GREEN: Color = Color::Rgb(66, 190, 101);
const BRIGHT: Color = Color::Rgb(224, 224, 224);
const TEXT: Color = Color::Rgb(196, 196, 196);
const DIM: Color = Color::Rgb(124, 124, 124);
const FAINT: Color = Color::Rgb(96, 96, 96);

fn severity_color(s: Severity) -> Color {
    match s {
        Severity::Blocked => RED,
        Severity::Waiting => ORANGE,
        Severity::Loose => YELLOW,
        Severity::Tidy => DIM,
    }
}

fn status_color(status: &str) -> Color {
    match herdr_api::Status::parse(status) {
        herdr_api::Status::Blocked => RED,
        herdr_api::Status::Done => ORANGE,
        herdr_api::Status::Working => YELLOW,
        herdr_api::Status::Idle => GREEN,
        herdr_api::Status::Unknown => DIM,
    }
}

fn panel(title: &str, count: Option<usize>) -> Block<'static> {
    let label = match count {
        Some(n) => format!(" {title} {n} "),
        None => format!(" {title} "),
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Rgb(58, 58, 66)))
        .title(Span::styled(label, Style::default().fg(DIM)))
}

/// Rows the loops panel can show, given its height and whether the cursor's
/// hint is expanded.
///
/// Split out so the scroll arithmetic can be tested without a terminal: an
/// off-by-one here is the difference between the last item being reachable and
/// being permanently invisible.
pub fn scroll_offset(total: usize, height: usize, cursor: usize, current: usize) -> usize {
    if total == 0 || height == 0 {
        return 0;
    }
    // The selected row costs two lines once its hint is expanded, so the window
    // is one shorter than the panel is tall.
    let usable = height.saturating_sub(1).max(1);
    let max_offset = total.saturating_sub(usable);
    let offset = if cursor < current {
        cursor
    } else if cursor >= current + usable {
        cursor + 1 - usable
    } else {
        current
    };
    offset.min(max_offset)
}

fn draw_loops(f: &mut Frame, area: Rect, app: &App) {
    let doc = app.doc();
    let items = doc.map(|d| d.loops(app.max_severity)).unwrap_or_default();
    let block = panel("open loops", Some(items.len()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if items.is_empty() {
        f.render_widget(
            Paragraph::new(if doc.is_some() {
                "Nothing waiting on you.\n\nEverything the collector can see is\ncommitted, pushed, answered, or green."
            } else {
                "collecting…"
            })
            .fg(DIM)
            .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    let end = (app.offset + inner.height as usize).min(items.len());
    for (i, item) in items.iter().enumerate().take(end).skip(app.offset) {
        // An expanded hint costs a row, so the budget is re-checked per item
        // rather than assumed from the window size.
        if lines.len() >= inner.height as usize {
            break;
        }
        let sev = Severity::parse(item.severity);
        let selected = i == app.cursor;
        let color = severity_color(sev);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", sev.mark()),
                Style::default().fg(color).bold(),
            ),
            Span::styled(
                truncate(&item.text, width.saturating_sub(3)),
                if selected {
                    Style::default().fg(BRIGHT).bold()
                } else {
                    Style::default().fg(TEXT)
                },
            ),
        ]));
        // Only the cursor's hint. Expanding all of them doubles the list and
        // buries the shape of it.
        if selected && !item.hint.is_empty() && lines.len() < inner.height as usize {
            lines.push(Line::from(Span::styled(
                format!("     → {}", truncate(&item.hint, width.saturating_sub(7))),
                Style::default().fg(FAINT),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_repos(f: &mut Frame, area: Rect, app: &App) {
    let repos = app.doc().map(Doc::active_repos).unwrap_or_default();
    let block = panel("moved", Some(repos.len()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if repos.is_empty() {
        f.render_widget(Paragraph::new("no commits, no dirt").fg(DIM), inner);
        return;
    }

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for repo in repos {
        if lines.len() >= inner.height as usize {
            break;
        }
        // Counts are the point of this panel, so they get the width they need
        // and the name absorbs the remainder.
        let mut counts: Vec<Span> = Vec::new();
        if !repo.commits.is_empty() {
            counts.push(Span::styled(
                format!(" {}●", repo.commits.len()),
                Style::default().fg(GREEN),
            ));
        }
        if repo.dirty > 0 {
            counts.push(Span::styled(
                format!(" {}✎", repo.dirty),
                Style::default().fg(YELLOW),
            ));
        }
        if repo.ahead > 0 {
            counts.push(Span::styled(
                format!(" {}↑", repo.ahead),
                Style::default().fg(ORANGE),
            ));
        }
        if repo.commits_by_others > 0 {
            counts.push(Span::styled(
                format!(" {}◦", repo.commits_by_others),
                Style::default().fg(DIM),
            ));
        }
        let tail: usize = counts.iter().map(|s| s.content.chars().count()).sum();
        let mut spans = vec![Span::styled(
            truncate(&repo.name, width.saturating_sub(tail)),
            Style::default().fg(BRIGHT),
        )];
        spans.extend(counts);
        lines.push(Line::from(spans));

        // Which checkout the dirt is in, when it is not the obvious one.
        for co in repo.busy_checkouts() {
            if lines.len() >= inner.height as usize {
                break;
            }
            if !co.is_worktree && repo.checkouts.len() == 1 {
                continue;
            }
            let mut marks = String::new();
            if co.dirty > 0 {
                marks.push_str(&format!(" {}✎", co.dirty));
            }
            if co.ahead > 0 {
                marks.push_str(&format!(" {}↑", co.ahead));
            }
            lines.push(Line::from(Span::styled(
                format!(
                    "  {}{}",
                    truncate(&co.branch, width.saturating_sub(marks.chars().count() + 2)),
                    marks
                ),
                Style::default().fg(FAINT),
            )));
        }

        if let Some(top) = repo.commits.first()
            && lines.len() < inner.height as usize
        {
            lines.push(Line::from(Span::styled(
                format!("  {}", truncate(&top.subject, width.saturating_sub(2))),
                Style::default().fg(DIM),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_agents(f: &mut Frame, area: Rect, app: &App) {
    let empty = Vec::new();
    let agents = app.doc().map(|d| &d.agents).unwrap_or(&empty);
    let block = panel("agents", Some(agents.len()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if agents.is_empty() {
        f.render_widget(Paragraph::new("no agents attached").fg(DIM), inner);
        return;
    }
    let width = inner.width as usize;
    let lines: Vec<Line> = agents
        .iter()
        .take(inner.height as usize)
        .map(|a| {
            let color = status_color(&a.status);
            let glyph = herdr_api::Status::parse(&a.status).glyph();
            let label = if a.title.is_empty() {
                a.agent.clone()
            } else {
                a.title.clone()
            };
            let head = format!("{glyph} {:<5} ", a.pane_id);
            let room = width.saturating_sub(head.chars().count());
            Line::from(vec![
                Span::styled(head, Style::default().fg(color)),
                Span::styled(truncate(&label, room), Style::default().fg(TEXT)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let doc = app.doc();
    let window = doc
        .map(|d| d.window.label.clone())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "…".into());
    let mut spans = vec![
        Span::styled(" daybook ", Style::default().fg(BRIGHT).bold()),
        Span::styled(format!("· {window}"), Style::default().fg(DIM)),
    ];
    if let Some(d) = doc {
        let t = &d.totals;
        spans.push(Span::styled(
            format!(
                "   {} commits in {} repos · {} sessions · {} PRs open",
                t.commits, t.repos_touched, t.sessions, t.prs_open
            ),
            Style::default().fg(TEXT),
        ));
        if t.prs_merged > 0 {
            spans.push(Span::styled(
                format!(" · {} merged", t.prs_merged),
                Style::default().fg(GREEN),
            ));
        }
        if t.reviews_requested > 0 {
            spans.push(Span::styled(
                format!(" · {} to review", t.reviews_requested),
                Style::default().fg(ORANGE),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(
        " q close  r refresh  j/k move  a all  n offline ",
        Style::default().fg(DIM),
    )];
    if app.collecting {
        spans.push(Span::styled("· collecting… ", Style::default().fg(YELLOW)));
    } else {
        spans.push(Span::styled(
            format!("· {}s ago ", app.age().as_secs()),
            Style::default().fg(FAINT),
        ));
    }
    if app.max_severity < 4 {
        spans.push(Span::styled(
            "· housekeeping hidden ",
            Style::default().fg(FAINT),
        ));
    }
    if !app.net {
        spans.push(Span::styled("· offline ", Style::default().fg(ORANGE)));
    }
    if let Some(d) = app.doc() {
        if d.from_cache {
            spans.push(Span::styled(
                format!("· cached {}s ", d.cache_age_seconds),
                Style::default().fg(FAINT),
            ));
        }
        if !d.errors.is_empty() {
            spans.push(Span::styled(
                format!("· {} notes ", d.errors.len()),
                Style::default().fg(ORANGE),
            ));
        }
        if d.schema != crate::model::SCHEMA {
            spans.push(Span::styled(
                format!("· schema {} ≠ {} ", d.schema, crate::model::SCHEMA),
                Style::default().fg(RED),
            ));
        }
    }
    if let Some(err) = &app.error {
        spans = vec![Span::styled(
            format!(" {} — q close, r retry", truncate(err, area.width as usize)),
            Style::default().fg(RED),
        )];
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn draw(f: &mut Frame, app: &App) {
    let full = f.area();
    if full.width < 54 || full.height < 12 {
        f.render_widget(
            Paragraph::new("pane too small for the daybook")
                .fg(DIM)
                .wrap(Wrap { trim: true }),
            full,
        );
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .split(full);
    draw_header(f, rows[0], app);
    draw_footer(f, rows[2], app);

    // 40% is the narrowest the right column can be before repository names
    // start losing their tails at a typical popup width.
    let cols =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(rows[1]);
    draw_loops(f, cols[0], app);

    let side =
        Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(cols[1]);
    draw_repos(f, side[0], app);
    draw_agents(f, side[1], app);
}

/// Plain-text frame for `--once`, and for when stdout is not a terminal.
///
/// Deliberately rendered here rather than delegating to the collector's own
/// `--text`: this is what makes the pane's own view inspectable in CI and in a
/// scrollback, and the two renderers would otherwise drift untested.
pub fn print_once(doc: &Doc, max_severity: u8) {
    let t = &doc.totals;
    println!(
        "daybook — {} · {} commits in {} repos · {} sessions · {} PRs open · {} agents",
        if doc.window.label.is_empty() {
            "(no window)"
        } else {
            &doc.window.label
        },
        t.commits,
        t.repos_touched,
        t.sessions,
        t.prs_open,
        t.agents
    );

    let loops = doc.loops(max_severity);
    println!("\nopen loops ({})", loops.len());
    if loops.is_empty() {
        println!("  nothing waiting on you");
    }
    for item in loops {
        println!("  {} {}", Severity::parse(item.severity).mark(), item.text);
        if !item.hint.is_empty() {
            println!("       → {}", item.hint);
        }
    }

    let repos = doc.active_repos();
    println!("\nmoved ({})", repos.len());
    for repo in repos {
        let mut bits = Vec::new();
        if !repo.commits.is_empty() {
            bits.push(format!("{}●", repo.commits.len()));
        }
        if repo.dirty > 0 {
            bits.push(format!("{}✎", repo.dirty));
        }
        if repo.ahead > 0 {
            bits.push(format!("{}↑", repo.ahead));
        }
        println!("  {:<28} {}", repo.name, bits.join(" "));
    }

    println!("\nagents ({})", doc.agents.len());
    for a in &doc.agents {
        let st = herdr_api::Status::parse(&a.status);
        println!(
            "  {} {:<8} {:<6} {}",
            st.glyph(),
            st.label(),
            a.pane_id,
            a.title
        );
    }

    if !doc.errors.is_empty() {
        println!("\nnotes");
        for e in &doc.errors {
            println!("  - {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_list_needs_no_scrolling() {
        assert_eq!(scroll_offset(0, 10, 0, 0), 0);
        assert_eq!(scroll_offset(5, 0, 3, 0), 0);
    }

    #[test]
    fn a_list_that_fits_never_scrolls() {
        for cursor in 0..4 {
            assert_eq!(scroll_offset(4, 10, cursor, 0), 0, "cursor {cursor}");
        }
    }

    #[test]
    fn moving_past_the_bottom_scrolls_by_one() {
        // height 5 → 4 usable rows, so cursor 4 is the first to push the window.
        assert_eq!(scroll_offset(20, 5, 3, 0), 0);
        assert_eq!(scroll_offset(20, 5, 4, 0), 1);
        assert_eq!(scroll_offset(20, 5, 5, 1), 2);
    }

    #[test]
    fn moving_above_the_window_scrolls_back() {
        assert_eq!(scroll_offset(20, 5, 2, 6), 2);
    }

    #[test]
    fn the_last_item_is_always_reachable() {
        // The regression this guards: an offset clamped to `total - height`
        // while the window is `height - 1` tall leaves the final row unseen.
        let (total, height) = (20, 5);
        let offset = scroll_offset(total, height, total - 1, 0);
        let usable = height - 1;
        assert!(
            offset + usable >= total,
            "offset {offset} cannot reach {total}"
        );
    }

    #[test]
    fn the_offset_never_leaves_the_list_short() {
        // Scrolling to the end and then shrinking the list must not leave a
        // window pointing past it.
        assert_eq!(scroll_offset(3, 10, 0, 90), 0);
    }

    #[test]
    fn severity_colours_are_distinct_per_band() {
        let colors = [
            severity_color(Severity::Blocked),
            severity_color(Severity::Waiting),
            severity_color(Severity::Loose),
            severity_color(Severity::Tidy),
        ];
        for (i, a) in colors.iter().enumerate() {
            for b in colors.iter().skip(i + 1) {
                assert_ne!(a, b, "two bands share a colour");
            }
        }
    }

    #[test]
    fn agent_colours_agree_with_the_severity_palette() {
        // A blocked agent and a blocked loop are the same red, deliberately.
        assert_eq!(status_color("blocked"), severity_color(Severity::Blocked));
        assert_eq!(status_color("done"), severity_color(Severity::Waiting));
        assert_eq!(status_color("nonsense"), severity_color(Severity::Tidy));
    }
}
