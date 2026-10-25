#!/usr/bin/env python3
"""test-smoke-real-provider.py — scripts/smoke/real_provider.py 的配对测试。

不联网、不读真实 `.env.local`、不发真实请求：只用临时文件和假进程覆盖
.env 解析、失败分类、退出码三态、空密钥自检、证据锁、超时进程组清理，
以及 probe 外层超时的机器分类。

用法：python3 -I scripts/test-smoke-real-provider.py
"""
from __future__ import annotations

import importlib.util
import json
import os
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "smoke" / "real_provider.py"

FAILS = 0


def load_module():
    spec = importlib.util.spec_from_file_location("real_provider", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


rp = load_module()


def check(name: str, condition: bool, detail: str = "") -> None:
    global FAILS
    if condition:
        print(f"PASS {name}")
    else:
        FAILS += 1
        print(f"FAIL {name}" + (f" — {detail}" if detail else ""))


def write_exec(path: Path, body: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def test_env_parsing() -> None:
    cases = [
        ('"sk-FAKE" # local', "sk-FAKE"),
        ("'glm-FAKE' # local", "glm-FAKE"),
        ("sk-FAKE # prod", "sk-FAKE"),
        ("sk-FAKE#prod", "sk-FAKE#prod"),
        ("  sk-FAKE  ", "sk-FAKE"),
        ("# only comment", ""),
        ('"unclosed', ""),
        ('""', ""),
    ]
    for raw, expected in cases:
        got = rp.parse_env_value(raw)
        check(f"parse_env_value({raw!r})", got == expected, f"got {got!r}")

    with tempfile.TemporaryDirectory() as d:
        path = Path(d) / ".env.local"
        path.write_text(
            "# comment\n"
            'export DEEPSEEK_API_KEY="sk-FAKE" # local\n'
            "ZAI_API_KEY = 'glm-FAKE' # trailing\n"
            "ANTHROPIC_API_KEY=sk-ANTH # prod\n"
            "OTHER_API_KEY=do-not-read\n"
            "NOEQUALS\n"
            "DEEPSEEK_API_KEY=second-value # duplicate wins\n",
            encoding="utf-8",
        )
        got = rp.read_env_file(path, {"DEEPSEEK_API_KEY", "ZAI_API_KEY",
                                      "ANTHROPIC_API_KEY"})
        expected = {
            "DEEPSEEK_API_KEY": "second-value",
            "ZAI_API_KEY": "glm-FAKE",
            "ANTHROPIC_API_KEY": "sk-ANTH",
        }
        check("read_env_file strips export/quotes/comments", got == expected, repr(got))
        check("read_env_file ignores unrelated names", "OTHER_API_KEY" not in got)

    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        check("env_candidates(non-git) is just <root>/.env.local",
              rp.env_candidates(root) == [root / ".env.local"])
        (root / ".env.local").write_text("K=v\n", encoding="utf-8")
        check("resolve_env_file picks the checkout copy",
              rp.resolve_env_file(root) == root / ".env.local")

    repo = SCRIPT.resolve().parents[2]
    candidates = rp.env_candidates(repo)
    check("env_candidates deduplicates", len(candidates) == len(set(candidates)))
    check("env_candidates starts at this checkout",
          candidates[0] == repo / ".env.local", repr(candidates))


def test_classify_failure() -> None:
    cases = [
        ({"kind": "timeout"}, "environment-or-provider (timeout)"),
        ({"kind": "harness"}, "harness (probe process failed; no product verdict)"),
        ({"status": "FAIL", "error": {"code": "auth_error", "message": "401"}},
         "environment-or-provider"),
        ({"status": "FAIL", "error": {"code": "server_error", "message": "502"}},
         "environment-or-provider"),
        ({"status": "FAIL", "error": {"code": "model_not_found"}},
         "environment-or-provider"),
        ({"status": "FAIL", "error": {"code": "connection_reset"}},
         "environment-or-provider"),
        ({"status": "FAIL", "error": {"code": "proxy_error"}},
         "environment-or-provider"),
        ({"status": "FAIL", "error": {"message": "TypeError: x is not a function"}},
         "product-or-protocol"),
        ({"status": "FAIL", "finishReason": "length", "text": ""},
         "environment-or-provider (token budget/thinking)"),
        ({"status": "FAIL", "finishReason": "length", "text": "partial"},
         "product-or-protocol"),
        # errors.rs:398 + anthropic.rs:522 + openai.rs:1170/1180: output
        # budget truncation is the same class as finishReason=length.
        ({"status": "FAIL", "error": {"code": "max_output_reached"}},
         "environment-or-provider (token budget/thinking)"),
        # prompt_too_long's paired code (errors.rs:338/502, fallback.rs:30).
        ({"status": "FAIL", "error": {"code": "context_overflow"}},
         "environment-or-provider"),
        ({"status": "FAIL", "error": {"code": "image_too_large"}},
         "environment-or-provider"),
        # provider_error is the gateway's fallback bucket (errors.rs:467),
        # including a 400 caused by a malformed request body: a product defect.
        ({"status": "FAIL", "error": {"code": "provider_error", "status": 400}},
         "product-or-protocol"),
        ({"status": "FAIL", "error": {"code": "invalid_request", "status": 400}},
         "product-or-protocol"),
        # An explicit 5xx is still an environment signal even when the code
        # fell back to provider_error.
        ({"status": "FAIL", "error": {"code": "provider_error", "status": 503}},
         "environment-or-provider"),
        # The probe now copies the canonical `status`. An upstream 5xx whose
        # non-canonical `type` became the code (`api_error`) is not in the
        # needle list, so status is the only environment signal.
        ({"status": "FAIL", "error": {"code": "api_error", "status": 503}},
         "environment-or-provider"),
    ]
    for payload, expected in cases:
        got = rp.classify_failure(payload)
        check(f"classify_failure({payload})", got == expected, got)


def test_probe_exit() -> None:
    passed = rp.apply_probe_exit({"status": "PASS"}, 0)
    check("probe exit 0 keeps PASS", passed["status"] == "PASS")

    crashed = rp.apply_probe_exit({"status": "PASS"}, 1)
    check("probe exit 1 downgrades PASS to FAIL",
          crashed["status"] == "FAIL" and crashed.get("kind") == "harness",
          repr(crashed))
    check("probe exit 1 keeps the original verdict in the detail",
          "after reporting PASS" in str(crashed.get("error")))

    failed = rp.apply_probe_exit({"status": "FAIL", "kind": "provider_error"}, 137)
    check("probe exit 137 does not rewrite an existing FAIL",
          failed["status"] == "FAIL" and "exitNote" in failed)


def test_self_check() -> None:
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        clean = directory / "clean.txt"
        clean.write_text("nothing secret here\n", encoding="utf-8")
        secret = "zz-FAKE-0123456789"
        empty = rp.self_check([clean], {})
        check("empty key set is not verifiable", empty["verifiable"] is False)
        check("empty key set fails the self-check", rp.self_check_ok(empty) is False)

        miss = rp.self_check([clean], {"K": secret})
        check("loaded key with no hit passes", rp.self_check_ok(miss) is True)
        leaked = directory / "leaked.txt"
        leaked.write_text(f"body {secret}\n", encoding="utf-8")
        hit = rp.self_check([clean, leaked], {"K": secret})
        check("loaded key hit fails", rp.self_check_ok(hit) is False)

        generic = directory / "generic.txt"
        # Build the fixture at runtime so this source (and its patch record)
        # never contains a literal that the generic secret scanner flags.
        generic.write_text("sk-" + "1234567890abcdef" + "\n", encoding="utf-8")
        generic_check = rp.self_check([clean, generic], {"K": secret})
        check("generic key pattern fails", rp.self_check_ok(generic_check) is False)

        # A concurrent run (or stale-lock reclaimer) can delete a file after
        # collect_artifacts() listed it. That must not raise FileNotFoundError
        # and look like a scenario FAIL.
        vanished = directory / "vanished.txt"
        vanished.write_text(f"body {secret}\n", encoding="utf-8")
        vanished.unlink()
        skipped = rp.self_check([clean, vanished], {"K": secret})
        check("vanished artifact is skipped, not a traceback",
              skipped.get("filesSkipped", 0) >= 1, repr(skipped))
        # A partial scan is not evidence of "no leak": the file list is short,
        # so 未命中 over it would be a false negative.
        check("vanished artifact marks the scan truncated",
              skipped.get("scanTruncated") is True, repr(skipped))
        check("vanished artifact does not make the self-check green",
              rp.self_check_ok(skipped) is False, repr(skipped))

        truncated = rp.ArtifactScan([clean], walk_errors=1,
                                    skipped_dirs=[str(directory / "gone")])
        truncated_check = rp.self_check(truncated, {"K": secret})
        check("walk error is surfaced as a truncated scan",
              truncated_check["walkErrors"] == 1
              and truncated_check["scanTruncated"] is True,
              repr(truncated_check))
        check("walk error fails the self-check",
              rp.self_check_ok(truncated_check) is False,
              repr(truncated_check))

        # Real os.walk(onerror=...) path: one unreadable directory must be
        # skipped while a sibling is still scanned, not abort the whole walk.
        nested = directory / "nested"
        nested.mkdir()
        (nested / "ok.txt").write_text("readable\n", encoding="utf-8")
        blocked_dir = nested / "blocked"
        blocked_dir.mkdir()
        (blocked_dir / "hidden.txt").write_text("unreadable\n", encoding="utf-8")
        os.chmod(blocked_dir, 0)
        try:
            scanned = rp.collect_artifacts(directory)
        finally:
            os.chmod(blocked_dir, 0o755)
        names = {path.name for path in scanned}
        check("collect_artifacts keeps walking after one directory error",
              "ok.txt" in names, repr(sorted(names)))
        check("collect_artifacts records the skipped directory",
              scanned.walk_errors >= 1, repr(scanned.skipped_dirs))
        check("a walk error fails the self-check",
              rp.self_check_ok(rp.self_check(scanned, {"K": secret})) is False)


def test_output_tokens() -> None:
    check("missing usage is None, not 0", rp.output_tokens({}) is None)
    check("missing outputTokens is None, not 0",
          rp.output_tokens({"usage": {}}) is None)
    check("bool is not accepted as a token count",
          rp.output_tokens({"usage": {"outputTokens": False}}) is None)
    check("negative is not accepted", rp.output_tokens(
        {"usage": {"outputTokens": -1}}) is None)
    check("reported usage is returned",
          rp.output_tokens({"usage": {"outputTokens": 23}}) == 23)


def empty_limits() -> dict:
    return {
        "requestsSent": 0,
        "requestsUnknown": [],
        "outputTokensReported": 0,
        "usageMissing": [],
        "echoMissing": [],
        "violations": [],
    }


def test_account_scenario() -> None:
    limits = empty_limits()
    payload = {"requests": 0}
    violations = rp.account_scenario(limits, "model_runtime_text", payload)
    check("missing usage produces no token violation (no data to compare)",
          violations == [], repr(violations))
    check("missing usage is recorded as missing, not 0",
          payload.get("usageStatus") == "missing"
          and limits["usageMissing"] == ["model_runtime_text"]
          and limits["outputTokensReported"] == 0,
          repr((payload, limits)))
    check("missing maxOutputTokens echo is recorded, not assumed",
          limits["echoMissing"] == ["model_runtime_text"], repr(limits))

    limits = empty_limits()
    payload = {"requests": 1, "requestsUnknown": True,
               "usage": {"outputTokens": 5}}
    rp.account_scenario(limits, "model_runtime_text", payload)
    check("unknown request count is not added to requestsSent",
          limits["requestsSent"] == 0
          and limits["requestsUnknown"] == ["model_runtime_text"],
          repr(limits))

    limits = empty_limits()
    payload = {"requests": 1, "usage": {"outputTokens": 129}}
    violations = rp.account_scenario(limits, "model_runtime_text", payload)
    check("per-request token cap is enforced",
          any("scenario outputTokens=129" in item for item in violations),
          repr(violations))
    check("reported usage is summed",
          limits["outputTokensReported"] == 129, repr(limits))

    limits = empty_limits()
    limits["outputTokensReported"] = 380
    payload = {"requests": 1, "usage": {"outputTokens": 10}}
    violations = rp.account_scenario(limits, "model_runtime_tool", payload)
    check("total token cap is enforced",
          any("total outputTokens=390" in item for item in violations),
          repr(violations))

    limits = empty_limits()
    payload = {"requests": 1, "maxOutputTokens": 99,
               "usage": {"outputTokens": 1}}
    violations = rp.account_scenario(limits, "model_runtime_text", payload)
    check("echoed maxOutputTokens mismatch is enforced",
          any("maxOutputTokens=99" in item for item in violations),
          repr(violations))

    # Request budget is checked after the fact, not only before the next
    # scenario: a probe retry can send 2 in one scenario and still report.
    limits = empty_limits()
    payload = {"requests": 2, "maxOutputTokens": 128,
               "usage": {"outputTokens": 1}}
    violations = rp.account_scenario(limits, "model_runtime_text", payload)
    check("more than one request in one scenario is a violation",
          any("sent 2 requests > 1" in item for item in violations),
          repr(violations))
    check("over-budget known requests are counted",
          limits["requestsSent"] == 2, repr(limits))

    limits = empty_limits()
    limits["requestsSent"] = 2
    payload = {"requests": 2, "maxOutputTokens": 128,
               "usage": {"outputTokens": 1}}
    violations = rp.account_scenario(limits, "model_runtime_tool", payload)
    check("known total above MAX_REQUESTS is a violation",
          any("total requests=4" in item for item in violations),
          repr(violations))

    limits = empty_limits()
    limits["requestsSent"] = 1
    limits["requestsUnknown"] = ["model_runtime_text"]
    payload = {"requests": 2, "maxOutputTokens": 128,
               "usage": {"outputTokens": 1}}
    violations = rp.account_scenario(limits, "model_runtime_tool", payload)
    check("known + unknown above MAX_REQUESTS is a violation",
          any("total requests=4" in item for item in violations),
          repr(violations))

    # Negative control for the budget: exactly at the cap is not a violation.
    limits = empty_limits()
    limits["requestsSent"] = 2
    payload = {"requests": 1, "maxOutputTokens": 128,
               "usage": {"outputTokens": 1}}
    violations = rp.account_scenario(limits, "model_runtime_tool", payload)
    check("exactly MAX_REQUESTS is not a budget violation",
          not any("total requests" in item for item in violations),
          repr(violations))

    # A well-formed echo is not recorded as missing.
    limits = empty_limits()
    payload = {"requests": 1, "maxOutputTokens": 128,
               "usage": {"outputTokens": 1}}
    rp.account_scenario(limits, "model_runtime_text", payload)
    check("present maxOutputTokens echo is not marked missing",
          limits["echoMissing"] == [], repr(limits))


def test_exit_codes() -> None:
    pass_report = {"scenarios": [
        {"layer": "model-runtime", "status": "PASS"},
        {"layer": "gateway-process", "status": "NOT_YET"},
    ]}
    check("all PASS exit 0", rp.final_exit_code(pass_report, True, 3) == rp.EXIT_OK)
    fail_report = {"scenarios": [
        {"layer": "model-runtime", "status": "FAIL"},
    ]}
    check("FAIL exit 1", rp.final_exit_code(fail_report, True, 3) == rp.EXIT_FAIL)
    blocked = {"scenarios": [{"layer": "model-runtime", "status": "BLOCKED"}]}
    check("BLOCKED with zero requests exit 3",
          rp.final_exit_code(blocked, True, 0) == rp.EXIT_BLOCKED)
    check("zero requests with self-check failure is still BLOCKED",
          rp.final_exit_code(blocked, False, 0) == rp.EXIT_BLOCKED)
    check("self-check failure after a run exit 2",
          rp.final_exit_code(pass_report, False, 3) == rp.EXIT_SELF_CHECK_FAILED)

    # README: exit 0 means *all* model-runtime scenarios PASS. A run that ran
    # out of request budget after two scenarios is partial, not green.
    mixed = {"scenarios": [
        {"layer": "model-runtime", "status": "PASS"},
        {"layer": "model-runtime", "status": "PASS"},
        {"layer": "model-runtime", "status": "BLOCKED"},
    ]}
    check("PASS+PASS+BLOCKED is partial exit 3",
          rp.final_exit_code(mixed, True, 2) == rp.EXIT_BLOCKED)
    one_blocked = {"scenarios": [
        {"layer": "model-runtime", "status": "PASS"},
        {"layer": "model-runtime", "status": "BLOCKED"},
    ]}
    check("PASS+BLOCKED is partial exit 3",
          rp.final_exit_code(one_blocked, True, 1) == rp.EXIT_BLOCKED)
    fail_and_blocked = {"scenarios": [
        {"layer": "model-runtime", "status": "FAIL"},
        {"layer": "model-runtime", "status": "BLOCKED"},
    ]}
    check("FAIL wins over BLOCKED (exit 1)",
          rp.final_exit_code(fail_and_blocked, True, 1) == rp.EXIT_FAIL)

    # Unverified token caps are not a green run: all scenarios PASS but no
    # usage / no echoed cap means the check never ran.
    usage_missing = {"scenarios": [
        {"layer": "model-runtime", "status": "PASS"},
    ], "limits": {"usageMissing": ["model_runtime_stream_cancel"],
                  "echoMissing": []}}
    check("all PASS but usage missing is BLOCKED (exit 3)",
          rp.final_exit_code(usage_missing, True, 1) == rp.EXIT_BLOCKED)
    echo_missing = {"scenarios": [
        {"layer": "model-runtime", "status": "PASS"},
    ], "limits": {"usageMissing": [],
                  "echoMissing": ["model_runtime_text"]}}
    check("all PASS but maxOutputTokens echo missing is BLOCKED (exit 3)",
          rp.final_exit_code(echo_missing, True, 1) == rp.EXIT_BLOCKED)
    verified = {"scenarios": [
        {"layer": "model-runtime", "status": "PASS"},
    ], "limits": {"usageMissing": [], "echoMissing": []}}
    check("all PASS with usage and echo verified is exit 0",
          rp.final_exit_code(verified, True, 1) == rp.EXIT_OK)
    fail_and_unverified = {"scenarios": [
        {"layer": "model-runtime", "status": "FAIL"},
    ], "limits": {"usageMissing": ["model_runtime_text"],
                  "echoMissing": []}}
    check("FAIL still wins over unverified (exit 1)",
          rp.final_exit_code(fail_and_unverified, True, 1) == rp.EXIT_FAIL)


def test_lock() -> None:
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        first = rp.EvidenceLock(evidence)
        ok, _ = first.acquire()
        check("first lock acquire", ok)
        second = rp.EvidenceLock(evidence)
        ok, reason = second.acquire()
        check("second lock refuses", not ok and "another run" in reason, reason)
        first.release()
        ok, _ = second.acquire()
        check("lock can be reacquired after release", ok)
        second.release()

    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        stale = evidence / ".smoke.lock"
        stale.mkdir()
        (stale / "owner.json").write_text(
            json.dumps({"pid": 999999, "startedAt": "2020-01-01T00:00:00+00:00"}),
            encoding="utf-8",
        )
        lock = rp.EvidenceLock(evidence)
        ok, reason = lock.acquire()
        check("stale lock is reclaimed", ok, reason)
        lock.release()


def test_lock_ownerless_is_held() -> None:
    """A fresh `.smoke.lock` with no owner.json is a run in progress."""
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        (evidence / ".smoke.lock").mkdir()
        lock = rp.EvidenceLock(evidence)
        ok, reason = lock.acquire()
        check("ownerless lock is held, not reclaimed", not ok, reason)
        check("ownerless lock is left in place",
              (evidence / ".smoke.lock").is_dir(), reason)
        shutil.rmtree(evidence / ".smoke.lock")

    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock_path = evidence / ".smoke.lock"
        lock_path.mkdir()
        (lock_path / "owner.json").write_text(
            json.dumps({"startedAt": "2020-01-01T00:00:00+00:00"}),
            encoding="utf-8",
        )
        lock = rp.EvidenceLock(evidence)
        ok, reason = lock.acquire()
        check("owner without a valid pid is held, not reclaimed", not ok, reason)
        check("invalid-owner lock is left in place", lock_path.is_dir(), reason)
        shutil.rmtree(lock_path)


def test_lock_concurrent_acquire_window() -> None:
    """Hook between mkdir and the owner write: a second run must refuse.

    This is the exact window the old code misread as a stale lock: A creates
    the directory, B sees it without owner.json and renames it away. The
    deterministic hook makes the race reproducible without killing a process.
    """
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        entered = threading.Event()
        release = threading.Event()
        first = rp.EvidenceLock(evidence)

        def pause_before_owner() -> None:
            entered.set()
            release.wait(10)

        first.after_mkdir = pause_before_owner
        result: dict = {}

        def acquire_first() -> None:
            result["first"] = first.acquire()

        thread = threading.Thread(target=acquire_first)
        thread.start()
        try:
            check("first run reached the mkdir/owner window", entered.wait(10))
            second = rp.EvidenceLock(evidence)
            ok, reason = second.acquire()
            check("second run refuses inside the mkdir/owner window",
                  not ok and "without a readable owner.json" in reason, reason)
            check("first run's new lock directory survives",
                  (evidence / ".smoke.lock").is_dir(), reason)
        finally:
            release.set()
            thread.join(10)
        check("first run still holds after the window",
              result.get("first", (False, ""))[0] and first.held,
              repr(result))
        first.release()


def test_lock_reclaim_verifies_owner() -> None:
    """Reclaim re-reads the moved owner; a changed owner aborts the reclaim."""
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock_path = evidence / ".smoke.lock"
        lock_path.mkdir()
        dead_owner = {"pid": 999999, "startedAt": "2020-01-01T00:00:00+00:00"}
        (lock_path / "owner.json").write_text(
            json.dumps(dead_owner), encoding="utf-8"
        )
        lock = rp.EvidenceLock(evidence)

        def replace_owner(stale: Path) -> None:
            (stale / "owner.json").write_text(
                json.dumps({"pid": os.getpid(), "startedAt": "2030-01-01T00:00:00+00:00"}),
                encoding="utf-8",
            )

        lock.after_reclaim_rename = replace_owner
        ok, reason = lock.acquire()
        check("reclaim refuses when the owner changed after rename",
              not ok and "owner changed" in reason, reason)
        check("the changed lock is put back", lock_path.is_dir(), reason)
        owner = json.loads((lock_path / "owner.json").read_text(encoding="utf-8"))
        check("the put-back lock still belongs to the new owner",
              owner.get("pid") == os.getpid(), repr(owner))
        shutil.rmtree(lock_path)


def test_lock_release_checks_owner() -> None:
    """release() must not rmtree a lock that now belongs to another pid."""
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock = rp.EvidenceLock(evidence)
        ok, reason = lock.acquire()
        check("release-owner test acquired the lock", ok, reason)
        (evidence / ".smoke.lock" / "owner.json").write_text(
            json.dumps({"pid": 1, "startedAt": "2020-01-01T00:00:00+00:00"}),
            encoding="utf-8",
        )
        lock.release()
        check("release leaves a lock owned by another pid",
              (evidence / ".smoke.lock").is_dir())
        check("release does not claim to still hold", lock.held is False)
        shutil.rmtree(evidence / ".smoke.lock")


def test_lock_age_escape_hatch() -> None:
    """A live-looking pid must not pin the lock forever.

    The pid can be reused (or `kill(pid, 0)` can raise PermissionError for a
    process owned by another uid). A lock older than the cap is reclaimed; a
    young one with a live pid is still held.
    """
    old = time.time() - 10 * 365 * 24 * 3600
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock_path = evidence / ".smoke.lock"
        lock_path.mkdir()
        (lock_path / "owner.json").write_text(
            json.dumps({"pid": os.getpid(),
                        "startedAt": "2020-01-01T00:00:00+00:00"}),
            encoding="utf-8",
        )
        os.utime(lock_path / "owner.json", (old, old))
        os.utime(lock_path, (old, old))
        lock = rp.EvidenceLock(evidence)
        lock.max_age_seconds = 60
        ok, reason = lock.acquire()
        check("alive pid with an over-age lock is reclaimed", ok, reason)
        check("age reclaim is recorded in the report note",
              "age cap" in lock.reclaim_note, repr(lock.reclaim_note))
        lock.release()

    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock_path = evidence / ".smoke.lock"
        lock_path.mkdir()
        (lock_path / "owner.json").write_text(
            json.dumps({"pid": os.getpid(), "startedAt": rp.iso_now()}),
            encoding="utf-8",
        )
        lock = rp.EvidenceLock(evidence)
        lock.max_age_seconds = 3600
        ok, reason = lock.acquire()
        check("alive pid with a young lock still refuses",
              not ok and "another run holds" in reason, reason)
        shutil.rmtree(lock_path)

    # PermissionError is "unknown", not proof of life: a young lock refuses,
    # an over-age lock uses the same age cap.
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock_path = evidence / ".smoke.lock"
        lock_path.mkdir()
        (lock_path / "owner.json").write_text(
            json.dumps({"pid": 4711, "startedAt": rp.iso_now()}),
            encoding="utf-8",
        )
        original_state = rp.pid_state
        rp.pid_state = lambda pid: "unknown"
        try:
            lock = rp.EvidenceLock(evidence)
            lock.max_age_seconds = 3600
            ok, reason = lock.acquire()
            check("unverifiable young pid refuses with an explanation",
                  not ok and "cannot verify owner pid" in reason, reason)
            (lock_path / "owner.json").write_text(
                json.dumps({"pid": 4711,
                            "startedAt": "2020-01-01T00:00:00+00:00"}),
                encoding="utf-8",
            )
            os.utime(lock_path / "owner.json", (old, old))
            os.utime(lock_path, (old, old))
            lock = rp.EvidenceLock(evidence)
            lock.max_age_seconds = 60
            ok, reason = lock.acquire()
            check("unverifiable over-age pid is reclaimed", ok, reason)
            check("unverifiable reclaim is recorded",
                  "unverifiable" in lock.reclaim_note,
                  repr(lock.reclaim_note))
            lock.release()
        finally:
            rp.pid_state = original_state


def test_lock_reclaim_checks_inode() -> None:
    """Same owner bytes but a different inode is a different lock."""
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock_path = evidence / ".smoke.lock"
        lock_path.mkdir()
        dead_owner = {"pid": 999999, "startedAt": "2020-01-01T00:00:00+00:00"}
        (lock_path / "owner.json").write_text(
            json.dumps(dead_owner), encoding="utf-8"
        )
        lock = rp.EvidenceLock(evidence)

        def swap_directory() -> None:
            # Same owner content, new inode: the path was re-created after the
            # reclaim decided the old directory was stale.
            shutil.rmtree(lock_path)
            lock_path.mkdir()
            (lock_path / "owner.json").write_text(
                json.dumps(dead_owner), encoding="utf-8"
            )

        lock.before_reclaim_rename = swap_directory
        ok, reason = lock.acquire()
        check("reclaim refuses when the directory inode changed",
              not ok and "identity changed" in reason, reason)
        check("the moved lock is put back over the empty reservation",
              lock_path.is_dir(), reason)
        check("no stale lock is left behind",
              not list(evidence.glob(".smoke.lock.stale-*")))
        check("the refused claim does not report held", lock.held is False)
        shutil.rmtree(lock_path)


def test_lock_failed_restore_keeps_reservation() -> None:
    """If the moved lock cannot be put back, keep the empty reservation.

    Removing it would reopen `.smoke.lock` while the moved owner may still
    believe it holds the lock. Fail-closed: leave an ownerless directory for
    the documented manual recovery, and do not let `abandon()` delete it.
    """
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock_path = evidence / ".smoke.lock"
        lock_path.mkdir()
        dead_owner = {"pid": 999999, "startedAt": "2020-01-01T00:00:00+00:00"}
        (lock_path / "owner.json").write_text(
            json.dumps(dead_owner), encoding="utf-8"
        )
        lock = rp.EvidenceLock(evidence)

        def swap_directory() -> None:
            shutil.rmtree(lock_path)
            lock_path.mkdir()
            (lock_path / "owner.json").write_text(
                json.dumps(dead_owner), encoding="utf-8"
            )

        lock.before_reclaim_rename = swap_directory
        real_rename = rp.os.rename
        calls = {"n": 0}

        def flaky_rename(src, dst):
            calls["n"] += 1
            if calls["n"] == 2:
                raise OSError("simulated restore failure")
            return real_rename(src, dst)

        rp.os.rename = flaky_rename
        try:
            ok, reason = lock.acquire()
        finally:
            rp.os.rename = real_rename
        check("failed restore still refuses the reclaim",
              not ok and "manual" in reason, reason)
        check("failed restore keeps the empty reservation",
              lock_path.is_dir(), reason)
        lock.abandon()
        check("abandon does not remove a kept reservation",
              lock_path.is_dir())
        check("the moved lock is kept for manual recovery",
              bool(list(evidence.glob(".smoke.lock.stale-*"))))
        shutil.rmtree(lock_path)
        for stale in evidence.glob(".smoke.lock.stale-*"):
            shutil.rmtree(stale)


def test_lock_release_checks_inode() -> None:
    """release() must not rmtree a replacement directory with our pid."""
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock = rp.EvidenceLock(evidence)
        ok, reason = lock.acquire()
        check("release-inode test acquired the lock", ok, reason)
        lock_path = evidence / ".smoke.lock"
        aside = evidence / ".smoke.lock.aside"
        os.rename(lock_path, aside)
        lock_path.mkdir()
        (lock_path / "owner.json").write_text(
            json.dumps({"pid": os.getpid(), "startedAt": rp.iso_now()}),
            encoding="utf-8",
        )
        lock.release()
        check("release leaves a replacement inode in place", lock_path.is_dir())
        check("release does not claim to hold", lock.held is False)
        shutil.rmtree(lock_path)
        shutil.rmtree(aside)


def test_lock_owner_write_survives_directory_move() -> None:
    """owner.json must be written relative to the fd, not the path name.

    This is the deterministic form of the three-process race: after our mkdir,
    a reclaimer moves our directory aside and a replacement lock appears at
    `.smoke.lock`. A path-based owner write lands in the replacement and makes
    two processes believe they own the same lock.
    """
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock_path = evidence / ".smoke.lock"
        moved = evidence / ".smoke.lock.moved"
        foreign = {"pid": 1, "startedAt": "2020-01-01T00:00:00+00:00"}
        lock = rp.EvidenceLock(evidence)

        def move_and_replace() -> None:
            os.rename(lock_path, moved)
            lock_path.mkdir()
            (lock_path / "owner.json").write_text(
                json.dumps(foreign), encoding="utf-8"
            )

        lock.after_mkdir = move_and_replace
        ok, reason = lock.acquire()
        check("owner write survives a directory move", ok, reason)
        moved_owner = json.loads(
            (moved / "owner.json").read_text(encoding="utf-8")
        )
        check("owner.json is written into the directory this process created",
              moved_owner.get("pid") == os.getpid(), repr(moved_owner))
        replacement_owner = json.loads(
            (lock_path / "owner.json").read_text(encoding="utf-8")
        )
        check("owner write does not overwrite a replacement lock",
              replacement_owner == foreign, repr(replacement_owner))
        lock.release()
        check("release does not delete the replacement directory",
              lock_path.is_dir())
        shutil.rmtree(lock_path)
        shutil.rmtree(moved)


def test_lock_abandon_after_interrupt() -> None:
    """Ctrl-C between mkdir and the owner write must not leave a stuck lock."""
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        lock = rp.EvidenceLock(evidence)

        def interrupt() -> None:
            raise KeyboardInterrupt

        lock.after_mkdir = interrupt
        raised = False
        try:
            lock.acquire()
        except KeyboardInterrupt:
            raised = True
        check("interrupted acquisition re-raises", raised)
        check("interrupted acquisition temporarily leaves its directory",
              (evidence / ".smoke.lock").is_dir())
        lock.abandon()
        check("abandon removes this attempt's partial lock",
              not (evidence / ".smoke.lock").exists())
        check("abandon does not claim to hold", lock.held is False)


def test_run_dirs() -> None:
    with tempfile.TemporaryDirectory() as d:
        evidence = Path(d)
        first = rp.make_run_dir(evidence)
        (first / "keep.txt").write_text("keep", encoding="utf-8")
        second = rp.make_run_dir(evidence)
        check("run directories are unique", first != second,
              f"{first.name} == {second.name}")
        check("both run directories exist", first.is_dir() and second.is_dir())
        check("existing run content is not overwritten",
              (first / "keep.txt").read_text(encoding="utf-8") == "keep")
        check("run directory names start with run-",
              first.name.startswith("run-") and second.name.startswith("run-"))


def test_run_bounded_timeout() -> None:
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        script = write_exec(
            directory / "spawn.sh",
            "#!/bin/sh\n"
            "sleep 30 &\n"
            "wait\n",
        )
        started = time.monotonic()
        result = rp.run_bounded(
            ["/bin/sh", str(script)], directory, os.environ.copy(), 0.5
        )
        elapsed = time.monotonic() - started
        check("run_bounded reports timeout",
              result["timedOut"] and result["exit"] != 0, repr(result))
        # The grandchild `sleep` inherits the pipes. If only the direct child
        # were killed, communicate() would block until sleep exits (~30 s).
        check("run_bounded kills the whole process group", elapsed < 5.0,
              f"elapsed={elapsed:.2f}s")


def test_run_bounded_keyboard_interrupt() -> None:
    """Ctrl-C must kill the detached child group before re-raising.

    The child is started with start_new_session=True, so it does not get the
    terminal's SIGINT. Without the BaseException branch it would keep sending
    real requests (or keep rustc writing) after the harness exits. This sends
    a real SIGINT to the test process while run_bounded() is inside
    communicate(), then checks the grandchild is gone.
    """
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        pidfile = directory / "grandchild.pid"
        script = write_exec(
            directory / "spawn.sh",
            "#!/bin/sh\n"
            "sleep 30 &\n"
            f"echo $! > {pidfile}\n"
            "wait\n",
        )

        def interrupt_when_running() -> None:
            for _ in range(100):
                if pidfile.is_file() and pidfile.read_text().strip():
                    break
                time.sleep(0.05)
            os.kill(os.getpid(), signal.SIGINT)

        timer = threading.Timer(0.1, interrupt_when_running)
        timer.start()
        raised = False
        try:
            try:
                rp.run_bounded(
                    ["/bin/sh", str(script)], directory, os.environ.copy(), 30
                )
            except KeyboardInterrupt:
                raised = True
        finally:
            timer.cancel()
        check("Ctrl-C is re-raised after cleanup", raised)
        try:
            grandchild = int(pidfile.read_text().strip())
        except (OSError, ValueError):
            check("Ctrl-C test started a grandchild", False, "no pid file")
            return
        gone = False
        for _ in range(100):
            try:
                os.kill(grandchild, 0)
            except ProcessLookupError:
                gone = True
                break
            time.sleep(0.05)
        check("Ctrl-C kills the whole child process group", gone,
              f"grandchild {grandchild} is still alive")


def test_run_bounded_popen_window() -> None:
    """Ctrl-C delivered inside `Popen` must still kill the child group.

    Without blocking SIGINT across `Popen`, the signal is raised after
    fork/exec but before the returned handle is assigned; the harness has no
    `proc` to kill and the detached grandchild survives. The injected
    `Popen` replacement sends a real SIGINT to this process while the mask is
    held, so the pending signal can only be delivered after the assignment.
    """
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        pidfile = directory / "grandchild.pid"
        script = write_exec(
            directory / "spawn.sh",
            "#!/bin/sh\n"
            "sleep 8 &\n"
            f"echo $! > {pidfile}\n"
            "wait\n",
        )
        real_popen = rp.subprocess.Popen

        def racing_popen(*args, **kwargs):
            handle = real_popen(*args, **kwargs)
            for _ in range(200):
                if pidfile.is_file() and pidfile.read_text().strip():
                    break
                time.sleep(0.01)
            os.kill(os.getpid(), signal.SIGINT)
            return handle

        rp.subprocess.Popen = racing_popen
        raised = False
        try:
            try:
                rp.run_bounded(
                    ["/bin/sh", str(script)], directory, os.environ.copy(), 30
                )
            except KeyboardInterrupt:
                raised = True
        finally:
            rp.subprocess.Popen = real_popen
        check("Ctrl-C inside Popen is re-raised after cleanup", raised)
        try:
            grandchild = int(pidfile.read_text().strip())
        except (OSError, ValueError):
            check("Popen-window test started a grandchild", False, "no pid file")
            return
        gone = False
        for _ in range(100):
            try:
                os.kill(grandchild, 0)
            except ProcessLookupError:
                gone = True
                break
            time.sleep(0.05)
        check("Ctrl-C inside Popen kills the whole child group", gone,
              f"grandchild {grandchild} is still alive")
        if not gone:
            try:
                os.kill(grandchild, signal.SIGKILL)
            except ProcessLookupError:
                pass


def test_build_timeout() -> None:
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        (directory / "scripts" / "smoke" / "probe").mkdir(parents=True)
        bindir = directory / "bin"
        bindir.mkdir()
        write_exec(bindir / "cargo", "#!/bin/sh\nsleep 30 &\nwait\n")
        evidence = directory / "evidence"
        evidence.mkdir()
        run_dir = evidence / "run-test"
        run_dir.mkdir()
        old_path = os.environ.get("PATH", "")
        old_timeout = rp.BUILD_TIMEOUT_SECONDS
        os.environ["PATH"] = f"{bindir}:{old_path}"
        rp.BUILD_TIMEOUT_SECONDS = 0.5
        try:
            started = time.monotonic()
            build = rp.build_probe(directory, evidence, run_dir, lambda text: text)
            elapsed = time.monotonic() - started
        finally:
            os.environ["PATH"] = old_path
            rp.BUILD_TIMEOUT_SECONDS = old_timeout
        check("build timeout is FAIL not traceback", build["status"] == "FAIL",
              repr(build))
        check("build timeout is flagged", build["timedOut"] is True)
        check("build timeout leaves a log", (run_dir / "probe-build.log").is_file())
        check("build timeout kills the process group", elapsed < 5.0,
              f"elapsed={elapsed:.2f}s")


def test_cargo_slots_path() -> None:
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        slots = write_exec(
            root / "scripts" / "cargo-slots.sh",
            "#!/bin/sh\necho 'cargo-slots: test 0 / 4'\nexit 0\n",
        )
        check("cargo-slots exists in the checkout", slots.is_file())
        result = rp.run_cargo_slots(root)
        check("run_cargo_slots uses root/scripts/cargo-slots.sh",
              result["status"] == "PASS", repr(result))

    with tempfile.TemporaryDirectory() as d:
        result = rp.run_cargo_slots(Path(d))
        check("missing cargo-slots is BLOCKED, never SKIPPED",
              result["status"] == "BLOCKED" and "missing" in result["detail"],
              repr(result))

    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        write_exec(root / "scripts" / "cargo-slots.sh", "#!/bin/sh\nsleep 30 &\nwait\n")
        old_timeout = rp.CARGO_SLOTS_TIMEOUT_SECONDS
        rp.CARGO_SLOTS_TIMEOUT_SECONDS = 0.4
        try:
            result = rp.run_cargo_slots(root)
        finally:
            rp.CARGO_SLOTS_TIMEOUT_SECONDS = old_timeout
        check("cargo-slots timeout is BLOCKED",
              result["status"] == "BLOCKED" and result["timedOut"] is True,
              repr(result))


def test_probe_exit_integration() -> None:
    """A probe that prints a valid PASS result and then crashes is a FAIL."""
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        binary = write_exec(
            directory / "fake-probe",
            "#!/bin/sh\n"
            "printf 'PROBE_RESULT {\"scenario\":\"text\",\"status\":\"PASS\","
            "\"requests\":1}\\n'\n"
            "exit 1\n",
        )
        run_dir = directory / "run"
        run_dir.mkdir()
        payload, code = rp.run_probe(
            binary, "text", "https://example.invalid", "m", "fake-key",
            run_dir, lambda text: text,
        )
        check("run_probe downgrades PASS when the probe exits nonzero",
              payload.get("status") == "FAIL"
              and payload.get("kind") == "harness"
              and code == 1,
              repr(payload))
        check("exit mismatch keeps the original verdict in the detail",
              "after reporting PASS" in str(payload.get("error")),
              repr(payload.get("error")))


def test_probe_startup_crash_counts_zero_requests() -> None:
    """A probe that dies before any request must not be charged one request.

    The old `setdefault("requests", 1)` turned a bad argv / startup panic into
    a phantom request. It is now 0/unknown (the budget still charges one slot
    for unknown, but the report does not claim a known request).
    """
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        binary = write_exec(directory / "fake-probe", "#!/bin/sh\nexit 2\n")
        run_dir = directory / "run"
        run_dir.mkdir()
        payload, code = rp.run_probe(
            binary, "text", "https://example.invalid", "m", "fake-key",
            run_dir, lambda text: text,
        )
        check("startup crash reports 0 known requests",
              payload.get("requests") == 0, repr(payload))
        check("startup crash marks the request count unknown",
              payload.get("requestsUnknown") is True, repr(payload))
        check("startup crash is a harness FAIL",
              payload.get("status") == "FAIL"
              and payload.get("kind") == "harness"
              and code == 2,
              repr(payload))


def test_probe_result_without_requests_is_unknown() -> None:
    """Early PROBE_RESULT failures (missing key/bad argv) send no request."""
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        binary = write_exec(
            directory / "fake-probe",
            "#!/bin/sh\n"
            "printf 'PROBE_RESULT {\"scenario\":\"text\",\"status\":\"FAIL\","
            "\"kind\":\"harness\"}\\n'\n"
            "exit 0\n",
        )
        run_dir = directory / "run"
        run_dir.mkdir()
        payload, _ = rp.run_probe(
            binary, "text", "https://example.invalid", "m", "fake-key",
            run_dir, lambda text: text,
        )
        check("PROBE_RESULT without requests defaults to 0",
              payload.get("requests") == 0, repr(payload))
        check("PROBE_RESULT without requests is marked unknown",
              payload.get("requestsUnknown") is True, repr(payload))


def test_probe_outer_timeout() -> None:
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        binary = write_exec(directory / "fake-probe", "#!/bin/sh\nsleep 30 &\nwait\n")
        run_dir = directory / "run"
        run_dir.mkdir()
        old_timeout = rp.PROBE_TIMEOUT_SECONDS
        rp.PROBE_TIMEOUT_SECONDS = 0.5
        try:
            started = time.monotonic()
            payload, code = rp.run_probe(
                binary, "text", "https://example.invalid", "m", "fake-key",
                run_dir, lambda text: text,
            )
            elapsed = time.monotonic() - started
        finally:
            rp.PROBE_TIMEOUT_SECONDS = old_timeout
        check("outer timeout synthesizes kind=timeout",
              payload.get("kind") == "timeout", repr(payload))
        check("outer timeout classifies as environment-or-provider (timeout)",
              rp.classify_failure(payload) == "environment-or-provider (timeout)")
        check("outer timeout is FAIL with a nonzero exit",
              payload.get("status") == "FAIL" and payload.get("exit") != 0)
        check("outer timeout counts one attempted request",
              payload.get("requests") == 1)
        check("outer timeout kills the process group", elapsed < 5.0,
              f"elapsed={elapsed:.2f}s")
        check("outer timeout keeps raw stdout/stderr",
              (run_dir / "raw" / "text.stdout.txt").is_file()
              and (run_dir / "raw" / "text.stderr.txt").is_file())


def test_build_probe_absolute_target() -> None:
    """CARGO_TARGET_DIR is resolved by cargo relative to the probe cwd.

    A relative evidence path must still produce an absolute target/binary, or
    the build succeeds in one directory while the harness checks another.
    """
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        (directory / "scripts" / "smoke" / "probe").mkdir(parents=True)
        bindir = directory / "bin"
        bindir.mkdir()
        write_exec(bindir / "cargo", "#!/bin/sh\nexit 0\n")
        evidence = directory / "evidence"
        evidence.mkdir()
        run_dir = evidence / "run"
        run_dir.mkdir()
        old_path = os.environ.get("PATH", "")
        old_cwd = os.getcwd()
        os.environ["PATH"] = f"{bindir}:{old_path}"
        os.chdir(directory)
        try:
            build = rp.build_probe(
                directory, Path("evidence"), run_dir, lambda text: text
            )
        finally:
            os.chdir(old_cwd)
            os.environ["PATH"] = old_path
        check("build target is absolute for a relative evidence dir",
              Path(build["binary"]).is_absolute(), build["binary"])


PROBE_RUST_TEST_TIMEOUT_SECONDS = 900


def run_probe_rust_tests(repo: Path, cargo: str, timeout: float) -> dict:
    """Run the probe crate tests through the group-killing runner.

    `cargo test` spawns rustc and the test binary. A bare
    `subprocess.run(timeout=...)` kills only the direct cargo child; the
    orphan test process keeps writing the shared target directory and
    `cargo-slots.sh` cannot see it (it only counts `cargo` processes). This
    helper is the single cargo-test call site so the timeout always kills the
    process group.
    """
    manifest = repo / "scripts" / "smoke" / "probe" / "Cargo.toml"
    return rp.run_bounded(
        [cargo, "test", "--offline", "--locked", "--manifest-path", str(manifest)],
        repo,
        os.environ.copy(),
        timeout,
    )


def test_probe_rust_tests() -> None:
    """Run the standalone probe crate's unit tests.

    The probe is its own workspace, so the product `cargo test --all` /
    clippy / fmt jobs never touch it. CI runs this Python suite, which now
    runs the Rust tests too. `cargo-slots.sh` is checked first (the repo cap
    is four concurrent cargo processes); a missing cargo or a full slot set
    is a hard failure here, not a silent skip, so a green suite cannot hide
    unexecuted probe tests.
    """
    repo = SCRIPT.resolve().parents[2]
    cargo = shutil.which("cargo")
    if cargo is None:
        check("probe Rust tests: cargo is on PATH", False, "cargo not found")
        return
    slots = rp.run_bounded(
        ["sh", str(repo / "scripts" / "cargo-slots.sh")],
        repo,
        os.environ.copy(),
        60,
    )
    if slots["timedOut"] or slots["exit"] != 0:
        check("probe Rust tests: cargo slot is available", False,
              (slots["stdout"] + slots["stderr"]).strip()[-300:])
        return
    try:
        result = run_probe_rust_tests(
            repo, cargo, PROBE_RUST_TEST_TIMEOUT_SECONDS
        )
    except OSError as exc:
        check("probe Rust tests pass", False, f"{type(exc).__name__}: {exc}")
        return
    tail = "\n".join((result["stdout"] + result["stderr"]).splitlines()[-15:])
    if result["timedOut"]:
        tail += (
            f"\n[harness] cargo test exceeded {PROBE_RUST_TEST_TIMEOUT_SECONDS}s; "
            "killed the whole process group"
        )
    check("probe Rust tests pass",
          result["exit"] == 0 and not result["timedOut"], tail)


def test_probe_rust_tests_timeout_kills_group() -> None:
    """The cargo-test runner must kill orphan rustc/test children on timeout."""
    with tempfile.TemporaryDirectory() as d:
        directory = Path(d)
        pidfile = directory / "grandchild.pid"
        fake_cargo = write_exec(
            directory / "cargo",
            "#!/bin/sh\n"
            "sleep 8 &\n"
            f"echo $! > {pidfile}\n"
            "wait\n",
        )
        started = time.monotonic()
        result = run_probe_rust_tests(directory, str(fake_cargo), 5.0)
        elapsed = time.monotonic() - started
        check("cargo test timeout is reported",
              result["timedOut"] is True, repr(result))
        check("cargo test timeout kills the process group", elapsed < 10.0,
              f"elapsed={elapsed:.2f}s")
        try:
            grandchild = int(pidfile.read_text().strip())
        except (OSError, ValueError):
            check("cargo timeout test started a grandchild", False, "no pid file")
            return
        gone = False
        for _ in range(100):
            try:
                os.kill(grandchild, 0)
            except ProcessLookupError:
                gone = True
                break
            time.sleep(0.05)
        check("cargo test timeout leaves no orphan test process", gone,
              f"grandchild {grandchild} is still alive")
        if not gone:
            try:
                os.kill(grandchild, signal.SIGKILL)
            except ProcessLookupError:
                pass


def test_constants() -> None:
    check("MAX_REQUESTS equals the scenario count",
          rp.MAX_REQUESTS == len(rp.SCENARIOS) == 3)
    check("MAX_TOTAL_OUTPUT_TOKENS is derived",
          rp.MAX_TOTAL_OUTPUT_TOKENS
          == rp.MAX_REQUESTS * rp.MAX_OUTPUT_TOKENS_PER_REQUEST)
    check("outer probe timeout exceeds the inner scenario timeout",
          rp.PROBE_TIMEOUT_SECONDS > rp.PROBE_SCENARIO_TIMEOUT_SECONDS)
    check("lock age cap exceeds one bounded run",
          rp.LOCK_MAX_AGE_SECONDS
          > rp.BUILD_TIMEOUT_SECONDS + rp.MAX_REQUESTS * rp.PROBE_TIMEOUT_SECONDS)


def main() -> int:
    test_env_parsing()
    test_classify_failure()
    test_probe_exit()
    test_self_check()
    test_output_tokens()
    test_account_scenario()
    test_exit_codes()
    test_lock()
    test_lock_ownerless_is_held()
    test_lock_concurrent_acquire_window()
    test_lock_reclaim_verifies_owner()
    test_lock_release_checks_owner()
    test_lock_age_escape_hatch()
    test_lock_reclaim_checks_inode()
    test_lock_failed_restore_keeps_reservation()
    test_lock_release_checks_inode()
    test_lock_owner_write_survives_directory_move()
    test_lock_abandon_after_interrupt()
    test_run_dirs()
    test_run_bounded_timeout()
    test_run_bounded_keyboard_interrupt()
    test_run_bounded_popen_window()
    test_build_timeout()
    test_cargo_slots_path()
    test_probe_exit_integration()
    test_probe_startup_crash_counts_zero_requests()
    test_probe_result_without_requests_is_unknown()
    test_probe_outer_timeout()
    test_build_probe_absolute_target()
    test_probe_rust_tests_timeout_kills_group()
    test_probe_rust_tests()
    test_constants()
    print(f"test-smoke-real-provider: {'OK' if not FAILS else f'{FAILS} FAIL'}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
