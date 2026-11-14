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
  counts cargo process *trees* the way scripts/cargo-slots.sh does: one
  `ps -Aww -o pid=,ppid=,args=` sample, then only processes whose whole
  ancestor chain holds no cargo (#1566 item 1 -- counting every first-word
  match double counts, since `cargo clippy` spawns `cargo-clippy` which in
  turn spawns `cargo check`: three matches, one slot). When the sandbox
  denies `ps`, the `pgrep -x` fallback cannot see ppid, so a nonzero
  reading only bounds the count from below and refuses immediately instead
  of waiting out the slot deadline; zero is released only after the whole
  name table read a clean no-match on two consecutive sweeps (#2516 item
  2 -- one sweep cannot see an entry point that started after the sweep
  had already probed its name). Both probes capture bytes and decode them
  here, and any control character in a sample is a probe failure (#2516
  item 3: universal newlines would fold a literal \r into \n and tear one
  row into two). A failure of both probes is fatal (never treated as zero).
  Registered residual (#2516 fix-r1 arbitration, 2026-11-09): the \n exemption
  above is a registered residual, not a closed invariant -- this probe trusts
  the local PATH, and with PATH attacker-controlled no whole-table census is
  in scope (a torn fragment can spell a lawful row). See the "已知残余"
  section of this fixture's README and follow-up #2609.
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
from contextlib import suppress
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence  # noqa: E402
from negative_selection import baseline_passed  # noqa: E402
from negative_process import mutating_sources  # noqa: E402

ROOT = Path(__file__).resolve().parents[5]
HTTP = ROOT / "src/pilotdeck-gateway/src/runtime/model/http.rs"
# #1111: the complete-parse family moved from http.rs into http/complete.rs;
# controls whose anchors are those bytes mutate the new file. Path-only change:
# anchor text, counts and verdict criteria are unchanged.
COMPLETE = ROOT / "src/pilotdeck-gateway/src/runtime/model/http/complete.rs"
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

CARGO_CAP = int(os.environ.get("NEGCTL_CARGO_CAP") or os.environ.get("PDRS_CARGO_SLOTS") or "4")  # #1287：宿主按主机配置名额
SLOT_WAIT_SECONDS = float(os.environ.get("NEGCTL_SLOT_WAIT_SECONDS", "1800"))
# #2516 fix-r1 (P2-a 1): each ps/pgrep sample is bounded. A hung wrapper (or a
# ps wedged behind a sandbox) must surface as a probe failure inside the wait
# loop, not hold the mutation window open for as long as the OS lets it.
PROBE_TIMEOUT_SECONDS = float(os.environ.get("NEGCTL_PROBE_TIMEOUT_SECONDS", "30"))
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

# #2516 item 3: every C0 control byte except \n (the ps row separator) plus
# DEL. A probe sample carrying any of these is a probe failure, never a
# zero count: with bytes captured and decoded in this file (never through
# universal newlines), \n is the only character that may lawfully break a
# row, and a \r in the wild means a wrapper spoke, not that the table is
# empty. Real macOS ps escapes argv control bytes (^M / \012), so a genuine
# sample carries none of these. The \n exemption is a registered residual
# (#2516 fix-r1 arbitration), not a closed invariant: this probe trusts the
# local PATH, and with PATH attacker-controlled no whole-table census is in
# scope. See this fixture's README ("已知残余") and follow-up #2609.
CONTROL_CHARACTERS = r"[\x00-\x09\x0b-\x1f\x7f]"

CONTROLS = [
    (
        "responses-delegates-to-chat-completions",
        COMPLETE,
        '"openai-responses" => parse_openai_responses_complete_response(raw, provider_id),',
        '"openai-responses" => parse_openai_complete_response(raw, provider_id),',
        TEST,
    ),
    (
        "openai-finish-reason-uses-tool_calls",
        COMPLETE,
        'Some("tool_calls" | "function_call") => "tool_call".into(),',
        'Some("tool_calls" | "function_call") => "tool_calls".into(),',
        TEST,
    ),
    (
        "responses-failed-finish-reason-unknown",
        COMPLETE,
        'Some("failed" | "cancelled") => "error".into(),',
        'Some("failed" | "cancelled") => "unknown".into(),',
        TEST,
    ),
    (
        # Same-source third protocol: the refusal -> content_filter arm.
        "anthropic-refusal-finish-unknown",
        COMPLETE,
        'Some("refusal") => "content_filter".into(),',
        'Some("refusal") => "unknown".into(),',
        TEST,
    ),
    (
        # The collision case now really reaches nextUniqueToolCallId's `_2`.
        "tool-id-suffix-starts-at-one",
        COMPLETE,
        "            let mut suffix = 2u64;",
        "            let mut suffix = 1u64;",
        TEST,
    ),
    (
        "complete-retries-429",
        COMPLETE,
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
        COMPLETE,
        "                if attempt < max_retries && is_retryable_request_error(&error) {",
        "                if attempt <= max_retries && is_retryable_request_error(&error) {",
        TRANSPORT_TEST,
    ),
    (
        # If complete() reused the stream idle resolver, the 300ms
        # streamTimeoutMs=10 case would fire at 10ms and report the stream
        # message instead of `Model request timed out after 300ms.`.
        "complete-timeout-uses-stream-idle",
        HTTP,
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
        HTTP,
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
        COMPLETE,
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
        HTTP,
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
        COMPLETE,
        '    let cache_read = read("cache_read_input_tokens");',
        "    let cache_read: Option<f64> = None;",
        TEST,
    ),
    (
        # Invert the dedupe condition: the duplicate is kept and the
        # non-matching mismatch case drops its first message block.
        "responses-output-text-dedupe-inverted",
        COMPLETE,
        "    if JsString::from_utf16(rest) == *output_text {",
        "    if JsString::from_utf16(rest) != *output_text {",
        TEST,
    ),
    (
        "anthropic-stop-sequence-unknown",
        COMPLETE,
        '        Some("end_turn" | "stop_sequence") => "stop".into(),',
        '        Some("end_turn") => "stop".into(),',
        TEST,
    ),
    (
        # The old helper decides emptiness on the raw string, so a
        # whitespace-only id is kept instead of generating a fresh id.
        "openai-tool-id-whitespace-kept",
        COMPLETE,
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
        HTTP,
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
        HTTP,
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
        HTTP,
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
        HTTP,
        """        self.bytes += data.len();""",
        """        self.bytes += String::from_utf8_lossy(data).encode_utf16().count();""",
        LIB_RESPONSE_BYTES_TEST,
    ),
    (
        # Equivalent to the r5 off-tree M9 control, now on the single shared
        # default tail. Both builds fall through to usize::MAX, so the pinned
        # V8 literal assertion (not a constant-vs-itself comparison) fails.
        "shape-probe-string-limit-default-removed",
        HTTP,
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
        HTTP,
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
LIB_TEST_NAMES = {control[4] for control in CONTROLS if control[0] in LIB_TESTS}

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


PUBLISH_DIR = ROOT / ".negctl-publish"


def atomic_write(path: Path, data: bytes) -> None:
    """Publish `data` over `path` via a temp file + fsync + one replace.

    For a target inside the worktree the temp lives in `PUBLISH_DIR` at the
    repository root, not beside the target (#1692 §9 r2 review): a SIGKILL does
    not run `finally`, and a residue inside `src/` is read back as an
    "untracked build input" by `negative_process._assert_input_areas`, which
    then refuses every later run. The protection is the location (the root is
    outside the scanned areas); the `.gitignore`d `.negctl-*` name only keeps
    `git status` clean, since that scan uses `git ls-files --others` and
    includes ignored files on purpose.
    `mkstemp` creates 0600, so the source mode is restored before the rename --
    otherwise every run would publish 0600 onto a tracked source and leave a
    mode-only `git diff` under `core.filemode=true`.

    Two shapes the r2 version got wrong (#1692 §9 r3 review):
    - this helper also CREATES the recovery sidecars (`STATE_DIR/state.json`,
      `STATE_DIR/original.http.rs`), which never exist on a clean start, so the
      source mode is only read from a target that already exists (0600, the
      `mkstemp` default, otherwise) -- an unconditional `path.stat()` raised
      `FileNotFoundError` before the first mutation;
    - those sidecars live under the system temp dir, which is often a different
      filesystem (tmpfs `/tmp` on Linux CI), where `os.replace` raises `EXDEV`.
      The scratch dir therefore follows the target: the repository-root
      `PUBLISH_DIR` for tracked sources inside the worktree (outside every
      scanned area, so a SIGKILL residue is not an "untracked build input"), and
      the target's own directory for anything outside it (same device, so the
      rename stays atomic).
    """
    scratch = PUBLISH_DIR if path.is_relative_to(ROOT) else path.parent
    scratch.mkdir(parents=True, exist_ok=True)
    mode = path.stat().st_mode & 0o7777 if path.exists() else 0o600
    fd, tmp = tempfile.mkstemp(dir=str(scratch), prefix=".negctl-" + path.name, suffix=".tmp")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(tmp, mode)
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
        # fix-r12 (review DS r11 sibling #1, same family as write_source_atomic):
        # the cleanup unlink is the only swallowed error, so a failed publish
        # always surfaces its own error instead of a PermissionError here.
        with suppress(OSError):
            Path(tmp).unlink(missing_ok=True)


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
    return state.get("root") == str(ROOT) and state.get("file") in (
        str(HTTP),
        str(COMPLETE),
    )


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
    mutated_file = Path(state["file"])
    current = sha256(mutated_file.read_bytes()) if mutated_file.exists() else ""
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


def begin_mutation(control: str, mutated: bytes, path: Path) -> None:
    global _active
    original = path.read_bytes()
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
                "file": str(path),
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
        "path": path,
    }
    atomic_write(path, mutated)


def restore_active() -> bool:
    """Restore only a source matching the recorded mutation or original."""
    global _active, _restoring
    if _active is None:
        return True
    if _restoring:
        return False
    _restoring = True
    try:
        current = sha256(_active["path"].read_bytes())
        if current == _active["original_sha256"]:
            pass
        elif current == _active["mutated_sha256"]:
            original = ORIGINAL_FILE.read_bytes()
            if sha256(original) != _active["original_sha256"]:
                raise SystemExit("negctl: sidecar original SHA-256 mismatch; refusing")
            atomic_write(_active["path"], original)
            if sha256(_active["path"].read_bytes()) != _active["original_sha256"]:
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
    """Count cargo process trees, the criterion scripts/cargo-slots.sh uses.

    One `ps -Aww -o pid=,ppid=,args=` sample (the wide single snapshot avoids
    counts torn between two samples), then a process counts only when no cargo
    appears anywhere in its ancestor chain: each tree is counted once, at its
    root. The walk follows the whole chain rather than only the direct parent
    to cover cargo -> rustc -> build-script -> cargo (a build.rs spawning
    cargo) with non-cargo processes in between. Matching the first command
    word alone -- what the old docstring claimed was "the same criterion" --
    double counts: `cargo clippy` spawns `cargo-clippy`, which spawns
    `cargo check`, three matches for one slot (#1566 item 1). Rows split on
    "\n" only, never splitlines(): a U+2028 inside some args must stay one
    row, or it would tear into a second parseable row that can rewrite the
    parent map (#1770 fix-r3).

    The sample is captured as bytes and decoded here, never through
    universal newlines: text=True wraps the pipe in io.TextIOWrapper with
    newline=None, which folds a literal \r into \n and tears one row into
    two, and the torn tail can pose as a parseable "int int rest" row that
    overwrites the parent map (#2516 item 3; a wrapper ps printing one \r
    is enough -- the real macOS ps escapes argv control bytes as ^M / \\012,
    so a genuine sample carries none). Every other control character in
    the decoded sample fails the probe the same way instead of becoming a
    zero count: bytes the row parser cannot fully account for must never
    release a mutation. \n itself is exempt because it is the row separator --
    a registered residual, not a closed invariant (#2516 fix-r1 arbitration):
    this probe trusts the local PATH, and with PATH attacker-controlled no
    whole-table census is in scope (a torn fragment can spell a lawful row);
    see this fixture's README ("已知残余") and follow-up #2609;
    invalid UTF-8 stays in the args column as surrogates, which can spell
    neither a pid nor a cargo name.

    A stdout that is nonempty yet has zero parseable rows is a probe failure,
    never a zero count: a sandbox/wrapper printing diagnostics with rc=0 (or
    a ps that ignored the -o format but still exited 0) must not be read as
    "no cargo" (#1770 fix-r1). One forged `int int rest` row clears that
    guard, so a second invariant holds: this process is alive while ps runs,
    a genuine full sample therefore always lists the caller's own pid, and a
    table that parsed yet lacks it is a probe failure too (#1770 fix-r2).
    """
    try:
        result = subprocess.run(
            ["ps", "-Aww", "-o", "pid=,ppid=,args="], capture_output=True,
            timeout=PROBE_TIMEOUT_SECONDS,
        )
    except OSError as error:
        return None, f"ps failed: {error}"
    except subprocess.TimeoutExpired:
        # #2516 fix-r1 (P2-a 1): a sample that never comes back is a probe
        # failure, never a count -- reading 0 here would release a mutation
        # beside a full quota with nobody even looking.
        return None, f"ps timed out after {PROBE_TIMEOUT_SECONDS}s"
    if result.returncode != 0:
        return None, (
            f"ps exit {result.returncode}: "
            f"{result.stderr.decode('utf-8', 'replace').strip()}"
        )
    stdout = result.stdout.decode("utf-8", "surrogateescape")
    if not stdout.strip():
        return None, "ps output empty"
    if re.search(CONTROL_CHARACTERS, stdout):
        # #2516 item 3: a control byte here (a \r most plausibly) means the
        # row structure is not what the parser assumes -- under text=True
        # that \r would already have torn a row and forged a parent-map
        # entry. Fail closed, never read the sample as a zero count.
        return None, "ps output contains control characters"
    parent: dict[int, int] = {}
    cargo_pids: set[int] = set()
    parsed = 0
    # split("\n"), not splitlines(): a U+2028 (or \x0b/\x0c/\x85) inside a
    # process's args must keep it one row -- splitlines() would tear the args
    # column into a second parseable "int int rest" row, letting a crafted
    # command line overwrite the parent map (e.g. reparent a live cargo onto
    # itself) and read a full slot as free. Same invariant as the pgrep rc=0
    # row parser below (#1770 fix-r3 -- the review's deferred P3, taken as
    # the 1-line follow-along it permitted, with form T as its control).
    for line in stdout.split("\n"):
        fields = line.split(None, 2)
        if len(fields) < 3 or not fields[0].isdigit() or not fields[1].isdigit():
            continue
        parsed += 1
        pid, ppid = int(fields[0]), int(fields[1])
        parent[pid] = ppid
        words = fields[2].split()
        if words and re.fullmatch(r"(?:.*/)?cargo(?:-[a-z]+)?", words[0]):
            cargo_pids.add(pid)
    if not parsed:
        # #1770 fix-r1: a nonempty stdout with zero parseable rows is a probe
        # failure, not "no cargo" -- reading it as count=0 would release a
        # mutation beside a full slot quota, so fail closed like every other
        # probe failure (fall back / refuse, never treat as zero).
        return None, "ps output had no parseable rows"
    if os.getpid() not in parent:
        # #1770 fix-r2: this process survives the whole ps run, so a real full
        # sample always contains its own row. A table that parsed without the
        # caller pid is not a ps sample (e.g. one forged "int int rest"
        # diagnostic row), and reading it as count=0 would again release a
        # mutation beside a full quota -- fail closed, never treat as zero.
        return None, "ps output lacks caller pid"
    count = 0
    for pid in cargo_pids:
        ancestor = parent.get(pid)
        # 4096 hops matches cargo-slots.sh: enough for any real chain, while
        # still terminating a torn snapshot whose parent map contains a cycle.
        hops = 0
        while ancestor is not None and ancestor not in cargo_pids and hops < 4096:
            ancestor = parent.get(ancestor)
            hops += 1
        if ancestor not in cargo_pids:
            count += 1
    return count, ""


def slots_from_pgrep() -> tuple[int | None, str]:
    """Fallback when `ps` is denied: a lower bound, never a count.

    `pgrep -x` cannot report ppid, so the ancestor dedup above is impossible
    here: a per-name tally counts one tree's `cargo`, `cargo-clippy` and
    `cargo check` separately. Without parentage the honest reading is "at
    least one cargo tree is alive" whenever any known entry point exists, so
    that is what this probe returns; `wait_for_slot` refuses to release a
    mutation on it (#1566 item 1).

    rc=0 without a pid row, or rc=1 (pgrep's "no match" code) with stdout
    or stderr, is a probe failure rather than a reading: a wrapper that
    swallows or injects output must not be read as "no entry point alive"
    (#1770 fix-r2, fix-r3 for the whitespace-only stdout and stderr shapes;
    #2516 item 1 made the stderr gate read the raw string, symmetric with
    the stdout gate, so whitespace-only stderr is "someone spoke" too).
    Zero means the whole name table read a clean no-match on two
    consecutive sweeps (#2516 item 2): each sweep probes every listed name
    once, and the second sweep closes the single-sweep gap where an entry
    point that started after the first sweep had already passed its name is
    never probed again. What stays outside even two sweeps is a name that
    both appears and vanishes between them (it no longer holds a slot) or
    starts after the second sweep's probe of its name -- the same snapshot
    window the single ps sample above has. Output is captured as bytes and
    decoded here, with any control character failing the probe, for the
    same reason as the ps sample (#2516 item 3). The list is this repo's
    cargo entry points: the built-in hot subcommands plus cargo-nextest
    (CI's test entry, .github/workflows/ci.yml). That last one is not
    built-in: cargo replaces itself (exec) with an external subcommand, so
    during `cargo nextest run` no process named `cargo` survives for the
    other names to find -- nextest must be listed by its own name (#1770
    fix-r3, measured on cb8: the exec chain keeps one pid from wrapper to
    cargo-nextest, with no cargo row as its parent). The scope still
    differs from the ps path above, which matches any `cargo-*` first
    word; an unlisted external subcommand would be missed here.
    """
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
        # #1770 fix-r3: external subcommand, CI's test entry. Not in the
        # built-in list above: cargo exec's it in place (measured), so during
        # `cargo nextest run` no `cargo` process exists for the built-in
        # names to find -- without this row the pgrep fallback would read a
        # full slot as 0 and release a mutation.
        "cargo-nextest",
    ]
    for _sweep in (1, 2):
        alive = False
        for name in names:
            try:
                result = subprocess.run(
                    ["pgrep", "-x", name], capture_output=True,
                    timeout=PROBE_TIMEOUT_SECONDS,
                )
            except OSError as error:
                return None, f"pgrep -x {name} failed: {error}"
            except subprocess.TimeoutExpired:
                # #2516 fix-r1 (P2-a 1): same fail-closed reading as the ps
                # sample above -- a hang is a probe failure, never a zero.
                return None, f"pgrep -x {name} timed out after {PROBE_TIMEOUT_SECONDS}s"
            stdout = result.stdout.decode("utf-8", "surrogateescape")
            stderr = result.stderr.decode("utf-8", "surrogateescape")
            if re.search(CONTROL_CHARACTERS, stdout) or re.search(CONTROL_CHARACTERS, stderr):
                # #2516 item 3: same fail-closed reading as the ps sample --
                # control bytes the row/pid parser cannot account for must
                # never be read as a clean no-match.
                return None, f"pgrep -x {name} output contains control characters"
            if result.returncode == 0:
                # split("\n"), not splitlines(): a U+2028 inside a forged row must
                # keep it one unparseable line (fail closed), not two pid rows.
                if any(re.fullmatch(r"\d+", line.strip()) for line in stdout.split("\n")):
                    alive = True
                else:
                    # #1770 fix-r2: rc=0 means pgrep matched, so its stdout must
                    # hold pid rows; a wrapper swallowing stdout yet passing rc=0
                    # through is a probe failure, never "no cargo" -> 0.
                    return None, f"pgrep -x {name} rc=0 without a pid row"
            elif result.returncode == 1:
                # #1770 fix-r2: rc=1 is pgrep's "no match", which prints
                # nothing; stdout here means a wrapper spoke instead of
                # pgrep, so the probe failed rather than observed an absence.
                # fix-r3: the raw stdout, not strip()ed -- pgrep prints nothing
                # at all on a clean no-match, so even whitespace-only output is
                # "someone spoke" (strip() folded it back into the empty string
                # pgrep never produces, and split("\n") above already keeps a
                # U+2028 forged row one line for the same reason).
                if stdout:
                    return None, f"pgrep -x {name} exit 1 with stdout: {stdout!r}"
                # fix-r3 + #2516 item 1: same reading for stderr, now on the
                # raw string like the stdout gate above -- a wrapper that
                # refuses to run pgrep, diagnoses on stderr (even
                # whitespace-only) and exits 1 is a probe failure too,
                # never a clean no-match sweep -> 0.
                if stderr:
                    return None, f"pgrep -x {name} exit 1 with stderr: {stderr!r}"
            else:
                return None, f"pgrep -x {name} exit {result.returncode}: {stderr.strip()}"
        if alive:
            return 1, ""
    return 0, ""


def cargo_slots() -> tuple[int, str, str]:
    # Documented gate; under a denied `ps` it misreports 0, so its output is
    # logged but never trusted as the count. 11-05：名额改由统一闸
    # (~/ws/pdrs-ops/bin/cargo-slot；没有该安装的远端 runner 宿主回退到
    # ~/ws/pdrs-runner-tools/bin/cargo) 决定，脚本不再接受上限参数；本文件的
    # CARGO_CAP 只作负控自己的节流口径，脚本的退出码在这里不使用。
    script = subprocess.run(
        ["bash", "scripts/cargo-slots.sh"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        # 300 s ≥ 脚本自身 bootstrap 预算 240 s（#1556 r6 sibling P1）：本机首次碰统一
        # 闸要等冷 du（实测 38.6 s，结构上限是闸自己的 180 s du 超时），之后走名额缓存。
        timeout=300,
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
        # #1566 item 1: a nonzero reading from the pgrep fallback is only a
        # lower bound -- the true tree count needs ppid, which pgrep cannot
        # give. It can neither rise to "full" (so the wait below would end)
        # nor be trusted as "free", so refuse immediately instead of holding
        # the mutated source for SLOT_WAIT_SECONDS on an unknowable number.
        # Zero is exact here (no entry point alive means no tree), so only
        # the nonzero bound refuses; since #1770 fix-r2 zero can only come
        # from clean whole-table sweeps, never from a failed probe, and
        # #2516 requires two of them back to back before it releases.
        if source == "pgrep":
            if count == 0:
                print(f"negctl: cargo slot free: 0/{CARGO_CAP} (probe=pgrep, no cargo entry point alive, {note})")
                return
            raise SystemExit(
                "negctl: ps denied and the pgrep fallback can only bound the cargo "
                f"count from below (>={count}); refusing to mutate"
            )
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
    mutated_file = Path(state["file"])
    current = sha256(mutated_file.read_bytes()) if mutated_file.exists() else ""
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
    mutated_file = Path(state["file"])
    current = sha256(mutated_file.read_bytes()) if mutated_file.exists() else ""
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
    atomic_write(mutated_file, original)
    if sha256(mutated_file.read_bytes()) != state.get("original_sha256"):
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
    text = COMPLETE.read_text()
    anchor = "/// Normalizes Anthropic finish reason to canonical format."
    if text.count(anchor) != 1:
        raise SystemExit(f"negctl: hold anchor count {text.count(anchor)}")
    mutated = text.replace(anchor, anchor + " (negctl recovery hold)", 1)
    wait_for_slot()
    with mutating_sources(ROOT, [COMPLETE]):
        begin_mutation("self-test-hold", mutated.encode(), COMPLETE)
        print(f"HELD sha256={sha256(COMPLETE.read_bytes())}", flush=True)
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
    with mutating_sources(ROOT, [HTTP, COMPLETE]):
        baseline_sha = {path: sha256(path.read_bytes()) for path in (HTTP, COMPLETE)}
        failures: list[str] = []
        baseline_ok: set[str] = set()
        # #819: every named target must be green before a mutation verdict
        # counts; otherwise a pre-existing failure would masquerade as a kill.
        for test_name in dict.fromkeys(control[4] for control in CONTROLS):
            wait_for_slot()
            code, output = run_test(test_name, test_name in LIB_TEST_NAMES)
            if code != 0 or not baseline_passed(output, test_name):
                failures.append(f"baseline {test_name}: not green (exit {code})")
            else:
                baseline_ok.add(test_name)
        for name, path, old, new, test_name in CONTROLS:
            text = path.read_text()
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
            begin_mutation(name, mutated.encode(), path)
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
        for path in (HTTP, COMPLETE):
            if sha256(path.read_bytes()) != baseline_sha[path]:
                failures.append(
                    f"final source SHA-256 does not match the pre-run bytes: {path}"
                )
        if failures:
            print("NEGATIVE CONTROLS FAILED:", *failures, sep="\n  ")
            return 1
        print(f"NEGATIVE CONTROLS PASS ({len(CONTROLS)}/{len(CONTROLS)})")
        return 0


if __name__ == "__main__":
    sys.exit(main())
