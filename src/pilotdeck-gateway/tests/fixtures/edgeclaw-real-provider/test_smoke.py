#!/usr/bin/env python3
# anchor-selfcheck: unsupported input-only unit controls; no source mutation definitions
"""#1120 input negative controls: no source mutations, cargo or real provider.

Every subTest is run before unittest reports the aggregate. Bad inputs must
be rejected by the same functions used for real observations and forwarding.
"""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import threading
import time
import sys
import unittest
from unittest.mock import patch
from urllib import error, request

ROOT = Path(__file__).resolve().parents[5]
spec = importlib.util.spec_from_file_location("edgeclaw_smoke", ROOT / "scripts/smoke/edgeclaw.py")
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)
SAMPLES = smoke.read_json(Path(__file__).with_name("cases.json"))


def observation(text="heron-settings.toml 43127"):
    return {"records": [{"content": text, "relativePath": "sample.md"}],
            "user-summary": {"identityBackground": [], "files": []},
            "recall": {"systemContext": text},
            "capture": {"captured": True, "normalizedMessages": SAMPLES[0]["messages"]},
            "before-flush": {"overview": {"pendingSessions": 1}},
            "after-flush": {"overview": {"pendingSessions": 0}},
            "flush": {"capturedSessions": 1, "failedSessions": 0},
            "entries": [{"relativePath": "sample.md"}], "index-traces": []}


def reply(**updates):
    result = {"model": smoke.MODEL, "usage": {"output_tokens": 4},
              "stop_reason": "end_turn", "content": [{"type": "text", "text": "{}"}]}
    result.update(updates)
    return result


def native_request(**updates):
    value = {"model": smoke.MODEL, "max_tokens": 32768,
             "messages": [{"role": "user", "content": "synthetic"}]}
    value.update(updates)
    return json.dumps(value).encode()


class Controls(unittest.TestCase):
    def test_multitoken_fact_requires_all_markers(self):
        partial = smoke.measure(SAMPLES[1], observation("British pytest"))
        self.assertFalse(partial["extracted"]["spelling"], "#1120 conjunctive fact")
        self.assertFalse(partial["recalled"]["spelling"], "#1120 conjunctive fact")

    def test_exact_facts_and_missing_fact_negative(self):
        good = smoke.measure(SAMPLES[0], observation())
        self.assertTrue(all(good["extracted"].values()), "#1120 positive extraction")
        self.assertTrue(all(good["recalled"].values()), "#1120 positive recall")
        for marker in ("heron-settings.toml", "43127"):
            with self.subTest(marker=marker):
                bad = smoke.measure(SAMPLES[0], observation(marker))
                self.assertFalse(all(bad["extracted"].values()), "#1120 missing fact must miss")
                self.assertFalse(all(bad["recalled"].values()), "#1120 missing recall must miss")

    def test_input_trace_and_l0_cannot_create_hits(self):
        data = observation("")
        for name in ("query", "input", "trace", "l0", "preview", "metadata"):
            data[name] = "heron-settings.toml 43127"
        data["records"][0]["description"] = "heron-settings.toml 43127"
        data["recall"]["trace"] = "heron-settings.toml 43127"
        quality = smoke.measure(SAMPLES[0], data)
        self.assertFalse(any(quality["extracted"].values()), "#1120 metadata is not durable content")
        self.assertFalse(any(quality["recalled"].values()), "#1120 trace is not injected context")

    def test_summary_content_counts_but_metadata_does_not(self):
        data = observation("")
        data["user-summary"] = {"identityBackground": ["43127"],
                                "files": [{"content": "heron-settings.toml"}]}
        self.assertTrue(all(smoke.measure(SAMPLES[0], data)["extracted"].values()), "#1120 actual summary content")
        data["user-summary"] = {"identityBackground": [], "files": [], "source": "heron-settings.toml 43127"}
        self.assertFalse(any(smoke.measure(SAMPLES[0], data)["extracted"].values()), "#1120 summary metadata excluded")

    def test_chatter_negative_and_unexpected_write(self):
        clean = observation("")
        clean["records"] = []
        self.assertFalse(smoke.measure(SAMPLES[2], clean)["chatter_only_any_write"], "#1120 empty memory positive")
        for text in ("紫色河马", "unrelated invented preference"):
            with self.subTest(text=text):
                quality = smoke.measure(SAMPLES[2], observation(text))
                self.assertTrue(quality["chatter_only_any_write"], "#1120 any pure-chatter durable write is contamination")
        self.assertTrue(smoke.measure(SAMPLES[0], observation("棉花糖"))["chatter_written"], "#1120 mixed chatter marker")

    def test_observation_integrity_negative_matrix(self):
        with tempfile.TemporaryDirectory() as temp:
            cell = Path(temp)
            good = observation()
            for name, value in good.items():
                smoke.write_json(cell / f"{name}.json", value)
            self.assertIn("extracted", smoke.observe(cell, SAMPLES[0], [{"verified": True}]), "#1120 complete observation")
            changes = [("capture", {}), ("before-flush", {"overview": {"pendingSessions": 0}}),
                       ("flush", {"capturedSessions": 1, "failedSessions": 1}),
                       ("flush", {"capturedSessions": 0, "failedSessions": 0}),
                       ("after-flush", {"overview": {"pendingSessions": 1}}),
                       ("records", [{}]), ("records", [None]), ("entries", []),
                       ("capture", {"captured": True, "normalizedMessages": []}),
                       ("user-summary", "wrong shape"),
                       ("recall", {}), ("recall", {"systemContext": None})]
            for name, value in changes:
                with self.subTest(name=name, value=value):
                    path = cell / f"{name}.json"
                    path.write_text(json.dumps(value))
                    with self.assertRaisesRegex(ValueError, "#1120", msg="#1120 malformed observation must block"):
                        smoke.observe(cell, SAMPLES[0], [{"verified": True}])
                    path.write_text(json.dumps(good[name]))
            for calls in ([], [{"verified": False}], [{}]):
                with self.subTest(calls=calls), self.assertRaisesRegex(ValueError, "#1120"):
                    smoke.observe(cell, SAMPLES[0], calls)

    def test_native_payload_limits(self):
        bounded = smoke.capped_payload(native_request())
        self.assertEqual(bounded["max_tokens"], 4096, "#1120 output cap")
        self.assertEqual(bounded["thinking"], {"type": "disabled"}, "#1120 no thinking budget")
        self.assertEqual(bounded["temperature"], 0, "#1120 identical temperature")
        for updates in ({"model": "wrong"}, {"stream": True}, {"messages": []},
                        {"max_tokens": 0}, {"max_tokens": True}, {"max_tokens": None}):
            with self.subTest(updates=updates), self.assertRaisesRegex(ValueError, "#1120"):
                smoke.capped_payload(native_request(**updates))
        with self.assertRaisesRegex(ValueError, "#1120 input byte cap"):
            smoke.capped_payload(b"x" * (smoke.MAX_INPUT_BYTES + 1))

    def test_usage_and_response_negative_matrix(self):
        self.assertEqual(smoke.verified_usage(reply(), 4096), 4, "#1120 positive usage")
        for updates in ({"usage": {}}, {"usage": {"output_tokens": True}},
                        {"usage": {"output_tokens": -1}}, {"usage": {"output_tokens": 4097}},
                        {"model": "wrong"}, {"stop_reason": "max_tokens"}, {"content": []}):
            with self.subTest(updates=updates), self.assertRaisesRegex(ValueError, "#1120"):
                smoke.verified_usage(reply(**updates), 4096)

    def test_relay_rejects_stale_cell_token_before_forwarding(self):
        with tempfile.TemporaryDirectory() as temp:
            relay = smoke.Relay(Path(temp), lambda s: s, {})
            relay.begin(Path(temp) / "first")
            stale = relay.token
            relay.begin(Path(temp) / "second")
            server = smoke.server_for(relay)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                with patch.object(relay, "forward", return_value=b"{}") as send:
                    url = f"http://127.0.0.1:{server.server_port}/v1/messages"
                    stale_request = request.Request(url, native_request(), {"x-api-key": stale})
                    with self.assertRaises(error.HTTPError, msg="#1120 stale cell cannot dispatch"):
                        try:
                            request.urlopen(stale_request, timeout=5)
                        except error.HTTPError as exc:
                            exc.close()
                            raise
                    self.assertEqual(send.call_count, 0, "#1120 stale token cannot use next cell budget")
                    valid = request.Request(url, native_request(), {"x-api-key": relay.token})
                    with request.urlopen(valid, timeout=5) as response:
                        self.assertEqual(response.status, 200, "#1120 current cell can dispatch")
                    self.assertEqual(send.call_count, 1, "#1120 one valid request")
            finally:
                server.shutdown()
                thread.join(5)
                server.server_close()

    def test_relay_budget_and_raw_failure_retention(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            relay = smoke.Relay(root, lambda s: s, {"expires_unix": time.time() + 600})
            relay.cell = root / "cell"
            raw = json.dumps(reply()).encode()
            result = {"stdout": json.dumps({"http_status": 200}) + "\n" + json.dumps({"hex": raw.hex()})
                      + "\n" + json.dumps({"complete": True}),
                      "stderr": "", "exit": 0, "timedOut": False}
            with patch.object(smoke.common, "run_bounded", return_value=result) as send:
                self.assertEqual(json.loads(relay.forward(native_request())), reply(), "#1120 forwarded real envelope shape")
                self.assertTrue((root / "dispatch-001.json").exists(), "#1120 pre-dispatch receipt")
                relay.cell_count = smoke.MAX_REQUESTS_PER_CELL
                with self.assertRaisesRegex(ValueError, "#1120 request budget", msg="#1120 per-cell budget guard"):
                    relay.forward(native_request())
                relay.cell_count = 0
                relay.count = smoke.MAX_REQUESTS
                with self.assertRaisesRegex(ValueError, "#1120 request budget", msg="#1120 total budget guard"):
                    relay.forward(native_request())
                self.assertEqual(send.call_count, 1, "#1120 exhausted budgets cannot reach provider")
            relay.count, relay.cell_count = 1, 0
            result["stdout"] = '{"http_status":200}\n' + json.dumps({"hex": b"not-json".hex()}) + '\n{"complete":true}'
            with patch.object(smoke.common, "run_bounded", return_value=result), self.assertRaises(ValueError):
                relay.forward(native_request())
            saved = smoke.read_json(root / "request-002.json")
            self.assertTrue(saved["response_text"] == "not-json", "#1120 invalid model bytes retained")

    def test_wall_timeout_and_expired_lease_never_pass(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            relay = smoke.Relay(root, lambda s: s, {"expires_unix": time.time() + 600})
            relay.cell = root / "cell"
            result = {"stdout": '{"http_status":200}\n' + json.dumps({"hex": b"partial".hex()})
                      + '\n{"complete":true}', "stderr": "", "exit": 0, "timedOut": True}
            with patch.object(smoke.common, "run_bounded", return_value=result):
                with self.assertRaisesRegex(ValueError, "#1120 bounded provider transport incomplete"):
                    relay.forward(native_request())
            self.assertEqual(smoke.read_json(root / "request-001.json")["response_text"], "partial", "#1120 timeout preserves received bytes")
            relay.lease["expires_unix"] = time.time() + 1
            with patch.object(smoke.common, "run_bounded") as send:
                with self.assertRaisesRegex(ValueError, "#1120 provider reservation expired"):
                    relay.forward(native_request())
                self.assertFalse(send.called, "#1120 expired lease never dispatches")

    def test_cargo_false_zero_with_ps_error_is_blocked(self):
        with tempfile.TemporaryDirectory() as temp:
            env = {"CARGO_TARGET_DIR": str(Path(temp) / "cargo-target"),
                   "CARGO_PROFILE_DEV_DEBUG": "line-tables-only",
                   "CARGO_PROFILE_TEST_DEBUG": "line-tables-only"}
            with patch.dict(os.environ, env), patch.object(smoke.common, "run_cargo_slots", return_value={
                    "exit": 0, "stdout": "cargo-slots: 0/4", "stderr": "ps: Operation not permitted"}), \
                    patch.object(smoke.common, "run_bounded") as build:
                with self.assertRaisesRegex(ValueError, "#1120 cargo slot count unavailable"):
                    smoke.build(Path(temp), lambda s: s)
                self.assertFalse(build.called, "#1120 unavailable slot count cannot start compiler")

    def test_expired_reservation_and_wrong_scope(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "lease.json"
            valid = {"issue": 1120, "owner": "test", "model": smoke.MODEL,
                     "provider_slots": 1, "expires_unix": 1000}
            path.write_text(json.dumps(valid))
            self.assertEqual(smoke.reservation(path, 500), valid, "#1120 valid reservation")
            for changes in ({"expires_unix": 550}, {"expires_unix": 99999}, {"owner": ""},
                            {"model": "wrong"}, {"issue": 1119}, {"provider_slots": 2}):
                with self.subTest(changes=changes):
                    path.write_text(json.dumps(valid | changes))
                    with self.assertRaisesRegex(ValueError, "#1120"):
                        smoke.reservation(path, 500)

    def test_one_reservation_cannot_be_consumed_twice(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "lease.json"
            smoke.write_json(path, {"issue": 1120, "model": smoke.MODEL, "owner": "offline control",
                                    "provider_slots": 1, "expires_unix": time.time() + 600})
            handle, lease = smoke.acquire_reservation(path)
            try:
                self.assertEqual(lease["inode"], [path.stat().st_dev, path.stat().st_ino], "#1120 locked lease inode")
                with self.assertRaises(BlockingIOError, msg="#1120 one reserved provider slot has one consumer"):
                    smoke.acquire_reservation(path)
            finally:
                handle.close()
            restored, _ = smoke.acquire_reservation(path)
            restored.close()

    def test_transport_worker_refuses_standalone_invocation(self):
        with tempfile.TemporaryDirectory() as temp:
            dispatch = Path(temp) / "dispatch.json"
            smoke.write_json(dispatch, {"parent_pid": -1})
            result = smoke.common.run_bounded([sys.executable, "-I", "-B", str(Path(smoke.__file__)),
                                               "--transport", str(dispatch)], ROOT,
                                              {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": temp}, 5)
            self.assertNotEqual(result["exit"], 0, "#1120 standalone worker rejected")
            self.assertIn("#1120 transport requires its owning harness parent", result["stderr"], "#1120 reject before credential lookup")
            self.assertFalse(dispatch.with_suffix(".sent").exists(), "#1120 no external dispatch receipt")

    def test_failed_calls_still_have_usage_accounting(self):
        relay = smoke.Relay(Path("unused"), lambda s: s, {})
        relay.count = 3
        relay.records = [{"response": reply(stop_reason="max_tokens")}, {"error": "timeout"}]
        result = smoke.accounting(relay)
        self.assertEqual(result["reported_output_tokens"], 4, "#1120 truncated usage still reported")
        self.assertEqual(result["unknown_usage_requests"], 2, "#1120 unfinished requests remain unknown")
        self.assertEqual(result["reserved_output_token_ceiling"], 3 * 4096, "#1120 all dispatches reserve caps")

    def test_malformed_responses_preserve_failure_accounting(self):
        for payload in (None, [], "unexpected", {"usage": None}, {"usage": []},
                        reply(usage={"output_tokens": True}), reply(usage={"output_tokens": -1})):
            with self.subTest(payload=payload), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                relay = smoke.Relay(root, lambda s: s, {"expires_unix": time.time() + 600})
                relay.cell = root / "cell"
                raw = json.dumps(payload)
                transport = {"stdout": '{"http_status":200}\n' + json.dumps({"hex": raw.encode().hex()})
                             + '\n{"complete":true}', "stderr": "", "exit": 0, "timedOut": False}
                with patch.object(smoke.common, "run_bounded", return_value=transport):
                    with self.assertRaisesRegex(ValueError, "#1120 missing/invalid/over-cap output usage"):
                        relay.forward(native_request())
                self.assertEqual(smoke.read_json(root / "request-001.json")["response_text"], raw,
                                 "#1120 malformed response evidence survives")
                result = smoke.accounting(relay)
                self.assertEqual(result["unknown_usage_requests"], 1, "#1120 malformed usage stays unknown")
                self.assertEqual(result["reported_output_tokens"], 0, "#1120 malformed usage cannot count tokens")
                self.assertEqual(result["verified_output_tokens"], 0, "#1120 malformed response never verified")
                smoke.write_json(root / "accounting.json", result)
                self.assertEqual(smoke.read_json(root / "accounting.json"), result,
                                 "#1120 failed response accounting can be published")

    def test_isolation_and_exclusive_evidence(self):
        with tempfile.TemporaryDirectory() as temp:
            cell = Path(temp)
            relay = smoke.Relay(cell, lambda s: s, {})
            with patch.dict(os.environ, {"ZAI_API_KEY": "must-not-inherit", "NODE_OPTIONS": "bad"}):
                env = smoke.isolated_env(cell, ROOT, relay, 12345)
            for key in ("HOME", "PILOT_HOME", "TMPDIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"):
                self.assertTrue(Path(env[key]).is_relative_to(cell), f"#1120 isolate {key}")
            self.assertNotIn("ZAI_API_KEY", env, "#1120 native children never inherit keys")
            self.assertNotIn("NODE_OPTIONS", env, "#1120 no inherited loader")
            path = cell / "exclusive.json"
            smoke.write_json(path, {"first": True})
            with self.assertRaises(FileExistsError, msg="#1120 exclusive evidence guard"):
                smoke.write_json(path, {"second": True})
            self.assertEqual(smoke.read_json(path), {"first": True}, "#1120 original evidence retained")

    def test_secret_scan_positive_and_prefix_negative(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "out.json"
            # Synthetic value assembled at runtime: not a credential, and kept
            # out of literal form so secret scanners do not need an allowlist.
            key = "".join(("synthetic", "Secret", "1120", "abcdef"))
            smoke.write_json(path, {"output": key}, smoke.common.redactor({"test": key}))
            self.assertTrue(smoke.common.self_check_ok(smoke.common.self_check([path], {"test": key})), "#1120 redaction positive")
            path.write_text(key[:8])
            self.assertFalse(smoke.common.self_check_ok(smoke.common.self_check([path], {"test": key})), "#1120 key prefix negative")
            self.assertFalse(smoke.common.self_check_ok(smoke.common.self_check([path], {})), "#1120 empty secrets never pass")

    def test_incomplete_groups_never_get_success_rate(self):
        row = {"case": SAMPLES[0]["id"], "implementation": "rust", "status": "OBSERVED",
               "quality": smoke.measure(SAMPLES[0], observation())}
        groups = smoke.comparison([row], SAMPLES)
        self.assertEqual(groups[0]["observed"], 1, "#1120 observed denominator")
        self.assertEqual(groups[0]["planned"], 3, "#1120 blocked attempts stay visible")
        with tempfile.TemporaryDirectory() as temp:
            smoke.report(Path(temp), groups)
            text = (Path(temp) / "comparison.md").read_text()
            self.assertIn("N/A", text, "#1120 partial groups are not rates")
            self.assertNotIn("100.0%", text, "#1120 cannot advertise partial 1/1")


if __name__ == "__main__":
    unittest.main(verbosity=2)
