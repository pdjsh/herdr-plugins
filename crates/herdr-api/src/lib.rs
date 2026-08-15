//! Minimal client for the herdr unix-socket API.
//!
//! Protocol, as observed against herdr 0.8.0 (protocol 19): one JSON object per
//! line over a `SOCK_STREAM` unix socket. A request is
//! `{"id": "...", "method": "...", "params": {...}}`; the reply is a single line
//! carrying either `result` or `error`. The server closes nothing on its own, so
//! the client reads exactly one line and drops the connection.
//!
//! The socket path comes from `HERDR_SOCKET_PATH`, which herdr sets for every
//! plugin process it spawns. Falling back to `~/.config/herdr/herdr.sock` keeps
//! the binaries runnable outside a plugin context, which is how they are
//! developed and tested.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

/// Fields shared by every response envelope.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

/// A git worktree backing a workspace. Present only for workspaces opened
/// against a repository; plain shell workspaces omit it entirely.
#[derive(Debug, Clone, Deserialize)]
pub struct Worktree {
    /// Path to the repository's `.git`. Shared by every workspace pointing at the
    /// same repo, which makes it the natural grouping key for the map.
    pub repo_key: String,
    pub repo_name: String,
    #[serde(default)]
    pub repo_root: String,
    #[serde(default)]
    pub checkout_path: String,
    #[serde(default)]
    pub is_linked_worktree: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub pane_count: u32,
    #[serde(default)]
    pub tab_count: u32,
    #[serde(default)]
    pub agent_status: String,
    #[serde(default)]
    pub worktree: Option<Worktree>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub pane_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub agent: String,
    /// Title the agent set on its pane, with herdr's state glyph removed. For
    /// Claude Code this is the current task, which is the only field that
    /// distinguishes one row from another when every agent reports `claude`.
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub agent_status: String,
    #[serde(default)]
    pub focused: bool,
    /// Monotonic counter bumped on every state transition. herdr exposes no
    /// wall-clock timestamp for agent activity, so this stands in for the
    /// "recency" score that drives node size and brightness.
    #[serde(default)]
    pub state_change_seq: u64,
    #[serde(default)]
    pub cwd: String,
}

/// Agent lifecycle state.
///
/// The five variants are the full `AgentStatus` enum from herdr's own API schema
/// (`/schemas/success_response/$defs/AgentStatus`). Note that the *request*-side
/// `PaneAgentState` carries only four — it has no `done` — so do not assume the
/// two are interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Waiting on the user — an approval prompt or a question.
    Blocked,
    /// Finished its task and idle-but-unread. Distinct from `Idle`, which is a
    /// agent sitting at a prompt having done nothing.
    Done,
    Working,
    Idle,
    Unknown,
}

impl Status {
    pub fn parse(s: &str) -> Self {
        match s {
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            "working" => Self::Working,
            "idle" => Self::Idle,
            _ => Self::Unknown,
        }
    }

    /// Glyphs match the ones herdr renders in its own sidebar, so the map reads
    /// the same way as the panel next to it.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Blocked => "⚠",
            Self::Done => "●",
            Self::Working => "✳",
            Self::Idle => "✓",
            Self::Unknown => "·",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }

    /// Sort weight for attention. `Done` ranks just under `Blocked`: neither is
    /// making progress, and both are waiting on a human to look at them.
    pub fn priority(self) -> u8 {
        match self {
            Self::Blocked => 0,
            Self::Done => 1,
            Self::Working => 2,
            Self::Idle => 3,
            Self::Unknown => 4,
        }
    }
}

/// Strip leading decoration from an agent-set terminal title.
///
/// `terminal_title_stripped` removes only *herdr's* own state glyph. Claude Code
/// prefixes its title with a spinner frame of its own (◐◑◒◓ …), which survives
/// and then flickers frame-to-frame in the map. Anything before the first
/// alphanumeric character is decoration.
pub fn clean_title(s: &str) -> String {
    s.trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string()
}

pub struct Client {
    socket: PathBuf,
}

impl Client {
    /// Resolve the socket the same way herdr's own CLI does.
    pub fn from_env() -> Result<Self> {
        let socket = match std::env::var_os("HERDR_SOCKET_PATH") {
            Some(p) => PathBuf::from(p),
            None => {
                let home = std::env::var_os("HOME")
                    .ok_or_else(|| anyhow!("neither HERDR_SOCKET_PATH nor HOME is set"))?;
                PathBuf::from(home).join(".config/herdr/herdr.sock")
            }
        };
        if !socket.exists() {
            return Err(anyhow!(
                "herdr socket not found at {} — is the server running?",
                socket.display()
            ));
        }
        Ok(Self { socket })
    }

    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connecting to {}", self.socket.display()))?;
        // Without a timeout a wedged server would hang the pane with no way out
        // but killing it, since a plugin pane has no other input loop.
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        let mut w = &stream;
        let req = serde_json::json!({ "id": "1", "method": method, "params": params });
        writeln!(w, "{req}").context("writing request")?;
        w.flush()?;

        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .context("reading response")?;
        if line.trim().is_empty() {
            return Err(anyhow!("empty response from herdr for {method}"));
        }

        let env: Envelope = serde_json::from_str(&line)
            .with_context(|| format!("decoding response for {method}"))?;
        if let Some(e) = env.error {
            return Err(anyhow!("herdr {method} failed [{}]: {}", e.code, e.message));
        }
        env.result
            .ok_or_else(|| anyhow!("herdr {method} returned neither result nor error"))
    }

    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        let v = self.call("workspace.list", serde_json::json!({}))?;
        Ok(serde_json::from_value(v["workspaces"].clone())?)
    }

    pub fn agents(&self) -> Result<Vec<Agent>> {
        let v = self.call("agent.list", serde_json::json!({}))?;
        Ok(serde_json::from_value(v["agents"].clone())?)
    }

    pub fn focus_workspace(&self, workspace_id: &str) -> Result<()> {
        self.call(
            "workspace.focus",
            serde_json::json!({ "workspace_id": workspace_id }),
        )?;
        Ok(())
    }

    /// Reorder a workspace. `insert_index` is the destination slot in the
    /// sidebar's workspace array.
    ///
    /// This method has no CLI equivalent — `herdr workspace` exposes only
    /// list/create/get/focus/rename/report-metadata/close — so the socket is the
    /// only way to reorder without dragging rows with the mouse.
    pub fn move_workspace(&self, workspace_id: &str, insert_index: usize) -> Result<()> {
        self.call(
            "workspace.move",
            serde_json::json!({ "workspace_id": workspace_id, "insert_index": insert_index }),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_status_the_schema_declares() {
        for (s, want) in [
            ("idle", Status::Idle),
            ("working", Status::Working),
            ("blocked", Status::Blocked),
            ("done", Status::Done),
            ("unknown", Status::Unknown),
        ] {
            assert_eq!(Status::parse(s), want, "parsing {s}");
        }
    }

    #[test]
    fn unrecognised_status_degrades_to_unknown() {
        assert_eq!(Status::parse("teleporting"), Status::Unknown);
        assert_eq!(Status::parse(""), Status::Unknown);
    }

    #[test]
    fn attention_order_puts_stalled_agents_first() {
        let mut v = vec![Status::Idle, Status::Blocked, Status::Working, Status::Done];
        v.sort_by_key(|s| s.priority());
        assert_eq!(
            v,
            vec![Status::Blocked, Status::Done, Status::Working, Status::Idle]
        );
    }

    #[test]
    fn strips_agent_spinner_frames() {
        assert_eq!(
            clean_title("◑ Sync chezmoi configs"),
            "Sync chezmoi configs"
        );
        assert_eq!(
            clean_title("✳ Find unreviewed processors"),
            "Find unreviewed processors"
        );
        assert_eq!(clean_title("Already clean"), "Already clean");
    }

    #[test]
    fn leaves_titles_that_are_only_decoration_empty() {
        // Callers fall back to the agent name when this returns empty.
        assert_eq!(clean_title("◑◒◓"), "");
        assert_eq!(clean_title(""), "");
    }
}
