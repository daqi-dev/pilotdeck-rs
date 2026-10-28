#!/usr/bin/env python3
# anchor-selfcheck: unsupported stdio middleman fixture, no mutation definitions
"""Deterministic stdio middleman between a sidecar host and the real sidecar.

The host speaks Module Protocol NDJSON on our stdin/stdout; we spawn the real
sidecar (tsx CLI) as a child, record every observed frame to a jsonl log, and
apply a declarative injection plan before delivery. Injection points are
declared as "the Nth frame matching <filter>" (1-based, counted per direction),
so a plan is deterministic and reproducible across runs.

Usage:
  stdio_mitm.py --log <frames.jsonl> [--plan <plan.json>] [--summary <summary.json>] -- <command> [args...]

plan.json: {"injections": [
  {"action": "drop"|"duplicate"|"early_final",
   "direction": "s2h"|"h2s", "kind": "request"|"response"|"event",
   "final": false,        # optional filter on event frames
   "nth": 2}]}            # 1-based index among matching frames

The protocol is full-duplex (cancel/ack/module_call responses travel h2s while
events travel s2h), so each direction is pumped by its own thread; frame
observation and plan state are serialized by one lock. A host that closes our
stdout early (for example right after accepting an injected premature final)
must not stop recording: s2h keeps observing and logging until the child is
reaped, with ``delivered`` reflecting only writes that actually went out.

Child ownership: the sidecar runs in its own process group. On host-side EOF we
mirror stdin EOF to the child and bound its exit (grace, then TERM, then KILL),
so a child stuck on module calls to a host that is already gone can never keep
our stdout reader blocked forever. The group, not the leader, decides the
escalation: a TERM-ignoring descendant that outlives a leader that already
exited still holds the pipes open, so KILL tracks the whole process group.
The TERM->KILL budget stays below the TS connection's killTimeoutMs (3s), so
the MITM always finishes its own reap and writes the summary before an outer
escalation could hit it.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
import time

from io_deadline import Lines, write, DeadlineExceeded

# Includes idle reads and backpressure writes, independent of peer cooperation.
PIPE_SECONDS = 30

# Bounded child reaping. The sidecar normally exits by itself once stdin ends;
# these are the escalation bounds, not the expected path. TERM grace + KILL
# wait must together stay under the TS connection's killTimeoutMs (3s).
CHILD_TERM_GRACE_SECONDS = 1
# After mirroring host-side stdin EOF, how long the child may take to exit on
# its own before the escalation above is applied.
CHILD_EXIT_GRACE_SECONDS = 2

MATCH_ACTIONS = ("drop", "duplicate", "early_final")


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", required=True, help="frame log jsonl path")
    parser.add_argument("--plan", help="injection plan json path (omitted = passthrough)")
    parser.add_argument("--summary", help="injection hit summary json path")
    parser.add_argument("command", nargs=argparse.REMAINDER, help="-- <command> [args...]")
    args = parser.parse_args(argv)
    if not args.command or args.command[0] != "--":
        parser.error("command must start with --")
    if len(args.command) < 2:
        parser.error("command after -- must not be empty")
    return args


def load_plan(path):
    if path is None:
        return {"injections": []}
    with open(path, "r", encoding="utf-8") as handle:
        plan = json.load(handle)
    injections = plan.get("injections")
    if not isinstance(injections, list):
        raise SystemExit(f"#969 mitm: plan 'injections' must be a list: {path}")
    for index, injection in enumerate(injections):
        if not isinstance(injection, dict):
            raise SystemExit(f"#969 mitm: injection #{index} must be an object: {path}")
        action = injection.get("action")
        if action not in MATCH_ACTIONS:
            raise SystemExit(f"#969 mitm: injection #{index} has unknown action {action!r}")
        if injection.get("direction") not in ("s2h", "h2s"):
            raise SystemExit(f"#969 mitm: injection #{index} needs direction s2h|h2s")
        if injection.get("kind") not in ("request", "response", "event"):
            raise SystemExit(f"#969 mitm: injection #{index} needs kind request|response|event")
        nth = injection.get("nth")
        if not isinstance(nth, int) or nth < 1:
            raise SystemExit(f"#969 mitm: injection #{index} needs 1-based nth")
    return {"injections": injections}


def frame_matches(frame, direction, injection):
    if direction != injection["direction"]:
        return False
    if frame.get("kind") != injection["kind"]:
        return False
    if "final" in injection and bool(frame.get("final")) != bool(injection["final"]):
        return False
    return True


def synthesize_terminal(session_id, turn_id):
    """A minimal valid AgentTurnResult payload for a premature final frame.

    The TS client's readFinalTerminal path accepts exactly this shape for
    outcome=completed; the marker stopReason makes the injected terminal
    distinguishable from any real sidecar terminal.
    """
    now = "2026-10-26T00:00:00.000Z"
    return {
        "result": {
            "type": "success",
            "sessionId": session_id,
            "turnId": turn_id,
            "stopReason": "mitm_premature_final",
            "usage": {},
            "permissionDenials": [],
            "turns": 1,
            "startedAt": now,
            "completedAt": now,
        },
        "messages": [],
    }


class Mitm:
    def __init__(self, plan, log_path, summary_path):
        self.injections = [
            {"spec": injection, "seen": 0, "fired": False, "matched_index": None}
            for injection in plan["injections"]
        ]
        self.log = open(log_path, "w", encoding="utf-8")
        self.summary_path = summary_path
        self.index = 0
        self.records = {"h2s": [], "s2h": []}
        # Session/turn identity for synthesized terminals, captured from the
        # host's execute request observed on h2s.
        self.session_id = None
        self.turn_id = None
        # Per-direction forwarding latches: once a peer's pipe is gone we keep
        # observing and logging, but never claim delivery again.
        self.forward = {"s2h": True, "h2s": True}
        self.lock = threading.Lock()

    def note_control_frame(self, frame):
        if frame.get("kind") == "request" and frame.get("method") == "execute":
            self.session_id = frame.get("sessionId")
            self.turn_id = frame.get("turnId")

    def actions_for(self, frame, direction):
        """Return the rewrite actions for one observed frame, mutating state."""
        matched = []
        for entry in self.injections:
            if entry["fired"] or not frame_matches(frame, direction, entry["spec"]):
                continue
            entry["seen"] += 1
            if entry["seen"] != entry["spec"]["nth"]:
                continue
            entry["fired"] = True
            entry["matched_index"] = self.index
            matched.append(entry["spec"]["action"])
        return matched

    def rewrite(self, frame, actions):
        if "early_final" in actions:
            if not self.session_id or not self.turn_id:
                raise SystemExit(
                    "#969 mitm: early_final injection fired before an execute "
                    "request was observed; no session/turn identity to synthesize")
            frame = dict(frame)
            frame["final"] = True
            frame["outcome"] = "completed"
            frame["eventType"] = "agent.execute.completed"
            frame["payload"] = synthesize_terminal(self.session_id, self.turn_id)
        return frame

    def observe(self, frame, direction):
        """Observe one frame under the lock; return (record, forward_count, line).

        The record is not logged yet: ``settle`` writes it once the pump knows
        how many copies actually reached the peer, so ``delivered`` can never
        claim a write that failed.
        """
        with self.lock:
            self.index += 1
            self.note_control_frame(frame)
            actions = self.actions_for(frame, direction)
            frame = self.rewrite(frame, actions)
            forward_count = 0 if "drop" in actions else 2 if "duplicate" in actions else 1
            if not self.forward[direction]:
                forward_count = 0
            record = {
                "index": len(self.records[direction]) + 1,
                "dir": direction,
                "action": actions,
                "delivered": False,
                "frame": frame,
            }
            self.records[direction].append(record)
            return record, forward_count, json.dumps(frame, ensure_ascii=False) + "\n"

    def settle(self, record, delivered):
        """Log one observed frame with its real delivery outcome."""
        with self.lock:
            record["delivered"] = bool(delivered)
            self.log.write(json.dumps(record, ensure_ascii=False) + "\n")
            self.log.flush()

    def close_log(self):
        self.log.close()
        # Two independent wire orders are recorded at observation, before forwarding.
        # Cross-direction scheduling has no total-order contract. No frame is omitted.
        Path(self.log.name).with_suffix(".json").write_text(
            json.dumps(self.records, ensure_ascii=False, indent=2) + "\n")
        if self.summary_path:
            summary = {
                "frames": self.index,
                "injections": [
                    {
                        "spec": entry["spec"],
                        "fired": entry["fired"],
                        "matchedIndex": entry["matched_index"],
                    }
                    for entry in self.injections
                ],
            }
            with open(self.summary_path, "w", encoding="utf-8") as handle:
                json.dump(summary, handle, ensure_ascii=False, indent=2)
                handle.write("\n")

    def unfired(self):
        return [entry for entry in self.injections if not entry["fired"]]


def group_alive(pgid):
    """Whether any process still lives in the child's process group.

    The leader exiting does not empty the group: descendants inherit it and
    can ignore TERM while holding the pipes open.
    """
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # The group exists but is not ours to signal; report it as alive so
        # callers keep their bounded budget instead of a silent success.
        return True
    return True


def kill_child_group(process):
    """TERM the group, then KILL whatever is still in it, leader or not.

    Deciding on the leader alone leaves TERM-ignoring descendants holding
    the pipes; the escalation tracks the group, and the leader's own exit is
    reaped by the poll below on the way.
    """
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        pass
    deadline = time.monotonic() + CHILD_TERM_GRACE_SECONDS
    while time.monotonic() < deadline:
        if process.poll() is not None and not group_alive(process.pid):
            return
        time.sleep(0.05)
    if group_alive(process.pid):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
    try:
        process.wait(timeout=CHILD_TERM_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        pass


def reap_after_host_eof(process):
    """Bound the child group's exit after the host closed our stdin.

    A child stuck on module calls to the departed host would otherwise keep the
    main thread blocked on its stdout forever. Give it a grace to finish on its
    own, then escalate; closing the pipes also ends the blocked reader. The
    leader's own exit does not decide this: descendants that inherited the
    group can still hold the pipes open, so the group is always swept.
    """
    deadline = time.monotonic() + CHILD_EXIT_GRACE_SECONDS
    while time.monotonic() < deadline and process.poll() is None:
        time.sleep(0.05)
    kill_child_group(process)
    for stream in (process.stdout, process.stdin):
        try:
            stream.close()
        except (OSError, ValueError):
            pass


def parse_frame(line, source):
    try:
        return json.loads(line)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"#969 mitm: {source} line is not JSON: {error}: {line[:200]!r}")


def main(argv):
    args = parse_args(argv)
    plan = load_plan(args.plan)
    mitm = Mitm(plan, args.log, args.summary)
    # The sidecar child owns its own process group so reaping is bounded even
    # when the host is gone first.
    child = subprocess.Popen(
        args.command[1:],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=None,
        bufsize=1,
        text=True,
        start_new_session=True,
    )

    def deliver_to_child(encoded, copies):
        written = 0
        for _ in range(copies):
            try:
                write(child.stdin, encoded, time.monotonic() + PIPE_SECONDS, "mitm h2s write")
                written += 1
            except (BrokenPipeError, ValueError):
                # The child is gone; stop forwarding, keep recording.
                mitm.forward["h2s"] = False
                break
        return written

    def stop_delivering_to_host():
        mitm.forward["s2h"] = False
        # Keep interpreter shutdown clean: flushing a broken stdout at exit
        # would turn a clean run into exit code 120.
        try:
            os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())
        except (OSError, ValueError):
            pass

    def deliver_to_host(encoded, copies):
        written = 0
        for _ in range(copies):
            try:
                write(sys.stdout, encoded, time.monotonic() + PIPE_SECONDS, "mitm s2h write")
                written += 1
            except (BrokenPipeError, ValueError):
                # The host is gone (for example it accepted a premature final
                # and closed). Stop delivering, keep recording.
                stop_delivering_to_host()
                break
        return written

    stopped = threading.Event()
    host_eof = threading.Event()

    def pump_host_to_sidecar():
        reader = Lines(sys.stdin)
        while line := reader.readline(time.monotonic() + PIPE_SECONDS, "mitm h2s read", stop=stopped):
            frame = parse_frame(line, "host")
            record, forward_count, encoded = mitm.observe(frame, "h2s")
            written_to_child = deliver_to_child(encoded, forward_count) if forward_count else 0
            mitm.settle(record, written_to_child > 0)
        host_eof.set()
        # Host closed our stdin: mirror it so the child can finish its turn,
        # then bound the wait — a child stuck on module calls to the departed
        # host must not keep the s2h reader blocked forever.
        try:
            child.stdin.close()
        except (BrokenPipeError, ValueError):
            pass
        reap_after_host_eof(child)

    def pump_sidecar_to_host():
        reader = Lines(child.stdout)
        while line := reader.readline(time.monotonic() + PIPE_SECONDS, "mitm s2h read"):
            frame = parse_frame(line, "sidecar")
            record, forward_count, encoded = mitm.observe(frame, "s2h")
            written_to_host = deliver_to_host(encoded, forward_count) if forward_count else 0
            mitm.settle(record, written_to_host > 0)

    def terminate(_signum, _frame):
        # A signal aimed at us must still reap the child group, even when the
        # leader is already gone and only descendants hold the pipes. Returning
        # (not exiting) lets the pumps finish and the summary be written.
        kill_child_group(child)

    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGINT, terminate)

    failures = []

    def guarded_host():
        try:
            pump_host_to_sidecar()
        except InterruptedError:
            pass
        except (DeadlineExceeded, OSError, ValueError, RuntimeError) as error:
            failures.append(str(error))
            kill_child_group(child)

    host_thread = threading.Thread(target=guarded_host, daemon=True)
    host_thread.start()
    try:
        try:
            pump_sidecar_to_host()
        except DeadlineExceeded as error:
            failures.append(str(error))
        except OSError as error:
            if not host_eof.is_set():
                failures.append(str(error))
        except (ValueError, RuntimeError) as error:
            # Only an explicitly owned fd-close is an expected shutdown.
            if isinstance(error, (UnicodeError, RuntimeError)) or not host_eof.is_set():
                failures.append(str(error))
        child.stdout.close()
    finally:
        # Unconditional: the leader may be gone while a TERM-ignoring
        # descendant still holds the pipes; the sweep itself is a no-op fast
        # path when the whole group is already dead.
        kill_child_group(child)
        stopped.set()
        host_thread.join(timeout=PIPE_SECONDS + CHILD_TERM_GRACE_SECONDS)
        unfired = mitm.unfired()
        mitm.close_log()
    if failures:
        print("#969 mitm: bounded I/O failure: " + "; ".join(failures), file=sys.stderr)
        return 4
    if unfired:
        specs = [entry["spec"] for entry in unfired]
        print(f"#969 mitm: declared injections never matched a frame: {specs}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
