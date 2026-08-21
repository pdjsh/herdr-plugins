#!/usr/bin/env python3
"""Unit tests for the daybook collector.

`unittest` rather than pytest, and no fixtures on disk: the collector's whole
premise is that it runs with nothing installed, so its tests have to as well.

    python3 -m unittest discover -s tools/daybook

Everything under test here is a pure function. The subprocess-driven halves
(git, gh, herdr) are exercised by running the collector for real — see the
`--text` output in the README — because mocking three CLIs would test the mocks.
"""

from __future__ import annotations

import datetime as dt
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

# The module has a hyphen in its name, which `import` cannot spell.
_SPEC = importlib.util.spec_from_file_location(
    "daybook_collect", Path(__file__).with_name("daybook-collect.py")
)
assert _SPEC and _SPEC.loader
db = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(db)

NOW = dt.datetime(2026, 8, 21, 9, 0, tzinfo=dt.timezone.utc)


def checkout(**over):
    base = {
        "name": "repo",
        "path": "/r/repo",
        "repo_key": "/r/repo/.git",
        "is_worktree": False,
        "branch": "main",
        "staged": 0,
        "unstaged": 0,
        "untracked": 0,
        "dirty_paths": [],
        "dirty": 0,
        "behind": 0,
        "ahead": 0,
        "has_upstream": True,
        "stashes": 0,
        "last_commit_at": None,
        "base": "origin/main",
        "behind_base": 0,
    }
    base.update(over)
    return base


def repo(**over):
    base = {
        "key": "/r/repo/.git",
        "name": "repo",
        "path": "/r/repo",
        "base": "origin/main",
        "checkouts": [checkout()],
        "commits": [],
        "commits_by_others": 0,
        "fetched_hours_ago": 1.0,
        "dirty": 0,
        "ahead": 0,
        "stashes": 0,
    }
    base.update(over)
    return base


def pr(**over):
    base = {
        "repo": "org/repo",
        "name": "repo",
        "number": 1,
        "title": "a change",
        "url": "https://example.invalid/1",
        "draft": False,
        "created_at": "2026-08-01T00:00:00Z",
        "updated_at": "2026-08-20T00:00:00Z",
        "closed_at": None,
        "comments": 0,
        "labels": [],
    }
    base.update(over)
    return base


def attention(repos=None, prs=None, sessions=None, agents=None, cfg=None):
    return db.build_attention(
        repos if repos is not None else [],
        prs if prs is not None else {"mine": [], "review_requested": [], "merged_in_window": []},
        sessions if sessions is not None else [],
        agents if agents is not None else [],
        NOW,
        cfg if cfg is not None else dict(db.DEFAULT_CONFIG),
    )


def kinds(items):
    return [i["kind"] for i in items]


class TestHelpers(unittest.TestCase):
    def test_short_collapses_whitespace_and_clips(self):
        self.assertEqual(db.short("  a\n\n  b  "), "a b")
        self.assertEqual(db.short("abcdef", 4), "abc…")
        self.assertEqual(db.short(None), "")
        self.assertEqual(db.short(""), "")

    def test_parse_iso_accepts_every_shape_the_sources_emit(self):
        # gh emits Z, git emits an offset, and some transcript records are naive.
        self.assertEqual(db.parse_iso("2026-08-21T09:00:00Z").tzinfo, dt.timezone.utc)
        self.assertIsNotNone(db.parse_iso("2026-08-21T09:00:00+02:00"))
        self.assertEqual(db.parse_iso("2026-08-21T09:00:00").tzinfo, dt.timezone.utc)

    def test_parse_iso_returns_none_rather_than_raising(self):
        for bad in (None, "", "not a date", "2026-13-45"):
            self.assertIsNone(db.parse_iso(bad), bad)

    def test_real_prompt_ignores_tool_results(self):
        self.assertTrue(db.is_real_prompt("do the thing"))
        self.assertTrue(db.is_real_prompt([{"type": "text", "text": "do it"}]))
        self.assertFalse(db.is_real_prompt([{"type": "tool_result", "content": "ok"}]))
        self.assertFalse(db.is_real_prompt([{"type": "text", "text": "   "}]))
        self.assertFalse(db.is_real_prompt(""))
        self.assertFalse(db.is_real_prompt(None))


class TestCheckoutLabelling(unittest.TestCase):
    def setUp(self):
        self.index = db.checkout_index(
            [
                repo(
                    name="rating-processors",
                    checkouts=[
                        checkout(path="/r/rating-processors", branch="main"),
                        checkout(
                            path="/wt/rating-processors/feature-x",
                            branch="feature/x",
                            is_worktree=True,
                        ),
                    ],
                )
            ]
        )

    def test_subdirectory_resolves_to_the_repository(self):
        self.assertEqual(
            db.label_for_cwd("/r/rating-processors/src/defi", self.index), "rating-processors"
        )

    def test_worktree_is_named_by_repo_and_branch(self):
        # The directory itself is called `feature-x`, which says nothing about
        # which repository it belongs to.
        self.assertEqual(
            db.label_for_cwd("/wt/rating-processors/feature-x", self.index),
            "rating-processors [feature/x]",
        )

    def test_longest_prefix_wins(self):
        nested = db.checkout_index(
            [
                repo(
                    name="outer",
                    checkouts=[
                        checkout(path="/r/outer", branch="main"),
                        checkout(
                            path="/r/outer/.worktrees/inner", branch="feat", is_worktree=True
                        ),
                    ],
                )
            ]
        )
        self.assertEqual(db.label_for_cwd("/r/outer/.worktrees/inner/src", nested), "outer [feat]")

    def test_path_outside_every_checkout_keeps_its_own_name(self):
        self.assertEqual(db.label_for_cwd("/Users/x/.config", self.index), ".config")
        self.assertEqual(db.label_for_cwd(None, self.index), "(unknown)")

    def test_sibling_prefix_is_not_a_match(self):
        # `/r/rating-processors-old` must not resolve to `/r/rating-processors`.
        self.assertEqual(
            db.label_for_cwd("/r/rating-processors-old", self.index), "rating-processors-old"
        )


class TestAttention(unittest.TestCase):
    def test_severities_sort_blocked_first(self):
        items = attention(
            repos=[repo(checkouts=[checkout(dirty=3)], dirty=3)],
            prs={
                "mine": [pr(number=7, mergeable="CONFLICTING")],
                "review_requested": [pr(number=8)],
                "merged_in_window": [],
            },
        )
        self.assertEqual([i["severity"] for i in items], sorted(i["severity"] for i in items))
        self.assertEqual(items[0]["kind"], "pr-conflict")

    def test_failing_checks_are_blocking(self):
        items = attention(
            prs={
                "mine": [pr(checks_failing=["build", "test"])],
                "review_requested": [],
                "merged_in_window": [],
            }
        )
        self.assertIn("pr-ci-red", kinds(items))
        self.assertEqual(items[0]["severity"], db.SEV_BLOCKED)
        self.assertIn("build", items[0]["text"])

    def test_dormant_prs_collapse_into_one_line(self):
        old = [pr(number=n, updated_at="2024-01-01T00:00:00Z") for n in range(1, 6)]
        items = attention(prs={"mine": old, "review_requested": [], "merged_in_window": []})
        dormant = [i for i in items if i["kind"] == "pr-dormant"]
        self.assertEqual(len(dormant), 1)
        self.assertIn("5 open PR(s)", dormant[0]["text"])
        # …and none of them is also listed individually as merely stale.
        self.assertNotIn("pr-stale", kinds(items))

    def test_stale_band_is_between_stale_and_dormant(self):
        items = attention(
            prs={
                "mine": [pr(updated_at="2026-08-10T00:00:00Z")],  # 11 days
                "review_requested": [],
                "merged_in_window": [],
            }
        )
        self.assertIn("pr-stale", kinds(items))
        self.assertNotIn("pr-dormant", kinds(items))

    def test_fresh_pr_raises_nothing(self):
        items = attention(
            prs={
                "mine": [pr(updated_at="2026-08-20T18:00:00Z")],
                "review_requested": [],
                "merged_in_window": [],
            }
        )
        self.assertEqual(items, [])

    def test_draft_is_never_also_stale(self):
        items = attention(
            prs={
                "mine": [pr(draft=True, updated_at="2026-01-01T00:00:00Z")],
                "review_requested": [],
                "merged_in_window": [],
            }
        )
        self.assertEqual(kinds(items), ["pr-draft"])

    def test_dirty_is_reported_per_checkout_with_its_branch(self):
        items = attention(
            repos=[
                repo(
                    name="r",
                    checkouts=[
                        checkout(branch="main", dirty=0),
                        checkout(branch="feat/a", dirty=2, is_worktree=True),
                    ],
                    dirty=2,
                )
            ]
        )
        dirty = [i for i in items if i["kind"] == "dirty"]
        self.assertEqual(len(dirty), 1)
        self.assertIn("[feat/a]", dirty[0]["text"])

    def test_stashes_are_counted_once_per_repository(self):
        # refs/stash lives in the common git dir, so three worktrees see the
        # same four stashes.
        items = attention(
            repos=[
                repo(
                    checkouts=[checkout(stashes=4), checkout(stashes=4), checkout(stashes=4)],
                    stashes=4,
                )
            ]
        )
        self.assertEqual(kinds(items).count("stashes"), 1)

    def test_branch_with_no_upstream_is_flagged_unless_it_is_the_default(self):
        items = attention(repos=[repo(checkouts=[checkout(branch="feat/x", has_upstream=False)])])
        self.assertIn("no-upstream", kinds(items))
        for default in ("main", "master", "(detached)"):
            quiet = attention(
                repos=[repo(checkouts=[checkout(branch=default, has_upstream=False)])]
            )
            self.assertNotIn("no-upstream", kinds(quiet), default)

    def test_blocked_and_done_agents_outrank_loose_ends(self):
        items = attention(
            repos=[repo(checkouts=[checkout(dirty=1)], dirty=1)],
            agents=[
                {"pane_id": "w1:p1", "status": "blocked", "title": "t", "agent": "claude", "cwd": "/r", "session_id": "s1"},
                {"pane_id": "w2:p1", "status": "done", "title": "u", "agent": "claude", "cwd": "/r", "session_id": "s2"},
                {"pane_id": "w3:p1", "status": "idle", "title": "v", "agent": "claude", "cwd": "/r", "session_id": "s3"},
            ],
        )
        self.assertEqual(kinds(items)[:3], ["agent-blocked", "agent-done", "dirty"])

    def test_a_live_pane_is_not_an_abandoned_session(self):
        session = {
            "session_id": "s1",
            "title": "t",
            "unanswered": True,
            "cwd": "/r",
        }
        agent = {
            "pane_id": "w1:p1",
            "status": "working",
            "title": "t",
            "agent": "claude",
            "cwd": "/r",
            "session_id": "s1",
        }
        self.assertNotIn("session-unanswered", kinds(attention(sessions=[session], agents=[agent])))
        # The same session with no pane attached is a genuine loose end.
        self.assertIn("session-unanswered", kinds(attention(sessions=[session])))


class TestWindow(unittest.TestCase):
    def test_explicit_since_is_taken_verbatim(self):
        start, window = db.resolve_window(dict(db.DEFAULT_CONFIG), "2026-08-18")
        self.assertEqual(start.date(), dt.date(2026, 8, 18))
        self.assertEqual(window["source"], "explicit")
        self.assertIn("18 Aug", window["label"])

    def test_a_malformed_since_falls_through_to_detection(self):
        # Detection shells out to git, which is fine here: the assertion is only
        # that the bad value was not honoured.
        _, window = db.resolve_window({**db.DEFAULT_CONFIG, "repo_roots": [], "extra_repos": [], "worktree_roots": [], "claude_projects": "/nonexistent"}, "the day before")
        self.assertNotEqual(window["source"], "explicit")


class TestRendering(unittest.TestCase):
    def empty_doc(self):
        return {
            "schema": db.SCHEMA,
            "window": {"label": "since yesterday"},
            "totals": {
                "commits": 0,
                "repos_touched": 0,
                "sessions": 0,
                "prs_open": 0,
                "prs_merged": 0,
                "agents": 0,
            },
            "attention": [],
            "repos": [],
            "prs": {"mine": [], "review_requested": [], "merged_in_window": []},
            "sessions": [],
            "agents": [],
            "errors": [],
        }

    def test_a_quiet_day_still_renders(self):
        text = db.render_text(self.empty_doc())
        self.assertIn("since yesterday", text)
        self.assertNotIn("Open loops", text)

    def test_sections_appear_only_when_populated(self):
        doc = self.empty_doc()
        doc["attention"] = [
            {"severity": db.SEV_BLOCKED, "kind": "k", "text": "something broke", "hint": "fix it", "where": ""}
        ]
        doc["sessions"] = [{"repo": "r", "title": "t", "unanswered": True}]
        text = db.render_text(doc)
        self.assertIn("Open loops", text)
        self.assertIn("something broke", text)
        self.assertIn("→ fix it", text)
        self.assertIn("(unanswered)", text)
        self.assertNotIn("Merged", text)

    def test_every_severity_has_a_marker(self):
        for sev in (db.SEV_BLOCKED, db.SEV_WAITING, db.SEV_LOOSE, db.SEV_TIDY):
            self.assertIn(sev, db.SEV_MARK)


class TestPrFlattening(unittest.TestCase):
    def test_search_output_flattens_to_the_schema(self):
        flat = db.flatten_pr(
            {
                "number": 12,
                "title": "  a\n  wrapped title ",
                "repository": {"name": "repo", "nameWithOwner": "org/repo"},
                "url": "u",
                "isDraft": True,
                "commentsCount": 3,
                "labels": [{"name": "bug"}, {}],
            }
        )
        self.assertEqual(flat["repo"], "org/repo")
        self.assertEqual(flat["name"], "repo")
        self.assertEqual(flat["title"], "a wrapped title")
        self.assertTrue(flat["draft"])
        self.assertEqual(flat["labels"], ["bug"])

    def test_a_search_hit_missing_its_repository_still_flattens(self):
        flat = db.flatten_pr({"number": 1})
        self.assertEqual(flat["repo"], "")
        self.assertEqual(flat["comments"], 0)


class TestCache(unittest.TestCase):
    def test_a_cache_from_an_older_schema_is_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "latest.json"
            path.write_text(json.dumps({"schema": db.SCHEMA - 1}))
            original = db.CACHE_PATH
            try:
                db.CACHE_PATH = path
                self.assertIsNone(db.read_cache(3600))
            finally:
                db.CACHE_PATH = original

    def test_a_fresh_cache_round_trips_and_is_marked(self):
        with tempfile.TemporaryDirectory() as tmp:
            original = db.CACHE_PATH
            try:
                db.CACHE_PATH = Path(tmp) / "latest.json"
                db.write_cache({"schema": db.SCHEMA, "totals": {"commits": 4}})
                got = db.read_cache(3600)
                self.assertTrue(got["from_cache"])
                self.assertEqual(got["totals"]["commits"], 4)
            finally:
                db.CACHE_PATH = original

    def test_max_age_zero_never_reads_the_cache(self):
        self.assertIsNone(db.read_cache(0))

    def test_the_cache_is_not_world_readable(self):
        # It holds commit subjects, PR titles, and the first line of prompts.
        with tempfile.TemporaryDirectory() as tmp:
            original = db.CACHE_PATH
            try:
                db.CACHE_PATH = Path(tmp) / "latest.json"
                db.write_cache({"schema": db.SCHEMA})
                mode = db.CACHE_PATH.stat().st_mode & 0o777
                self.assertEqual(mode, 0o600, oct(mode))
            finally:
                db.CACHE_PATH = original

    def test_a_failed_write_is_swallowed(self):
        # The cache is an optimisation; an unwritable directory must not take
        # the whole briefing down.
        original = db.CACHE_PATH
        try:
            db.CACHE_PATH = Path("/nonexistent-root-dir/daybook/latest.json")
            db.write_cache({"schema": db.SCHEMA})
        finally:
            db.CACHE_PATH = original


if __name__ == "__main__":
    unittest.main()
