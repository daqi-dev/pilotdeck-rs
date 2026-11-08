#!/usr/bin/env python3
"""Real-baseline positive control and independent inventory deletion controls."""
import argparse
import copy
import json
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

    def test_channel_key_literal_drift_fails(self):
        # The #2283 pre-fix misalignment, reconstructed on the real loader.rs:
        # deleting one UNMIGRATED_CHANNEL_KEYS line leaves every entry below
        # it one line stale — each pointing at the adjacent next key, wecom
        # at the closing `];` (issue #2286). Range-only validation passed on
        # exactly this form; the key-literal assertion must go red per entry.
        loader = "src/pilotdeck-gateway/src/channel/loader.rs"
        drifted = {"gateway.channel:whatsapp": 52, "gateway.channel:signal": 53,
                   "gateway.channel:mattermost": 54, "gateway.channel:matrix": 55,
                   "gateway.channel:slack": 56, "gateway.channel:discord": 57,
                   "gateway.channel:telegram": 58, "gateway.channel:qq": 59,
                   "gateway.channel:wecom": 60}
        data = copy.deepcopy(self.data)
        for row in data["entries"]:
            if row["id"] in drifted:
                row["evidence"] = f"{loader}:{drifted[row['id']]}"
        errors = check(data, self.baseline)
        for key in drifted:
            self.assertIn("evidence key literal mismatch: " + key, errors)
        for unaffected in ("gateway.channel:bluebubbles", "gateway.channel:dingtalk",
                           "gateway.channel:email", "gateway.channel:homeassistant"):
            self.assertNotIn("evidence key literal mismatch: " + unaffected, errors)
        for row in data["entries"]:  # restore must go back to green
            if row["id"] in drifted:
                row["evidence"] = f"{loader}:{drifted[row['id']] - 1}"
        self.assertEqual(check(data, self.baseline), [])

    def test_line_numbers_count_only_real_newlines(self):
        # channel/sms/mod.rs really contains U+2028/U+2029 (in the line-1761
        # doc comment): splitlines() would count 1876 lines and admit 1875,
        # the "\n"/editor count is 1874 and must reject it.
        for number, valid in [(433, True), (1683, True), (1874, True), (1875, False), (1876, False)]:
            with self.subTest(line=number):
                self.assertIs(local_ref(f"src/pilotdeck-gateway/src/channel/sms/mod.rs:{number}"), valid)

    def test_evidence_forms_without_a_mechanical_key_stay_range_only(self):
        # Deliberate fail-open set (see plan): block-opening anchors
        # (sidecar:* at `for option in [`), match arms (`"user_dialog_claim" =>`),
        # non-bare key segments, non-.rs evidence — no single key-literal
        # form to assert, so these stay range-only by design, not by drift.
        for key, ref in [("sidecar:factory",
                          "src/pilotdeck-transport/src/bin/pilotdeck-agent-loop-sidecar.rs:26"),
                         ("gateway.method:user_dialog_claim",
                          "src/pilotdeck-gateway/src/native_process/methods/user_dialog.rs:142"),
                         ("gateway.channel:weird/key",
                          "src/pilotdeck-gateway/src/channel/loader.rs:50"),
                         ("gateway.background:always-on",
                          "specs/gateway-parity/diffs/A7.8-ALWAYS-ON-BACKGROUND-001.md:24")]:
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
        noisy = split_lines(path.read_text())
        # Anchor the interior comment to the table's own opening line, not
        # to a file-level absolute offset: any edit above UNMIGRATED_
        # CHANNEL_KEYS would silently push the insert outside the table and
        # turn this control into a no-comment rerun (#2286 fix-r2,
        # anthropic boundary P2 / simplify P2, two families).
        header = next(i for i, line in enumerate(noisy)
                      if "UNMIGRATED_CHANNEL_KEYS" in line)
        noisy.insert(header + 1, "    // churn")
        tables = {path: (noisy, literal_table_lines(noisy))}
        current = {row["id"]: int(row["evidence"].rsplit(":", 1)[1])
                   for row in self.data["entries"]
                   if row["id"] in ("gateway.channel:whatsapp", "gateway.channel:signal",
                                    "gateway.channel:mattermost", "gateway.channel:matrix",
                                    "gateway.channel:slack", "gateway.channel:discord",
                                    "gateway.channel:telegram", "gateway.channel:qq",
                                    "gateway.channel:wecom")}
        for key, number in current.items():
            with self.subTest(key=key, form="drifted"):
                self.assertEqual(
                    key_literal_error(key, f"{loader}:{number + 2}", tables),
                    "evidence key literal mismatch: " + key)
        for key, number in current.items():  # +1 (comment shift) restores green
            with self.subTest(key=key, form="restored"):
                self.assertIsNone(key_literal_error(key, f"{loader}:{number + 1}", tables))
        # A member line carrying a trailing // comment must stay green too:
        # literal_member admits it into the table, so the comparison has to
        # strip the comment instead of flagging a mismatch (fix-r2, DS
        # boundary P1 / sibling P1).
        annotated = split_lines(path.read_text())
        annotated.insert(header + 1, "    // churn")
        whatsapp = current["gateway.channel:whatsapp"] + 1  # on-key after shift
        annotated[whatsapp - 1] += "  // keep last"
        tables_annotated = {path: (annotated, literal_table_lines(annotated))}
        self.assertIsNone(key_literal_error(
            "gateway.channel:whatsapp", f"{loader}:{whatsapp}", tables_annotated))
        self.assertEqual(key_literal_error(
            "gateway.channel:whatsapp", f"{loader}:{whatsapp + 1}", tables_annotated),
            "evidence key literal mismatch: gateway.channel:whatsapp")

    def test_for_in_arrays_stay_range_only(self):
        # Non-binding arrays (a for-in list) never trigger the assertion.
        # The 1268 probe is the discriminating one: it anchors the
        # MISMATCHED key 'connection' on the "keep-alive" member line —
        # if the const/static gate were dropped and the list re-entered
        # the table, this would flag a mismatch (fix-r2; the old probe
        # keyed 1267, whose literal matches, and could never turn red).
        self.assertIsNone(key_literal_error(
            "gateway.header:connection",
            "src/pilotdeck-gateway/src/http_proxy.rs:1268"))
        self.assertIsNone(key_literal_error(
            "gateway.header:connection",
            "src/pilotdeck-gateway/src/http_proxy.rs:1267"))

    def test_unknown_express_shapes_fail(self):
        subprocess.run(["node", str(Path(__file__).with_name("test_shapes.mjs"))], check=True, timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, default=ROOT / "resources/PilotDeck")
    args, remaining = parser.parse_known_args()
    BASELINE = args.baseline
    unittest.main(argv=[__file__, *remaining])
