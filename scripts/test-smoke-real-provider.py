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
import stat
import subprocess
import sys
import tempfile
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


def test_constants() -> None:
    check("MAX_REQUESTS equals the scenario count",
          rp.MAX_REQUESTS == len(rp.SCENARIOS) == 3)
    check("MAX_TOTAL_OUTPUT_TOKENS is derived",
          rp.MAX_TOTAL_OUTPUT_TOKENS
          == rp.MAX_REQUESTS * rp.MAX_OUTPUT_TOKENS_PER_REQUEST)
    check("outer probe timeout exceeds the inner scenario timeout",
          rp.PROBE_TIMEOUT_SECONDS > rp.PROBE_SCENARIO_TIMEOUT_SECONDS)


def main() -> int:
    test_env_parsing()
    test_classify_failure()
    test_probe_exit()
    test_self_check()
    test_exit_codes()
    test_lock()
    test_run_dirs()
    test_run_bounded_timeout()
    test_build_timeout()
    test_cargo_slots_path()
    test_probe_exit_integration()
    test_probe_outer_timeout()
    test_build_probe_absolute_target()
    test_constants()
    print(f"test-smoke-real-provider: {'OK' if not FAILS else f'{FAILS} FAIL'}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
