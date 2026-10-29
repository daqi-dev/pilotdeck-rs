#!/usr/bin/env python3
"""Mutation negative controls for the model-complete TS oracle.

Each control mutates this worktree only, compiles the selected target (the
model_complete integration oracle, or a lib unit test for the cap maintenance
controls), and requires a real assertion failure ("test result: FAILED") from
the mutated build. Compile errors do not count.

Recovery protocol (concurrency P2):
- Before any mutation the original bytes and their SHA-256 are persisted to a
  state directory *outside* the worktree, then the source is replaced
  atomically. A SIGTERM/SIGINT/SIGHUP handler restores the original
  synchronously and exits; SIGKILL cannot run a handler, so the next invocation
  refuses to run while the source still hashes to the recorded mutated bytes
  (or to an unknown value). `--recover` restores only a value that matches the
  recorded mutation; unknown edits are never overwritten.
- The cargo slot check runs *before* the mutation, waits a bounded time, and
  uses the same process-name criterion as scripts/cargo-slots.sh. `ps` is the
  primary probe; when the sandbox denies `ps`, `pgrep -x` for the known cargo
  entry points is the fallback, and a failure of both probes is fatal (never
  treated as zero).
- Every in-place run also enters `negative_process.mutating_sources`: it takes
  the shared `.negative-source.lock` and refuses to start unless the whole
  gateway product tree matches HEAD, so a residue from any SIGKILLed control
  (not just this script) blocks the next run instead of being adopted as the
  original. `--recover` is the deliberate exception: it restores a recorded
  mutant whose hash matches its own sidecar, not HEAD.
- Cargo runs in its own process group; on timeout the whole group is signalled
  (SIGTERM, then SIGKILL) and reaped before the source is restored. The group
  currently running is tracked, so a SIGTERM/SIGINT/SIGHUP while a test child is
  alive reclaims that whole group (bounded SIGTERM, then SIGKILL) and confirms it
  is gone before restoring; the handler is guarded against re-entry.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence  # noqa: E402
from negative_selection import baseline_passed  # noqa: E402
from negative_process import mutating_sources  # noqa: E402

ROOT = Path(__file__).resolve().parents[5]
HTTP = ROOT / "src/pilotdeck-gateway/src/runtime/model/http.rs"
MANIFEST = ROOT / "src/Cargo.toml"
TARGET = os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target"))
TEST = "native_complete_model_matches_real_ts_complete"
RETRY_TEST = "complete_429_is_retryable_but_is_not_retried"
TRANSPORT_TEST = "native_complete_transport_retry_matches_real_ts"
TIMEOUT_TEST = "native_complete_timeout_uses_provider_timeout_and_ignores_stream_timeout"
LIB_CAP_BOUNDARY_TEST = (
    "runtime::model::http::tests::complete_shape_probe_cap_boundary_is_exact_across_frames"
)
LIB_DEFAULT_LIMIT_TEST = "runtime::model::http::tests::shape_string_limit_defaults_to_v8_max"
LIB_RESPONSE_BYTES_TEST = (
    "runtime::model::http::tests::response_text_cap_counts_received_bytes_not_utf16_units"
)
LIB_RESPONSE_DRAIN_TEST = "runtime::model::http::tests::complete_response_text_overlong_body_reads_to_eof_then_throws_plain_error"

CARGO_CAP = int(os.environ.get("NEGCTL_CARGO_CAP", "4"))
SLOT_WAIT_SECONDS = float(os.environ.get("NEGCTL_SLOT_WAIT_SECONDS", "1800"))
TEST_TIMEOUT = float(os.environ.get("NEGCTL_TEST_TIMEOUT", "600"))
TERM_GRACE = float(os.environ.get("NEGCTL_TERM_GRACE", "10"))
SIGNAL_GRACE = float(os.environ.get("NEGCTL_SIGNAL_GRACE", "5"))
STATE_DIR = Path(
    os.environ.get("NEGCTL_STATE_DIR")
    or (
        Path(tempfile.gettempdir())
        / ("pdrs-negctl-" + hashlib.sha256(str(ROOT).encode()).hexdigest()[:16])
    )
)
STATE_FILE = STATE_DIR / "state.json"
ORIGINAL_FILE = STATE_DIR / "original.http.rs"

CONTROLS = [
    (
        "responses-delegates-to-chat-completions",
        '"openai-responses" => parse_openai_responses_complete_response(raw, provider_id),',
        '"openai-responses" => parse_openai_complete_response(raw, provider_id),',
        TEST,
    ),
    (
        "openai-finish-reason-uses-tool_calls",
        'Some("tool_calls" | "function_call") => "tool_call".into(),',
        'Some("tool_calls" | "function_call") => "tool_calls".into(),',
        TEST,
    ),
    (
        "responses-failed-finish-reason-unknown",
        'Some("failed" | "cancelled") => "error".into(),',
        'Some("failed" | "cancelled") => "unknown".into(),',
        TEST,
    ),
    (
        # Same-source third protocol: the refusal -> content_filter arm.
        "anthropic-refusal-finish-unknown",
        'Some("refusal") => "content_filter".into(),',
        'Some("refusal") => "unknown".into(),',
        TEST,
    ),
    (
        # The collision case now really reaches nextUniqueToolCallId's `_2`.
        "tool-id-suffix-starts-at-one",
        "            let mut suffix = 2u64;",
        "            let mut suffix = 1u64;",
        TEST,
    ),
    (
        "complete-retries-429",
        """            return Err(ModelStreamError::Provider(error));
        }

        let protocol_value = field(&provider, "protocol").clone();""",
        """            if attempt < max_retries {
                let (_, delay_ms) =
                    retry_decide_request(&provider, attempt, (options.random)());
                let _ = delay(delay_ms, options).await?;
                attempt += 1.0;
                continue;
            }
            return Err(ModelStreamError::Provider(error));
        }

        let protocol_value = field(&provider, "protocol").clone();""",
        RETRY_TEST,
    ),
    (
        # The TS bound is `attempt <= maxRetries`; this makes the inner retry
        # predicate inclusive too and changes the 1.5/1 cases from the pinned
        # request counts and exhausted-error shape.
        "complete-retry-inner-boundary-inclusive",
        "                if attempt < max_retries && is_retryable_request_error(&error) {",
        "                if attempt <= max_retries && is_retryable_request_error(&error) {",
        TRANSPORT_TEST,
    ),
    (
        # If complete() reused the stream idle resolver, the 300ms
        # streamTimeoutMs=10 case would fire at 10ms and report the stream
        # message instead of `Model request timed out after 300ms.`.
        "complete-timeout-uses-stream-idle",
        """        let ms = field(provider, "timeoutMs")
            .as_number()
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);""",
        """        let ms = idle_timeout_ms(provider, options);""",
        TIMEOUT_TEST,
    ),
    (
        # A tenfold timer still satisfies every lower bound (the fixture's
        # delays also pass), so only the per-case upper bound derived from the
        # baseline's requested timeout rejects it: 100ms -> ~1000ms against a
        # 600ms ceiling. This is the P2-1 false-PASS guard for duration scaling.
        "complete-timeout-duration-scaled-tenfold",
        """    let deadline = (!timeout_ms.is_nan() && timeout_ms != 0.0)
        .then(|| tokio::time::Instant::now() + duration(timeout_ms));""",
        """    let deadline = (!timeout_ms.is_nan() && timeout_ms != 0.0)
        .then(|| tokio::time::Instant::now() + duration(timeout_ms * 10.0));""",
        TIMEOUT_TEST,
    ),
    (
        # Drop the abort short-circuit: "request aborted" is not a premature
        # close, so the canonical retryable flag would retry 3 times.
        "complete-abort-not-prioritized",
        """fn is_retryable_request_error(error: &ModelStreamError) -> bool {
    if error.is_abort() {
        return false;
    }""",
        """fn is_retryable_request_error(error: &ModelStreamError) -> bool {
    if false && error.is_abort() {
        return false;
    }""",
        TRANSPORT_TEST,
    ),
    (
        # Add one wire field; the per-case method/path/body comparison must
        # reject it even though canonical response and request count match.
        "complete-request-body-extra-field",
        "    let final_body = providers::final_body(body, provider);",
        """    let final_body = {
        let mut value = providers::final_body(body, provider);
        if let JsValue::Object(object) = &mut value {
            object.insert("negctl_extra".into(), JsValue::Bool(true));
        }
        value
    };""",
        TEST,
    ),
    (
        "anthropic-usage-cache-read-dropped",
        '    let cache_read = read("cache_read_input_tokens");',
        "    let cache_read: Option<f64> = None;",
        TEST,
    ),
    (
        # Invert the dedupe condition: the duplicate is kept and the
        # non-matching mismatch case drops its first message block.
        "responses-output-text-dedupe-inverted",
        "    if JsString::from_utf16(rest) == *output_text {",
        "    if JsString::from_utf16(rest) != *output_text {",
        TEST,
    ),
    (
        "anthropic-stop-sequence-unknown",
        '        Some("end_turn" | "stop_sequence") => "stop".into(),',
        '        Some("end_turn") => "stop".into(),',
        TEST,
    ),
    (
        # The old helper decides emptiness on the raw string, so a
        # whitespace-only id is kept instead of generating a fresh id.
        "openai-tool-id-whitespace-kept",
        """            let id = id_state.choose(
                non_empty_trimmed_string(field(record, "id")).cloned(),
                generated,
            );""",
        """            let id = id_state.choose(non_empty_string(field(record, "id")).cloned(), generated);""",
        TEST,
    ),
    (
        # Bypass only the Anthropic shape predicate at the http.rs call site.
        # The first 200 `{}` is then accepted and the pinned fallback case
        # (2 requests, second-endpoint canonical) fails on request count.
        "anthropic-shape-check-bypassed",
        """                    let accepted = parsed.as_ref().is_some_and(|value| {
                        providers::expected_complete_response_shape(
                            field(provider, "protocol"),
                            value,
                        )
                    });""",
        """                    let accepted = parsed.as_ref().is_some_and(|value| {
                        is(field(provider, "protocol"), "anthropic")
                            || providers::expected_complete_response_shape(
                                field(provider, "protocol"),
                                value,
                            )
                    });""",
        TEST,
    ),
    (
        # Same call-site bypass for Responses: the first 200 `{}` must not be
        # accepted, so the target case's request count/canonical still hold.
        "responses-shape-check-bypassed",
        """                    let accepted = parsed.as_ref().is_some_and(|value| {
                        providers::expected_complete_response_shape(
                            field(provider, "protocol"),
                            value,
                        )
                    });""",
        """                    let accepted = parsed.as_ref().is_some_and(|value| {
                        is(field(provider, "protocol"), "openai-responses")
                            || providers::expected_complete_response_shape(
                                field(provider, "protocol"),
                                value,
                            )
                    });""",
        TEST,
    ),
    (
        # The shared reader's in-loop cap boundary must reject exactly
        # limit + 1 received bytes across frames. The target test also proves
        # the over-limit prefix is discarded before a later body read error;
        # widening the comparison keeps that prefix and fails the assertion.
        # The r7 anchor duplicated once #641 copied the loop into
        # `response_text`; r8 moved both onto the shared `CappedText::push`,
        # r9 switched the counter from UTF-16 units to received bytes (#647).
        "capped-text-cap-allows-limit-plus-one",
        """        self.bytes += data.len();
        if self.bytes > string_limit() {""",
        """        self.bytes += data.len();
        if self.bytes > string_limit() + 1 {""",
        LIB_CAP_BOUNDARY_TEST,
    ),
    (
        # The cap must count received UTF-8 bytes, not decoded UTF-16 units
        # (node v22.22 real-TS probe, _tmp/20261025-issue-647-probe/).
        # Reverting the counter to units lets the multibyte "bytes over the
        # cap, units under it" body pass, and the target test's expect_err
        # fails.
        "capped-text-cap-counts-utf16-units",
        """        self.bytes += data.len();""",
        """        self.bytes += String::from_utf8_lossy(data).encode_utf16().count();""",
        LIB_RESPONSE_BYTES_TEST,
    ),
    (
        # Equivalent to the r5 off-tree M9 control, now on the single shared
        # default tail. Both builds fall through to usize::MAX, so the pinned
        # V8 literal assertion (not a constant-vs-itself comparison) fails.
        "shape-probe-string-limit-default-removed",
        """pub(crate) fn string_limit() -> usize {
    #[cfg(test)]
    {
        if let Some(limit) = STRING_LIMIT.with(std::cell::Cell::get) {
            return limit;
        }
    }
    super::count_http::MAX_STRING_LENGTH
}""",
        """pub(crate) fn string_limit() -> usize {
    #[cfg(test)]
    {
        if let Some(limit) = STRING_LIMIT.with(std::cell::Cell::get) {
            return limit;
        }
    }
    usize::MAX
}""",
        LIB_DEFAULT_LIMIT_TEST,
    ),
    (
        # The reader must keep draining an over-limit body to EOF: breaking
        # out of the loop on the frame that crosses the cap leaves the
        # scripted body unpolled, so the target test's EOF-flag assertion
        # fails deterministically (r9 replaced the old 70ms wall-clock floor,
        # which only held while the frames were 40ms apart).
        "response-text-returns-before-eof",
        """                if let Some(data) = frame.data_ref() {
                    // The string conversion has not run yet; `push` keeps
                    // draining past the cap so EOF / the body idle timeout
                    // still decide and a body failure keeps priority over
                    // the string error.
                    capped.push(data);
                }""",
        """                if let Some(data) = frame.data_ref() {
                    // The string conversion has not run yet; `push` keeps
                    // draining past the cap so EOF / the body idle timeout
                    // still decide and a body failure keeps priority over
                    // the string error.
                    if !capped.push(data) {
                        break;
                    }
                }""",
        LIB_RESPONSE_DRAIN_TEST,
    ),
]

# These lib controls need a different cargo target than the integration
# oracle controls. Keep the existing runner/recovery protocol unchanged.
LIB_TESTS = {
    "capped-text-cap-allows-limit-plus-one",
    "capped-text-cap-counts-utf16-units",
    "shape-probe-string-limit-default-removed",
    "response-text-returns-before-eof",
}
# The baseline loop iterates full test names, not mutation names. Using the
# mutation name there selected the integration target and ran 0 lib tests with
# exit 0, so every lib baseline looked red and every lib mutant uncatchable
# (host r1).
LIB_TEST_NAMES = {control[3] for control in CONTROLS if control[0] in LIB_TESTS}

# These controls must fail on the newly pinned target case itself, not on some
# unrelated assertion or a compile error. The case name is part of the panic
# message (`<case>: request count` / `<case>: canonical response`), and lib
# test output repeats the full test path on its `... FAILED` line.
TARGET_CASE = {
    "anthropic-shape-check-bypassed": "anthropic-200-empty-endpoint-fallback:",
    "responses-shape-check-bypassed": "responses-200-empty-endpoint-fallback:",
    "capped-text-cap-allows-limit-plus-one":
        "#818 cap boundary: limit + 1 must reject the first candidate",
    "capped-text-cap-counts-utf16-units":
        "#818 byte cap: 9 received bytes must pass the 5-byte cap",
    "shape-probe-string-limit-default-removed":
        "#818 string limit defaults to v8 max",
    "response-text-returns-before-eof":
        "#818 overlong body reads to EOF before the error",
}

_active: dict | None = None
_active_proc: subprocess.Popen | None = None
_active_pgid: int | None = None
_restoring = False
_handling_signal = False


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def atomic_write(path: Path, data: bytes) -> None:
    """Write `data` to `path` via a same-directory temp file + fsync + replace."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=str(path.parent), prefix=path.name + ".", suffix=".tmp")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
        try:
            directory = os.open(str(path.parent), os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        except OSError:
            pass
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def load_state() -> dict | None:
    if not STATE_FILE.exists():
        return None
    try:
        return json.loads(STATE_FILE.read_text())
    except (OSError, ValueError) as error:
        raise SystemExit(f"negctl: unreadable recovery state {STATE_FILE}: {error}")


def clear_state() -> None:
    for path in (STATE_FILE, ORIGINAL_FILE):
        try:
            path.unlink()
        except FileNotFoundError:
            pass


def pid_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def group_alive(pgid: int) -> bool:
    """Whether any process in `pgid` still exists (zombies count until reaped)."""
    if pgid <= 0:
        return False
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def track_proc(proc: subprocess.Popen) -> None:
    global _active_proc, _active_pgid
    _active_proc = proc
    _active_pgid = proc.pid


def untrack_proc(proc: subprocess.Popen) -> None:
    global _active_proc, _active_pgid
    if _active_proc is proc:
        _active_proc = None
        _active_pgid = None


def reap_leader_nowait(leader: int) -> bool:
    """Reap the direct child with a raw waitpid; safe inside a signal handler.

    Popen.wait()/communicate() take an internal lock that the interrupted stack
    may already hold, so only os.waitpid(WNOHANG) is used here. Without this,
    the leader would stay a zombie and keep its process group visible.
    """
    try:
        reaped, _ = os.waitpid(leader, os.WNOHANG)
    except ChildProcessError:
        return True
    except OSError:
        return False
    return reaped == leader


def reclaim_group_for_signal(pgid: int) -> bool:
    """Bounded SIGTERM -> SIGKILL of `pgid`, safe inside a signal handler."""
    if not group_alive(pgid):
        return True
    try:
        os.killpg(pgid, signal.SIGTERM)
    except ProcessLookupError:
        return True
    except PermissionError:
        return False
    deadline = time.monotonic() + SIGNAL_GRACE
    while time.monotonic() < deadline:
        reap_leader_nowait(pgid)
        if not group_alive(pgid):
            return True
        time.sleep(0.05)
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        return True
    except PermissionError:
        return False
    deadline = time.monotonic() + SIGNAL_GRACE
    while time.monotonic() < deadline:
        reap_leader_nowait(pgid)
        if not group_alive(pgid):
            return True
        time.sleep(0.05)
    return not group_alive(pgid)


def state_matches_this_worktree(state: dict) -> bool:
    return state.get("root") == str(ROOT) and state.get("http") == str(HTTP)


def startup_check() -> None:
    """Refuse to run on a mutation left by an unclean exit (e.g. SIGKILL)."""
    def refuse(message: str) -> None:
        print(f"negctl: {message}", file=sys.stderr)
        raise SystemExit(3)

    state = load_state()
    if state is None:
        return
    if not state_matches_this_worktree(state):
        refuse(f"recovery state {STATE_FILE} belongs to another worktree")
    pid = int(state.get("pid", 0))
    if pid != os.getpid() and pid_alive(pid):
        refuse(f"a recorded control run is still active (pid {pid})")
    current = sha256(HTTP.read_bytes()) if HTTP.exists() else ""
    if current == state.get("original_sha256"):
        print("negctl: clearing stale recovery state (source is already the original)")
        clear_state()
        return
    if current == state.get("mutated_sha256"):
        refuse(
            "source matches an UNRECOVERED mutation from pid "
            f"{pid} ({state.get('control')}); run --recover before mutating again"
        )
    refuse(
        "source hash matches neither the recorded original nor mutation; "
        "refusing to overwrite unknown changes"
    )


def begin_mutation(control: str, mutated: bytes) -> None:
    global _active
    original = HTTP.read_bytes()
    original_sha = sha256(original)
    mutated_sha = sha256(mutated)
    if original == mutated:
        raise SystemExit(f"negctl: {control}: mutation did not change the source")
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    atomic_write(ORIGINAL_FILE, original)
    atomic_write(
        STATE_FILE,
        json.dumps(
            {
                "pid": os.getpid(),
                "root": str(ROOT),
                "http": str(HTTP),
                "control": control,
                "original_sha256": original_sha,
                "mutated_sha256": mutated_sha,
                "started_at": time.time(),
            },
            indent=1,
        ).encode(),
    )
    _active = {
        "original_sha256": original_sha,
        "mutated_sha256": mutated_sha,
        "control": control,
    }
    atomic_write(HTTP, mutated)


def restore_active() -> bool:
    """Restore only a source matching the recorded mutation or original."""
    global _active, _restoring
    if _active is None:
        return True
    if _restoring:
        return False
    _restoring = True
    try:
        current = sha256(HTTP.read_bytes())
        if current == _active["original_sha256"]:
            pass
        elif current == _active["mutated_sha256"]:
            original = ORIGINAL_FILE.read_bytes()
            if sha256(original) != _active["original_sha256"]:
                raise SystemExit("negctl: sidecar original SHA-256 mismatch; refusing")
            atomic_write(HTTP, original)
            if sha256(HTTP.read_bytes()) != _active["original_sha256"]:
                raise SystemExit("negctl: restore SHA-256 mismatch")
        else:
            print(
                "negctl: source changed to an unknown hash; refusing to overwrite it",
                file=sys.stderr,
            )
            return False
        clear_state()
        _active = None
        return True
    finally:
        _restoring = False


def safe_print(*args, **kwargs) -> None:
    """Best-effort logging: a closed stdout/stderr must not skip recovery."""
    try:
        print(*args, **kwargs)
    except BaseException:  # noqa: BLE001 - diagnostics only
        pass


def install_signal_handlers() -> None:
    def handler(signum: int, _frame) -> None:
        global _handling_signal
        # A second signal while the first handler is reclaiming/restoring must
        # not recurse into another bounded wait.
        if _handling_signal:
            return
        _handling_signal = True
        try:
            pgid = _active_pgid
            reaped = True
            try:
                if pgid is not None:
                    reaped = reclaim_group_for_signal(pgid)
            finally:
                # Restore even if the reclaim probe itself raised; the group
                # signal has already been sent at that point.
                restore_active()
            if pgid is not None:
                if reaped:
                    safe_print(
                        f"negctl: signal {signum}: reclaimed process group {pgid}",
                        flush=True,
                    )
                else:
                    safe_print(
                        f"negctl: signal {signum}: process group {pgid} still alive "
                        f"after SIGKILL grace {SIGNAL_GRACE}s",
                        file=sys.stderr,
                        flush=True,
                    )
        except BaseException as error:  # noqa: BLE001 - best effort before exit
            safe_print(
                f"negctl: signal {signum} cleanup failed: {error}",
                file=sys.stderr,
                flush=True,
            )
        finally:
            os._exit(128 + signum)

    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(signum, handler)


def slots_from_ps() -> tuple[int | None, str]:
    """Same criterion as scripts/cargo-slots.sh: first command word is cargo."""
    try:
        result = subprocess.run(["ps", "-axo", "command="], capture_output=True, text=True)
    except OSError as error:
        return None, f"ps failed: {error}"
    if result.returncode != 0:
        return None, f"ps exit {result.returncode}: {result.stderr.strip()}"
    count = 0
    for line in result.stdout.splitlines():
        parts = line.split()
        if parts and re.fullmatch(r"(?:.*/)?cargo(?:-[a-z]+)?", parts[0]):
            count += 1
    return count, ""


def slots_from_pgrep() -> tuple[int | None, str]:
    """Fallback when `ps` is denied: count known cargo entry-point names."""
    names = [
        "cargo",
        "cargo-clippy",
        "cargo-fmt",
        "cargo-test",
        "cargo-build",
        "cargo-check",
        "cargo-run",
        "cargo-bench",
        "cargo-doc",
    ]
    total = 0
    for name in names:
        try:
            result = subprocess.run(["pgrep", "-x", name], capture_output=True, text=True)
        except OSError as error:
            return None, f"pgrep -x {name} failed: {error}"
        if result.returncode == 0:
            total += len([line for line in result.stdout.splitlines() if line.strip()])
        elif result.returncode != 1:
            return None, f"pgrep -x {name} exit {result.returncode}: {result.stderr.strip()}"
    return total, ""


def cargo_slots() -> tuple[int, str, str]:
    # Documented gate; under a denied `ps` it misreports 0, so its output is
    # logged but never trusted as the count.
    script = subprocess.run(
        ["bash", "scripts/cargo-slots.sh", str(CARGO_CAP)],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=10,
    )
    note = (script.stdout + script.stderr).strip()
    count, error = slots_from_ps()
    source = "ps"
    if count is None:
        count, error = slots_from_pgrep()
        source = "pgrep"
    if count is None:
        raise SystemExit(
            "negctl: cannot count cargo slots (ps denied and pgrep failed: "
            f"{error}); refusing to mutate"
        )
    return count, source, note


def wait_for_slot() -> None:
    deadline = time.monotonic() + SLOT_WAIT_SECONDS
    while True:
        count, source, note = cargo_slots()
        if count < CARGO_CAP:
            print(f"negctl: cargo slot free: {count}/{CARGO_CAP} (probe={source}, {note})")
            return
        if time.monotonic() >= deadline:
            raise SystemExit(
                f"negctl: cargo slots stayed full ({count}/{CARGO_CAP}) for "
                f"{SLOT_WAIT_SECONDS}s; refusing to mutate"
            )
        time.sleep(10)


def _terminate_group(proc: subprocess.Popen) -> str:
    """SIGTERM the whole group, then SIGKILL it if any member remains."""
    pgid = proc.pid
    output = ""
    try:
        os.killpg(pgid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        output = proc.communicate(timeout=TERM_GRACE)[0] or ""
    except subprocess.TimeoutExpired:
        pass
    # The leader may exit while a grandchild ignores SIGTERM; confirm the whole
    # group before restoring the source, then escalate with a bounded wait.
    deadline = time.monotonic() + TERM_GRACE
    while time.monotonic() < deadline and group_alive(pgid):
        time.sleep(0.05)
    if group_alive(pgid):
        try:
            os.killpg(pgid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + TERM_GRACE
        while time.monotonic() < deadline and group_alive(pgid):
            time.sleep(0.05)
    if proc.poll() is None:
        try:
            output += proc.communicate(timeout=TERM_GRACE)[0] or ""
        except subprocess.TimeoutExpired:
            pass
    return output


def run_bounded(cmd: list[str], timeout: float, env: dict | None = None) -> tuple[int, str, bool]:
    """Run `cmd` in its own process group; on timeout kill and reap the group."""
    proc = subprocess.Popen(
        cmd,
        cwd=ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        start_new_session=True,
    )
    track_proc(proc)
    try:
        try:
            output = proc.communicate(timeout=timeout)[0] or ""
            return proc.returncode, output, False
        except subprocess.TimeoutExpired:
            output = _terminate_group(proc)
            return (
                124,
                output + f"\n[negctl] timed out after {timeout}s; process group terminated\n",
                True,
            )
    finally:
        # Any path that leaves the child alive (including an unexpected
        # exception) must not leave a stale active group behind.
        try:
            if proc.poll() is None:
                _terminate_group(proc)
                if proc.poll() is None:
                    print(
                        f"negctl: process group {proc.pid} survived termination",
                        file=sys.stderr,
                    )
        finally:
            untrack_proc(proc)


def run_test(test_name: str, lib_target: bool = False) -> tuple[int, str]:
    # The cargo slot is checked before the mutation; do not mutate without it.
    target = ["--lib"] if lib_target else ["--test", "model_complete"]
    command = [
        "cargo",
        "test",
        "--manifest-path",
        str(MANIFEST),
        "-p",
        "pilotdeck-gateway",
        *target,
        test_name,
        "--",
        "--exact",
    ]
    return_code, output, _ = run_bounded(
        command,
        TEST_TIMEOUT,
        env=dict(os.environ, CARGO_TARGET_DIR=TARGET),
    )
    return return_code, output


def check_state_command() -> int:
    state = load_state()
    if state is None:
        print("negctl: clean (no recovery state)")
        return 0
    current = sha256(HTTP.read_bytes()) if HTTP.exists() else ""
    if current == state.get("original_sha256"):
        print("negctl: stale recovery state, source is original")
        return 0
    print(
        f"negctl: UNRECOVERED mutation control={state.get('control')} pid={state.get('pid')}"
    )
    return 3


def recover_command() -> int:
    state = load_state()
    if state is None:
        print("negctl: clean (no recovery state)")
        return 0
    if not state_matches_this_worktree(state):
        print("negctl: recovery state belongs to another worktree; refusing", file=sys.stderr)
        return 5
    pid = int(state.get("pid", 0))
    if pid != os.getpid() and pid_alive(pid):
        print(f"negctl: recorded control run still active (pid {pid}); refusing", file=sys.stderr)
        return 5
    current = sha256(HTTP.read_bytes()) if HTTP.exists() else ""
    if current == state.get("original_sha256"):
        clear_state()
        print("negctl: source already original; cleared recovery state")
        return 0
    if current != state.get("mutated_sha256"):
        print("negctl: source does not match the recorded mutation; refusing", file=sys.stderr)
        return 4
    original = ORIGINAL_FILE.read_bytes()
    if sha256(original) != state.get("original_sha256"):
        print("negctl: sidecar original SHA-256 mismatch; refusing", file=sys.stderr)
        return 6
    atomic_write(HTTP, original)
    if sha256(HTTP.read_bytes()) != state.get("original_sha256"):
        print("negctl: restored SHA-256 mismatch", file=sys.stderr)
        return 6
    clear_state()
    print("negctl: recovered original source from sidecar")
    return 0


def self_test_timeout() -> int:
    """Real process-group reclamation: kill a shell and its background child."""
    code, output, timed_out = run_bounded(
        ["sh", "-c", "sleep 30 & echo GRANDCHILD=$!; wait"], 1.0
    )
    print(output, end="")
    match = re.search(r"GRANDCHILD=(\d+)", output)
    if not match:
        print("negctl: timeout self-test did not observe the grandchild", file=sys.stderr)
        return 1
    pid = int(match.group(1))
    gone = False
    for _ in range(50):
        if not pid_alive(pid):
            gone = True
            break
        time.sleep(0.1)
    print(
        f"negctl: timeout self-test timed_out={timed_out} exit={code} grandchild={pid} gone={gone}"
    )
    return 0 if timed_out and gone else 1


def self_test_hold() -> int:
    """Mutate the real source then wait; a signal handler or --recover restores."""
    install_signal_handlers()
    startup_check()
    text = HTTP.read_text()
    anchor = "/// Normalizes Anthropic finish reason to canonical format."
    if text.count(anchor) != 1:
        raise SystemExit(f"negctl: hold anchor count {text.count(anchor)}")
    mutated = text.replace(anchor, anchor + " (negctl recovery hold)", 1)
    wait_for_slot()
    with mutating_sources(ROOT, [HTTP]):
        begin_mutation("self-test-hold", mutated.encode())
        print(f"HELD sha256={sha256(HTTP.read_bytes())}", flush=True)
        while True:
            time.sleep(1)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--recover", action="store_true", help="restore a recorded mutation")
    parser.add_argument("--check-state", action="store_true", help="report unrecovered state")
    parser.add_argument(
        "--self-test-timeout", action="store_true", help="prove process-group reclamation"
    )
    parser.add_argument(
        "--self-test-hold", action="store_true", help="hold a real mutation for signal tests"
    )
    args = parser.parse_args(argv)

    if args.check_state:
        return check_state_command()
    if args.recover:
        return recover_command()
    startup_check()
    if args.self_test_timeout:
        return self_test_timeout()
    if args.self_test_hold:
        return self_test_hold()

    if not os.environ.get("CARGO_TARGET_DIR"):
        print("CARGO_TARGET_DIR must be supplied by the caller", file=sys.stderr)
        return 2

    install_signal_handlers()
    # The model-complete controls write the same product tree as every
    # other in-place negative control: take the shared flock and prove
    # every product byte is HEAD before the first mutation.
    with mutating_sources(ROOT, [HTTP]):
        baseline_sha = sha256(HTTP.read_bytes())
        failures: list[str] = []
        baseline_ok: set[str] = set()
        # #819: every named target must be green before a mutation verdict
        # counts; otherwise a pre-existing failure would masquerade as a kill.
        for test_name in dict.fromkeys(control[3] for control in CONTROLS):
            wait_for_slot()
            code, output = run_test(test_name, test_name in LIB_TEST_NAMES)
            if code != 0 or not baseline_passed(output, test_name):
                failures.append(f"baseline {test_name}: not green (exit {code})")
            else:
                baseline_ok.add(test_name)
        for name, old, new, test_name in CONTROLS:
            text = HTTP.read_text()
            anchor_count = text.count(old)
            if anchor_count != 1:
                failures.append(f"{name}: mutation anchor count {anchor_count}")
                continue
            mutated = text.replace(old, new, 1)
            if mutated == text:
                failures.append(f"{name}: mutation did not change source")
                continue
            # Slot check must precede the mutation so a waiting script never leaves
            # mutated source visible to another cargo invocation in this worktree.
            wait_for_slot()
            begin_mutation(name, mutated.encode())
            try:
                code, output = run_test(test_name, name in LIB_TESTS)
                evidence = panic_evidence(output, test_name)
                caught = (code != 0 and test_name in baseline_ok
                          and named_test_failed(output, test_name))
                target = TARGET_CASE.get(name)
                if target is not None:
                    caught = caught and target in evidence
                print(f"{name}: {'caught' if caught else 'NOT CAUGHT'} (exit {code})")
                if target is not None:
                    print(f"  target case in output: {target in output}")
                if caught and target is not None:
                    # Keep the real target FAILED / panic lines in the log; a bare
                    # "caught" is not by itself evidence of which assertion failed.
                    lines = output.splitlines()
                    for index, line in enumerate(lines):
                        if target in line or "panicked at" in line:
                            print("  " + line.strip())
                            if "panicked at" in line:
                                for extra in lines[index + 1 : index + 5]:
                                    print("  " + extra.strip())
                if not caught:
                    tail = "\n".join(output.splitlines()[-25:])
                    print(tail)
                    failures.append(f"{name}: mutated build did not fail the assertion")
            finally:
                restored = restore_active()
            if not restored:
                failures.append(f"{name}: restore refused; recovery state kept")
                break
        if sha256(HTTP.read_bytes()) != baseline_sha:
            failures.append("final source SHA-256 does not match the pre-run bytes")
        if failures:
            print("NEGATIVE CONTROLS FAILED:", *failures, sep="\n  ")
            return 1
        print(f"NEGATIVE CONTROLS PASS ({len(CONTROLS)}/{len(CONTROLS)})")
        return 0


if __name__ == "__main__":
    sys.exit(main())
