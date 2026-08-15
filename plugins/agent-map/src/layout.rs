//! Radial placement of agent nodes around their repository group.
//!
//! Adapted from ClawTab's `useRecencyLayout`, with two changes forced by the
//! target being a character grid rather than a zoomable canvas:
//!
//!   * Cells are ~2x taller than wide, so every vertical distance is halved.
//!     Without this correction the "circles" render as tall ellipses.
//!   * There is no pan or zoom to escape a collision, so the whole layout is
//!     scaled to fit the viewport and overlapping nodes step outward through
//!     discrete ring slots instead of being nudged by a few pixels.
//!
//! Recency comes from `state_change_seq` rather than a timestamp — herdr exposes
//! no wall-clock time for agent activity — so it is a *rank* within the current
//! set, not an absolute age. An agent that has never changed state since the
//! server started sorts oldest.

use herdr_api::{Agent, Status};

/// Character cells are roughly twice as tall as they are wide.
const CELL_ASPECT: f64 = 0.5;

const AGENT_W_MIN: u16 = 16;
const AGENT_W_MAX: u16 = 30;
const AGENT_H: u16 = 4;
const GROUP_H: u16 = 3;

/// Orbit radius for a group's agents, in character columns. Kept tight — the
/// vertical component is halved by CELL_ASPECT, so a large value reads as a
/// generous horizontal gap but an enormous vertical one, and agents visually
/// detach from the group they belong to.
const ORBIT_BASE: f64 = 15.0;
/// Added per extra agent so a busy repo's ring does not self-intersect.
const ORBIT_PER_AGENT: f64 = 2.2;
/// Gap enforced between any two boxes.
const CLEARANCE: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    fn overlaps(&self, o: &Rect, pad: u16) -> bool {
        let (ax0, ay0) = (self.x.saturating_sub(pad), self.y.saturating_sub(pad));
        let (ax1, ay1) = (self.x + self.w + pad, self.y + self.h + pad);
        let (bx0, by0) = (o.x, o.y);
        let (bx1, by1) = (o.x + o.w, o.y + o.h);
        ax0 < bx1 && bx0 < ax1 && ay0 < by1 && by0 < ay1
    }

    pub fn center(&self) -> (u16, u16) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

pub struct GroupNode {
    pub label: String,
    pub rect: Rect,
    pub agent_count: usize,
    /// Worst status among the group's agents, so a collapsed group still shows
    /// that something inside it needs attention.
    pub status: Status,
}

pub struct AgentNode {
    pub rect: Rect,
    pub title: String,
    pub workspace: String,
    pub status: Status,
    /// 0.0 = least recently active in this set, 1.0 = most recent.
    pub recency: f64,
    pub focused: bool,
    /// Index of the owning group, for drawing the connector.
    pub group: usize,
}

pub struct Map {
    pub groups: Vec<GroupNode>,
    pub agents: Vec<AgentNode>,
}

/// One repository (or one standalone workspace) and the agents inside it.
pub struct Group {
    pub label: String,
    pub agents: Vec<Agent>,
}

/// Bucket agents by repository, falling back to the workspace label for
/// workspaces with no git worktree attached.
pub fn group_agents(
    agents: &[herdr_api::Agent],
    workspaces: &[herdr_api::Workspace],
) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut keys: Vec<String> = Vec::new();

    for a in agents {
        let ws = workspaces.iter().find(|w| w.workspace_id == a.workspace_id);
        // repo_key groups a repo's main checkout together with each of its linked
        // worktrees, which is exactly the clustering that makes the map useful:
        // "everything I have running against rating-processors".
        let (key, label) = match ws.and_then(|w| w.worktree.as_ref()) {
            Some(wt) => (wt.repo_key.clone(), wt.repo_name.clone()),
            None => {
                let l = ws.map(|w| w.label.clone()).unwrap_or_else(|| "—".into());
                (format!("ws:{}", a.workspace_id), l)
            }
        };
        match keys.iter().position(|k| *k == key) {
            Some(i) => groups[i].agents.push(a.clone()),
            None => {
                keys.push(key);
                groups.push(Group {
                    label,
                    agents: vec![a.clone()],
                });
            }
        }
    }
    // Busiest repos first so they claim the roomiest slots.
    groups.sort_by_key(|g| std::cmp::Reverse(g.agents.len()));
    groups
}

fn lerp_u16(a: u16, b: u16, t: f64) -> u16 {
    a + ((b - a) as f64 * t.clamp(0.0, 1.0)).round() as u16
}

/// Place a box centred on (cx, cy), clamped inside the viewport.
fn centred(cx: f64, cy: f64, w: u16, h: u16, area: Rect) -> Rect {
    let x = (cx - w as f64 / 2.0).round();
    let y = (cy - h as f64 / 2.0).round();
    let max_x = area.x + area.w.saturating_sub(w);
    let max_y = area.y + area.h.saturating_sub(h);
    Rect {
        x: (x.max(area.x as f64) as u16).min(max_x),
        y: (y.max(area.y as f64) as u16).min(max_y),
        w,
        h,
    }
}

/// Walk outward through ring slots until the box clears everything already
/// placed. Mirrors ClawTab's overlap search, but stepping by whole rings because
/// a character grid cannot express sub-cell nudges.
fn place_avoiding(
    centre: (f64, f64),
    radius: f64,
    angle: f64,
    size: (u16, u16),
    area: Rect,
    taken: &[Rect],
) -> Rect {
    let (cx, cy) = centre;
    let (w, h) = size;
    for ring in 0..6 {
        let r = radius + ring as f64 * (h as f64 + 2.0);
        for step in 0..8 {
            // Alternate either side of the ideal angle so nodes stay near their
            // intended bearing rather than all drifting one way.
            let delta = (step as f64 / 8.0) * std::f64::consts::TAU / 6.0;
            for sign in [1.0, -1.0] {
                let a = angle + sign * delta;
                let px = cx + r * a.cos();
                let py = cy + r * a.sin() * CELL_ASPECT;
                let rect = centred(px, py, w, h, area);
                if !taken.iter().any(|t| rect.overlaps(t, CLEARANCE)) {
                    return rect;
                }
            }
        }
    }
    centred(cx, cy, w, h, area)
}

pub fn build(groups: &[Group], area: Rect) -> Map {
    let cx = area.x as f64 + area.w as f64 / 2.0;
    let cy = area.y as f64 + area.h as f64 / 2.0;

    // Recency is a rank across every agent on screen, not per group, so sizes
    // stay comparable between clusters.
    let seqs: Vec<u64> = groups
        .iter()
        .flat_map(|g| g.agents.iter().map(|a| a.state_change_seq))
        .collect();
    let lo = seqs.iter().copied().min().unwrap_or(0);
    let hi = seqs.iter().copied().max().unwrap_or(0);
    let span = (hi - lo) as f64;

    let mut out_groups = Vec::new();
    let mut out_agents = Vec::new();
    let mut taken: Vec<Rect> = Vec::new();

    // Groups sit on their own ring around the viewport centre; a lone group takes
    // the centre itself.
    let gr = if groups.len() <= 1 {
        0.0
    } else {
        let by_w = area.w as f64 * 0.26;
        let by_h = area.h as f64 * 0.26 / CELL_ASPECT;
        by_w.min(by_h).max(14.0)
    };

    for (gi, g) in groups.iter().enumerate() {
        let ga = if groups.len() <= 1 {
            0.0
        } else {
            // Start at 0° (due east) rather than the top. Terminals are far wider
            // than they are tall, and starting at -90° puts the common two-group
            // case in a vertical stack that wastes the entire width.
            (gi as f64 / groups.len() as f64) * std::f64::consts::TAU
        };
        let gcx = cx + gr * ga.cos();
        let gcy = cy + gr * ga.sin() * CELL_ASPECT;

        let worst = g
            .agents
            .iter()
            .map(|a| Status::parse(&a.agent_status))
            .min_by_key(|s| s.priority())
            .unwrap_or(Status::Unknown);

        let gw = (g.label.chars().count() as u16 + 6).clamp(10, 34);
        let grect = centred(gcx, gcy, gw, GROUP_H, area);
        taken.push(grect);
        out_groups.push(GroupNode {
            label: g.label.clone(),
            rect: grect,
            agent_count: g.agents.len(),
            status: worst,
        });

        let orbit = ORBIT_BASE + g.agents.len() as f64 * ORBIT_PER_AGENT;
        for (ai, a) in g.agents.iter().enumerate() {
            let recency = if span > 0.0 {
                (a.state_change_seq - lo) as f64 / span
            } else {
                1.0
            };
            let w = lerp_u16(AGENT_W_MIN, AGENT_W_MAX, recency);
            let angle = -std::f64::consts::FRAC_PI_2
                + (ai as f64 / g.agents.len().max(1) as f64) * std::f64::consts::TAU;
            let rect = place_avoiding((gcx, gcy), orbit, angle, (w, AGENT_H), area, &taken);
            taken.push(rect);

            let title = a
                .terminal_title_stripped
                .as_deref()
                .map(herdr_api::clean_title)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| a.agent.clone());

            out_agents.push(AgentNode {
                rect,
                title,
                workspace: a.workspace_id.clone(),
                status: Status::parse(&a.agent_status),
                recency,
                focused: a.focused,
                group: gi,
            });
        }
    }

    Map {
        groups: out_groups,
        agents: out_agents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 120,
            h: 40,
        }
    }

    fn agent(pane: &str, ws: &str, seq: u64, status: &str) -> Agent {
        Agent {
            pane_id: pane.into(),
            workspace_id: ws.into(),
            agent: "claude".into(),
            terminal_title_stripped: Some(format!("task {pane}")),
            agent_status: status.into(),
            focused: false,
            state_change_seq: seq,
            cwd: "/tmp".into(),
        }
    }

    fn workspace(id: &str, repo: Option<&str>) -> herdr_api::Workspace {
        herdr_api::Workspace {
            workspace_id: id.into(),
            number: 1,
            label: format!("ws-{id}"),
            focused: false,
            pane_count: 1,
            tab_count: 1,
            agent_status: "idle".into(),
            worktree: repo.map(|r| herdr_api::Worktree {
                repo_key: format!("/repos/{r}/.git"),
                repo_name: r.into(),
                repo_root: format!("/repos/{r}"),
                checkout_path: format!("/repos/{r}"),
                is_linked_worktree: false,
            }),
        }
    }

    #[test]
    fn workspaces_sharing_a_repo_collapse_into_one_group() {
        let ws = vec![
            workspace("w1", Some("alpha")),
            workspace("w2", Some("alpha")),
            workspace("w3", None),
        ];
        let agents = vec![
            agent("w1:p1", "w1", 1, "idle"),
            agent("w2:p1", "w2", 2, "working"),
            agent("w3:p1", "w3", 3, "idle"),
        ];
        let groups = group_agents(&agents, &ws);
        assert_eq!(groups.len(), 2, "two repo groups expected");
        // Sorted by size, so the shared repo leads with both of its agents.
        assert_eq!(groups[0].label, "alpha");
        assert_eq!(groups[0].agents.len(), 2);
    }

    #[test]
    fn nodes_stay_inside_the_viewport() {
        let ws: Vec<_> = (1..=6).map(|i| workspace(&format!("w{i}"), None)).collect();
        let agents: Vec<_> = (1..=6)
            .map(|i| agent(&format!("w{i}:p1"), &format!("w{i}"), i, "working"))
            .collect();
        let map = build(&group_agents(&agents, &ws), area());
        let a = area();
        for n in &map.agents {
            assert!(
                n.rect.x + n.rect.w <= a.w && n.rect.y + n.rect.h <= a.h,
                "agent node {:?} escaped the viewport",
                n.rect
            );
        }
        for g in &map.groups {
            assert!(g.rect.x + g.rect.w <= a.w && g.rect.y + g.rect.h <= a.h);
        }
    }

    #[test]
    fn agent_nodes_do_not_overlap_each_other() {
        let ws: Vec<_> = (1..=5).map(|i| workspace(&format!("w{i}"), None)).collect();
        let agents: Vec<_> = (1..=5)
            .map(|i| agent(&format!("w{i}:p1"), &format!("w{i}"), i, "idle"))
            .collect();
        let map = build(&group_agents(&agents, &ws), area());
        for (i, a) in map.agents.iter().enumerate() {
            for b in map.agents.iter().skip(i + 1) {
                assert!(
                    !a.rect.overlaps(&b.rect, 0),
                    "{:?} overlaps {:?}",
                    a.rect,
                    b.rect
                );
            }
        }
    }

    #[test]
    fn recency_spans_the_full_range_and_drives_width() {
        let ws: Vec<_> = (1..=3).map(|i| workspace(&format!("w{i}"), None)).collect();
        let agents = vec![
            agent("w1:p1", "w1", 10, "idle"),
            agent("w2:p1", "w2", 50, "idle"),
            agent("w3:p1", "w3", 90, "idle"),
        ];
        let map = build(&group_agents(&agents, &ws), area());
        let oldest = map.agents.iter().find(|a| a.workspace == "w1").unwrap();
        let newest = map.agents.iter().find(|a| a.workspace == "w3").unwrap();
        assert_eq!(oldest.recency, 0.0);
        assert_eq!(newest.recency, 1.0);
        assert!(newest.rect.w > oldest.rect.w, "recent agents render wider");
    }

    #[test]
    fn a_single_agent_still_lays_out() {
        let ws = vec![workspace("w1", None)];
        let agents = vec![agent("w1:p1", "w1", 0, "idle")];
        let map = build(&group_agents(&agents, &ws), area());
        assert_eq!(map.groups.len(), 1);
        assert_eq!(map.agents.len(), 1);
        // A lone agent has no span to rank against, so it reads as fully recent.
        assert_eq!(map.agents[0].recency, 1.0);
    }

    #[test]
    fn group_shows_the_most_urgent_status_inside_it() {
        let ws = vec![
            workspace("w1", Some("alpha")),
            workspace("w2", Some("alpha")),
        ];
        let agents = vec![
            agent("w1:p1", "w1", 1, "idle"),
            agent("w2:p1", "w2", 2, "blocked"),
        ];
        let map = build(&group_agents(&agents, &ws), area());
        assert_eq!(map.groups[0].status, Status::Blocked);
    }
}
