#!/usr/bin/env python3
"""Real-baseline positive control and independent inventory deletion controls."""
import argparse
import copy
import json
import subprocess
import unittest
from pathlib import Path

from check import LEDGER, ROOT, check, enumeration, read, unique
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

    def test_unknown_express_shapes_fail(self):
        subprocess.run(["node", str(Path(__file__).with_name("test_shapes.mjs"))], check=True, timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, default=ROOT / "resources/PilotDeck")
    args, remaining = parser.parse_known_args()
    BASELINE = args.baseline
    unittest.main(argv=[__file__, *remaining])
