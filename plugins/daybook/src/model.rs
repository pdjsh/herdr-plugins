//! The collector's JSON document, as Rust, plus the plumbing that produces it.
//!
//! Every field is `#[serde(default)]`. The collector and this pane version
//! independently, and a pane that refuses to render because one new field is
//! missing is worse than one that renders a slightly thinner picture. The
//! `schema` number guards the shape changes that actually matter.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

/// Schema this pane was written against. A document from the future still
/// renders — the fields it shares are still the fields it shares — but the
/// mismatch is surfaced rather than hidden.
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Window {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub days_back: i64,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Totals {
    #[serde(default)]
    pub commits: usize,
    #[serde(default)]
    pub repos_touched: usize,
    #[serde(default)]
    pub repos_scanned: usize,
    #[serde(default)]
    pub sessions: usize,
    #[serde(default)]
    pub prs_open: usize,
    #[serde(default)]
    pub prs_merged: usize,
    #[serde(default)]
    pub reviews_requested: usize,
    #[serde(default)]
    pub agents: usize,
    #[serde(default)]
    pub agents_blocked: usize,
    #[serde(default)]
    pub agents_done: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Commit {
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub subject: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Checkout {
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub is_worktree: bool,
    #[serde(default)]
    pub dirty: usize,
    #[serde(default)]
    pub ahead: usize,
    #[serde(default)]
    pub behind_base: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Repo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub commits: Vec<Commit>,
    #[serde(default)]
    pub commits_by_others: usize,
    #[serde(default)]
    pub dirty: usize,
    #[serde(default)]
    pub ahead: usize,
    #[serde(default)]
    pub checkouts: Vec<Checkout>,
}

impl Repo {
    /// Whether this repository did anything worth a row.
    pub fn active(&self) -> bool {
        !self.commits.is_empty() || self.dirty > 0 || self.ahead > 0
    }

    /// Checkouts carrying uncommitted or unpushed work, most interesting first.
    pub fn busy_checkouts(&self) -> Vec<&Checkout> {
        let mut v: Vec<&Checkout> = self
            .checkouts
            .iter()
            .filter(|c| c.dirty > 0 || c.ahead > 0)
            .collect();
        v.sort_by_key(|c| (usize::MAX - c.dirty, usize::MAX - c.ahead));
        v
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Pr {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Prs {
    #[serde(default)]
    pub mine: Vec<Pr>,
    #[serde(default)]
    pub review_requested: Vec<Pr>,
    #[serde(default)]
    pub merged_in_window: Vec<Pr>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub unanswered: bool,
    #[serde(default)]
    pub session_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Agent {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub agent: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Item {
    #[serde(default)]
    pub severity: u8,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub hint: String,
    /// Path or URL the item points at. `where` is a Rust keyword.
    #[serde(default, rename = "where")]
    pub where_: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Doc {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub window: Window,
    #[serde(default)]
    pub totals: Totals,
    #[serde(default)]
    pub repos: Vec<Repo>,
    #[serde(default)]
    pub prs: Prs,
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub attention: Vec<Item>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub net: bool,
    #[serde(default)]
    pub from_cache: bool,
    #[serde(default)]
    pub cache_age_seconds: u64,
}

impl Doc {
    pub fn active_repos(&self) -> Vec<&Repo> {
        self.repos.iter().filter(|r| r.active()).collect()
    }

    /// Attention items at or above a severity band (lower number = louder).
    pub fn loops(&self, max_severity: u8) -> Vec<&Item> {
        self.attention
            .iter()
            .filter(|i| i.severity <= max_severity)
            .collect()
    }
}

/// Attention bands. The numbers are the collector's, not ours — they are part
/// of the schema, and the `/standup` skill colours its briefing by the same
/// ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Stuck: cannot progress without a human.
    Blocked,
    /// Waiting on me to look at it.
    Waiting,
    /// Work in flight that will rot if left alone.
    Loose,
    /// Housekeeping.
    Tidy,
}

impl Severity {
    pub fn parse(n: u8) -> Self {
        match n {
            0 | 1 => Self::Blocked,
            2 => Self::Waiting,
            3 => Self::Loose,
            _ => Self::Tidy,
        }
    }

    /// Two columns wide for every band, so the text starts on one column no
    /// matter what a row's severity is.
    pub fn mark(self) -> &'static str {
        match self {
            Self::Blocked => "!!",
            Self::Waiting => " !",
            Self::Loose => " ·",
            Self::Tidy => "  ",
        }
    }
}

// ---------------------------------------------------------------------------
// running the collector
// ---------------------------------------------------------------------------

/// The plugin root, derived from this executable's own path.
///
/// A plugin binary lives at `<plugin_root>/target/<profile>/daybook`, which the
/// manifest's `--target-dir ./target` guarantees. Three levels up is therefore
/// the plugin root — no working directory involved.
fn plugin_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.parent()?.to_path_buf())
}

/// Where the collector script was found, and how to invoke it.
#[derive(Debug, Clone)]
pub struct Collector {
    path: PathBuf,
    /// A `.py` path is handed to `python3` rather than executed, so a checkout
    /// that lost the exec bit still works.
    via_python: bool,
}

impl Collector {
    /// Resolve the collector, preferring the most explicit source.
    ///
    /// The plugin and the script ship in the same repository but do not have to
    /// stay together: `herdr plugin install` clones the whole repo, so the copy
    /// beside the binary is the one that matches it, while a hand-linked plugin
    /// may be pointed at a script installed on `PATH` instead.
    pub fn locate() -> Result<Self> {
        if let Some(explicit) = std::env::var_os("DAYBOOK_COLLECT") {
            let path = PathBuf::from(explicit);
            if !path.exists() {
                return Err(anyhow!(
                    "DAYBOOK_COLLECT points at {}, which does not exist",
                    path.display()
                ));
            }
            return Ok(Self::at(path));
        }

        let mut candidates: Vec<PathBuf> = Vec::new();
        // The copy that shipped with this binary, found relative to the binary
        // rather than to the working directory. herdr does spawn panes with the
        // plugin root as their cwd, but anchoring on cwd would mean that running
        // the binary from a directory someone else can write to executes their
        // `../../tools/daybook/daybook-collect.py`. The executable's own
        // location cannot be redirected that way.
        if let Some(root) = plugin_root() {
            candidates.push(root.join("../../tools/daybook/daybook-collect.py"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(&home).join(".local/bin/daybook-collect"));
            candidates.push(
                PathBuf::from(&home)
                    .join("Projects/herdr-plugins/tools/daybook/daybook-collect.py"),
            );
        }
        for candidate in candidates {
            if candidate.exists() {
                return Ok(Self::at(candidate));
            }
        }
        // Last resort: let the OS resolve it, and report honestly if it cannot.
        if Command::new("daybook-collect")
            .arg("--help")
            .output()
            .is_ok()
        {
            return Ok(Self::at(PathBuf::from("daybook-collect")));
        }
        Err(anyhow!(
            "cannot find daybook-collect — set DAYBOOK_COLLECT to its path, \
             or symlink it into ~/.local/bin"
        ))
    }

    /// A collector that is never actually run, for tests that only need the
    /// surrounding state machine.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::at(PathBuf::from("/nonexistent/daybook-collect.py"))
    }

    fn at(path: PathBuf) -> Self {
        let via_python = path.extension().is_some_and(|e| e == "py");
        Self { path, via_python }
    }

    /// The resolved script path, for `--where`. Which of the several candidate
    /// locations won is the first thing worth knowing when a pane comes up empty.
    pub fn display(&self) -> String {
        self.path.display().to_string()
    }

    /// Collect once. `max_age` lets the first paint come from a warm cache
    /// instead of a five-second wait; `r` in the pane passes zero.
    pub fn run(&self, max_age: u64, net: bool) -> Result<Doc> {
        let mut cmd = if self.via_python {
            let mut c = Command::new("python3");
            c.arg(&self.path);
            c
        } else {
            Command::new(&self.path)
        };
        cmd.arg("--max-age").arg(max_age.to_string());
        if !net {
            cmd.arg("--no-net");
        }
        let out = cmd
            .output()
            .with_context(|| format!("running {}", self.path.display()))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!(
                "{} exited {}: {}",
                self.path.display(),
                out.status.code().unwrap_or(-1),
                first_line(&err)
            ));
        }
        parse(&out.stdout)
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_string()
}

pub fn parse(bytes: &[u8]) -> Result<Doc> {
    let doc: Doc = serde_json::from_slice(bytes).context("decoding collector JSON")?;
    Ok(doc)
}

/// Fit a string into `max` display columns, ellipsising rather than panicking
/// on a multi-byte boundary.
pub fn truncate(s: &str, max: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_bands_cover_every_number_the_collector_can_emit() {
        assert_eq!(Severity::parse(1), Severity::Blocked);
        assert_eq!(Severity::parse(2), Severity::Waiting);
        assert_eq!(Severity::parse(3), Severity::Loose);
        assert_eq!(Severity::parse(4), Severity::Tidy);
    }

    #[test]
    fn unexpected_severities_degrade_to_the_quietest_band() {
        // A future collector adding severity 5 must not shout.
        assert_eq!(Severity::parse(9), Severity::Tidy);
        assert_eq!(Severity::parse(255), Severity::Tidy);
        // Zero would be louder than blocked, which no band is.
        assert_eq!(Severity::parse(0), Severity::Blocked);
    }

    #[test]
    fn every_mark_is_two_columns_wide() {
        for s in [
            Severity::Blocked,
            Severity::Waiting,
            Severity::Loose,
            Severity::Tidy,
        ] {
            assert_eq!(s.mark().chars().count(), 2, "{s:?}");
        }
    }

    #[test]
    fn an_empty_object_parses_into_an_empty_document() {
        // The pane must survive a collector that bailed early and printed `{}`.
        let doc = parse(b"{}").expect("empty object");
        assert_eq!(doc.schema, 0);
        assert!(doc.attention.is_empty());
        assert!(doc.active_repos().is_empty());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let doc = parse(br#"{"schema":1,"invented_later":{"a":1},"totals":{"commits":3}}"#)
            .expect("unknown fields");
        assert_eq!(doc.totals.commits, 3);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(parse(b"not json").is_err());
        assert!(parse(b"").is_err());
    }

    #[test]
    fn loops_filter_by_severity_band() {
        let doc: Doc = serde_json::from_str(
            r#"{"attention":[
                 {"severity":1,"text":"a"},
                 {"severity":3,"text":"b"},
                 {"severity":4,"text":"c"}]}"#,
        )
        .unwrap();
        assert_eq!(doc.loops(4).len(), 3);
        assert_eq!(doc.loops(3).len(), 2);
        assert_eq!(doc.loops(1).len(), 1);
    }

    #[test]
    fn a_repo_is_active_when_anything_moved_in_it() {
        assert!(!Repo::default().active());
        assert!(
            Repo {
                dirty: 1,
                ..Default::default()
            }
            .active()
        );
        assert!(
            Repo {
                ahead: 2,
                ..Default::default()
            }
            .active()
        );
        assert!(
            Repo {
                commits: vec![Commit::default()],
                ..Default::default()
            }
            .active()
        );
    }

    #[test]
    fn busy_checkouts_skip_the_clean_ones_and_lead_with_the_dirtiest() {
        let repo = Repo {
            checkouts: vec![
                Checkout {
                    branch: "main".into(),
                    ..Default::default()
                },
                Checkout {
                    branch: "feat/a".into(),
                    dirty: 1,
                    ..Default::default()
                },
                Checkout {
                    branch: "feat/b".into(),
                    dirty: 9,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let busy = repo.busy_checkouts();
        assert_eq!(busy.len(), 2);
        assert_eq!(busy[0].branch, "feat/b");
    }

    #[test]
    fn truncate_never_splits_a_character() {
        assert_eq!(truncate("héllo wörld", 6), "héllo…");
        assert_eq!(truncate("short", 20), "short");
        assert_eq!(truncate("abc", 1), "…");
        assert_eq!(truncate("", 4), "");
    }

    #[test]
    fn a_python_path_is_run_through_the_interpreter() {
        assert!(Collector::at(PathBuf::from("/x/daybook-collect.py")).via_python);
        assert!(!Collector::at(PathBuf::from("/x/daybook-collect")).via_python);
    }

    #[test]
    fn stderr_is_reported_by_its_last_meaningful_line() {
        // Python prints a traceback; the useful part is the bottom of it.
        assert_eq!(
            first_line("Traceback:\n  File x\nValueError: nope\n\n"),
            "ValueError: nope"
        );
        assert_eq!(first_line("   \n"), "no output");
    }
}
