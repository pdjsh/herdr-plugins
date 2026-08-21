//! The morning read: what happened while you were away, and what is still open.
//!
//! A viewer, like `agent-map`. It shells out to `daybook-collect` — the same
//! script the `/standup` Claude Code skill uses — so the pane and the briefing
//! can never tell you different things about the same day. Nothing here mutates
//! anything: no pane jumping, no PR actions, no git commands. Deciding what to
//! act on is the point; acting is what the rest of herdr is for.
//!
//! Two refresh cadences, because the sources age at very different rates:
//! agents and dirty files change minute to minute, while pull-request state
//! costs a dozen network round trips to learn. So the pane re-reads git,
//! transcripts and the agent roster every 30s offline, refreshes pull requests
//! every 5 minutes, and carries the last known PR state forward in between.
//! `r` forces a full refresh.
//!
//! `--once` prints a single plain-text frame instead of opening a TUI.

mod model;
mod ui;

use std::io::{IsTerminal, Write, stdout};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, cursor};
use model::{Collector, Doc, Item, Prs};
use ratatui::prelude::*;

/// Cheap pass: git, transcripts, the agent roster. ~1.5s on a dozen repos.
const LIVE_REFRESH: Duration = Duration::from_secs(30);
/// Expensive pass: everything, including the GitHub round trips.
const FULL_REFRESH: Duration = Duration::from_secs(300);
/// A first paint from a cache this fresh beats a five-second blank pane.
const FIRST_PAINT_MAX_AGE: u64 = 900;

/// Attention kinds that only a networked collection can produce. Carried
/// forward across offline refreshes so pull requests do not blink out of the
/// list every 30 seconds.
fn is_pr_item(item: &Item) -> bool {
    item.kind.starts_with("pr-") || item.kind == "review-requested"
}

pub struct App {
    doc: Option<Doc>,
    pub error: Option<String>,
    pub cursor: usize,
    pub offset: usize,
    /// Highest severity number shown. 3 hides the housekeeping band; `a` toggles.
    pub max_severity: u8,
    pub net: bool,
    pub collecting: bool,
    /// True while the displayed pull-request data came from an earlier pass.
    pub prs_carried: bool,
    /// Rows the open-loops panel had in the last frame. Kept on the state so
    /// `clamp` can re-derive the scroll offset without a terminal to measure.
    list_height: usize,
    collector: Collector,
    rx: Option<Receiver<Result<Doc, String>>>,
    last: Instant,
    last_full: Option<Instant>,
    /// The last networked answer about pull requests.
    ///
    /// Held separately rather than read back off the previous document, so that
    /// a run of offline refreshes keeps carrying the same snapshot forward
    /// instead of losing it after the first one — and so carrying twice cannot
    /// duplicate the items.
    net_prs: Option<NetPrs>,
}

/// Everything a networked pass knows that an offline one does not.
#[derive(Debug, Clone, Default)]
struct NetPrs {
    prs: Prs,
    open: usize,
    merged: usize,
    reviews_requested: usize,
    items: Vec<Item>,
}

impl NetPrs {
    fn from(doc: &Doc) -> Self {
        Self {
            prs: doc.prs.clone(),
            open: doc.totals.prs_open,
            merged: doc.totals.prs_merged,
            reviews_requested: doc.totals.reviews_requested,
            items: doc
                .attention
                .iter()
                .filter(|i| is_pr_item(i))
                .cloned()
                .collect(),
        }
    }
}

impl App {
    fn new(collector: Collector) -> Self {
        Self {
            doc: None,
            error: None,
            cursor: 0,
            offset: 0,
            max_severity: 4,
            net: true,
            collecting: false,
            prs_carried: false,
            // Replaced by the real height on the first frame; non-zero so a
            // clamp before the first draw still behaves.
            list_height: 20,
            collector,
            rx: None,
            last: Instant::now(),
            last_full: None,
            net_prs: None,
        }
    }

    pub fn doc(&self) -> Option<&Doc> {
        self.doc.as_ref()
    }

    pub fn age(&self) -> Duration {
        self.last.elapsed()
    }

    /// Kick off a collection on a worker thread.
    ///
    /// On a thread because the collector takes seconds and a pane that stops
    /// answering `q` while it waits is a pane you have to kill.
    fn start(&mut self, max_age: u64, net: bool) {
        if self.collecting {
            return;
        }
        let (tx, rx) = channel();
        let collector = self.collector.clone();
        std::thread::spawn(move || {
            let result = collector.run(max_age, net).map_err(|e| format!("{e:#}"));
            // The receiver is gone only if the pane already exited.
            let _ = tx.send(result);
        });
        self.rx = Some(rx);
        self.collecting = true;
    }

    /// Take a finished collection, if one is ready. Returns true if the view
    /// changed and needs redrawing.
    fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(Ok(doc)) => {
                self.apply(doc);
                self.rx = None;
                self.collecting = false;
                true
            }
            Ok(Err(message)) => {
                self.error = Some(message);
                self.rx = None;
                self.collecting = false;
                // Both clocks have to move, or the next iteration sees a due
                // full refresh (`last_full` is None until one succeeds) and
                // respawns the collector every ~120ms forever. A fast-failing
                // script — missing `python3`, a syntax error — would otherwise
                // spin, and an intermittently-failing one would hammer `gh`.
                self.mark_attempted();
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                // The worker panicked. Say so rather than spinning forever on a
                // channel that will never produce anything.
                self.error = Some("the collector thread died".into());
                self.rx = None;
                self.collecting = false;
                true
            }
        }
    }

    /// Note that a collection was attempted, successfully or not, so neither
    /// cadence fires again immediately.
    fn mark_attempted(&mut self) {
        let now = Instant::now();
        self.last = now;
        self.last_full = Some(now);
    }

    fn apply(&mut self, mut doc: Doc) {
        self.prs_carried = false;
        if doc.net {
            self.net_prs = Some(NetPrs::from(&doc));
            // A cached document does not reset the full-refresh clock. The first
            // paint accepts a cache up to 15 minutes old, and treating that as
            // "just refreshed" would defer the real pass by another 5 — showing
            // 20-minute-old CI state with nothing saying so.
            if !doc.from_cache {
                self.last_full = Some(Instant::now());
            }
        } else if let Some(snapshot) = &self.net_prs {
            // An offline pass knows nothing about pull requests. Blanking them
            // would be a lie of omission, so the last networked answer is
            // carried forward and flagged as older than the rest.
            doc.prs = snapshot.prs.clone();
            doc.totals.prs_open = snapshot.open;
            doc.totals.prs_merged = snapshot.merged;
            doc.totals.reviews_requested = snapshot.reviews_requested;
            doc.attention.extend(snapshot.items.iter().cloned());
            doc.attention.sort_by(|a, b| {
                a.severity
                    .cmp(&b.severity)
                    .then(a.kind.cmp(&b.kind))
                    .then(a.text.cmp(&b.text))
            });
            self.prs_carried = true;
        }
        self.doc = Some(doc);
        self.error = None;
        self.last = Instant::now();
        self.clamp();
    }

    fn visible(&self) -> usize {
        self.doc
            .as_ref()
            .map(|d| d.loops(self.max_severity).len())
            .unwrap_or(0)
    }

    /// Pull the cursor and the scroll offset back inside the list.
    ///
    /// Both, not just the cursor: a refresh that shortens the list leaves an
    /// offset past its end, and the panel then renders `items[offset..len]` —
    /// which is empty — so it paints blank while its border still reports a
    /// count. `list_height` is what the last frame actually had room for.
    fn clamp(&mut self) {
        let n = self.visible();
        if n == 0 {
            self.cursor = 0;
            self.offset = 0;
            return;
        }
        self.cursor = self.cursor.min(n - 1);
        self.offset = ui::scroll_offset(n, self.list_height, self.cursor, self.offset);
    }

    fn move_cursor(&mut self, delta: isize, height: usize) {
        let n = self.visible();
        if n == 0 {
            return;
        }
        self.list_height = height;
        let next = (self.cursor as isize + delta).clamp(0, n as isize - 1) as usize;
        self.cursor = next;
        self.offset = ui::scroll_offset(n, height, self.cursor, self.offset);
    }
}

/// Height of the open-loops list, derived from the same constraints `ui::draw`
/// uses: one header row, one footer row, and the panel's own two borders.
///
/// The event loop needs this before drawing, to know how far a `j` should
/// scroll, and a mismatch with the real layout would strand the cursor.
fn loops_height(area: Rect) -> usize {
    area.height.saturating_sub(4) as usize
}

fn run_tui(mut app: App) -> Result<()> {
    enable_raw_mode()?;
    stdout()
        .execute(EnterAlternateScreen)?
        .execute(cursor::Hide)?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;

    // A warm cache paints immediately; the full pass lands a few seconds later.
    app.start(FIRST_PAINT_MAX_AGE, true);

    let result = (|| -> Result<()> {
        loop {
            term.draw(|f| ui::draw(f, &app))?;
            let size = term.size()?;
            let height = loops_height(Rect::new(0, 0, size.width, size.height));
            // Recorded so a collection landing between frames can clamp against
            // the height the user is actually looking at.
            app.list_height = height;

            if event::poll(Duration::from_millis(120))?
                && let Event::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
            {
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('r') => app.start(0, app.net),
                    KeyCode::Char('j') | KeyCode::Down => app.move_cursor(1, height),
                    KeyCode::Char('k') | KeyCode::Up => app.move_cursor(-1, height),
                    KeyCode::Char('d') | KeyCode::PageDown => {
                        app.move_cursor(height as isize / 2, height)
                    }
                    KeyCode::Char('u') | KeyCode::PageUp => {
                        app.move_cursor(-(height as isize) / 2, height)
                    }
                    KeyCode::Char('g') | KeyCode::Home => app.move_cursor(isize::MIN / 2, height),
                    KeyCode::Char('G') | KeyCode::End => app.move_cursor(isize::MAX / 2, height),
                    KeyCode::Char('a') => {
                        // Housekeeping is worth hiding when the list is long.
                        app.max_severity = if app.max_severity >= 4 { 3 } else { 4 };
                        app.clamp();
                    }
                    KeyCode::Char('n') => {
                        app.net = !app.net;
                        app.start(0, app.net);
                    }
                    _ => {}
                }
            }

            app.poll();

            if !app.collecting {
                let full_due = app
                    .last_full
                    .map(|t| t.elapsed() >= FULL_REFRESH)
                    .unwrap_or(true);
                if app.net && full_due {
                    app.start(0, true);
                } else if app.last.elapsed() >= LIVE_REFRESH {
                    app.start(0, false);
                }
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

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let once = args.iter().any(|a| a == "--once");
    let net = !args.iter().any(|a| a == "--no-net");
    let where_ = args.iter().any(|a| a == "--where");

    let collector = match Collector::locate() {
        Ok(c) => c,
        Err(e) => {
            // A missing collector is a setup problem, not a crash. Say what to
            // do about it on one line and exit non-zero.
            eprintln!("daybook: {e}");
            std::process::exit(2);
        }
    };

    if where_ {
        println!("{}", collector.display());
        return Ok(());
    }

    if once || !stdout().is_terminal() {
        let doc = collector.run(0, net)?;
        ui::print_once(&doc, 4);
        return Ok(());
    }

    run_tui(App::new(collector))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(severities: &[u8]) -> Doc {
        let items: Vec<String> = severities
            .iter()
            .enumerate()
            .map(|(i, s)| format!(r#"{{"severity":{s},"text":"item {i}","kind":"k{i}"}}"#))
            .collect();
        serde_json::from_str(&format!(
            r#"{{"schema":1,"net":true,"attention":[{}]}}"#,
            items.join(",")
        ))
        .unwrap()
    }

    fn app_with(severities: &[u8]) -> App {
        let mut app = App::new(Collector::for_test());
        app.apply(doc_with(severities));
        app
    }

    #[test]
    fn the_cursor_cannot_leave_the_list() {
        let mut app = app_with(&[1, 2, 3]);
        app.move_cursor(-5, 10);
        assert_eq!(app.cursor, 0);
        app.move_cursor(50, 10);
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn an_empty_list_swallows_movement() {
        let mut app = app_with(&[]);
        app.move_cursor(1, 10);
        assert_eq!(app.cursor, 0);
        assert_eq!(app.offset, 0);
    }

    #[test]
    fn hiding_housekeeping_pulls_the_cursor_back_into_range() {
        let mut app = app_with(&[1, 4, 4, 4]);
        app.move_cursor(3, 10);
        assert_eq!(app.cursor, 3);
        app.max_severity = 3;
        app.clamp();
        // Only the severity-1 item survives the filter.
        assert_eq!(app.visible(), 1);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn a_shorter_document_does_not_strand_the_cursor() {
        let mut app = app_with(&[1, 1, 1, 1, 1]);
        app.move_cursor(4, 10);
        app.apply(doc_with(&[1, 1]));
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn an_offline_pass_carries_pull_requests_forward() {
        let mut app = App::new(Collector::for_test());
        app.apply(
            serde_json::from_str(
                r#"{"schema":1,"net":true,
                    "totals":{"prs_open":3},
                    "prs":{"mine":[{"number":1,"name":"r","title":"t"}]},
                    "attention":[{"severity":1,"kind":"pr-ci-red","text":"r#1 red"},
                                 {"severity":3,"kind":"dirty","text":"r dirty"}]}"#,
            )
            .unwrap(),
        );
        app.apply(
            serde_json::from_str(
                r#"{"schema":1,"net":false,
                    "attention":[{"severity":3,"kind":"dirty","text":"r dirty"}]}"#,
            )
            .unwrap(),
        );
        let doc = app.doc().unwrap();
        assert!(app.prs_carried);
        assert_eq!(doc.totals.prs_open, 3);
        assert_eq!(doc.prs.mine.len(), 1);
        // The PR item is back, and the ordering by severity still holds.
        assert_eq!(doc.attention.len(), 2);
        assert_eq!(doc.attention[0].kind, "pr-ci-red");
    }

    #[test]
    fn a_networked_pass_replaces_carried_pull_requests() {
        let mut app = App::new(Collector::for_test());
        app.apply(
            serde_json::from_str(
                r#"{"schema":1,"net":true,"totals":{"prs_open":3},
                    "attention":[{"severity":1,"kind":"pr-ci-red","text":"old"}]}"#,
            )
            .unwrap(),
        );
        app.apply(
            serde_json::from_str(r#"{"schema":1,"net":true,"totals":{"prs_open":0}}"#).unwrap(),
        );
        assert!(!app.prs_carried);
        assert_eq!(app.doc().unwrap().totals.prs_open, 0);
        assert!(app.doc().unwrap().attention.is_empty());
    }

    #[test]
    fn two_offline_passes_in_a_row_do_not_duplicate_pull_request_items() {
        let mut app = App::new(Collector::for_test());
        app.apply(
            serde_json::from_str(
                r#"{"schema":1,"net":true,
                    "attention":[{"severity":1,"kind":"pr-ci-red","text":"red"}]}"#,
            )
            .unwrap(),
        );
        for _ in 0..3 {
            app.apply(serde_json::from_str(r#"{"schema":1,"net":false,"attention":[]}"#).unwrap());
        }
        // Carrying forward from a carried-forward document must not stack up.
        assert_eq!(app.doc().unwrap().attention.len(), 1);
    }

    #[test]
    fn pr_items_are_recognised_by_kind() {
        for kind in ["pr-ci-red", "pr-conflict", "pr-stale", "review-requested"] {
            assert!(
                is_pr_item(&Item {
                    kind: kind.into(),
                    ..Default::default()
                }),
                "{kind}"
            );
        }
        for kind in ["dirty", "unpushed", "agent-blocked", "session-unanswered"] {
            assert!(
                !is_pr_item(&Item {
                    kind: kind.into(),
                    ..Default::default()
                }),
                "{kind}"
            );
        }
    }

    #[test]
    fn a_shortened_list_does_not_leave_the_panel_scrolled_past_its_end() {
        // The regression: `clamp` fixed the cursor but not the offset, so the
        // panel rendered `items[offset..len]` — empty — and painted blank while
        // its border still reported a count.
        let mut app = app_with(&[1; 30]);
        app.move_cursor(29, 6);
        assert!(
            app.offset > 5,
            "expected to be scrolled, got {}",
            app.offset
        );
        app.apply(doc_with(&[1, 1, 1]));
        assert!(
            app.offset < app.visible(),
            "offset {} is past the {} remaining items",
            app.offset,
            app.visible()
        );
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn hiding_a_band_also_pulls_the_offset_back() {
        let mut app = app_with(&[1, 4, 4, 4, 4, 4, 4, 4, 4, 4]);
        app.move_cursor(9, 4);
        app.max_severity = 3;
        app.clamp();
        assert_eq!(app.visible(), 1);
        assert_eq!(app.offset, 0);
    }

    #[test]
    fn a_failed_collection_moves_both_refresh_clocks() {
        // Otherwise `full_due` is true forever (last_full stays None) and the
        // loop respawns the collector every ~120ms.
        let mut app = App::new(Collector::for_test());
        assert!(app.last_full.is_none());
        app.mark_attempted();
        assert!(app.last_full.is_some());
        assert!(app.age() < Duration::from_secs(1));
    }

    #[test]
    fn a_cached_document_does_not_reset_the_full_refresh_clock() {
        // The first paint accepts a 15-minute-old cache. Treating that as "just
        // refreshed" would defer the real pass by another five.
        let mut app = App::new(Collector::for_test());
        app.apply(serde_json::from_str(r#"{"schema":1,"net":true,"from_cache":true}"#).unwrap());
        assert!(
            app.last_full.is_none(),
            "a cached document must still leave a full pass due"
        );
        app.apply(serde_json::from_str(r#"{"schema":1,"net":true}"#).unwrap());
        assert!(app.last_full.is_some());
    }

    #[test]
    fn the_loops_height_matches_the_real_layout() {
        // Guards against `ui::draw`'s constraints and this shortcut drifting.
        let area = Rect::new(0, 0, 100, 30);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(1),
        ])
        .split(area);
        let cols = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(rows[1]);
        assert_eq!(
            loops_height(area),
            cols[0].height.saturating_sub(2) as usize
        );
    }
}
