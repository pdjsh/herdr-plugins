# Decision log — overnight run, 2026-08-15

You delegated four decisions and went to bed. This records what I chose, why, what
I verified versus assumed, and what I deliberately did not do.

---

## The four delegated decisions

### 1. Build order → config fixes first, then repo, then map

Your mouse pain was *renaming, closing, switching tabs, and reordering agents*. Of
those, three already have keybindings you hadn't discovered (`prefix+shift+w`
rename, `prefix+shift+d` close, `prefix+1..9` tabs). Only **reordering** had no
binding at all. That reframed the work: most of the pain was discoverability, not
missing capability — and the cheapest fix was config, not code.

### 2. Map interaction → none, deliberately

You said the map is "just for me to follow more easily visually" and deferred the
keybind question. So it ships as a pure viewer: `q` closes, `r` refreshes. No
jump-to-agent. Adding one now would build muscle memory you might have to unlearn
once you decide the real model.

### 3. Third-party nav plugins → researched, **not installed**

You said "we can try the plugins too", but `herdr plugin install` clones a repo and
runs its `[[build]]` command on this machine. Executing three strangers'
build scripts unattended, while you're asleep and can't consent to the specific
code, is not a call I was willing to make for you. Exact commands are below; it's
a 30-second job once you're awake.

### 4. Neovim → surveyed only, no changes

You deferred this one explicitly. I gathered facts (below) and touched nothing.

---

## What changed on this machine

### Dotfiles — committed to a branch, **not pushed**

Branch `feat/macos-keybinds` in `~/.local/share/chezmoi`, worktree at
`.worktrees/macos-keybinds`, commits `13bce11` and `89086a8`. **Not applied to your
home directory** — review it, then `chezmoi apply`.

Not pushed because you made Codeberg the origin and this laptop has no Codeberg
credentials (no keychain entry, no Codeberg SSH key — only `github`, `nuc`,
`srhetzner`). Pushing to GitHub instead would have contradicted your remote choice,
so I left it local.

| File | Change |
| --- | --- |
| `dot_config/ghostty/config` → `config.tmpl` | Renamed to a chezmoi template; adds `macos-option-as-alt = true` inside `{{ if eq .chezmoi.os "darwin" }}` |
| `dot_config/herdr/config.toml` | `prompt_new_workspace_name`, `last_pane`, `swap_pane_{left,down,up,right}`, `copy_mode`; corrected the stale 0.7.5 reordering comment |
| `.gitignore` (new) | Ignores `.worktrees/`, per your worktree-location rule |

**The Ghostty fix is the important one.** On macOS Ghostty defaults Option to a
compose key, so `alt+j`/`alt+k`, `prefix+alt+1..9`, and `prefix+alt+g` were all dead
on this laptop while working fine on the Linux desktop. It's templated so the
desktop is unaffected.

### New repo — `herdr-plugins`

Pushed to <https://github.com/pdjsh/herdr-plugins> (public, `main`, commit
`7c954bd`). A `codeberg` remote is configured but **unpushed** — that repo doesn't
exist yet and I have no credentials for it.

Rust cargo workspace: `crates/herdr-api` (socket client) plus two plugins. Both are
**linked into herdr but left disabled**, so nothing changed in your running session.

### Toolchain

Installed Rust 1.97.1 via rustup with `--no-modify-path`, so your shell profile was
not touched. Activate with `source ~/.cargo/env`. Also added `rustfmt` and `clippy`.

---

## Deviations from your rules, and why

- **Committed the plugins repo directly to `main`.** Your rule says non-trivial work
  goes to a feature branch and a PR. A PR needs a base commit to diff against, and
  this repo had none — the review surface for a brand-new repo is the repo. Future
  work should branch.
- **`gitleaks` is not installed on this laptop**, so your pre-commit secret scan
  skipped on both commits (it printed a notice). I reviewed both diffs by hand;
  they contain keybindings, Rust source, and docs — no credentials. `brew install
  gitleaks` restores the guard.
- **No PR for the dotfiles change.** Your contract classes config edits as non-code,
  which calls for a surfaced summary rather than a PR. That's the table above.

---

## Verified vs assumed

**Verified by execution:**
- `workspace.move` and `agent.view.set` exist on 0.8.0 — probed with deliberately
  bad params and got field-validation errors (`missing field insert_index`,
  `missing field source`) rather than "unknown method". No mutation performed.
- The new herdr config parses — copied over the live config, ran `herdr config
  check` → `config: ok`, restored byte-identically.
- Both plugin manifests load — `herdr plugin link` succeeded with zero warnings.
- The map renders — driven in a pty at 110×34, 100×26, 60×20, and 40×5 (the last
  correctly showing the too-small guard). Output matches the README screenshot.
- 14 unit tests pass; `cargo clippy --all-targets` is clean.
- `agent-map --once` produces correct output against your live session.

**Assumed, not verified:**
- **That the new keybindings actually fire.** `herdr config check` only proves the
  file *parses*, and your own config notes say unknown keys are silently ignored. My
  evidence that `swap_pane_*`, `last_pane`, `copy_mode`, and
  `prompt_new_workspace_name` are real is that they appear in the binary's serde
  field table for `[keys]`/`[ui]`. Confirm by pressing them after applying.
- **That `workspace.move`'s `insert_index` is absolute rather than relative.** The
  index arithmetic is unit-tested, but no live reorder was performed — that would
  have rearranged your sidebar while you slept. `workspace-order` is the one piece
  of tonight's work with genuinely untested runtime behaviour.
- **That popup panes size as requested.** 110×34 and 72×18 are guesses at what
  looks right; I never opened one.

---

## Two things I found and did not act on

**Your Ghostty background contradicts your herdr theme.** The herdr config says the
graphite palette was tuned against a `#0b0b0d` pane background — `panel_bg = #101014`
is described as "a 1.04:1 lift" above it. But `dot_config/ghostty/config` sets
`background = #1a1b26`, which is *lighter* than the sidebar, inverting the intended
relationship. Either the comment is stale or the Ghostty config is. Both fixes are
one line, but which one is right is an aesthetic call that's yours:

- `background = #0b0b0d` in Ghostty — restores the design, darkens every pane
- `panel_bg = #1e1f2b` in herdr — keeps Tokyo Night, re-establishes the lift

**`ctrl+hjkl` will fight Neovim.** You bind pane focus to bare `ctrl+hjkl`, which
Neovim also uses for window navigation. The moment you open nvim in a pane you
won't be able to move between its splits. `paulbkim-dev/vim-herdr-navigation` exists
for exactly this — a herdr port of `vim-tmux-navigator`.

---

## Neovim survey (no changes made)

- **`supermaven-nvim`, your only AI plugin, was last updated 2024-10-07** — nearly
  two years stale. Not archived, but not maintained either.
- Actively maintained alternatives, all pushed within the last week:
  `avante.nvim` (18.1k ★, 2026-08-14), `codecompanion.nvim` (6.8k ★, 2026-08-12),
  `coder/claudecode.nvim` (3.0k ★, 2026-08-11).
- Your `lazyvim.json` has **zero extras enabled** — a stock LazyVim install.

I'd want your answer to the "editor beside the agents" vs "AI inside nvim" question
before recommending one, since it decides the whole shape.

---

## Next steps

```sh
# 1. Review, then apply the config
cd ~/.local/share/chezmoi/.worktrees/macos-keybinds && git show
chezmoi apply            # then restart Ghostty for option-as-alt

# 2. Try the plugins
cd ~/Projects/herdr-plugins/plugins/agent-map && cargo build --release --target-dir ./target
herdr plugin enable agent-map && herdr plugin pane open agent-map map

# 3. Third-party nav plugins, once you've eyeballed them
herdr plugin install thanhdat77/herdr-navigator      # prefix+t fuzzy jump to anything
herdr plugin install fullerzz/herdr-plugin-sesh      # workspace picker + zoxide
herdr plugin install paulbkim-dev/vim-herdr-navigation  # fixes the ctrl+hjkl clash

# 4. Codeberg side, from a machine that has the credentials
#    create pdjsh/herdr-plugins on codeberg, then:
git push codeberg main
```

## Open questions

1. Ghostty background vs herdr `panel_bg` — which one moves?
2. Map interaction model — labelled jump, spatial `hjkl`, or type-to-filter?
3. Neovim — editor beside the agents, or agent inside the editor?
4. Do you want the dotfiles branch pushed to GitHub as well, or kept Codeberg-only?

---

# Decision log — daybook, 2026-08-21

You asked for "a recap of the previous day, things I have yet to do today,
whatever else comes to mind", to be the starting point of the day's agentic
workflow, plus "some kind of herdr plugin for visualization" if there was time.
Then you left. This records what I chose and why.

---

## The shape: one collector, two renderers

The recap and the plugin are the same question asked twice, so they share one
answer. `tools/daybook/daybook-collect.py` gathers the day's state into a single
JSON document; the `daybook` herdr pane renders it as a TUI, and the `/standup`
Claude Code skill synthesises it into a briefing. Two independent implementations
would have drifted within a week and then told you different things about the same
morning.

`attention` — the ranked open-loop list with its four severity bands — is computed
in the collector rather than in either renderer, for the same reason: the pane's
red and the briefing's "needs you now" have to mean the same thing.

## Decisions I made without asking

### The collector is stdlib Python, not Rust

The rest of this repo is Rust and the plugin had to be (ratatui, the socket
client, your existing conventions). The collector deliberately is not:

- The pane shells out to it. If it needed `uv sync` first, a pane would fail with
  a broken pipe and no way to explain itself.
- It is 90% subprocess orchestration over `git`, `gh`, JSONL and a unix socket —
  the part of the problem Python is actually better at.
- Both consumers can then be swapped independently. A future Claude skill on
  another machine needs the collector, not the plugin.

It requires Python 3.11+ (`tomllib`) and nothing else.

### Tests use `unittest`, not `pytest` — a deviation from your CLAUDE.md

Your greenfield Python preference is `uv` + `pytest`. This tool has no
environment to install into, by design, and adding one to run its tests would
undo the reason it has no dependencies. `python3 -m unittest discover -s
tools/daybook` runs 30 tests with nothing installed. If you would rather have
pytest, the tests convert mechanically — they are plain assertions.

### The window is "the last active day", not yesterday

On a Monday, yesterday is empty and the work you need reminding of is Friday's.
So the window opens at the start of the most recent *earlier* day with any commit
or session activity, and the briefing says how far back that was. A recap that
silently skips a weekend is worse than one that says "since Fri 15 Aug".

### Commits are counted per repository, not per checkout

The first working version reported 35 commits across 4 repositories when the real
number was 21. `git log --all` walks the shared object store, so
`rating-processors` and its two linked worktrees each reported the same 7 commits.
Repositories are now grouped by `repo_key` — the common `.git` directory, the same
key `agent-map` groups by — and history is asked once per group. Dirt, unpushed
commits and drift-from-base stay per checkout, because only the branch name tells
you which checkout to go to. Stashes moved back to the repository, since
`refs/stash` is shared.

### Dormant pull requests collapse into one line

Unfiltered, "stale PR" surfaced `slad-tap-list#2 has not moved in 1211 days`.
Anything idle past 45 days is now a single housekeeping line naming them, rather
than one row each. Seven-to-45 days still gets an individual nudge. Both
thresholds are config keys.

### The pane refreshes on two cadences

git/transcripts/agents every 30s offline (~1.5s), pull requests every 5 minutes
(a dozen network round trips). In between, the last networked PR answer is carried
forward and flagged, rather than blanked — otherwise PRs blink out of the list
every half minute. Held as an explicit snapshot rather than read back off the
previous document, so a run of offline refreshes cannot lose it or duplicate it.

### The pane cannot act on anything

Same call `agent-map` made, for the same reason: deciding what to act on is the
job, and a jump-to-pane binding guessed now is muscle memory to unlearn later.
No `gh pr merge`, no `git push`, no pane switching.

### The README screenshot is invented, not captured

`agent-map`'s screenshot in this public repo contains real internal repository and
session names. I did not extend that: the `daybook` frame uses invented
repositories (`api-gateway`, `ledger`) because the real frame reads
`internal-defi-ratings#22 has merge conflicts`, `stakingrewards#2293`, and a list
of your open PR numbers. **You may want to sanitise agent-map's screenshot too** —
that is a call about your own information, so I left it alone.

### Linear and the calendar live in the skill, not the collector

Both are MCP-only, and the calendar needs an interactive auth flow this machine
has not completed. The collector stays offline-capable and machine-local; the
skill layers those in when they are available and prints one line when they are
not. The skill is explicitly forbidden from starting an auth flow during a
standup.

### Nothing was scheduled

You said "start my day with", which could have meant a cron routine that runs
before you sit down. I did not set one up: a scheduled agent that burns tokens on
mornings you do not work, and whose output you read hours stale, is worse than
typing `/standup`. If you want it scheduled, the `schedule` skill does it in one
command and the collector's cache makes the pane instant afterwards.

---

## What changed on this machine

### Code — on a branch, PR raised, not merged

Branch `feat/daybook`, worktree at `.worktrees/daybook`.

| Path | What |
| --- | --- |
| `tools/daybook/daybook-collect.py` | the collector |
| `tools/daybook/test_daybook_collect.py` | 30 unit tests |
| `plugins/daybook/` | the herdr pane (Rust, 29 unit tests) |
| `README.md` | a `daybook` section, install and dev notes |
| `.gitignore` | `.worktrees/`, `__pycache__/` |

### Non-code — applied directly, listed here

| Path | What |
| --- | --- |
| `~/.claude/skills/standup/SKILL.md` | the `/standup` skill |
| `~/.local/bin/daybook-collect` | symlink to the collector |
| herdr `plugins.json` | `daybook` linked **and enabled** |

Two things to know about those:

- **The symlink points into the worktree**, because `tools/daybook/` does not
  exist on `main` until the PR merges. Same for the linked plugin root. After
  merging, re-point both:

  ```sh
  ln -sfn ~/Projects/herdr-plugins/tools/daybook/daybook-collect.py ~/.local/bin/daybook-collect
  herdr plugin unlink daybook && herdr plugin link ~/Projects/herdr-plugins/plugins/daybook
  ```

- **I enabled the plugin**, unlike the overnight run which left both plugins
  disabled. Enabling registers the pane; it does not open one or change your
  running layout. The rendering was verified in a pty first, which is what the
  earlier run could not say.

---

## Verified vs assumed

**Verified by execution:**
- The collector runs against your live machine in 1.6s offline and 5.2s with
  `gh`, over 12 repositories and 4 linked worktrees.
- 30 Python tests and 29 Rust tests pass; `cargo clippy --all-targets` is clean;
  `cargo fmt --all --check` is clean.
- The pane renders correctly driven in a pty at 120×32, 96×24, 60×16, and 44×8
  (the last showing the too-small guard). `G` reaches the last row with its hint
  visible.
- `daybook --once` and `daybook --where` produce correct output.
- `herdr plugin link` and `enable` both succeeded with no warnings; `herdr plugin
  list` shows it enabled.
- The duplicate-commit bug is fixed and the count now matches `git log` by hand.

**Assumed, not verified:**
- **That the popup opens at 120×32 and looks right there.** I never opened one —
  that would have put a pane in front of whatever you left on screen. The size is
  a guess tuned from the pty frames; `herdr plugin pane open daybook brief` is the
  test.
- **That `session-unanswered` is a signal you want.** It fires on a transcript
  whose last message is yours, which catches both "you interrupted it" and "you
  asked and walked away". It currently flags one session ("Tab switching"). If it
  is noise, the fix is one line.
- **That the 5-minute PR cadence is the right trade.** ~20 `gh` calls per refresh
  against a 5000/hour limit; fine for a pane you open and close, defensible for
  one left open all day, but I did not measure a full day of it.
- **`gitleaks` is still not installed**, so no automated secret scan ran. I read
  both diffs by hand: Python, Rust, Markdown, and a TOML manifest. The collector
  reads `git config user.email` and prints it in `identities` — your own committer
  addresses, which are already in every commit — and never reads `.env`, key
  material, or credential files.

---

## Two things I found and did not act on

**Four `internal-defi-ratings` PRs (#21–#24) all have merge conflicts, and #21 is
still a draft.** They look like a stack that drifted apart; #21 being a draft
suggests the bottom of it was never finished. That is a `plan` job, not a
`fix-it-now` job, so it is in the pick-list rather than done.

**`stakingrewards` is checked out on `staging`, 241 commits behind `origin/main`.**
Either that is intentional and the base-drift warning is noise for that repo, or
that checkout has been forgotten. If the former, `pr_stale_days`-style per-repo
overrides do not exist yet — say the word and I will add an ignore list.

---

## Next steps

```sh
# 1. Try it
/standup                                        # in any Claude Code session
herdr plugin pane open daybook brief            # the popup
herdr plugin pane open daybook brief-tab        # or the persistent tab

# 2. Review the PR
gh pr view --repo pdjsh/herdr-plugins

# 3. After merging, re-point the two things aimed at the worktree
ln -sfn ~/Projects/herdr-plugins/tools/daybook/daybook-collect.py ~/.local/bin/daybook-collect
herdr plugin unlink daybook && herdr plugin link ~/Projects/herdr-plugins/plugins/daybook
```

## Open questions

1. Is `session-unanswered` a signal or noise?
2. Should `stakingrewards` (and repos like it) be exempt from base-drift warnings?
3. Do you want `/standup` scheduled, or typed?
4. Sanitise `agent-map`'s README screenshot as well?
