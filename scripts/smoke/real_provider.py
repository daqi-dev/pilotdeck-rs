#!/usr/bin/env python3
"""Real-provider smoke harness (stdlib only; runs under `python3 -I`).

Layer 0 (runnable now): the gateway model runtime's public `complete_model` /
`stream_model` HTTP path, exercised through `scripts/smoke/probe/` against the
llm-center Anthropic endpoint.

Layer 1 (NOT_YET): the full gateway process path (#589 SubmitHost/WS + host
composition + permission/hooks/steer). Those scenarios are reported as NOT_YET
with the exact missing wiring; they are never marked PASS.

Secrets: read at runtime from the repository `.env.local` (or --env-file),
searching this checkout then the main checkout, passed only in the probe child
environment, redacted from every artifact, and never printed. The self-check
reports hit/miss only. An empty key set fails the self-check instead of
passing vacuously.

Exit codes: 0 every model-runtime scenario PASS **and** every applicable token
cap was actually verified (usage observed, cap echoed) and the secret
self-check verified; 1 any scenario FAIL; 2 secret self-check failed
(including a partial artifact scan); 3 BLOCKED (no model-runtime scenario
actually ran, a model-runtime scenario did not PASS, usage/cap accounting was
incomplete, or another run owns the evidence lock). Missing usage is never a
green run, except for the documented `stream-cancel` not-applicable case
(abort before the Anthropic usage block at end of stream).
"""

import argparse
import errno
import fcntl
import json
import os
import re
import signal
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

# llm-center host root, same value as scripts/run-review-roles.py:47.
LLM_CENTER_BASE_URL = "https://llm-center.modelbest.co"

# Hard limits for one smoke run. These are the single source of truth: the
# probe receives the per-request token cap and the per-scenario timeout as
# argv and echoes them back, so the report never quotes a second copy.
SCENARIOS = ("text", "stream-cancel", "tool")
MAX_REQUESTS = len(SCENARIOS)
MAX_OUTPUT_TOKENS_PER_REQUEST = 128
MAX_TOTAL_OUTPUT_TOKENS = MAX_REQUESTS * MAX_OUTPUT_TOKENS_PER_REQUEST
# Inner timeout enforced by the probe around one scenario.
PROBE_SCENARIO_TIMEOUT_SECONDS = 45
# Outer harness cap: the inner limit plus room to serialize a result. On
# expiry the whole probe process group is killed and the scenario is marked
# timeout (environment-or-provider), never product-or-protocol.
PROBE_TIMEOUT_SECONDS = PROBE_SCENARIO_TIMEOUT_SECONDS + 15
BUILD_TIMEOUT_SECONDS = 600
CARGO_SLOTS_TIMEOUT_SECONDS = 30

# `stream-cancel` aborts on the first content delta. In the Anthropic Messages
# protocol the `usage` block is emitted at the end of the stream, so an abort
# at the first delta structurally cannot observe it. The token cap for that
# scenario is therefore *not applicable*, not *unknown*: no request was left
# unverified by not reading the stream tail. Every other scenario with no
# usage is still UNVERIFIED and keeps the run BLOCKED.
USAGE_NOT_APPLICABLE = {
    "model_runtime_stream_cancel": (
        "the scenario aborts on the first stream delta; the Anthropic Messages "
        "usage block arrives at the end of the stream, so it is structurally "
        "absent. The output-token cap does not apply to this aborted request."
    ),
}

# Fixed regular file inside the evidence root, guarded by `fcntl.flock`.
LOCK_FILENAME = ".smoke.lock"

# Exit codes (also documented in scripts/smoke/README.md).
EXIT_OK = 0
EXIT_FAIL = 1
EXIT_SELF_CHECK_FAILED = 2
EXIT_BLOCKED = 3


# Ports reserved for the future #589 host/WS assembly; checked before use.
RESERVED_PORTS = (28789, 28790, 28810, 28811, 28812)

# Missing wiring for the acceptance steps that cannot run on origin/main.
NOT_YET = {
    "gateway_e2e_text": (
        "no gateway process entry: src/pilotdeck-gateway/src/main.rs:15-32 is a "
        "WebSocket/HTTP proxy only; src/pilotdeck-cli/src/main.rs:63 'server' and "
        "src/pilotdeck-cli/src/sdk.rs:41-44 'run' are Failure::Todo; the local "
        "gateway path at src/pilotdeck-cli/src/main.rs:104-106 is Todo"
    ),
    "gateway_e2e_tool": (
        "same missing SubmitHost/WS entry as gateway_e2e_text; model.stream is a "
        "host callback (src/pilotdeck-transport/src/default_factory.rs:841-851) "
        "and the parent host composition is not wired"
    ),
    "gateway_e2e_cancel": (
        "no run/turn process path to cancel; runtime cancel exists only inside "
        "the library (src/pilotdeck-gateway/src/runtime/turn/live.rs) and is not "
        "reachable from a shipped binary"
    ),
    "permission_ask_allow_deny": (
        "#586 acceptance step 2: needs the SubmitHost/WS + per-turn permission "
        "port binding from #589; factory registers placeholders only "
        "(src/pilotdeck-gateway/src/runtime/project/interaction.rs)"
    ),
    "hooks_invocation": (
        "#586 acceptance step 2: needs the host-owned session contribution "
        "HooksSettings/Binding from #589; no host process composes HookRuntime "
        "with a real turn"
    ),
    "steer_durable_order": (
        "#586 acceptance step 3: needs session mailbox + durable recorder in a "
        "live turn (#589); no run process accepts steer"
    ),
    "reload_generation": (
        "#586 acceptance step 4: needs old-turn reload/generation drain through "
        "the host composition (#589)"
    ),
    "cleanup_once": (
        "#586 acceptance step 3/5: needs the live host to observe handler and "
        "resource release counters; not reachable without #589"
    ),
}

GENERIC_SECRET = re.compile(
    r"(?:sk-[A-Za-z0-9_-]{16,}|x-api-key\s*:\s*\S+|Bearer\s+[A-Za-z0-9._-]{16,})",
    re.IGNORECASE,
)


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def iso_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def env_candidates(root: Path) -> list[Path]:
    """`.env.local` candidates, same search as scripts/run-review-roles.py.

    A worktree does not carry the ignored `.env.local`; fall back to the main
    checkout (the parent of `git rev-parse --git-common-dir`). Deduplicated so
    a normal checkout yields one path.
    """
    roots = [root]
    try:
        common = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--path-format=absolute",
             "--git-common-dir"],
            capture_output=True, text=True, timeout=10,
        )
        if common.returncode == 0 and common.stdout.strip():
            roots.append(Path(common.stdout.strip()).parent)
    except (OSError, subprocess.SubprocessError):
        pass
    ordered: list[Path] = []
    for candidate in (item / ".env.local" for item in roots):
        if candidate not in ordered:
            ordered.append(candidate)
    return ordered


def resolve_env_file(root: Path) -> Path | None:
    return next((path for path in env_candidates(root) if path.is_file()), None)


def parse_env_value(raw: str) -> str:
    """dotenv subset modeled on scripts/review-deepseek.py:parse_env_value.

    A quoted value ends at its closing quote, so `"sk-x" # local` keeps the
    value and drops the comment. An unquoted value drops ` # …` (whitespace
    before `#`), per POSIX .env semantics; `sk-x#prod` stays literal. Unlike
    the review loader, an unbalanced quote returns "" (the key is unusable and
    the run is BLOCKED) instead of aborting.
    """
    raw = raw.strip()
    if not raw or raw.startswith("#"):
        return ""
    if raw[0] in "'\"":
        end = raw.find(raw[0], 1)
        if end < 0:
            return ""  # unbalanced quote: treat as unusable, never guess
        return raw[1:end]
    return re.split(r"\s+#", raw, maxsplit=1)[0].strip()


def read_env_file(path: Path, names: set[str]) -> dict[str, str]:
    """Read only the requested names. Never log keys or values."""
    found: dict[str, str] = {}
    if not path.is_file():
        return found
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return found
    for line in lines:
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):].lstrip()
        key, sep, value = line.partition("=")
        if not sep:
            continue
        key = key.strip()
        if key not in names:
            continue
        value = parse_env_value(value)
        if value:
            found[key] = value
    return found


class EvidenceLock:
    """One writer per evidence directory, enforced by the kernel (`flock`).

    The lock is the fixed regular file `.smoke.lock` in the evidence root.
    Acquisition is `os.open(O_CREAT|O_RDWR)` followed by
    `fcntl.flock(LOCK_EX|LOCK_NB)`; a second process gets `EWOULDBLOCK` and is
    refused with exit 3. The kernel releases the lock when the owning process
    exits or is killed, including SIGKILL. There is no stale-lock reclaim, no
    rename, and no pid-liveness probe: those conditions cannot exist without a
    directory-ownership protocol to maintain.

    The pid/start time/argv written into the file after acquisition are
    diagnostics only. They are never read to decide who owns the lock; the
    kernel's lock state is the only authority.

    `--self-check-only` takes `LOCK_SH`: it excludes a normal (exclusive) run,
    while concurrent read-only self-checks may proceed together. It never
    rewrites the owner diagnostics.
    """

    def __init__(self, evidence: Path, shared: bool = False):
        self.path = evidence / LOCK_FILENAME
        self.shared = shared
        self.held = False
        self.note = ""
        self.fd: int | None = None

    def _write_owner(self, fd: int) -> None:
        """Best-effort diagnostics. Never part of the ownership decision."""
        payload = json.dumps(
            {
                "pid": os.getpid(),
                "startedAt": iso_now(),
                "argv": sys.argv,
            },
            ensure_ascii=False,
            indent=2,
        ).encode("utf-8")
        os.lseek(fd, 0, os.SEEK_SET)
        os.ftruncate(fd, 0)
        written = 0
        while written < len(payload):
            written += os.write(fd, payload[written:])
        os.fsync(fd)

    def acquire(self) -> tuple[bool, str]:
        flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_CLOEXEC", 0)
        try:
            fd = os.open(self.path, flags, 0o644)
        except OSError as exc:
            return False, f"cannot open {self.path}: {exc}"
        mode = fcntl.LOCK_SH if self.shared else fcntl.LOCK_EX
        try:
            fcntl.flock(fd, mode | fcntl.LOCK_NB)
        except OSError as exc:
            try:
                os.close(fd)
            except OSError:
                pass
            if exc.errno in (errno.EACCES, errno.EAGAIN):
                kind = "shared" if self.shared else "exclusive"
                return False, (
                    f"another run holds {self.path} "
                    f"({kind} flock is not available)"
                )
            return False, f"cannot lock {self.path}: {exc}"
        self.fd = fd
        self.held = True
        if not self.shared:
            try:
                self._write_owner(fd)
            except OSError as exc:
                # Diagnostics only: the kernel lock is still held.
                self.note = f"owner diagnostics not written: {exc}"
                print(
                    f"WARNING: {self.note}; the flock is still held",
                    file=sys.stderr,
                )
        return True, ""

    def release(self) -> None:
        if not self.held:
            return
        self.held = False
        fd, self.fd = self.fd, None
        if fd is None:
            return
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        except OSError:
            pass
        try:
            os.close(fd)
        except OSError:
            pass


def _write_text_atomic(path: Path, text: str) -> None:
    """Replace one small file atomically (used by concurrent self-checks)."""
    temporary = path.with_name(f"{path.name}.{os.getpid()}.{time.time_ns()}.tmp")
    try:
        temporary.write_text(text, encoding="utf-8")
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except OSError:
            pass


def _read_lock_owner(path: Path) -> dict | None:
    """Read the diagnostic owner block; never used for an ownership decision."""
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return value if isinstance(value, dict) else None


def make_run_dir(evidence: Path) -> Path:
    """Exclusive run directory; never overwrite or delete a previous run."""
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    base = evidence / f"run-{stamp}-{os.getpid()}"
    candidate = base
    counter = 1
    while True:
        try:
            candidate.mkdir(parents=True)
            return candidate
        except FileExistsError:
            candidate = Path(f"{base}-{counter}")
            counter += 1


def caps_verified(limits: dict) -> bool:
    """True when every *applicable* token cap was checked.

    `usageNotApplicable` is deliberately not consulted: those scenarios have a
    structural reason why usage cannot arrive (stream-cancel), so they neither
    verify nor unverify the cap. Missing usage anywhere else, or a missing
    `maxOutputTokens` echo, still leaves the cap unverified.
    """
    return not (limits.get("usageMissing") or limits.get("echoMissing"))


def final_exit_code(report: dict, post_ok: bool, requests_sent: int) -> int:
    """Three-state outcome: PASS=0, FAIL=1, self-check=2, BLOCKED=3.

    Exit 0 means *every* model-runtime scenario PASSed **and** every
    applicable token cap was verified, not merely "at least one PASS". A run
    with PASS + BLOCKED (for example the request budget ran out before the
    tool scenario) is partial and exits 3. Missing usage (outside the
    documented not-applicable scenario) or a missing `maxOutputTokens` echo
    means the cap check never ran, so that is BLOCKED too: an unverified cap
    is never a green run.
    """
    if not post_ok:
        return EXIT_BLOCKED if requests_sent == 0 else EXIT_SELF_CHECK_FAILED
    model = [s["status"] for s in report["scenarios"] if s.get("layer") == "model-runtime"]
    if any(status == "FAIL" for status in model):
        return EXIT_FAIL
    if requests_sent == 0 or not model:
        return EXIT_BLOCKED
    if any(status != "PASS" for status in model):
        return EXIT_BLOCKED
    limits = report.get("limits") or {}
    if not caps_verified(limits):
        return EXIT_BLOCKED
    return EXIT_OK




def key_env_for(model: str) -> str:
    lowered = model.lower()
    if lowered.startswith("glm"):
        return "ZAI_API_KEY"
    if lowered.startswith("deepseek"):
        return "DEEPSEEK_API_KEY"
    if lowered.startswith("claude"):
        return "ANTHROPIC_API_KEY"
    return "ANTHROPIC_API_KEY"


def redactor(secrets: dict[str, str]):
    values = [v for v in secrets.values() if len(v) >= 4]

    def redact(text: str) -> str:
        for value in values:
            text = text.replace(value, "[REDACTED]")
        return GENERIC_SECRET.sub("[REDACTED]", text)

    return redact


def port_state(port: int) -> str:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        sock.bind(("127.0.0.1", port))
        return "free"
    except OSError:
        return "in_use"
    finally:
        sock.close()


class _SigintBlocked:
    """Defer SIGINT around `Popen` so a Ctrl-C cannot lose the child handle.

    `Popen` forks/execs before it returns. If SIGINT is delivered between the
    fork and the assignment of the returned object, the harness never gets a
    handle to kill the new process group and the detached probe keeps sending
    real requests with the key in its environment. Blocking SIGINT for the
    duration of `Popen` turns that window into a pending signal that is
    delivered immediately after the assignment, inside the caller's try.
    """

    def __enter__(self):
        self.previous = None
        self.active = False
        if hasattr(signal, "pthread_sigmask"):
            try:
                self.previous = signal.pthread_sigmask(
                    signal.SIG_BLOCK, {signal.SIGINT}
                )
                self.active = True
            except (OSError, ValueError):
                self.previous = None
        return self

    def __exit__(self, *_exc):
        if self.active:
            signal.pthread_sigmask(signal.SIG_SETMASK, self.previous)
        return False


def run_bounded(cmd: list[str], cwd: Path, env: dict[str, str],
                timeout: float) -> dict:
    """Run one child in its own process group and kill the whole group on
    timeout or on any non-local exception (Ctrl-C / KeyboardInterrupt).

    `subprocess.run(timeout=...)` only kills the direct child. `cargo` spawns
    `rustc` children that keep writing the shared target directory after
    cargo dies, so the timeout must be a group kill (same rule as
    scripts/cli-parity/README.md:16). The child is started with
    `start_new_session=True`, so it does not receive the terminal's SIGINT;
    the harness has to forward the kill explicitly or the probe keeps sending
    real requests after the user pressed Ctrl-C.

    SIGINT is blocked across `Popen` itself, not merely across
    `communicate()`: assigning the process handle before the signal is
    delivered is what makes the cleanup branch reachable at all.
    """
    started = time.time()
    proc = None

    def kill_group(handle) -> None:
        if handle is None:
            return
        try:
            os.killpg(handle.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            try:
                handle.kill()
            except ProcessLookupError:
                pass

    try:
        with _SigintBlocked():
            proc = subprocess.Popen(
                cmd,
                cwd=cwd,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
        timed_out = False
        try:
            stdout, stderr = proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            kill_group(proc)
            stdout, stderr = proc.communicate()
    except BaseException:
        # KeyboardInterrupt/SystemExit must not leave the detached probe (or
        # cargo/rustc) running with the key in its environment.
        kill_group(proc)
        if proc is not None:
            try:
                proc.communicate()
            except BaseException:
                pass
        raise
    return {
        "stdout": stdout or "",
        "stderr": stderr or "",
        "exit": proc.returncode,
        "timedOut": timed_out,
        "elapsedSeconds": round(time.time() - started, 3),
    }


def run_cargo_slots(root: Path) -> dict:
    # Every checkout (including this worktree) carries its own copy. The old
    # root.parent.parent path only worked when the tree happened to be nested
    # two levels under the main checkout, and silently skipped the guard
    # elsewhere. A missing script is BLOCKED, never SKIPPED.
    script = root / "scripts" / "cargo-slots.sh"
    if not script.is_file():
        return {"status": "BLOCKED", "detail": f"missing {script}"}
    try:
        result = run_bounded(
            ["sh", str(script)], root, os.environ.copy(), CARGO_SLOTS_TIMEOUT_SECONDS
        )
    except OSError as exc:
        return {
            "status": "BLOCKED",
            "detail": f"{type(exc).__name__}: {exc}",
        }
    return {
        "status": "PASS" if result["exit"] == 0 and not result["timedOut"] else "BLOCKED",
        "exit": result["exit"],
        "timedOut": result["timedOut"],
        "elapsedSeconds": result["elapsedSeconds"],
        "stdout": redactor({})(result["stdout"].strip()),
        "stderr": redactor({})(result["stderr"].strip()),
        "detail": (
            f"cargo-slots timed out after {CARGO_SLOTS_TIMEOUT_SECONDS}s"
            if result["timedOut"]
            else ""
        ),
    }


def build_probe(root: Path, evidence: Path, run_dir: Path, redact) -> dict:
    probe = root / "scripts" / "smoke" / "probe"
    target = (evidence / "cargo-target").resolve()
    log_path = run_dir / "probe-build.log"
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target)
    env.pop("PDRS_PROVIDER_KEY", None)
    started = time.time()
    binary = target / "debug" / "real-provider-probe"
    try:
        result = run_bounded(
            ["cargo", "build", "--offline"],
            probe,
            env,
            BUILD_TIMEOUT_SECONDS,
        )
    except OSError as exc:
        log = f"cargo build could not start: {type(exc).__name__}: {exc}\n"
        log_path.write_text(redact(log), encoding="utf-8")
        return {
            "status": "FAIL",
            "exit": None,
            "timedOut": False,
            "binary": str(binary),
            "elapsedSeconds": round(time.time() - started, 2),
            "log": str(log_path),
            "error": f"{type(exc).__name__}: {exc}",
        }
    log = result["stdout"] + ("\n" + result["stderr"] if result["stderr"] else "")
    if result["timedOut"]:
        log += (
            f"\n[harness] cargo build exceeded {BUILD_TIMEOUT_SECONDS}s; "
            "killed the whole process group\n"
        )
    log_path.write_text(redact(log), encoding="utf-8")
    status = (
        "PASS"
        if result["exit"] == 0 and not result["timedOut"] and binary.is_file()
        else "FAIL"
    )
    return {
        "status": status,
        "exit": result["exit"],
        "timedOut": result["timedOut"],
        "binary": str(binary),
        "elapsedSeconds": round(time.time() - started, 2),
        "log": str(log_path),
    }



def classify_failure(payload: dict) -> str:
    """Attribute a FAIL using the gateway's canonical error codes
    (runtime/model/http/errors.rs), not the Node-era strings (`econn`,
    `fetch failed`) that Rust hyper never emits.

    `provider_error` is deliberately absent: errors.rs uses it as the
    fallback bucket for statuses that match no rule (including a 400 caused by
    a malformed request body), so treating it as an environment signal would
    mislabel a product/protocol defect. Such a payload stays
    `product-or-protocol`; an explicit status >= 500 is still an environment
    signal even when the code fell back to `provider_error`.
    """
    kind = str(payload.get("kind") or "")
    if kind == "timeout":
        return "environment-or-provider (timeout)"
    if kind == "harness":
        return "harness (probe process failed; no product verdict)"
    finish = str(payload.get("finishReason") or "").lower()
    body = payload.get("text")
    if finish == "length" and isinstance(body, str) and not body.strip():
        # 20261018-deepseek-thinking: provider-side reasoning can consume the
        # whole output budget and leave an empty text body. That is model /
        # budget behaviour, not a product protocol defect.
        return "environment-or-provider (token budget/thinking)"
    error = payload.get("error")
    text = json.dumps(error, ensure_ascii=False).lower() if error is not None else ""
    status = None
    if isinstance(error, dict):
        code = str(error.get("code") or "").lower()
        text += " " + code
        raw_status = error.get("status")
        if isinstance(raw_status, (int, float)) and not isinstance(raw_status, bool):
            status = raw_status
        # The canonical output-budget code is the same class as
        # finishReason=length: the model hit the smoke budget, not a protocol
        # defect. errors.rs:398, anthropic.rs:522, openai.rs:1170/1180.
        if code == "max_output_reached":
            return "environment-or-provider (token budget/thinking)"
    if status is not None and status >= 500:
        return "environment-or-provider"
    for needle in (
        "auth_error",
        "billing",
        "model_not_found",
        "payload_too_large",
        "rate_limit",
        "server_error",
        "timeout",
        "overloaded",
        "prompt_too_long",
        "context_overflow",
        "request_too_large",
        "image_too_large",
        "dns",
        "connection_reset",
        "connection_refused",
        "tls",
        "proxy_error",
        "certificate",
        "econnreset",
        "econnrefused",
        "network",
        "fetch failed",
        "socket hang up",
    ):
        if needle in text:
            return "environment-or-provider"
    return "product-or-protocol"


def apply_probe_exit(payload: dict, code: int) -> dict:
    """A probe that emitted a result and then crashed is not a PASS."""
    payload["exit"] = code
    if code != 0:
        prior = payload.get("status")
        if prior != "FAIL":
            payload["status"] = "FAIL"
            payload["kind"] = "harness"
            payload["error"] = f"probe exited {code} after reporting {prior}"
        else:
            payload["exitNote"] = f"probe exited {code}"
    return payload



def run_probe(binary: Path, scenario: str, base_url: str, model: str, key: str,
              run_dir: Path, redact) -> tuple[dict, int]:
    home = run_dir / "home"
    cwd = run_dir / "cwd"
    tmp = run_dir / "tmp"
    for path in (home, cwd, tmp):
        path.mkdir(parents=True, exist_ok=True)
    child_env = {
        "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
        "HOME": str(home),
        "TMPDIR": str(tmp),
        "LANG": "C.UTF-8",
        "PDRS_PROVIDER_KEY": key,
    }
    started = time.time()
    try:
        result = run_bounded(
            [
                str(binary),
                scenario,
                base_url,
                model,
                str(MAX_OUTPUT_TOKENS_PER_REQUEST),
                str(PROBE_SCENARIO_TIMEOUT_SECONDS),
            ],
            cwd,
            child_env,
            PROBE_TIMEOUT_SECONDS,
        )
        stdout, stderr, code = result["stdout"], result["stderr"], result["exit"]
        timed_out = result["timedOut"]
    except OSError as exc:
        stdout = ""
        stderr = f"{type(exc).__name__}: {exc}"
        code = -1
        timed_out = False
    elapsed = round(time.time() - started, 3)
    raw_dir = run_dir / "raw"
    raw_dir.mkdir(parents=True, exist_ok=True)
    (raw_dir / f"{scenario}.stdout.txt").write_text(redact(stdout), encoding="utf-8")
    (raw_dir / f"{scenario}.stderr.txt").write_text(redact(stderr), encoding="utf-8")
    payload = None
    for line in stdout.splitlines():
        if line.startswith("PROBE_RESULT "):
            try:
                payload = json.loads(line[len("PROBE_RESULT "):])
            except json.JSONDecodeError:
                payload = None
    if payload is None:
        if timed_out:
            kind = "timeout"
            error = (
                f"probe exceeded {PROBE_TIMEOUT_SECONDS}s "
                "(harness killed the process group)"
            )
            # It may have sent one request before the hard cap; that is the
            # conservative upper bound and is charged to the budget.
            requests = 1
            requests_unknown = False
        else:
            kind = "harness"
            error = "probe produced no PROBE_RESULT line"
            # No result does not mean one request was sent: a probe that
            # crashes on startup (bad argv, missing runtime, panic before the
            # HTTP call) sends zero. Do not invent a request; record the
            # count as unknown and charge it separately for the budget guard.
            requests = 0
            requests_unknown = True
        payload = {
            "scenario": scenario,
            "status": "FAIL",
            "kind": kind,
            "error": error,
            "exit": code,
            "timedOut": timed_out,
            "requests": requests,
            "requestsUnknown": requests_unknown,
        }
    else:
        # Early probe failures (`PDRS_PROVIDER_KEY` missing, bad argv) emit a
        # PROBE_RESULT before any request. A missing counter is 0/unknown, not
        # the old default of 1.
        if not isinstance(payload.get("requests"), (int, float)) or isinstance(
            payload.get("requests"), bool
        ):
            payload["requests"] = 0
            payload["requestsUnknown"] = True
    payload["elapsedSeconds"] = elapsed
    payload["timedOut"] = timed_out
    payload["raw"] = {
        "stdout": str(raw_dir / f"{scenario}.stdout.txt"),
        "stderr": str(raw_dir / f"{scenario}.stderr.txt"),
    }
    return apply_probe_exit(payload, code), code


def missing_scenarios() -> list[dict]:
    return [
        {
            "id": key,
            "layer": "gateway-process",
            "status": "NOT_YET",
            "missing": value,
        }
        for key, value in NOT_YET.items()
    ]


def write_markdown(report: dict, path: Path) -> None:
    lines = [
        "# Real provider smoke report",
        "",
        f"- generated: {report['generatedAt']}",
        f"- commit: `{report['git']['commit']}` on `{report['git']['branch']}`",
        f"- endpoint: `{report['provider']['baseUrl']}` (Anthropic Messages)",
        f"- model: `{report['provider']['model']}`",
        f"- credential source: `{report['provider']['keyAlias']}` (value never read out)",
        f"- limits: {report['limits']['maxRequests']} requests, "
        f"{report['limits']['maxOutputTokensPerRequest']} output tokens/request, "
        f"{report['limits']['maxTotalOutputTokens']} total; "
        f"{report['limits']['probeScenarioTimeoutSeconds']} s per probe scenario, "
        f"{report['limits']['probeHardTimeoutSeconds']} s harness hard cap",
        f"- requests sent: {report['limits']['requestsSent']} known"
        f" + {len(report['limits'].get('requestsUnknown', []))} unknown; "
        f"output tokens reported by probe: "
        f"{report['limits']['outputTokensReported']}",
        f"- usage missing (unknown, not 0): "
        f"{report['limits'].get('usageMissing', []) or 'none'}",
        f"- usage not applicable (structural, no cap check): "
        f"{[item['scenario'] for item in report['limits'].get('usageNotApplicable', [])] or 'none'}",
        f"- maxOutputTokens echo missing: "
        f"{report['limits'].get('echoMissing', []) or 'none'}",
        f"- exit code: {report.get('exitCode', 'pending')}"
        " (0 all model-runtime PASS with every applicable token cap verified, "
        "1 FAIL, 2 self-check, 3 BLOCKED/partial/unverified)",
        f"- ports checked: {report['ports']}",
        "",
        "## Scenarios",
        "",
        "| scenario | layer | status | detail |",
        "| --- | --- | --- | --- |",
    ]
    for item in report["scenarios"]:
        detail = (
            item.get("detail")
            or item.get("missing")
            or item.get("error")
            or item.get("text")
            or ""
        )
        if not detail and item.get("toolCalls"):
            detail = f"toolCalls={len(item['toolCalls'])}"
        if not detail and item.get("firstDeltaChars"):
            detail = f"firstDeltaChars={item['firstDeltaChars']}"
        if isinstance(detail, dict):
            detail = json.dumps(detail, ensure_ascii=False)
        detail = str(detail).replace("|", "\\|").replace("\n", " ")
        if item.get("usageStatus") == "missing":
            detail = (detail + " usage=missing").strip()
        elif item.get("usageStatus") == "not-applicable":
            detail = (detail + " usage=not-applicable").strip()
        lines.append(
            f"| `{item['id']}` | {item['layer']} | **{item['status']}** | {detail[:300]} |"
        )
    usage_missing = report["limits"].get("usageMissing", [])
    lines += [
        "",
        "## Usage / limit accounting",
        "",
        f"- requests: {report['limits']['requestsSent']} known"
        f" + {len(report['limits'].get('requestsUnknown', []))} unknown"
        " (each unknown is charged as one slot against the request budget)",
    ]
    usage_not_applicable = report["limits"].get("usageNotApplicable", [])
    if usage_missing:
        lines.append(
            "- output tokens: **unknown** for "
            + ", ".join(f"`{item}`" for item in usage_missing)
            + "; missing usage is recorded as unknown, never as 0, so the "
            "per-request and total token caps are **UNVERIFIED** for those "
            "scenarios. This makes the run BLOCKED (exit 3), never green."
        )
    else:
        lines.append(
            "- output tokens: reported for every model-runtime scenario where "
            "the cap applies; the per-request and total token caps were "
            "checked."
        )
    for item in usage_not_applicable:
        lines.append(
            f"- output-token cap **not applicable** for `{item['scenario']}`: "
            f"{item['reason']} This is neither a pass of the cap nor an "
            "unverified request, so it does not block a green run."
        )
    echo_missing = report["limits"].get("echoMissing", [])
    if echo_missing:
        lines.append(
            "- maxOutputTokens echo: **missing** for "
            + ", ".join(f"`{item}`" for item in echo_missing)
            + "; the harness could not verify that the per-request cap was "
            "applied, so the run is BLOCKED (exit 3), never green."
        )
    violations = report["limits"].get("violations", [])
    if violations:
        lines.append("- limit violations:")
        lines.extend(f"  - {item}" for item in violations)
    lines += [
        "",
        "## Failure classification",
        "",
    ]
    failures = [s for s in report["scenarios"] if s["status"] == "FAIL"]
    if not failures:
        lines.append("No FAIL.")
    else:
        for item in failures:
            lines.append(
                f"- `{item['id']}`: {item.get('classification', 'product-or-protocol')}"
            )
    lines += [
        "",
        "## Secret self-check",
        "",
    ]
    check = report["secretSelfCheck"]
    if not check.get("verifiable", False):
        lines.append(
            f"0 keys loaded ({check.get('files', 0)} files scanned), "
            "cannot verify key leakage: **FAIL/NOT_RUN**"
        )
    else:
        lines += [
            "| key alias | result |",
            "| --- | --- |",
        ]
        for item in check["keys"]:
            lines.append(f"| `{item['alias']}` | {item['result']} |")
        lines.append("")
        lines.append(
            f"Scanned {check['files']} artifact files "
            f"(excluding cargo-target); generic-pattern hits: "
            f"{check['genericHits']}; vanished during scan (skipped): "
            f"{check.get('filesSkipped', 0)}; directories skipped (walk "
            f"error): {check.get('walkErrors', 0)}; scan truncated: "
            f"{check.get('scanTruncated', False)}."
        )
        if check.get("scanTruncated"):
            lines.append(
                "A partial scan is **not** a pass: the self-check failed "
                "(exit 2 / finalPass false) rather than reporting 未命中 for "
                "bytes that were never read."
            )
    lines.append(
        f"- token caps verified (every applicable cap): "
        f"{report.get('capsVerified', False)}"
    )
    lines.append(
        f"- overall pass (exit 0): {report.get('overallPass', False)}"
    )
    lines.append(
        f"finalPass (secret self-check): {report.get('finalPass', False)}"
    )
    lines += [
        "",
        "## What is not proven",
        "",
        "The three PASS/FAIL rows exercise the model HTTP runtime directly. "
        "They do not prove the gateway process, SubmitHost/WS, permission, "
        "hooks, steer, reload drain, or cleanup counters; those acceptance "
        "steps are NOT_YET above and stay 待人工验收.",
        "",
    ]
    path.write_text("\n".join(lines), encoding="utf-8")


def write_reports(report: dict, directories: list[Path]) -> None:
    """Write the summary to the evidence root and into the run directory.

    The run copy is authoritative and never overwritten; the root copy is the
    latest index. Raw per-scenario records stay under the unique run dir.
    """
    for directory in directories:
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "report.json").write_text(
            json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        write_markdown(report, directory / "report.md")


def write_limits(run_dir: Path, limits: dict) -> None:
    (run_dir / "limits.json").write_text(
        json.dumps(
            {
                "declared": {
                    "maxRequests": MAX_REQUESTS,
                    "maxOutputTokensPerRequest": MAX_OUTPUT_TOKENS_PER_REQUEST,
                    "maxTotalOutputTokens": MAX_TOTAL_OUTPUT_TOKENS,
                    "probeScenarioTimeoutSeconds": PROBE_SCENARIO_TIMEOUT_SECONDS,
                    "probeHardTimeoutSeconds": PROBE_TIMEOUT_SECONDS,
                    "buildTimeoutSeconds": BUILD_TIMEOUT_SECONDS,
                    "cargoSlotsTimeoutSeconds": CARGO_SLOTS_TIMEOUT_SECONDS,
                },
                "actual": limits,
                "scenarios": list(SCENARIOS),
                "note": (
                    "Declared limits are the constants in "
                    "scripts/smoke/real_provider.py; the probe receives the "
                    "token cap and inner timeout as argv and echoes them back."
                ),
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )


class ArtifactScan(list):
    """List of artifact paths plus the walk errors that truncated the scan.

    A plain list is still accepted by `self_check` (tests pass one); missing
    attributes mean "complete scan".
    """

    def __init__(self, paths=(), walk_errors: int = 0,
                 skipped_dirs=()):
        super().__init__(paths)
        self.walk_errors = walk_errors
        self.skipped_dirs = list(skipped_dirs)


def collect_artifacts(evidence: Path) -> ArtifactScan:
    """List evidence files, skipping unreadable directories one at a time.

    `Path.rglob()` aborts the whole walk when a directory disappears midway
    (a concurrent process can remove a run artifact). That made the secret
    scan fail open on a partial file list. `os.walk(onerror=...)` skips only
    the directory that failed and reports it, so the rest is still scanned and
    the caller can refuse to call a partial scan clean.
    """
    files: list[Path] = []
    skipped_dirs: list[str] = []

    def onerror(exc: OSError) -> None:
        skipped_dirs.append(str(getattr(exc, "filename", None) or exc))

    for dirpath, dirnames, filenames in os.walk(
        evidence, onerror=onerror, followlinks=False
    ):
        dirnames[:] = [name for name in dirnames if name != "cargo-target"]
        for name in filenames:
            files.append(Path(dirpath) / name)
    return ArtifactScan(sorted(files), len(skipped_dirs), skipped_dirs)


def self_check(files: list[Path], secrets: dict[str, str]) -> dict:
    keys = []
    walk_errors = getattr(files, "walk_errors", 0)
    skipped = 0
    readable: list[bytes] = []
    for path in files:
        try:
            readable.append(path.read_bytes())
        except OSError:
            # Another process removed the file after collect_artifacts()
            # listed it. A vanished file cannot contain a leak, but the scan
            # is still partial and `self_check_ok` refuses to call it clean.
            skipped += 1
    for alias, value in secrets.items():
        needle = value.encode()
        prefix = value[:8].encode()
        hit = any(needle in data or prefix in data for data in readable)
        keys.append({"alias": alias, "result": "命中" if hit else "未命中"})
    generic = 0
    for data in readable:
        generic += len(
            GENERIC_SECRET.findall(data.decode("utf-8", "replace"))
        )
    return {
        "files": len(files),
        "filesSkipped": skipped,
        "walkErrors": walk_errors,
        "skippedDirs": list(getattr(files, "skipped_dirs", [])),
        "scanTruncated": bool(skipped or walk_errors),
        "keys": keys,
        "keysLoaded": len(secrets),
        "verifiable": bool(secrets),
        "genericHits": generic,
    }


def self_check_ok(check: dict) -> bool:
    """An empty key set or a partial scan is NOT a pass.

    `all([])` used to make the leak check vacuously green when `.env.local`
    was missing or unreadable. A skipped file or an unreadable directory means
    unknown bytes were not scanned; reporting "未命中" for that list would be a
    false negative, so the self-check fails instead.
    """
    return (
        bool(check.get("verifiable"))
        and not check.get("scanTruncated", False)
        and all(item["result"] == "未命中" for item in check.get("keys", []))
        and check.get("genericHits") == 0
    )


def output_tokens(payload: dict) -> int | None:
    """Reported output tokens, or None when the provider sent no usage.

    None is deliberately distinct from 0: an absent usage event must not pass
    the per-request/total token caps by looking like "0 tokens used".
    """
    usage = payload.get("usage")
    if isinstance(usage, dict):
        value = usage.get("outputTokens")
        if (
            isinstance(value, (int, float))
            and not isinstance(value, bool)
            and value >= 0
        ):
            return int(value)
    return None


def account_scenario(limits: dict, sid: str, payload: dict) -> list[str]:
    """Account one probe result against the request/token budget.

    Unknown request counts are charged as one slot for the guard but never
    reported as a known request. Missing usage is recorded in
    `limits.usageMissing` and is not counted as 0 tokens; the per-request and
    total token checks are skipped for that scenario because there is no data
    to compare, and `final_exit_code` returns BLOCKED instead of green. The one
    exception is `USAGE_NOT_APPLICABLE` (stream-cancel): the aborted stream
    structurally cannot carry a tail usage block, so its cap is recorded as
    *not applicable* instead of *missing*. A missing `maxOutputTokens` echo is
    recorded in `limits.echoMissing` for the same reason: the cap cannot be
    shown to have been applied.

    The request budget is enforced after the fact as well as before the next
    scenario: a probe that retries can send more than one request per scenario
    and still report a plausible count, so the known+unknown total is checked
    here, after accounting, and a per-scenario count above one is a violation.
    A scenario that reports PASS but a known count of 0 is also a violation:
    "no request" is incomplete, not a verified green scenario, and the zero
    case must be as strict as the retry case.
    """
    reported_requests = payload.get("requests")
    if payload.get("requestsUnknown") or not (
        isinstance(reported_requests, (int, float))
        and not isinstance(reported_requests, bool)
        and reported_requests >= 0
    ):
        limits["requestsUnknown"].append(sid)
        per_scenario_requests = None
    else:
        per_scenario_requests = int(reported_requests)
        limits["requestsSent"] += per_scenario_requests
    tokens = output_tokens(payload)
    if tokens is None:
        reason = USAGE_NOT_APPLICABLE.get(sid)
        if reason:
            payload["usageStatus"] = "not-applicable"
            entry = {"scenario": sid, "reason": reason}
            if entry not in limits["usageNotApplicable"]:
                limits["usageNotApplicable"].append(entry)
        else:
            payload["usageStatus"] = "missing"
            limits["usageMissing"].append(sid)
    else:
        payload["usageStatus"] = "reported"
        limits["outputTokensReported"] += tokens
    violations = []
    if payload.get("status") == "PASS" and per_scenario_requests is None:
        # Symmetric with the known-0 case: a PASS that omits the counter is
        # not allowed to evade the zero check by looking "unknown".
        violations.append(
            f"scenario {sid} reported PASS without a known request count; "
            "a scenario that ran must report at least one request"
        )
    if (
        per_scenario_requests is not None
        and per_scenario_requests < 1
        and payload.get("status") == "PASS"
    ):
        violations.append(
            f"scenario {sid} reported {per_scenario_requests} requests but "
            "PASS; a scenario that ran must send one request (0 is "
            "incomplete)"
        )
    if per_scenario_requests is not None and per_scenario_requests > 1:
        violations.append(
            f"scenario {sid} sent {per_scenario_requests} requests > 1 "
            "(one request per scenario)"
        )
    accounted = limits["requestsSent"] + len(limits["requestsUnknown"])
    if accounted > MAX_REQUESTS:
        violations.append(
            f"total requests={accounted} (known + unknown charged) > "
            f"{MAX_REQUESTS}"
        )
    echoed = payload.get("maxOutputTokens")
    if (
        not isinstance(echoed, (int, float))
        or isinstance(echoed, bool)
    ):
        if sid not in limits["echoMissing"]:
            limits["echoMissing"].append(sid)
    elif int(echoed) != MAX_OUTPUT_TOKENS_PER_REQUEST:
        violations.append(
            f"probe echoed maxOutputTokens={echoed}, "
            f"expected {MAX_OUTPUT_TOKENS_PER_REQUEST}"
        )
    if tokens is not None and tokens > MAX_OUTPUT_TOKENS_PER_REQUEST:
        violations.append(
            f"scenario outputTokens={tokens} > "
            f"{MAX_OUTPUT_TOKENS_PER_REQUEST}"
        )
    if (
        tokens is not None
        and limits["outputTokensReported"] > MAX_TOTAL_OUTPUT_TOKENS
    ):
        violations.append(
            f"total outputTokens={limits['outputTokensReported']} > "
            f"{MAX_TOTAL_OUTPUT_TOKENS}"
        )
    return violations


def scenario_id(name: str) -> str:
    return f"model_runtime_{name.replace('-', '_')}"


def blocked_rows(reason: str) -> list[dict]:
    return [
        {
            "id": scenario_id(name),
            "layer": "model-runtime",
            "status": "BLOCKED",
            "missing": reason,
            "raw": {},
        }
        for name in SCENARIOS
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="deepseek-v4.1-flash-mb")
    parser.add_argument("--base-url", default=LLM_CENTER_BASE_URL)
    parser.add_argument("--key-env", default=None,
                        help="override the .env.local key name")
    parser.add_argument(
        "--env-file",
        default=None,
        help="explicit .env file; default searches this checkout then the "
             "main checkout (git common dir parent)",
    )
    parser.add_argument("--evidence-dir", default=None)
    parser.add_argument("--self-check-only", action="store_true",
                        help="scan existing evidence artifacts for key leakage")
    args = parser.parse_args()

    root = repo_root()
    evidence = Path(args.evidence_dir) if args.evidence_dir else (
        root / "_tmp" / f"{datetime.now().strftime('%Y%m%d')}-real-provider-smoke"
    )
    # Absolute paths matter twice: raw/run directories must not depend on the
    # caller's cwd, and CARGO_TARGET_DIR is resolved by cargo relative to the
    # probe cwd (scripts/smoke/probe), not to this script.
    evidence = evidence.expanduser().resolve()
    evidence.mkdir(parents=True, exist_ok=True)

    key_alias = args.key_env or key_env_for(args.model)
    env_names = {key_alias, "ANTHROPIC_API_KEY", "DEEPSEEK_API_KEY", "ZAI_API_KEY"}
    env_path = Path(args.env_file) if args.env_file else resolve_env_file(root)
    secrets = read_env_file(env_path, env_names) if env_path else {}
    # Redact/scan every key this run learned about, not only the one passed
    # to the child. Only `key` below reaches the child environment.
    redact = redactor(secrets)
    key = secrets.get(key_alias, "")

    if args.self_check_only:
        # LOCK_SH: excludes a normal (LOCK_EX) run; concurrent read-only
        # self-checks may run together. The report file is replaced atomically
        # so two of them cannot interleave a partial write.
        lock = EvidenceLock(evidence, shared=True)
        acquired, reason = lock.acquire()
        if not acquired:
            print(f"BLOCKED: {reason}", file=sys.stderr)
            return EXIT_BLOCKED
        try:
            files = collect_artifacts(evidence)
            check = self_check(files, secrets)
            lines = [
                f"env file: {env_path or 'none found'}",
                f"keys loaded: {check['keysLoaded']}",
                f"verifiable: {check['verifiable']}",
                f"files: {check['files']}",
                f"files skipped (vanished during scan): {check['filesSkipped']}",
                f"directories skipped (walk error): {check['walkErrors']}",
                f"scan truncated: {check['scanTruncated']}",
                f"generic-pattern hits: {check['genericHits']}",
            ]
            for item in check["keys"]:
                lines.append(f"{item['alias']}: {item['result']}")
            _write_text_atomic(
                evidence / "self-check-final.txt", "\n".join(lines) + "\n"
            )
            post = self_check(collect_artifacts(evidence), secrets)
            print("final secret self-check (all artifacts, cargo-target excluded):")
            print(f"  env file: {env_path or 'none found'}")
            print(f"  keys loaded: {post['keysLoaded']}")
            for item in post["keys"]:
                print(f"  {item['alias']}: {item['result']}")
            print(f"  generic-pattern hits: {post['genericHits']}")
            print(f"  files skipped (vanished during scan): {post['filesSkipped']}")
            print(f"  directories skipped (walk error): {post['walkErrors']}")
            print(f"  scan truncated: {post['scanTruncated']}")
            if not post["verifiable"]:
                print("  0 keys loaded, cannot verify key leakage")
            if post["scanTruncated"]:
                print("  partial scan: refused (a skipped file/directory is not a pass)")
            ok = self_check_ok(post)
            print(f"  finalPass: {ok}")
            return EXIT_OK if ok else EXIT_SELF_CHECK_FAILED
        finally:
            lock.release()

    # One writer per evidence directory. A second run is refused by the kernel
    # (`flock`) before touching any artifact; it never deletes the first run's
    # raw records.
    lock = EvidenceLock(evidence)
    try:
        acquired, reason = lock.acquire()
        if not acquired:
            print(f"BLOCKED: {reason}", file=sys.stderr)
            return EXIT_BLOCKED
        run_dir = make_run_dir(evidence)
        (run_dir / "owner.json").write_text(
            json.dumps(
                {"pid": os.getpid(), "startedAt": iso_now(), "argv": sys.argv},
                ensure_ascii=False,
                indent=2,
            ),
            encoding="utf-8",
        )
        limits = {
            "maxRequests": MAX_REQUESTS,
            "maxOutputTokensPerRequest": MAX_OUTPUT_TOKENS_PER_REQUEST,
            "maxTotalOutputTokens": MAX_TOTAL_OUTPUT_TOKENS,
            "probeScenarioTimeoutSeconds": PROBE_SCENARIO_TIMEOUT_SECONDS,
            "probeHardTimeoutSeconds": PROBE_TIMEOUT_SECONDS,
            "requestsSent": 0,
            "requestsUnknown": [],
            "outputTokensReported": 0,
            "usageMissing": [],
            "usageNotApplicable": [],
            "echoMissing": [],
            "violations": [],
        }
        write_limits(run_dir, limits)

        git_commit = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"], cwd=root,
            capture_output=True, text=True,
        ).stdout.strip() or "unknown"
        git_branch = subprocess.run(
            ["git", "branch", "--show-current"], cwd=root,
            capture_output=True, text=True,
        ).stdout.strip() or "unknown"

        ports = {str(port): port_state(port) for port in RESERVED_PORTS}
        slots = run_cargo_slots(root)

        build = {"status": "SKIPPED", "detail": "no key available"}
        scenarios: list[dict] = []
        if not key:
            searched = (
                str(env_path)
                if env_path
                else ", ".join(str(path) for path in env_candidates(root))
            )
            scenarios.extend(
                blocked_rows(f"{key_alias} is not present in {searched}")
            )
        elif slots["status"] == "BLOCKED":
            scenarios.extend(
                blocked_rows(
                    slots.get("detail")
                    or "cargo-slots.sh reports all slots busy or timed out"
                )
            )
        else:
            build = build_probe(root, evidence, run_dir, redact)
            if build["status"] != "PASS":
                if build.get("timedOut"):
                    why = (
                        f"probe build timed out after {BUILD_TIMEOUT_SECONDS}s; "
                        "process group killed; see probe-build.log"
                    )
                else:
                    why = "probe build failed; see probe-build.log"
                scenarios.extend(blocked_rows(why))
            else:
                binary = Path(build["binary"])
                for name in SCENARIOS:
                    sid = scenario_id(name)
                    budget_used = limits["requestsSent"] + len(
                        limits["requestsUnknown"]
                    )
                    if budget_used >= MAX_REQUESTS:
                        scenarios.append({
                            "id": sid,
                            "layer": "model-runtime",
                            "status": "BLOCKED",
                            "missing": "MAX_REQUESTS exhausted",
                            "raw": {},
                        })
                        continue
                    payload, _ = run_probe(
                        binary, name, args.base_url, args.model, key, run_dir, redact
                    )
                    payload["id"] = sid
                    payload["layer"] = "model-runtime"
                    violations = account_scenario(limits, sid, payload)
                    if violations:
                        limits["violations"].extend(violations)
                        payload["status"] = "FAIL"
                        payload["kind"] = "harness"
                        payload["error"] = "; ".join(violations)
                    if payload.get("status") == "FAIL":
                        payload["classification"] = classify_failure(payload)
                    scenarios.append(payload)

        scenarios.extend(missing_scenarios())

        report = {
            "generatedAt": iso_now(),
            "git": {"commit": git_commit, "branch": git_branch},
            "provider": {
                "baseUrl": args.base_url,
                "model": args.model,
                "protocol": "anthropic",
                "keyAlias": key_alias,
                "envFile": str(env_path) if env_path else None,
                "envCandidates": [str(path) for path in env_candidates(root)],
            },
            "limits": limits,
            "ports": ports,
            "cargoSlots": slots,
            "build": build,
            "evidenceLock": {
                "mechanism": "fcntl.flock",
                "file": str(lock.path),
                "exclusive": not lock.shared,
                "holder": _read_lock_owner(lock.path),
                "note": lock.note or None,
            },
            "scenarios": scenarios,
            "notes": [
                "Layer 0 drives pilotdeck-gateway's public model HTTP runtime "
                "(complete_model/stream_model) through scripts/smoke/probe.",
                "Layer 1 gateway/permission/hooks/steer scenarios are NOT_YET, "
                "not failures and not passes.",
                "requestsSent/outputTokensReported come from the probe's echoed "
                "counters. A probe with no result is recorded as 0/unknown, "
                "not as one invented request; unknown request counts are "
                "charged as one slot against the budget and listed in "
                "limits.requestsUnknown. A PASS scenario must report at least "
                "one known request; a known 0 is a limits.violations entry. "
                "Missing usage is listed in limits.usageMissing and is never "
                "counted as 0 tokens; a missing maxOutputTokens echo is listed "
                "in limits.echoMissing. Either one makes the run exit 3 (caps "
                "unverified), never 0, except for the per-scenario "
                "limits.usageNotApplicable entry (stream-cancel aborts before "
                "the Anthropic tail usage block).",
            ],
        }

        # First pass: artifacts with a placeholder check, so the scan sees the
        # report itself too. Then write the final check result and rescan.
        report["secretSelfCheck"] = {
            "files": 0, "filesSkipped": 0, "keys": [],
            "keysLoaded": len(secrets), "verifiable": bool(secrets),
            "genericHits": 0,
        }
        report["finalPass"] = False
        report["capsVerified"] = False
        report["overallPass"] = False
        report["exitCode"] = EXIT_BLOCKED
        write_reports(report, [run_dir, evidence])
        check = self_check(collect_artifacts(evidence), secrets)
        report["secretSelfCheck"] = check
        write_reports(report, [run_dir, evidence])
        final = self_check(collect_artifacts(evidence), secrets)
        final["finalPass"] = self_check_ok(final)
        report["secretSelfCheck"] = final
        report["finalPass"] = final["finalPass"]
        requests_accounted = limits["requestsSent"] + len(
            limits["requestsUnknown"]
        )
        report["exitCode"] = final_exit_code(
            report, final["finalPass"], requests_accounted
        )
        report["capsVerified"] = caps_verified(limits)
        report["overallPass"] = report["exitCode"] == EXIT_OK
        write_limits(run_dir, limits)
        write_reports(report, [run_dir, evidence])

        # Post-write scan: verify the exact bytes on disk once more.
        post = self_check(collect_artifacts(evidence), secrets)
        post_ok = self_check_ok(post)
        code = final_exit_code(report, post_ok, requests_accounted)
        if code != report["exitCode"] or post_ok != report["finalPass"]:
            report["secretSelfCheck"] = post
            report["finalPass"] = post_ok
            report["exitCode"] = code
            report["capsVerified"] = caps_verified(limits)
            report["overallPass"] = code == EXIT_OK
            write_reports(report, [run_dir, evidence])

        print(f"evidence: {evidence}")
        print(f"run: {run_dir}")
        print(f"env file: {env_path or 'none found'} "
              f"(keys loaded: {post['keysLoaded']})")
        print(f"model: {args.model} via {args.base_url} (key={key_alias})")
        print(f"requests sent: {limits['requestsSent']} known / {MAX_REQUESTS}"
              f" (+{len(limits['requestsUnknown'])} unknown, each charged as 1)")
        print(f"output tokens reported: {limits['outputTokensReported']} / "
              f"{MAX_TOTAL_OUTPUT_TOKENS}")
        if limits["usageMissing"]:
            print("usage missing (unknown, not 0): "
                  + ", ".join(limits["usageMissing"]))
            print("token caps UNVERIFIED for scenarios with missing usage "
                  "(exit 3, not green)")
        for item in limits["usageNotApplicable"]:
            print(f"usage not applicable: {item['scenario']} "
                  f"({item['reason']})")
        if limits["echoMissing"]:
            print("maxOutputTokens echo missing: "
                  + ", ".join(limits["echoMissing"]))
            print("token cap application UNVERIFIED (exit 3, not green)")
        for violation in limits["violations"]:
            print(f"limit violation: {violation}")
        for item in report["scenarios"]:
            line = f"{item['status']:8} {item['id']} ({item['layer']})"
            if item["status"] == "FAIL":
                line += f" [{item.get('classification', 'product-or-protocol')}]"
            if item["status"] == "BLOCKED" and item.get("missing"):
                line += f" [{item['missing']}]"
            print(line)
        print("secret self-check (artifacts, cargo-target excluded):")
        for item in post["keys"]:
            print(f"  {item['alias']}: {item['result']}")
        print(f"  generic-pattern hits: {post['genericHits']}")
        print(f"  files skipped (vanished during scan): {post['filesSkipped']}")
        print(f"  directories skipped (walk error): {post['walkErrors']}")
        print(f"  scan truncated: {post['scanTruncated']}")
        if not post["verifiable"]:
            print("  0 keys loaded, cannot verify key leakage")
        if post["scanTruncated"]:
            print("  partial scan: self-check failed (not a pass)")
        print(f"  finalPass: {post_ok}")
        print(f"report: {evidence / 'report.md'} (run copy: {run_dir / 'report.md'})")
        print(f"exitCode: {code}")
        return code
    finally:
        lock.release()


if __name__ == "__main__":
    sys.exit(main())
