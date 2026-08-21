# herdr-plugins

Plugins for [herdr](https://herdr.dev), written in Rust. A cargo workspace: each
plugin is its own crate under `plugins/`, sharing a small socket-API client in
`crates/herdr-api`.

Requires herdr **0.8.0+** (the plugin system and the `workspace.move` /
`agent.view.set` methods do not exist before it).

| Plugin | What it does |
| --- | --- |
| [`agent-map`](plugins/agent-map) | Radial overview of every running agent, grouped by repository |
| [`daybook`](plugins/daybook) | The morning read: what moved and what is still open, across every repo and agent |
| [`workspace-order`](plugins/workspace-order) | Reorder workspaces from the keyboard |

`daybook` also ships a standalone collector, [`tools/daybook`](tools/daybook),
which the plugin and a Claude Code skill both read from.

## agent-map

A read-only map of what every agent is doing. Repositories are the anchor nodes;
their agents orbit them. Agents that changed state most recently render wider and
brighter, so the map answers "what moved while I was away" at a glance.

```
   ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━┓     ╭──────────────╮
   ┃✳ w3                       ┃     │⚠ w2          │
   ┃Sync chezmoi and herdr con…┃     │Find unreview…│
   ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━┛     ╰──────────────╯
                 ·                           ·
           ╔═══════════╗          ╔═════════════════════╗
           ║ .config 1 ║          ║ rating-processors 2 ║
           ╚═══════════╝          ╚═════════════════════╝
                                             ·
                              ╭────────────────────────────╮
                              │● w4                        │
                              │Structure protocol rating w…│
                              ╰────────────────────────────╯
 3 agents ⚠ 1 ● 1 ✳ 1 ✓ 0 · updated 1s ago — q close, r refresh
```

Grouping is by `worktree.repo_key`, so a repository's main checkout and all of its
linked worktrees collapse into one node — `rating-processors` above holds both a
normal workspace and a worktree workspace. Workspaces with no git worktree stand
alone under their own label.

Keys: `q`/`Esc` close, `r` refresh. It refreshes itself every 2s.

**It deliberately cannot jump to an agent yet.** The interaction model is still
undecided, and shipping a guess would build muscle memory that has to be unlearnt.
`--once` prints a single plain-text frame instead of opening a TUI.

### Layout

Adapted from [ClawTab](https://github.com/tonisives/clawtab)'s `useRecencyLayout`,
with two changes forced by a character grid:

- Cells are ~2× taller than wide, so vertical distances are halved. Without that
  correction the orbits render as tall ellipses.
- There is no pan or zoom, so the layout is clamped to the viewport and colliding
  nodes step outward through discrete ring slots rather than being nudged.

Recency uses `state_change_seq`, a monotonic counter — herdr exposes no wall-clock
timestamp for agent activity. It is therefore a *rank within the current set*, not
an age: with one agent on screen, that agent always reads as fully recent.

## daybook

Open it with the first coffee. The left column is everything waiting on you,
ranked; the right is what moved while you were away and what is running now.

```
 daybook · since yesterday   22 commits in 3 repos · 7 sessions · 5 PRs open
┌ open loops 9 ──────────────────────────────────────────┐┌ moved 3 ───────────────────────────┐
│!! api-gateway#88 has failing checks: build, e2e        ││api-gateway 14●                     │
│     → gh pr checks 88 -R acme/api-gateway              ││  feat: retry budget per upstream    │
│!! ledger#12 has merge conflicts                        ││ledger 7● 2✎                        │
│ ! w8:p1 finished and is unread — index backfill        ││  main 2✎                           │
│ ! session ended on your prompt — Tab switching         ││dotfiles 1✎                         │
│ · ledger [main]: 2 uncommitted file(s)                 │└────────────────────────────────────┘
│ · ledger [feature/backfill]: branch exists only here   │┌ agents 4 ──────────────────────────┐
│ · api-gateway#84 has not moved in 8 days               ││● w8:p1 index backfill              │
│   3 open PR(s) dormant over 45 days: …                 ││✳ w3:p1 retry budget                │
└────────────────────────────────────────────────────────┘└────────────────────────────────────┘
 q close  r refresh  j/k move  a all  n offline · 5s ago
```

Four bands, and the colours are agent-map's, so a blocked agent is the same red
everywhere: **`!!`** stuck (red), **`!`** waiting on you (orange), **`·`** a loose
end (yellow), blank housekeeping (grey). `j`/`k` moves the cursor and expands that
row's suggested next move; only the selected row expands, because a list where
every entry is two lines tall stops being scannable at about six entries.

Keys: `j`/`k` move, `d`/`u` half-page, `g`/`G` ends, `a` show or hide the
housekeeping band, `n` toggle offline, `r` refresh, `q`/`Esc` close.

**It deliberately cannot act on anything.** No jumping to a pane, no `gh pr
merge`, no `git push`. Deciding what to act on is the whole job; acting is what
the rest of herdr is for.

### Where the data comes from

The pane shells out to `tools/daybook/daybook-collect.py`, which is also what the
`/standup` Claude Code skill runs. One collector, two renderers — a TUI and a
synthesised briefing — so the pane and the briefing can never disagree about what
happened. It gathers four sources, each degrading on its own:

| Source | Answers |
| --- | --- |
| `git` | commits in the window, dirty checkouts, unpushed branches, drift from base |
| `gh` | your open PRs with CI and review state, reviews requested of you, what merged |
| `~/.claude/projects` | which Claude Code sessions ran, what they were about, which ended on an unanswered prompt |
| herdr socket | the live agent roster |

Commits are counted per **repository**, not per checkout: `git log --all` walks the
shared object store, so asking each linked worktree separately reported a repo's
work once per worktree. Dirt and unpushed commits stay per checkout, because only
the branch name tells you which one to go to.

The window is not "yesterday". It opens at the start of the most recent *earlier*
day with any activity, and says how far back that was — on a Monday that is
Friday, and a briefing that silently skips a weekend is worse than one that says
"since Fri 15 Aug".

### Refresh cadence

Two cadences, because the sources age at very different rates. Every 30s the pane
re-reads git, transcripts and the agent roster offline (under a second over a
dozen repositories). Every 5 minutes it also re-reads pull requests, which cost a
dozen network round trips. In between, the last known PR state is carried forward
rather than blanked — the alternative is PRs blinking out of the list every half
minute. `r` forces a full refresh; `n` pins it offline.

The offline pass is run with `--no-cache`, because the cache is shared with
`/standup`: a document with empty pull-request lists must never be what a briefing
reads. For the same reason `--max-age` will not serve an offline-collected
document to a caller that wanted the network, nor a differently-windowed one to a
caller that passed `--since`.

### Using the collector on its own

```sh
daybook-collect              # JSON on stdout
daybook-collect --text       # a plain-text frame
daybook-collect --no-net     # skip gh entirely
daybook-collect --since 2026-08-18
daybook-collect --max-age 300 # reuse the cache if it is fresher than this
```

It is stdlib-only Python 3.11+ with no install step, because the pane shells out
to it and a virtualenv would make the pane fail in ways a TUI cannot explain.
Override any default in `~/.config/daybook/config.toml`:

```toml
repo_roots = ["~/Projects", "~/work"]
extra_repos = ["~/.local/share/chezmoi"]
identities = ["me@example.com"]   # beyond what git config reports
pr_stale_days = 7
pr_dormant_days = 45
```

`daybook --where` prints which collector the pane resolved, which is the first
thing worth knowing when a pane comes up empty.

## workspace-order

Sidebar order is workspace creation order, and under `agent_panel_sort = "spaces"`
that order is what `alt+1..9` and `alt+j/k` aim at. herdr 0.8.0 can change it via
`workspace.move { workspace_id, insert_index }`, but ships **no keybinding and no
CLI subcommand** for it — `herdr workspace` offers only
list/create/get/focus/rename/report-metadata/close. Dragging rows with the mouse
was the only way. This closes that gap.

Keys: `j`/`k` move the cursor, `J`/`K` move the selected workspace, `Enter`
focuses and exits, `q` exits.

Note that rename and close *do* already have bindings — `prefix+shift+w` and
`prefix+shift+d` — so this plugin deliberately does not reimplement them.

## Install

From GitHub:

```sh
herdr plugin install pdjsh/herdr-plugins/plugins/agent-map
herdr plugin install pdjsh/herdr-plugins/plugins/daybook
herdr plugin install pdjsh/herdr-plugins/plugins/workspace-order
```

`herdr plugin install` only understands GitHub, so from the Codeberg canonical
copy, clone and link instead:

```sh
git clone https://codeberg.org/pdjsh/herdr-plugins ~/Projects/herdr-plugins
herdr plugin link ~/Projects/herdr-plugins/plugins/agent-map
herdr plugin link ~/Projects/herdr-plugins/plugins/daybook
herdr plugin link ~/Projects/herdr-plugins/plugins/workspace-order
```

`daybook` additionally wants its collector on `PATH`, so the pane and the
`/standup` skill find the same one:

```sh
ln -sfn ~/Projects/herdr-plugins/tools/daybook/daybook-collect.py ~/.local/bin/daybook-collect
```

Then enable and open:

```sh
herdr plugin enable agent-map
herdr plugin pane open --plugin agent-map --entrypoint map       # popup
herdr plugin pane open --plugin agent-map --entrypoint map-tab   # persistent tab

herdr plugin enable daybook
herdr plugin pane open --plugin daybook --entrypoint brief
herdr plugin pane open --plugin daybook --entrypoint brief-tab
```

`herdr plugin pane open` takes named flags, not positional arguments — a
positional plugin id fails with `unknown option`. Close a popup with `q` from
inside it: as of 0.8.0 an overlay pane is not addressable by
`herdr plugin pane close`, which only finds `tab`-placed ones.

Each plugin builds into its own `target/` (via `--target-dir ./target`) rather
than the workspace's shared one, because manifest commands resolve relative to the
plugin root.

## Development

```sh
cargo test                    # unit tests, no herdr server needed
cargo run -p agent-map -- --once
cargo run -p daybook -- --once

python3 -m unittest discover -s tools/daybook   # the collector's own tests
```

`crates/herdr-api` resolves the socket from `HERDR_SOCKET_PATH` — which herdr sets
for every plugin process — falling back to `~/.config/herdr/herdr.sock` so the
binaries stay runnable outside a plugin context.

## Licence

MIT.
