#!/usr/bin/env python3
"""Gather the state of a working day into one JSON document.

This is the single source of truth behind two very different renderers: the
`daybook` herdr pane (a TUI dashboard) and the `/standup` Claude Code skill (a
synthesised morning briefing). Keeping the collection here rather than in either
consumer means the pane and the briefing can never disagree about what happened.

It is deliberately dependency-free stdlib Python: the herdr plugin shells out to
it, so an install step involving a virtualenv would make the pane fail in ways a
TUI cannot explain.

Four sources, none of them required:

  git       every repository under the configured roots, plus their linked
            worktrees — commits in the window, dirty files, unpushed work
  gh        pull requests: mine, ones awaiting my review, ones merged in the
            window (skipped entirely with --no-net)
  claude    ~/.claude/projects transcripts — which sessions ran, what they were
            about, which ended on an unanswered prompt
  herdr     the live agent roster over herdr's unix socket

Every source degrades independently. A missing `gh`, a stopped herdr server, or
an unreadable transcript adds a line to `errors` and leaves the rest intact,
because a briefing that renders four fifths of the picture beats one that
refuses to render at all.

Usage:
    daybook-collect                 # JSON to stdout
    daybook-collect --text          # plain-text frame instead
    daybook-collect --no-net        # skip gh (fast, offline)
    daybook-collect --since 2026-08-18
    daybook-collect --max-age 300   # reuse the cache if it is under 5min old
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime as dt
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

SCHEMA = 1


def expand(p: str | Path) -> Path:
    return Path(os.path.expandvars(str(p))).expanduser()


HOME = Path.home()


def _xdg(var: str, default: str) -> Path:
    """An XDG base directory, tolerating the ways it is usually wrong.

    Set-but-empty is common in stripped launchd and systemd environments, and
    taking it verbatim yields a *relative* path — so the cache would land in
    whatever repository the pane happened to be launched from, inside a checkout
    it is simultaneously reporting as dirty. A `~` in the value is expanded for
    the same reason: this is a hand-edited variable.
    """
    value = os.environ.get(var, "").strip()
    return expand(value) if value else expand(default)


CACHE_PATH = _xdg("XDG_CACHE_HOME", "~/.cache") / "daybook" / "latest.json"
CONFIG_PATH = _xdg("XDG_CONFIG_HOME", "~/.config") / "daybook" / "config.toml"

# Defaults chosen from this machine's layout. Everything here is overridable in
# config.toml so the script survives being synced to another machine.
DEFAULT_CONFIG: dict[str, Any] = {
    # Directories whose immediate children are repositories.
    "repo_roots": ["~/Projects"],
    # Individual repositories that do not live under a root.
    "extra_repos": ["~/.local/share/chezmoi"],
    # Where herdr parks the worktrees it creates. These are discovered through
    # `git worktree list` anyway; the root is listed so an orphaned worktree
    # whose parent repo has moved still shows up.
    "worktree_roots": ["~/.herdr/worktrees"],
    # Extra author identities beyond what git config reports. Matched
    # case-insensitively against both name and email.
    "identities": [],
    # Transcript directory for Claude Code sessions.
    "claude_projects": "~/.claude/projects",
    # Hard ceiling on subprocess runtime. A wedged git or gh must not hang a
    # herdr pane that has no other input loop.
    "timeout": 20,
    # Per-PR detail costs one network round trip each; past this many we keep
    # the list but stop enriching it.
    "pr_detail_limit": 20,
    # An open PR untouched for this long is a loose end worth a nudge. Past
    # `pr_dormant_days` it stops being today's problem and is collapsed into a
    # single housekeeping line — otherwise a couple of abandoned PRs in other
    # people's repositories crowd out everything actionable.
    "pr_stale_days": 7,
    "pr_dormant_days": 45,
    # Branches that are supposed to lag the default branch. "N commits behind
    # origin/main" is only actionable on a branch you intend to merge; on a
    # long-lived one it is the normal state of affairs, so warning about it is
    # pure noise. Matched exactly, or as a prefix when the entry ends in `/`.
    "long_lived_branches": [
        "main",
        "master",
        "staging",
        "develop",
        "dev",
        "production",
        "prod",
        "trunk",
        "release/",
        "hotfix/",
    ],
}

# ---------------------------------------------------------------------------
# small helpers
# ---------------------------------------------------------------------------


def run(
    cmd: list[str], cwd: Path | None = None, timeout: float = 20
) -> tuple[int, str, str]:
    """Run a command, never raise. Returns (rc, stdout, stderr)."""
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(cwd) if cwd else None,
            capture_output=True,
            text=True,
            timeout=timeout,
            # A collector must never block on a prompt; git and gh both read
            # stdin for credentials given the chance.
            stdin=subprocess.DEVNULL,
            env={**os.environ, "GIT_TERMINAL_PROMPT": "0", "GIT_OPTIONAL_LOCKS": "0"},
        )
        return proc.returncode, proc.stdout, proc.stderr
    except subprocess.TimeoutExpired:
        return 124, "", f"timed out after {timeout}s: {' '.join(cmd[:3])}"
    except FileNotFoundError:
        return 127, "", f"not found: {cmd[0]}"
    except OSError as exc:  # permissions, ENOENT on cwd, …
        return 126, "", f"{type(exc).__name__}: {exc}"


def parse_iso(s: str | None) -> dt.datetime | None:
    """Parse the ISO-8601 shapes git, gh and Claude Code each emit."""
    if not s:
        return None
    text = s.strip().replace("Z", "+00:00")
    try:
        parsed = dt.datetime.fromisoformat(text)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)
    return parsed


def local_day(when: dt.datetime) -> dt.date:
    return when.astimezone().date()


def short(text: str | None, limit: int = 90) -> str:
    """Collapse whitespace and clip. Transcripts contain multi-KB prompts."""
    if not text:
        return ""
    flat = re.sub(r"\s+", " ", text).strip()
    return flat if len(flat) <= limit else flat[: limit - 1] + "…"


def load_config() -> dict[str, Any]:
    cfg = dict(DEFAULT_CONFIG)
    if not CONFIG_PATH.exists():
        return cfg
    try:
        import tomllib

        with CONFIG_PATH.open("rb") as handle:
            cfg.update(tomllib.load(handle))
    except Exception:
        # A malformed config must not take the briefing down with it; the
        # resulting `errors` entry is added by the caller that can see `out`.
        cfg["_config_error"] = f"could not read {CONFIG_PATH}"
    return cfg


# ---------------------------------------------------------------------------
# identity
# ---------------------------------------------------------------------------


def identities(cfg: dict[str, Any], repos: list[Path], net: bool = True) -> set[str]:
    """Every name/email that counts as "me" when attributing a commit.

    Gathered rather than configured because this machine has two git identities
    (a personal one and a work one) and per-repo overrides decide which applies
    where. Anything missed here shows up as someone else's commit, which is a
    visible-but-harmless wrong answer rather than a crash.
    """
    found: set[str] = set()
    for key in ("user.email", "user.name"):
        _, out, _ = run(["git", "config", "--global", "--get-all", key])
        found.update(line.strip().lower() for line in out.splitlines() if line.strip())
    for repo in repos[:40]:
        _, out, _ = run(["git", "-C", str(repo), "config", "--get-all", "user.email"])
        found.update(line.strip().lower() for line in out.splitlines() if line.strip())
    # `gh api user` is a network round trip, and `--no-net` promises none. On a
    # plane that call costs a DNS timeout on every refresh.
    if net and shutil.which("gh"):
        _, out, _ = run(["gh", "api", "user", "--jq", ".login"], timeout=10)
        if out.strip():
            found.add(out.strip().lower())
    found.update(str(i).strip().lower() for i in cfg.get("identities", []) if str(i).strip())
    return {f for f in found if f}


# ---------------------------------------------------------------------------
# repositories
# ---------------------------------------------------------------------------


def discover_repos(cfg: dict[str, Any]) -> list[Path]:
    seen: dict[str, Path] = {}

    def add(path: Path) -> None:
        if not path.exists():
            return
        # `.git` is a directory in a normal checkout and a file in a linked
        # worktree, so both shapes have to pass.
        if not (path / ".git").exists():
            return
        seen.setdefault(str(path.resolve()), path)

    for root in cfg.get("repo_roots", []):
        base = expand(root)
        if not base.is_dir():
            continue
        for child in sorted(base.iterdir()):
            if child.is_dir() and not child.name.startswith("."):
                add(child)
    for extra in cfg.get("extra_repos", []):
        add(expand(extra))
    for root in cfg.get("worktree_roots", []):
        base = expand(root)
        if not base.is_dir():
            continue
        # <root>/<repo>/<branch-slug>
        for repo_dir in sorted(base.iterdir()):
            if not repo_dir.is_dir():
                continue
            for wt in sorted(repo_dir.iterdir()):
                if wt.is_dir():
                    add(wt)
    return list(seen.values())


def repo_key(path: Path, timeout: float) -> str:
    """Group a checkout with the repository it belongs to.

    Mirrors herdr's own `worktree.repo_key`: the common `.git` directory, which
    a main checkout and all of its linked worktrees share. Grouping the same way
    herdr does means the pane and the sidebar draw the same boundaries.
    """
    _, out, _ = run(
        ["git", "-C", str(path), "rev-parse", "--path-format=absolute", "--git-common-dir"],
        timeout=timeout,
    )
    return out.strip() or str(path.resolve())


def inspect_checkout(path: Path, timeout: float) -> dict[str, Any]:
    """Working-tree facts for one checkout.

    Strictly per-checkout: branch, dirt, and how far its HEAD has drifted.
    Commit history deliberately lives one level up, on the repository — see
    `repo_history` for why.
    """
    g = ["git", "-C", str(path)]
    info: dict[str, Any] = {
        "name": path.name,
        "path": str(path),
        "repo_key": repo_key(path, timeout),
        "is_worktree": (path / ".git").is_file(),
    }

    _, out, _ = run(g + ["rev-parse", "--abbrev-ref", "HEAD"], timeout=timeout)
    info["branch"] = out.strip() or "(detached)"

    # `--porcelain=v1 -z` because filenames containing newlines exist and would
    # otherwise inflate every count below.
    _, out, _ = run(
        g + ["status", "--porcelain=v1", "-z", "--untracked-files=normal"], timeout=timeout
    )
    staged = unstaged = untracked = 0
    dirty_paths: list[str] = []
    fields = out.split("\0")
    i = 0
    while i < len(fields):
        entry = fields[i]
        i += 1
        if len(entry) < 4:
            continue
        x, y, name = entry[0], entry[1], entry[3:]
        # Renames *and copies* emit `XY <to>\0<from>\0`. Missing the copy case
        # meant the old path was parsed as a fresh entry, with its first two
        # characters read as status codes.
        if x in "RC" or y in "RC":
            i += 1
        if x == "?" and y == "?":
            untracked += 1
        else:
            if x not in " ?":
                staged += 1
            if y not in " ?":
                unstaged += 1
        if len(dirty_paths) < 8:
            dirty_paths.append(name)
    info.update(staged=staged, unstaged=unstaged, untracked=untracked, dirty_paths=dirty_paths)
    info["dirty"] = staged + unstaged + untracked

    # Unpushed work. No upstream at all is itself worth reporting: that is a
    # branch which exists only on this laptop.
    rc, out, _ = run(
        g + ["rev-list", "--left-right", "--count", "@{upstream}...HEAD"], timeout=timeout
    )
    parts = out.split()
    if rc == 0 and len(parts) == 2 and all(p.isdigit() for p in parts):
        info["behind"], info["ahead"] = int(parts[0]), int(parts[1])
        info["has_upstream"] = True
    else:
        info["behind"] = info["ahead"] = 0
        info["has_upstream"] = False

    _, out, _ = run(g + ["stash", "list"], timeout=timeout)
    info["stashes"] = len([line for line in out.splitlines() if line.strip()])

    _, out, _ = run(g + ["log", "-1", "--format=%aI"], timeout=timeout)
    info["last_commit_at"] = out.strip() or None

    # Distance from the base branch, for the "sync with base before you push"
    # rule. Measured against local remote-tracking refs — no fetch, because a
    # collector must not mutate the repository it is reporting on. How stale
    # those refs are is reported separately as `fetched_hours_ago`.
    info["base"] = None
    info["behind_base"] = 0
    for base in ("origin/main", "origin/master"):
        rc, _, _ = run(g + ["rev-parse", "--verify", "--quiet", base], timeout=timeout)
        if rc != 0:
            continue
        info["base"] = base
        rc, out, _ = run(g + ["rev-list", "--count", f"HEAD..{base}"], timeout=timeout)
        if rc == 0 and out.strip().isdigit():
            info["behind_base"] = int(out.strip())
        break

    return info


def repo_history(path: Path, since_iso: str, me: set[str], timeout: float) -> dict[str, Any]:
    """Commits in the window for a whole repository, not a single checkout.

    `git log --all` walks the shared object store, so every linked worktree of
    a repository answers this identically. Asking each checkout separately is
    what made an early version report a repo's commits three times over. It is
    asked once per `repo_key` instead.

    All authors are fetched and split here rather than filtered with `--author`,
    because the same walk then also answers "did the base branch move under me
    while I was away?".
    """
    sep = "\x1f"
    _, out, _ = run(
        [
            "git", "-C", str(path), "log",
            f"--since={since_iso}", "--no-merges", "--all",
            f"--pretty=format:%H{sep}%aI{sep}%an{sep}%ae{sep}%s",
        ],
        timeout=timeout,
    )
    mine: list[dict[str, Any]] = []
    others = 0
    seen: set[str] = set()
    for line in out.splitlines():
        parts = line.split(sep)
        if len(parts) < 5:
            continue
        sha, when, name, email, subject = parts[:5]
        if sha in seen:
            continue
        seen.add(sha)
        if name.strip().lower() in me or email.strip().lower() in me:
            mine.append({"sha": sha[:9], "at": when, "subject": short(subject, 120)})
        else:
            others += 1
    mine.sort(key=lambda c: c["at"], reverse=True)

    # Everything above compares against local refs, so an old fetch silently
    # understates `behind` and `behind_base`. Say how old.
    fetched = None
    try:
        _, common, _ = run(
            ["git", "-C", str(path), "rev-parse", "--path-format=absolute", "--git-common-dir"],
            timeout=timeout,
        )
        stamp = (Path(common.strip()) / "FETCH_HEAD").stat().st_mtime
        age = dt.datetime.now(dt.timezone.utc) - dt.datetime.fromtimestamp(stamp, dt.timezone.utc)
        fetched = round(age.total_seconds() / 3600, 1)
    except OSError:
        pass
    return {"commits": mine, "commits_by_others": others, "fetched_hours_ago": fetched}


def collect_repos(
    cfg: dict[str, Any], since: dt.datetime, me: set[str], checkout_paths: list[Path]
) -> tuple[list[dict], list[str]]:
    """Every repository, each with its checkouts nested under it."""
    timeout = float(cfg.get("timeout", 20))
    since_iso = since.isoformat()
    errors: list[str] = []
    if not checkout_paths:
        return [], ["no git repositories found under the configured roots"]

    # Git here is IO-bound, and a dozen repositories inspected serially is the
    # difference between a pane that opens instantly and one that visibly stalls.
    checkouts: list[dict[str, Any]] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        futures = {pool.submit(inspect_checkout, p, timeout): p for p in checkout_paths}
        for fut in concurrent.futures.as_completed(futures):
            path = futures[fut]
            try:
                checkouts.append(fut.result())
            except Exception as exc:  # noqa: BLE001 — one bad repo must not kill the run
                errors.append(f"git {path.name}: {type(exc).__name__}: {exc}")

    groups: dict[str, list[dict[str, Any]]] = {}
    for c in checkouts:
        groups.setdefault(c["repo_key"], []).append(c)

    repos: list[dict[str, Any]] = []
    for key, members in groups.items():
        # The main checkout names the repository. A group of nothing but linked
        # worktrees (the parent having moved or been deleted) falls back to the
        # shallowest path, which is the closest thing to a canonical one.
        members.sort(key=lambda c: (c["is_worktree"], len(c["path"])))
        primary = members[0]
        repos.append(
            {
                "key": key,
                "name": primary["name"],
                "path": primary["path"],
                "base": primary["base"],
                "checkouts": members,
            }
        )

    def safe_history(repo: dict[str, Any]) -> dict[str, Any]:
        # `pool.map` re-raises inside the consuming loop, which would take the
        # whole collection down over one malformed repository — exactly the
        # failure mode the checkout fan-out above guards against.
        try:
            return repo_history(Path(repo["path"]), since_iso, me, timeout)
        except Exception as exc:  # noqa: BLE001
            errors.append(f"git log {repo['name']}: {type(exc).__name__}: {exc}")
            return {"commits": [], "commits_by_others": 0, "fetched_hours_ago": None}

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        histories = list(pool.map(safe_history, repos))
        for repo, history in zip(repos, histories):
            repo.update(history)
            repo["dirty"] = sum(c["dirty"] for c in repo["checkouts"])
            repo["ahead"] = sum(c["ahead"] for c in repo["checkouts"])
            # `refs/stash` is shared by every worktree, so this is a repository
            # count, not something to sum across checkouts.
            repo["stashes"] = repo["checkouts"][0]["stashes"]

    repos.sort(key=lambda r: (-len(r["commits"]), -r["dirty"], r["name"]))
    return repos, errors


# ---------------------------------------------------------------------------
# pull requests
# ---------------------------------------------------------------------------

PR_SEARCH_FIELDS = "number,title,repository,url,createdAt,updatedAt,closedAt,isDraft,commentsCount,labels"


def gh_search(args: list[str], timeout: float) -> tuple[list[dict], str | None]:
    rc, out, err = run(
        ["gh", "search", "prs", "--json", PR_SEARCH_FIELDS] + args, timeout=timeout
    )
    if rc != 0:
        return [], short(err or f"gh search prs {' '.join(args)} failed", 160)
    try:
        return json.loads(out or "[]"), None
    except json.JSONDecodeError:
        return [], "gh search prs returned unparseable JSON"


def flatten_pr(raw: dict[str, Any]) -> dict[str, Any]:
    repo = (raw.get("repository") or {}).get("nameWithOwner") or ""
    return {
        "repo": repo,
        "name": (raw.get("repository") or {}).get("name") or repo.split("/")[-1],
        "number": raw.get("number"),
        "title": short(raw.get("title"), 110),
        "url": raw.get("url"),
        "draft": bool(raw.get("isDraft")),
        "created_at": raw.get("createdAt"),
        "updated_at": raw.get("updatedAt"),
        "closed_at": raw.get("closedAt"),
        "comments": raw.get("commentsCount") or 0,
        "labels": [lbl.get("name") for lbl in (raw.get("labels") or []) if lbl.get("name")],
    }


def enrich_pr(pr: dict[str, Any], timeout: float) -> dict[str, Any]:
    """Add review, mergeability and CI state — one round trip per PR.

    `gh search` cannot report any of these, and they are exactly the fields
    that decide whether a PR is waiting on me or on someone else.
    """
    rc, out, err = run(
        [
            "gh", "pr", "view", str(pr["number"]), "-R", pr["repo"],
            "--json", "reviewDecision,mergeable,statusCheckRollup,reviewRequests",
        ],
        timeout=timeout,
    )
    if rc != 0:
        pr["detail_error"] = short(err, 120)
        return pr
    try:
        data = json.loads(out or "{}")
    except json.JSONDecodeError:
        pr["detail_error"] = "unparseable gh pr view output"
        return pr

    pr["review_decision"] = data.get("reviewDecision") or ""
    pr["mergeable"] = data.get("mergeable") or ""
    pr["reviewers_pending"] = len(data.get("reviewRequests") or [])

    failing: list[str] = []
    pending = 0
    for check in data.get("statusCheckRollup") or []:
        # Checks and legacy statuses report through different field names.
        state = (check.get("conclusion") or check.get("state") or "").upper()
        name = check.get("name") or check.get("context") or "check"
        if state in ("FAILURE", "TIMED_OUT", "CANCELLED", "ERROR", "ACTION_REQUIRED"):
            failing.append(name)
        elif state in ("", "PENDING", "IN_PROGRESS", "QUEUED", "EXPECTED"):
            pending += 1
    pr["checks_failing"] = failing[:6]
    pr["checks_pending"] = pending
    return pr


def collect_prs(cfg: dict[str, Any], since: dt.datetime) -> tuple[dict[str, Any], list[str]]:
    prs: dict[str, Any] = {"mine": [], "review_requested": [], "merged_in_window": []}
    errors: list[str] = []
    if not shutil.which("gh"):
        return prs, ["gh not on PATH — pull requests skipped"]
    timeout = float(cfg.get("timeout", 20))

    # `--sort` matters more than it looks: the default is `best-match`, so a
    # relevance-ranked slice of 40 can omit yesterday's merges entirely on an
    # account with a long history. `--merged-at` narrows it server-side; the
    # client-side filter below stays as a backstop for older `gh` builds that
    # ignore the flag.
    since_day = since.date().isoformat()
    queries = {
        "mine": ["--author=@me", "--state=open", "--sort=updated", "--limit=30"],
        "review_requested": [
            "--review-requested=@me", "--state=open", "--sort=updated", "--limit=30",
        ],
        "merged_in_window": [
            "--author=@me", "--merged", f"--merged-at=>={since_day}",
            "--sort=updated", "--limit=40",
        ],
    }
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        futures = {k: pool.submit(gh_search, a, timeout) for k, a in queries.items()}
        for key, fut in futures.items():
            raw, err = fut.result()
            if err:
                errors.append(err)
            prs[key] = [flatten_pr(p) for p in raw]

    prs["merged_in_window"] = [
        p
        for p in prs["merged_in_window"]
        if (parse_iso(p.get("closed_at")) or dt.datetime.min.replace(tzinfo=dt.timezone.utc)) >= since
    ]

    limit = int(cfg.get("pr_detail_limit", 20))
    needing = (prs["mine"] + prs["review_requested"])[:limit]
    if needing:
        # `enrich_pr` mutates the dicts in place, and those dicts are the same
        # objects the buckets hold — the mapped results are deliberately dropped.
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
            list(pool.map(lambda p: enrich_pr(p, timeout), needing))
    if len(prs["mine"]) + len(prs["review_requested"]) > limit:
        errors.append(
            f"stopped enriching after {limit} pull requests — raise pr_detail_limit to see CI state on the rest"
        )

    for bucket in prs.values():
        bucket.sort(key=lambda p: p.get("updated_at") or "", reverse=True)
    return prs, errors


# ---------------------------------------------------------------------------
# Claude Code sessions
# ---------------------------------------------------------------------------


def read_session(path: Path, since: dt.datetime) -> dict[str, Any] | None:
    """Summarise one transcript, streaming it.

    Transcripts run to megabytes — tool results dominate — so nothing is held
    beyond the running summary. The interesting records are sparse:
    `ai-title` (the session's own summary of itself), `last-prompt`, and the
    user/assistant messages that bracket the activity.
    """
    title = last_prompt = cwd = branch = None
    first_at: dt.datetime | None = None
    last_at: dt.datetime | None = None
    user_turns = assistant_turns = 0
    last_role = None
    in_window = False

    try:
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if not line.strip():
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue
                kind = rec.get("type")
                if kind == "ai-title":
                    title = rec.get("aiTitle") or title
                    continue
                if kind == "last-prompt":
                    last_prompt = rec.get("lastPrompt") or last_prompt
                    continue
                if kind not in ("user", "assistant"):
                    continue
                # Sub-agent traffic is the parent session's business, not a
                # session of its own.
                if rec.get("isSidechain") or rec.get("isMeta"):
                    continue
                when = parse_iso(rec.get("timestamp"))
                if when:
                    first_at = when if first_at is None else min(first_at, when)
                    last_at = when if last_at is None else max(last_at, when)
                    if when >= since:
                        in_window = True
                cwd = rec.get("cwd") or cwd
                branch = rec.get("gitBranch") or branch

                message = rec.get("message") or {}
                content = message.get("content")
                if kind == "user":
                    # A user record also carries tool results; only a real
                    # prompt counts as a turn.
                    if is_real_prompt(content):
                        user_turns += 1
                        last_role = "user"
                else:
                    assistant_turns += 1
                    last_role = "assistant"
    except OSError as exc:
        return {"path": str(path), "error": f"{type(exc).__name__}: {exc}"}

    if not in_window:
        return None
    return {
        "session_id": path.stem,
        "path": str(path),
        # Filled in by the caller, which knows where the repositories are.
        "repo": None,
        "cwd": cwd,
        "branch": branch,
        "title": short(title, 80) or short(last_prompt, 80) or "(untitled)",
        "last_prompt": short(last_prompt, 160),
        "started_at": first_at.isoformat() if first_at else None,
        "ended_at": last_at.isoformat() if last_at else None,
        "user_turns": user_turns,
        "assistant_turns": assistant_turns,
        # A transcript whose final message is the user's is one where the
        # answer never arrived — an interrupted session, and the single most
        # useful "you left this hanging" signal in the whole file.
        "unanswered": last_role == "user",
    }


def is_real_prompt(content: Any) -> bool:
    if isinstance(content, str):
        return bool(content.strip())
    if isinstance(content, list):
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_result":
                return False
            if block.get("type") == "text" and str(block.get("text", "")).strip():
                return True
        return False
    return False


def checkout_index(repos: list[dict]) -> list[tuple[str, str]]:
    """(path, label) for every checkout, longest path first.

    Sorted so a longest-prefix lookup is a linear scan with an early exit: a
    worktree nested inside its parent repository has to win over the parent.
    """
    index: list[tuple[str, str]] = []
    for repo in repos:
        for co in repo["checkouts"]:
            label = repo["name"] if not co["is_worktree"] else f"{repo['name']} [{co['branch']}]"
            index.append((co["path"].rstrip("/"), label))
    index.sort(key=lambda entry: len(entry[0]), reverse=True)
    return index


def label_for_cwd(cwd: str | None, index: list[tuple[str, str]]) -> str:
    """Name the repository a session was working in.

    The transcript records only a working directory, so a session started in a
    subdirectory would otherwise be labelled `src` or `tools`, and one started
    in a linked worktree would be labelled with the branch slug rather than the
    repository. Anything outside every known checkout keeps its own name.
    """
    if not cwd:
        return "(unknown)"
    for path, label in index:
        if cwd == path or cwd.startswith(path + "/"):
            return label
    return Path(cwd).name


def collect_sessions(cfg: dict[str, Any], since: dt.datetime) -> tuple[list[dict], list[str]]:
    root = expand(cfg.get("claude_projects", "~/.claude/projects"))
    if not root.is_dir():
        return [], [f"no Claude Code transcripts at {root}"]

    # mtime is a cheap pre-filter: a transcript untouched since the window
    # opened cannot contain activity inside it, and this cuts a 35MB corpus
    # down to the handful of files worth parsing.
    cutoff = since.timestamp()
    candidates = [
        p
        for p in root.glob("*/*.jsonl")
        if _mtime(p) >= cutoff
    ]
    sessions: list[dict[str, Any]] = []
    errors: list[str] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        for result in pool.map(lambda p: read_session(p, since), candidates):
            if result is None:
                continue
            if "error" in result:
                errors.append(f"transcript {Path(result['path']).name}: {result['error']}")
                continue
            sessions.append(result)
    sessions.sort(key=lambda s: s.get("ended_at") or "", reverse=True)
    return sessions, errors


def _mtime(path: Path) -> float:
    try:
        return path.stat().st_mtime
    except OSError:
        return 0.0


# ---------------------------------------------------------------------------
# herdr agents
# ---------------------------------------------------------------------------


def collect_agents(cfg: dict[str, Any]) -> tuple[list[dict], list[str]]:
    """Ask herdr what is running.

    Shelling out to `herdr agent list` rather than speaking the socket protocol
    directly: the CLI already resolves the socket path the same way the plugin
    host does, and this script's other consumer is a herdr plugin that gets the
    roster from the socket anyway.
    """
    if not shutil.which("herdr"):
        return [], []  # not a herdr machine; silence rather than an error
    rc, out, err = run(["herdr", "agent", "list"], timeout=float(cfg.get("timeout", 20)))
    if rc != 0:
        return [], [short(f"herdr agent list failed: {err or rc}", 140)]
    try:
        envelope = json.loads(out.splitlines()[0] if out.strip() else "{}")
    except (json.JSONDecodeError, IndexError):
        return [], ["herdr agent list returned unparseable JSON"]
    raw = ((envelope.get("result") or {}).get("agents")) or []
    agents = []
    for a in raw:
        title = a.get("terminal_title_stripped") or a.get("terminal_title") or ""
        agents.append(
            {
                "pane_id": a.get("pane_id"),
                "workspace_id": a.get("workspace_id"),
                "agent": a.get("agent") or "agent",
                "status": a.get("agent_status") or "unknown",
                # Claude Code leaves its own spinner frame in the title even
                # after herdr strips its glyph; anything before the first
                # alphanumeric is decoration.
                "title": short(re.sub(r"^[^0-9A-Za-z]+", "", title), 80),
                "cwd": a.get("cwd") or "",
                "session_id": ((a.get("agent_session") or {}).get("value")),
                "state_change_seq": a.get("state_change_seq") or 0,
            }
        )
    order = {"blocked": 0, "done": 1, "working": 2, "idle": 3}
    agents.sort(key=lambda a: (order.get(a["status"], 4), a["pane_id"] or ""))
    return agents, []


# ---------------------------------------------------------------------------
# window
# ---------------------------------------------------------------------------


def resolve_window(
    cfg: dict[str, Any], override: str | None
) -> tuple[dt.datetime, dict[str, Any]]:
    """Pick the start of the recap window.

    Not "yesterday": on a Monday, yesterday is empty and the interesting work
    is Friday's. So the window opens at the start of the most recent *earlier*
    day that shows any activity at all, and reports how far back that was — a
    briefing that silently skips a weekend's worth of context is worse than one
    that says "since Friday".
    """
    now = dt.datetime.now().astimezone()
    today = now.date()

    if override:
        try:
            day = dt.date.fromisoformat(override)
            start = dt.datetime.combine(day, dt.time.min).astimezone()
            return start, {
                "since": start.isoformat(),
                "until": now.isoformat(),
                "days_back": (today - day).days,
                "source": "explicit",
                "label": f"since {day.strftime('%a %d %b')}",
            }
        except ValueError:
            pass  # fall through to detection; the error is reported by the caller

    days = activity_days(cfg, now)
    previous = [d for d in sorted(days, reverse=True) if d < today]
    day = previous[0] if previous else today - dt.timedelta(days=1)
    start = dt.datetime.combine(day, dt.time.min).astimezone()
    back = (today - day).days
    return start, {
        "since": start.isoformat(),
        "until": now.isoformat(),
        "days_back": back,
        "source": "detected" if previous else "fallback",
        "label": (
            "since yesterday"
            if back == 1
            else f"since {day.strftime('%a %d %b')} ({back} days)"
        ),
    }


def activity_days(cfg: dict[str, Any], now: dt.datetime) -> set[dt.date]:
    """Days in the recent past with either a commit or a Claude session.

    Deliberately cheap: a 21-day `git log` per repo and transcript mtimes. The
    window has to be known before the expensive collection can start, so this
    cannot afford to be thorough.
    """
    horizon = (now - dt.timedelta(days=21)).date().isoformat()
    days: set[dt.date] = set()
    timeout = float(cfg.get("timeout", 20))

    def repo_days(path: Path) -> set[dt.date]:
        _, out, _ = run(
            ["git", "-C", str(path), "log", "--all", f"--since={horizon}", "--pretty=format:%aI"],
            timeout=timeout,
        )
        found = set()
        for line in out.splitlines():
            when = parse_iso(line)
            if when:
                found.add(local_day(when))
        return found

    repos = discover_repos(cfg)
    if repos:
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            for found in pool.map(repo_days, repos):
                days |= found

    root = expand(cfg.get("claude_projects", "~/.claude/projects"))
    if root.is_dir():
        floor = (now - dt.timedelta(days=21)).timestamp()
        for path in root.glob("*/*.jsonl"):
            m = _mtime(path)
            if m >= floor:
                days.add(dt.datetime.fromtimestamp(m).astimezone().date())
    return days


# ---------------------------------------------------------------------------
# attention — the ranked open-loop list
# ---------------------------------------------------------------------------

# Severity bands, lowest number first. Both renderers key their colours off
# these, so the numbers are part of the schema.
SEV_BLOCKED = 1  # something is stuck and cannot progress without me
SEV_WAITING = 2  # someone or something is waiting on my attention
SEV_LOOSE = 3  # work in flight that will rot if left
SEV_TIDY = 4  # housekeeping


def is_long_lived(branch: str, patterns: list[str]) -> bool:
    """Whether a branch is one that is expected to sit behind the default branch.

    A trailing `/` in a pattern makes it a prefix match, so `release/` covers
    `release/2026-08` without also matching `release-notes-fix`.
    """
    name = (branch or "").strip()
    for pattern in patterns:
        text = str(pattern).strip()
        if not text:
            continue
        if text.endswith("/"):
            if name.startswith(text):
                return True
        elif name == text:
            return True
    return False


def build_attention(
    repos: list[dict],
    prs: dict[str, Any],
    sessions: list[dict],
    agents: list[dict],
    now: dt.datetime,
    cfg: dict[str, Any],
) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    stale_days = int(cfg.get("pr_stale_days", 7))
    dormant_days = int(cfg.get("pr_dormant_days", 45))
    long_lived = list(cfg.get("long_lived_branches", []))

    def add(sev: int, kind: str, text: str, hint: str = "", where: str = "") -> None:
        items.append({"severity": sev, "kind": kind, "text": text, "hint": hint, "where": where})

    for a in agents:
        if a["status"] == "blocked":
            add(
                SEV_BLOCKED,
                "agent-blocked",
                f"{a['pane_id']} is waiting on you — {a['title'] or a['agent']}",
                "answer the prompt in that pane",
                a["cwd"],
            )
        elif a["status"] == "done":
            add(
                SEV_WAITING,
                "agent-done",
                f"{a['pane_id']} finished and is unread — {a['title'] or a['agent']}",
                "read the result, then close or reuse the pane",
                a["cwd"],
            )

    for pr in prs.get("mine", []):
        ref = f"{pr['name']}#{pr['number']}"
        if pr.get("checks_failing"):
            add(
                SEV_BLOCKED,
                "pr-ci-red",
                f"{ref} has failing checks: {', '.join(pr['checks_failing'][:3])}",
                f"gh pr checks {pr['number']} -R {pr['repo']}",
                pr["url"] or "",
            )
        if pr.get("mergeable") == "CONFLICTING":
            add(
                SEV_BLOCKED,
                "pr-conflict",
                f"{ref} has merge conflicts",
                "merge the base branch in and resolve before a reviewer sees it",
                pr["url"] or "",
            )
        if pr.get("review_decision") == "CHANGES_REQUESTED":
            add(
                SEV_WAITING,
                "pr-changes-requested",
                f"{ref} has changes requested",
                f"gh pr view {pr['number']} -R {pr['repo']} --comments",
                pr["url"] or "",
            )
        if pr.get("draft"):
            add(SEV_LOOSE, "pr-draft", f"{ref} is still a draft", "mark ready or close it", pr["url"] or "")

    # Staleness, banded. A PR idle for a week wants a nudge; one idle for three
    # years is archaeology, and listing each of those individually is how a
    # briefing stops being read.
    dormant: list[str] = []
    for pr in prs.get("mine", []):
        updated = parse_iso(pr.get("updated_at"))
        if not updated or pr.get("draft"):
            continue
        idle = (now - updated).days
        if idle >= dormant_days:
            dormant.append(f"{pr['name']}#{pr['number']}")
        elif idle >= stale_days:
            add(
                SEV_LOOSE,
                "pr-stale",
                f"{pr['name']}#{pr['number']} has not moved in {idle} days",
                "nudge the reviewer or close it",
                pr["url"] or "",
            )
    if dormant:
        add(
            SEV_TIDY,
            "pr-dormant",
            f"{len(dormant)} open PR(s) dormant over {dormant_days} days: {', '.join(dormant[:6])}"
            + ("…" if len(dormant) > 6 else ""),
            "close them or accept that they are not coming back",
        )

    for pr in prs.get("review_requested", []):
        add(
            SEV_WAITING,
            "review-requested",
            f"{pr['name']}#{pr['number']} wants your review — {pr['title']}",
            f"gh pr diff {pr['number']} -R {pr['repo']}",
            pr["url"] or "",
        )

    # Working-tree loose ends are per checkout, not per repository: a repo can
    # have a clean main and three dirty worktrees, and only naming the branch
    # tells you which one to go to.
    for repo in repos:
        for co in repo["checkouts"]:
            label = f"{repo['name']} [{co['branch']}]"
            if co["dirty"]:
                add(
                    SEV_LOOSE,
                    "dirty",
                    f"{label}: {co['dirty']} uncommitted file(s)",
                    "commit, stash, or discard",
                    co["path"],
                )
            if co["ahead"]:
                add(
                    SEV_LOOSE,
                    "unpushed",
                    f"{label}: {co['ahead']} commit(s) not pushed",
                    "git push",
                    co["path"],
                )
            if (
                not co["has_upstream"]
                and co["branch"] != "(detached)"
                and not is_long_lived(co["branch"], long_lived)
            ):
                add(
                    SEV_LOOSE,
                    "no-upstream",
                    f"{label}: branch exists only on this machine",
                    "git push -u origin HEAD",
                    co["path"],
                )
            # Only branches you intend to merge. A long-lived branch trailing
            # the default one is its normal state, not a loose end.
            if co["behind_base"] >= 20 and not is_long_lived(co["branch"], long_lived):
                add(
                    SEV_TIDY,
                    "behind-base",
                    f"{label}: {co['behind_base']} commits behind {co['base']}",
                    f"merge {co['base']} in before opening or updating a PR",
                    co["path"],
                )
        # `refs/stash` lives in the common git dir, so the stash list is a
        # property of the repository — reporting it per checkout would count the
        # same stashes once per worktree.
        if repo["stashes"] >= 3:
            add(
                SEV_TIDY,
                "stashes",
                f"{repo['name']}: {repo['stashes']} stashes",
                "git stash list — drop what you no longer need",
                repo["path"],
            )

    live_sessions = {a.get("session_id") for a in agents if a.get("session_id")}
    for s in sessions:
        # A live pane's own unanswered prompt is the pane you are typing in, not
        # an abandoned thread.
        if s["unanswered"] and s["session_id"] not in live_sessions:
            add(
                SEV_WAITING,
                "session-unanswered",
                f"session ended on your prompt — {s['title']}",
                f"claude --resume {s['session_id']}",
                s.get("cwd") or "",
            )

    items.sort(key=lambda i: (i["severity"], i["kind"], i["text"]))
    return items


# ---------------------------------------------------------------------------
# assembly
# ---------------------------------------------------------------------------


def collect(cfg: dict[str, Any], args: argparse.Namespace) -> dict[str, Any]:
    now = dt.datetime.now().astimezone()
    errors: list[str] = []
    if cfg.get("_config_error"):
        errors.append(cfg["_config_error"])
    if args.since:
        try:
            dt.date.fromisoformat(args.since)
        except ValueError:
            errors.append(f"--since {args.since!r} is not YYYY-MM-DD; detecting the window instead")
            args.since = None

    since, window = resolve_window(cfg, args.since)
    # Discovered once and threaded through: it walks the filesystem, and both
    # identity detection and repository inspection need the same list.
    checkout_paths = discover_repos(cfg)
    me = identities(cfg, checkout_paths, net=not args.no_net)

    # git, gh, transcripts and the socket touch nothing in common, so the whole
    # collection is one fan-out. On this machine that is ~1s instead of ~5s.
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        f_repos = pool.submit(collect_repos, cfg, since, me, checkout_paths)
        f_prs = (
            pool.submit(collect_prs, cfg, since)
            if not args.no_net
            else None
        )
        f_sessions = pool.submit(collect_sessions, cfg, since)
        f_agents = pool.submit(collect_agents, cfg)

        repos, e = f_repos.result()
        errors += e
        if f_prs is None:
            prs: dict[str, Any] = {"mine": [], "review_requested": [], "merged_in_window": []}
        else:
            prs, e = f_prs.result()
            errors += e
        sessions, e = f_sessions.result()
        errors += e
        agents, e = f_agents.result()
        errors += e

    # Sessions are labelled once both halves are in: naming the repository a
    # session ran in needs the grouped checkouts, which only exist after the
    # git fan-out has finished.
    index = checkout_index(repos)
    for session in sessions:
        session["repo"] = label_for_cwd(session.get("cwd"), index)

    active = [r for r in repos if r["commits"] or r["dirty"] or r["ahead"]]
    return {
        "schema": SCHEMA,
        "generated_at": now.isoformat(),
        "window": window,
        "identities": sorted(me),
        "totals": {
            "commits": sum(len(r["commits"]) for r in repos),
            "repos_touched": len([r for r in repos if r["commits"]]),
            "repos_active": len(active),
            "repos_scanned": len(repos),
            "checkouts_scanned": sum(len(r["checkouts"]) for r in repos),
            "sessions": len(sessions),
            "prs_open": len(prs["mine"]),
            "prs_merged": len(prs["merged_in_window"]),
            "reviews_requested": len(prs["review_requested"]),
            "agents": len(agents),
            "agents_blocked": len([a for a in agents if a["status"] == "blocked"]),
            "agents_done": len([a for a in agents if a["status"] == "done"]),
        },
        "repos": repos,
        "prs": prs,
        "sessions": sessions,
        "agents": agents,
        "attention": build_attention(repos, prs, sessions, agents, now, cfg),
        "errors": errors,
        "net": not args.no_net,
    }


# ---------------------------------------------------------------------------
# text rendering
# ---------------------------------------------------------------------------

SEV_MARK = {SEV_BLOCKED: "!!", SEV_WAITING: " !", SEV_LOOSE: " ·", SEV_TIDY: "  "}


def render_text(doc: dict[str, Any]) -> str:
    out: list[str] = []
    t = doc["totals"]
    out.append(f"daybook — {doc['window']['label']}")
    out.append(
        f"  {t['commits']} commits in {t['repos_touched']} repos · {t['sessions']} sessions · "
        f"{t['prs_open']} open PRs · {t['prs_merged']} merged · {t['agents']} agents"
    )

    if doc["attention"]:
        out.append("")
        out.append("Open loops")
        for item in doc["attention"]:
            out.append(f"  {SEV_MARK.get(item['severity'], '  ')} {item['text']}")
            if item["hint"]:
                out.append(f"       → {item['hint']}")

    touched = [r for r in doc["repos"] if r["commits"] or r["dirty"] or r["ahead"]]
    if touched:
        out.append("")
        out.append("Repos")
        for r in touched:
            bits = []
            if r["commits"]:
                bits.append(f"{len(r['commits'])} commits")
            if r["dirty"]:
                bits.append(f"{r['dirty']} dirty")
            if r["ahead"]:
                bits.append(f"{r['ahead']} unpushed")
            if r["commits_by_others"]:
                bits.append(f"{r['commits_by_others']} by others")
            out.append(f"  {r['name']} — {', '.join(bits)}")
            for co in r["checkouts"]:
                marks = []
                if co["dirty"]:
                    marks.append(f"{co['dirty']} dirty")
                if co["ahead"]:
                    marks.append(f"{co['ahead']} unpushed")
                if not marks:
                    continue
                kind = "worktree" if co["is_worktree"] else "checkout"
                out.append(f"      [{co['branch']}] {kind}: {', '.join(marks)}")
            for c in r["commits"][:4]:
                out.append(f"      {c['sha']} {c['subject']}")
            if len(r["commits"]) > 4:
                out.append(f"      … {len(r['commits']) - 4} more")

    if doc["prs"]["merged_in_window"]:
        out.append("")
        out.append("Merged")
        for p in doc["prs"]["merged_in_window"]:
            out.append(f"  {p['name']}#{p['number']} {p['title']}")

    if doc["sessions"]:
        out.append("")
        out.append("Sessions")
        for s in doc["sessions"][:12]:
            flag = " (unanswered)" if s["unanswered"] else ""
            out.append(f"  {s['repo']}: {s['title']}{flag}")

    if doc["agents"]:
        out.append("")
        out.append("Agents")
        for a in doc["agents"]:
            out.append(f"  {a['status']:<8} {a['pane_id']}  {a['title']}")

    if doc["errors"]:
        out.append("")
        out.append("Notes")
        for e in doc["errors"]:
            out.append(f"  - {e}")
    return "\n".join(out)


# ---------------------------------------------------------------------------
# cache + cli
# ---------------------------------------------------------------------------


def read_cache(max_age: float, net: bool = True, since: str | None = None) -> dict[str, Any] | None:
    """A cached document, if it answers the question actually being asked.

    Age is not the only thing that makes a cache wrong. A document collected with
    `--no-net` has empty pull-request lists, and serving that to a caller who
    wanted the network reports "no open PRs" with nothing in `errors` — the herdr
    pane refreshes offline every 30s, so without this check a `/standup` run
    would almost always read one. An explicit `--since` likewise describes a
    different window than whatever the cache was built for.
    """
    if max_age <= 0:
        return None
    try:
        age = dt.datetime.now().timestamp() - CACHE_PATH.stat().st_mtime
        if age > max_age:
            return None
        doc = json.loads(CACHE_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    if not isinstance(doc, dict) or doc.get("schema") != SCHEMA:
        return None  # a cache from an older shape is worse than no cache
    if net and not doc.get("net"):
        return None
    if since and (doc.get("window") or {}).get("since", "")[:10] != since:
        return None
    doc["from_cache"] = True
    doc["cache_age_seconds"] = round(age)
    return doc


def write_cache(doc: dict[str, Any]) -> None:
    try:
        CACHE_PATH.parent.mkdir(parents=True, exist_ok=True)
        # Write-then-rename so a reader never sees a half-written document. The
        # pid is in the temporary name because the herdr pane and a Claude
        # session collect independently and can easily overlap.
        tmp = CACHE_PATH.with_suffix(f".{os.getpid()}.tmp")
        tmp.write_text(json.dumps(doc), encoding="utf-8")
        # 0600, not the umask default. The document carries commit subjects,
        # pull-request titles and the opening line of Claude prompts — the same
        # material as the transcripts it is derived from, and there is no reason
        # for a second, world-readable copy of it.
        os.chmod(tmp, 0o600)
        tmp.replace(CACHE_PATH)
    except OSError:
        pass  # the cache is an optimisation, never a requirement


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        prog="daybook-collect",
        description="Gather the state of a working day into one JSON document.",
    )
    ap.add_argument("--text", action="store_true", help="render a plain-text frame instead of JSON")
    ap.add_argument("--since", metavar="YYYY-MM-DD", help="open the window on this day instead of detecting it")
    ap.add_argument("--no-net", action="store_true", help="skip gh; git, transcripts and herdr only")
    ap.add_argument(
        "--max-age",
        type=float,
        default=0,
        metavar="SECONDS",
        help="reuse the cached document if it is younger than this",
    )
    ap.add_argument("--no-cache", action="store_true", help="do not write the cache")
    ap.add_argument("--indent", type=int, default=None, help="pretty-print JSON with this indent")
    args = ap.parse_args(argv)

    doc = read_cache(args.max_age, net=not args.no_net, since=args.since)
    if doc is None:
        doc = collect(load_config(), args)
        if not args.no_cache:
            write_cache(doc)

    if args.text:
        print(render_text(doc))
    else:
        json.dump(doc, sys.stdout, indent=args.indent)
        sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
    except BrokenPipeError:
        # `daybook-collect | head` is a normal thing to do.
        os._exit(0)
