#!/usr/bin/env python3
"""Real-baseline positive control and independent inventory deletion controls."""
import argparse
import copy
import json
import re
import subprocess
import unittest
from pathlib import Path

from check import (LEDGER, ROOT, check, enumeration, key_literal_error, local_ref,
                   literal_table_lines, read, split_lines, unique)
BASELINE = ROOT / "resources/PilotDeck"


class CoverageControls(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.data = read(LEDGER)
        cls.baseline = enumeration(BASELINE)

    def test_real_baseline_and_runtime_route_reconciliation(self):
        self.assertEqual(check(self.data, self.baseline), [])
        runtime = read(ROOT / "docs/experiments/exp008-ui-server-proxy/router-enumeration.json")
        routes = {("ALL" if row["method"] == "_ALL" else row["method"]) + " "
                  + (row["path"].rstrip("/") or "/") for row in runtime["routes"]}
        static = {row["id"].removeprefix("ui.http:") for row in self.baseline["entries"]
                  if row["id"].startswith("ui.http:")}
        self.assertEqual(static, routes)
        self.assertEqual(len(routes), 263)

    def test_delete_every_entry_fails(self):
        # Exercise every real row, including bins/defaults/aliases and each surface.
        for index, row in enumerate(self.data["entries"]):
            with self.subTest(entry=row["id"]):
                data = dict(self.data, entries=self.data["entries"][:index] + self.data["entries"][index + 1:])
                self.assertIn("missing entry: " + row["id"], check(data, self.baseline))

    def test_critical_nonregistered_cli_paths_are_enumerated(self):
        keys = {row["id"] for row in self.baseline["entries"]}
        for key in ["bin:pilotdeck", "bin:pilotdeck-agent-loop-sidecar", "cli:pilotdeck/<stdin-lines>",
                    "cli:pilotdeck/<argv-as-prompt>", "cli:pilotdeck/sessions/<default-list>",
                    "cli:pilotdeck/settings/<default-get>", "cli:pilotdeck/sessions/messages",
                    "cli:pilotdeck/gateway/setup/workwechat", "cli:pilotdeck/tui//permissions/clear",
                    "sidecar:factory", "sidecar:tcp", "gateway.channel:apiServer",
                    "ui.ws:/plugin-ws/*", "desktop.ipc:pilotdeck:update-pause",
                    "gateway.channel-http:webhook:POST /webhooks/:name",
                    "gateway.channel-http:wecom-callback:GET *",
                    "gateway.channel-http:sms:POST <webhookPath; default=/sms/incoming>",
                    "desktop.backend-dependency:pilotdeck-config"]:
            self.assertIn(key, keys)

    def test_bad_metadata_and_extra_duplicate_entries_fail(self):
        for field, value in [("group", ""), ("group", "missing"), ("status", "complete"),
                             ("status", ""), ("evidence", ""), ("evidence", "../outside.md"),
                             ("evidence", "docs/plan/006_phase-d-cli.md:999999"),
                             ("evidence", "docs/plan/006_phase-d-cli.md#不存在"),
                             ("sources", [])]:
            data = copy.deepcopy(self.data)
            data["entries"][0][field] = value
            self.assertTrue(check(data, self.baseline), (field, value))
        for key, value in [("stage", ""), ("issue", ""), ("task", ""), ("acceptance", ""),
                           ("task", "AGENTS.md"), ("acceptance", ".github/workflows/ci.yml")]:
            data = copy.deepcopy(self.data)
            data["groups"]["D1"][key] = value
            self.assertTrue(check(data, self.baseline), key)
        data = copy.deepcopy(self.data)
        data["groups"]["orphan"] = dict(data["groups"]["D1"])
        self.assertIn("unused product group: orphan", check(data, self.baseline))
        data = copy.deepcopy(self.data)
        data["entries"][0]["group"] = "D0"
        self.assertTrue(check(data, self.baseline))
        for name in ("BROKEN", "D1"):
            data = copy.deepcopy(self.data)
            data["groups"][name] = "not-an-object"
            self.assertIn(name + ": invalid group", check(data, self.baseline))
        for mode in ("duplicate", "extra", "hash", "baseline"):
            data = copy.deepcopy(self.data)
            if mode == "duplicate":
                data["entries"].append(data["entries"][0])
            elif mode == "extra":
                data["entries"].append(dict(data["entries"][0], id="cli:invented"))
            elif mode == "hash":
                data["source_hashes"].clear()
            else:
                data["baseline"] = "main"
            self.assertTrue(check(data, self.baseline), mode)

    def test_duplicate_json_members_are_rejected(self):
        with self.assertRaises(ValueError):
            json.loads('{"entries": [], "entries": []}', object_pairs_hook=unique)

    def loader_table_sample(self, source):
        # All channels now have module evidence. Keep the mutation control tied
        # to the real production loader table, without requiring any channel to
        # remain unmigrated solely to exercise the evidence checker.
        header = next(i for i, line in enumerate(source)
                      if re.fullmatch(r"\s*(?:pub(?:\([^)]*\))?\s+)?const\s+CHANNEL_LOADER_KEYS\b.*", line))
        end = next(i for i in range(header + 1, len(source)) if source[i].strip() == "];")
        anchors = {}
        for i in range(header + 1, end):
            match = re.fullmatch(r'\s*"([A-Za-z0-9_-]+)"\s*,?\s*(?://.*)?', source[i])
            if match:
                anchors["gateway.channel:" + match[1]] = i + 1
        self.assertGreaterEqual(len(anchors), 2)
        sample = copy.deepcopy(self.data)
        seen = set()
        for row in sample["entries"]:
            if row["id"] in anchors:
                row["evidence"] = "src/pilotdeck-gateway/src/channel/loader.rs:" + str(anchors[row["id"]])
                seen.add(row["id"])
        self.assertEqual(seen, set(anchors))
        self.assertEqual(check(sample, self.baseline), [])
        return sample, header

    def test_channel_key_literal_drift_fails(self):
        # The #2283 pre-fix misalignment, reconstructed on the real loader.rs:
        # every entry anchored at a member line drifts one line stale — each
        # pointing at the adjacent next key, wecom at the closing `];`
        # (issue #2286). Range-only validation passed on exactly this form;
        # the key-literal assertion must go red per entry.
        loader = "src/pilotdeck-gateway/src/channel/loader.rs"
        # Derived from the ledger and the table itself, not hardcoded
        # (#2286 fix-r3, DS sibling P2#1): every entry anchored at a member
        # line of the table drifts by exactly +1; entries anchored in
        # loader.rs but outside the literal table stay put.
        source = split_lines((ROOT / loader).read_text())
        sample, _ = self.loader_table_sample(source)
        inside = literal_table_lines(source)
        # The drift target is the next table line strictly below the anchor
        # (the adjacent key, the closing `];` for the last member), derived
        # from the table scan instead of a hardcoded +1: an interior
        # comment between members must not push the probe onto a
        # transparent line and false-red (r5 DS boundary P2#2). Anchors
        # keep their original line for the restore pass.
        drifted = {}
        for row in sample["entries"]:
            if not row["evidence"].startswith(loader + ":"):
                continue
            anchor = int(row["evidence"].rsplit(":", 1)[1])
            if anchor not in inside:
                continue
            nxt = min((l for l in inside if l > anchor), default=None)
            if nxt is not None:
                drifted[row["id"]] = (anchor, nxt)
        # never silently vacuous (r5 DS boundary P0#1 / sibling P2#2): a
        # table-scan regression that empties the derived set must red
        # here instead of vacuously passing the restore assertion. >=1,
        # not >=2 (r6 DS boundary P3#3/sibling P3#3): legitimate migration
        # may legally shrink the anchored set to a single entry, and one
        # entry still exercises both the red and the restore.
        self.assertGreaterEqual(len(drifted), 1)
        # Single snapshot for derivation AND validation (r6 DS
        # concurrency P2#2, the fix-r3 single-snapshot discipline): check()
        # accepts the same tables cache key_literal_error uses, so the
        # drift targets are asserted against the version they were
        # derived from even if a local writer touches loader.rs mid-test.
        snapshot = {(ROOT / loader).resolve(): (source, inside)}
        data = copy.deepcopy(sample)
        for row in data["entries"]:
            if row["id"] in drifted:
                row["evidence"] = f"{loader}:{drifted[row['id']][1]}"
        errors = check(data, self.baseline, tables=dict(snapshot))
        # every drifted entry goes red per entry — each now points at the
        # adjacent next key, wecom at the closing `];`. The restore below
        # proves the correct anchors stay green, which also rules out a
        # ruler that flags every table member regardless of correctness.
        for key in drifted:
            self.assertIn("evidence key literal mismatch: " + key, errors)
        for row in data["entries"]:  # restore must go back to green
            if row["id"] in drifted:
                row["evidence"] = f"{loader}:{drifted[row['id']][0]}"
        self.assertEqual(check(data, self.baseline, tables=dict(snapshot)), [])

    def test_line_numbers_count_only_real_newlines(self):
        # channel/sms/mod.rs really contains U+2028/U+2029 (in a doc
        # comment above parse_chat_id_from_session_key): splitlines()
        # counts two lines more than the "\n"/editor count. The boundary
        # is derived from the file itself — the module is live product
        # code and a hardcoded 1874/1875/1876 false-fails on any
        # unrelated edit (r5 DS sibling P1#1 / boundary P2#3: the same
        # anti-pattern fix-r3 removed everywhere else in this PR). n is
        # a pure str.count, NOT len(split_lines(text)) (r6 DS boundary
        # P1#1): deriving it from the ruler's own function made the three
        # local_ref assertions tautological — any split_lines regression
        # that miscounts would move n with it and stay green.
        module = "src/pilotdeck-gateway/src/channel/sms/mod.rs"
        text = (ROOT / module).read_text()
        n = text.count("\n") + (0 if text.endswith("\n") else 1)  # editor count
        m = len(text.splitlines())  # additionally cuts U+2028/U+2029
        # the premise: the extra separators are really there, otherwise
        # this control no longer discriminates the old splitlines count
        self.assertGreater(m, n)
        self.assertIs(local_ref(f"{module}:{n}"), True)
        self.assertIs(local_ref(f"{module}:{n + 1}"), False)
        self.assertIs(local_ref(f"{module}:{m}"), False)
        # local_ref re-reads the module, so prove the snapshot is still
        # current: a mid-test edit must red here instead of silently
        # desynchronizing the assertions (r6 DS concurrency P2#1).
        self.assertEqual((ROOT / module).read_text(), text)

    def test_evidence_forms_without_a_mechanical_key_stay_range_only(self):
        # Deliberate fail-open set (see plan): block-opening anchors
        # (sidecar:* at `for option in [`), match arms (`"user_dialog_claim" =>`),
        # non-bare key segments, non-.rs evidence — no single key-literal
        # form to assert, so these stay range-only by design, not by drift.
        # The three ledger-backed refs are sourced from the ledger itself
        # (r6 DS sibling P2#1): hardcoded absolute lines on live product
        # files silently degrade to a vacuous pass after unrelated edits.
        # The synthetic non-bare key is content-located on a real member
        # line so the adversarial anchoring survives table edits.
        ledger = {row["id"]: row.get("evidence") for row in self.data["entries"]}
        loader = "src/pilotdeck-gateway/src/channel/loader.rs"
        whatsapp = next(i for i, line in enumerate(
            split_lines((ROOT / loader).read_text()), 1)
            if line.strip() == '"whatsapp",')
        for key, ref in [("sidecar:factory", ledger["sidecar:factory"]),
                         ("gateway.method:user_dialog_claim",
                          ledger["gateway.method:user_dialog_claim"]),
                         ("gateway.channel:weird/key", f"{loader}:{whatsapp}"),
                         ("gateway.background:always-on",
                          ledger["gateway.background:always-on"])]:
            with self.subTest(key=key):
                self.assertIsNone(key_literal_error(key, ref))

    def test_literal_table_scan_tolerates_interior_comments_and_blanks(self):
        # Review P0 (#2286 fix-r1): a comment or blank line inside the table
        # used to break the strictly-consecutive member run and silently drop
        # the whole table back to range-only validation.
        clean = ['pub const T: &[&str] = &[', '    "a",', '    "b",', '];']
        noisy = ['pub const T: &[&str] = &[', '    "a", // first', '    // note',
                 '', '    "b",', '];']
        aborted = ['pub const T: &[&str] = &[', '    "a",', '    compute(),',
                   '    "b",', '];']
        self.assertEqual(literal_table_lines(clean), {2, 3, 4})
        self.assertEqual(literal_table_lines(noisy), {2, 5, 6})
        # other code inside the block keeps the members seen so far
        self.assertEqual(literal_table_lines(aborted), {2})
        # a table whose `];` is the file's last line still closes (the EOF
        # table is a table — review P2: range-only fallback is a false PASS)
        self.assertEqual(literal_table_lines(['pub const T: &[&str] = &[', '    "a",', '];']),
                         {2, 3})
        # a wrapped opener (rustfmt) and an opener with a trailing comment
        # are still tables (fix-r2, DS boundary P0 / sibling P2)
        self.assertEqual(literal_table_lines(
            ['pub const T: &[&str] =', '    &[', '    "a",', '    "b",', '];']), {3, 4, 5})
        self.assertEqual(literal_table_lines(
            ['pub const T: &[&str] = &[ // keep sorted', '    "a",', '];']), {2, 3})
        # a // comment between the declaration and the wrapped &[ is
        # transparent, same as inside the table (fix-r3, DS boundary P1 /
        # sibling P2#2)
        self.assertEqual(literal_table_lines(
            ['pub const T: &[&str] =', '    // why wrap', '    &[',
             '    "a",', '];']), {4, 5})
        # a wrapped opener that leaves the `&` on the declaration line
        # (`= &` ⏎ `[`) is the same rustfmt split and stays a table
        # (fix-r4, codex r3 boundary P0: the fix-r3 `=`/`,` tail alone
        # dropped this form back to range-only — a false-PASS regression)
        self.assertEqual(literal_table_lines(
            ['pub const T: &[&str] = &', '    [', '    "a",', '    "b",', '];']),
            {3, 4, 5})
        self.assertEqual(literal_table_lines(
            ['pub const T: &[&str] = &', '    // why wrap', '    [',
             '    "a",', '];']), {4, 5})
        # a closure body mentioning static is not a key table, even though
        # the line above a bare `[` declares a binding-shaped word
        # (fix-r3, DS boundary P2#2: title.rs / recovery.rs shapes)
        self.assertEqual(
            literal_table_lines(['static E: LazyLock<[u8; 1]> = LazyLock::new(|| {',
                                '    [', '    "a",', '];']), set())
        # a wrapped `&[` under a non-binding line stays out
        self.assertEqual(literal_table_lines(
            ['let xs =', '    &[', '    "a",', '];']), set())
        # non-binding arrays (let locals, for-in iteration lists, the
        # &'static lifetime) stay out
        self.assertEqual(literal_table_lines(['let xs = [', '    "a",', '];']), set())
        self.assertEqual(
            literal_table_lines(['for name in [', '    "a",', '] {']), set())
        self.assertEqual(
            literal_table_lines(["let xs: &'static [&str] = &[", '    "a",', '];']), set())

    def test_channel_key_literal_drift_with_interior_comment_fails(self):
        # The P0 negative control: the #2283-style adjacent-key drift on a
        # table that now carries an interior comment line must still go red
        # per entry (wecom lands on the closing `];`).
        loader = "src/pilotdeck-gateway/src/channel/loader.rs"
        path = (ROOT / loader).resolve()
        # One snapshot, two in-memory derivations: re-reading between the
        # noisy and annotated controls would reuse a stale header index if
        # anything edited the file in between (#2286 fix-r3, DS
        # concurrency P2#1). The anchor matches the declaration shape, not
        # any line mentioning the name — a doc comment above the table must
        # not steal it (DS sibling P2#3 / simplify P2#1).
        base = split_lines(path.read_text())
        sample, header = self.loader_table_sample(base)
        noisy = list(base)
        noisy.insert(header + 1, "    // churn")
        tables = {path: (noisy, literal_table_lines(noisy))}
        # The drifted set is derived from the ledger and the table itself,
        # not a hardcoded key list: channels migrate out of the table as
        # the port lands (slack left in #1344, bluebubbles in #1343 —
        # their ledger anchors moved to the new modules), and a hardcoded
        # list goes stale against exactly that and false-fails the merge
        # ref (#2286 fix-r5, the CI semantic-conflict catch). Any
        # gateway.channel:* entry anchored on a table member line drifts.
        inside_base = literal_table_lines(base)
        inside_noisy = literal_table_lines(noisy)
        # Anchors below the insert point only (r6 DS boundary P2#2 /
        # concurrency P3#3, two-family): literal_table_lines covers every
        # const table in the file, but the +1 comment shift applies only
        # to lines after the insert — an entry anchored above it (another
        # table's member) would get a wrong restored position and
        # false-red. header is 0-based, so 1-based anchors must satisfy
        # anchor - 1 >= header + 1, i.e. anchor > header + 1.
        current = {row["id"]: int(row["evidence"].rsplit(":", 1)[1])
                   for row in sample["entries"]
                   if row["evidence"].startswith(loader + ":")
                   and int(row["evidence"].rsplit(":", 1)[1]) in inside_base
                   and int(row["evidence"].rsplit(":", 1)[1]) > header + 1}
        self.assertGreaterEqual(len(current), 1)  # never silently vacuous
        for key, number in current.items():
            with self.subTest(key=key, form="drifted"):
                # next table line below the anchor (adjacent key, or the
                # closing `];` for the last member), shifted +1 by the
                # inserted comment — derived, not hardcoded +2, so an
                # interior comment between members cannot push the probe
                # onto a transparent line and false-red (r5 DS boundary
                # P2#2). default skip, same semantics as the sibling
                # test (r6 DS sibling P3#2): a partial-scan edge with no
                # table line below the anchor skips the probe instead of
                # silently falling back to a hardcoded offset.
                nxt = min((l for l in inside_base if l > number), default=None)
                if nxt is not None:
                    self.assertEqual(
                        key_literal_error(key, f"{loader}:{nxt + 1}", tables),
                        "evidence key literal mismatch: " + key)
        for key, number in current.items():  # +1 (comment shift) restores green
            with self.subTest(key=key, form="restored"):
                self.assertIsNone(key_literal_error(key, f"{loader}:{number + 1}", tables))
        # A member line carrying a trailing // comment must stay green too:
        # literal_member admits it into the table, so the comparison has to
        # strip the comment instead of flagging a mismatch (fix-r2, DS
        # boundary P1 / sibling P1). The first anchored key in table order
        # stands in for the hardcoded whatsapp (same derivation reason).
        annotated = list(base)
        annotated.insert(header + 1, "    // churn")
        first_key, first_number = min(current.items(), key=lambda kv: kv[1])
        on_key = first_number + 1  # on-key after shift
        after_key = min((l for l in inside_noisy if l > on_key), default=None)
        annotated[on_key - 1] += "  // keep last"
        tables_annotated = {path: (annotated, literal_table_lines(annotated))}
        self.assertIsNone(key_literal_error(
            first_key, f"{loader}:{on_key}", tables_annotated))
        if after_key is not None:
            self.assertEqual(key_literal_error(
                first_key, f"{loader}:{after_key}", tables_annotated),
                "evidence key literal mismatch: " + first_key)

    def test_for_in_arrays_stay_range_only(self):
        # Non-binding arrays (a for-in list) never trigger the assertion.
        # The 1268 probe is the discriminating one: it anchors the
        # MISMATCHED key 'connection' on the "keep-alive" member line —
        # if the const/static gate were dropped and the list re-entered
        # the table, this would flag a mismatch (fix-r2; the old probe
        # keyed 1267, whose literal matches, and could never turn red).
        proxy = "src/pilotdeck-gateway/src/http_proxy.rs"
        proxy_lines = split_lines((ROOT / proxy).read_text())
        keep_alive = next(i for i, line in enumerate(proxy_lines, 1)
                          if line.strip() == '"keep-alive",')
        connection = next(i for i, line in enumerate(proxy_lines, 1)
                          if line.strip() == '"connection",')
        self.assertIsNone(key_literal_error(
            "gateway.header:connection", f"{proxy}:{keep_alive}"))
        self.assertIsNone(key_literal_error(
            "gateway.header:connection", f"{proxy}:{connection}"))

    def test_unknown_express_shapes_fail(self):
        subprocess.run(["node", str(Path(__file__).with_name("test_shapes.mjs"))], check=True, timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, default=ROOT / "resources/PilotDeck")
    args, remaining = parser.parse_known_args()
    BASELINE = args.baseline
    unittest.main(argv=[__file__, *remaining])
