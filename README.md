# herdr-plugins

Plugins for [herdr](https://herdr.dev), written in Rust. A cargo workspace: each
plugin is its own crate under `plugins/`, sharing a small socket-API client in
`crates/herdr-api`.

Requires herdr **0.8.0+** (the plugin system and the `workspace.move` /
`agent.view.set` methods do not exist before it).

| Plugin | What it does |
| --- | --- |
| [`agent-map`](plugins/agent-map) | Radial overview of every running agent, grouped by repository |
| [`workspace-order`](plugins/workspace-order) | Reorder workspaces from the keyboard |

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
herdr plugin install pdjsh/herdr-plugins/plugins/workspace-order
```

`herdr plugin install` only understands GitHub, so from the Codeberg canonical
copy, clone and link instead:

```sh
git clone https://codeberg.org/pdjsh/herdr-plugins ~/Projects/herdr-plugins
herdr plugin link ~/Projects/herdr-plugins/plugins/agent-map
herdr plugin link ~/Projects/herdr-plugins/plugins/workspace-order
```

Then enable and open:

```sh
herdr plugin enable agent-map
herdr plugin pane open agent-map map          # popup
herdr plugin pane open agent-map map-tab      # persistent tab
```

Each plugin builds into its own `target/` (via `--target-dir ./target`) rather
than the workspace's shared one, because manifest commands resolve relative to the
plugin root.

## Development

```sh
cargo test                    # unit tests, no herdr server needed
cargo run -p agent-map -- --once
```

`crates/herdr-api` resolves the socket from `HERDR_SOCKET_PATH` — which herdr sets
for every plugin process — falling back to `~/.config/herdr/herdr.sock` so the
binaries stay runnable outside a plugin context.

## Licence

MIT.
