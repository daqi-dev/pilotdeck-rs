#!/usr/bin/env python3
# anchor-selfcheck: unsupported forged-result comparator attacks, no source mutation definitions
"""#969 red-team attack suite: forged driver evidence against the selftest.

Feeds one forged driver-result / frames / summary triple per attack straight
into ``verify_caught`` (run_case is replaced in memory; no process runs, no
source is touched). Every attack is a way a wrong TS observable could try to
pass as ``verified``:

  - JSON type confusion Python's ``==`` accepts (``True == 1``, ``1 == 1.0``),
    top level and nested, in both the ruler fields and frame sequences;
  - null versus missing keys, missing versus extra keys;
  - array order and shape (event list/details disagreement, non-lists);
  - failure cells swallowing their exception instead of ``verified:false``.

An attack is caught only when its case cell carries ``verified:false`` and a
dedicated ``#969`` message naming the problem (some attacks additionally
require a marker like "boolean" or "missing key" in that message). An attack
that verifies — or crashes the verdict — prints ``ESCAPED`` and fails the
suite. negative_controls.py weakens the comparator and binds its mutants to
these exact ESCAPED lines.
"""
import importlib.util
import json
from pathlib import Path
import sys

FIXTURE = Path(__file__).resolve().parent
NAME = "text-passthrough"

spec = importlib.util.spec_from_file_location("live_e2e_selftest", FIXTURE / "selftest.py")
selftest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(selftest)


def forged_frames(count):
    """Gap-free passthrough frames matching the base result's event stream."""
    return [{
        "index": index,
        "dir": "s2h",
        "action": [],
        "delivered": True,
        "frame": {"kind": "event", "sequence": index, "final": False},
    } for index in range(count)]


def forged_result():
    """A driver result consistent with EXPECTED for the attacked cell."""
    expected = selftest.EXPECTED[NAME]
    details = []
    for kind, model_event_type in expected["events"]:
        entry = {"type": kind}
        if model_event_type is not None:
            entry["modelEventType"] = model_event_type
        details.append(entry)
    return {
        "scenario": "text",
        "injection": "passthrough",
        "events": [kind for kind, _ in expected["events"]],
        "eventDetails": details,
        "terminal": dict(expected["terminal"]),
        "error": None,
        "modelCalls": 1,
        "toolCalls": [],
        "toolResults": [],
        "modelInterrupted": False,
    }


# Each attack: (name, marker expected in the dedicated message, forge(result,
# frames)). A missing marker only relaxes to "some dedicated #969 message"
# when the marker is None; the verdict check (verified:false + #969 message)
# always applies.
ATTACKS = [
    ("bool-for-int-count", "boolean",
     lambda result, frames: result.update(modelCalls=True)),
    ("float-for-int-count", "number",
     lambda result, frames: result.update(modelCalls=1.0)),
    ("bool-in-terminal", "boolean",
     lambda result, frames: result["terminal"].update(turns=True)),
    ("float-in-terminal", "number",
     lambda result, frames: result["terminal"].update(turns=1.0)),
    ("string-for-int-in-terminal", "string",
     lambda result, frames: result["terminal"].update(messageCount="2")),
    ("extra-terminal-key", "unexpected key",
     lambda result, frames: result["terminal"].update(errors=[])),
    ("missing-terminal-key", "missing key",
     lambda result, frames: result["terminal"].pop("stopReason")),
    ("missing-error-field", "must carry the 'error' field",
     lambda result, frames: result.pop("error")),
    ("missing-terminal-field", "must carry the 'terminal' field",
     lambda result, frames: result.pop("terminal")),
    ("int-for-null-error", "integer",
     lambda result, frames: result.update(error=0)),
    ("bool-in-frame-sequence", "boolean",
     lambda result, frames: frames[3]["frame"].update(sequence=True)),
    ("float-in-frame-sequence", "number",
     lambda result, frames: frames[3]["frame"].update(sequence=3.0)),
    ("events-reordered", "event sequence must match",
     lambda result, frames: (result["events"].reverse(),
                            result["eventDetails"].reverse())),
    ("eventdetails-not-a-list", None,
     lambda result, frames: result.update(eventDetails="bogus")),
    ("events-not-a-list", None,
     lambda result, frames: result.update(events=5)),
    # The report contract (README: any mismatch ends in verified:false): a
    # failing case must state its verdict explicitly, not just raise.
    ("failing-case-carries-verified-false", "model call count",
     lambda result, frames: result.update(modelCalls=2)),
]


def run_attack(name, forge):
    """Feed one forgery through the real verification path."""
    result = forged_result()
    frames = forged_frames(len(result["events"]))
    forge(result, frames)
    selftest.run_case = lambda *args: (result, frames, {"injections": []}, Path("."))
    return selftest.verify_caught(NAME, "text", "passthrough", 0,
                                  Path("."), Path("."), Path("mock.log"))


def caught(name, marker, cell):
    if cell.get("verified") is not False:
        return f"verified={cell.get('verified')!r}"
    message = cell.get("failed")
    if not isinstance(message, str) or "#969" not in message:
        return f"no dedicated message: {message!r}"
    if marker and marker not in message:
        return f"message lacks {marker!r}: {message}"
    return None


def main():
    # Sanity first: the unmodified base must verify, or every attack result
    # below would be meaningless.
    baseline = run_attack("baseline", lambda result, frames: None)
    if baseline.get("verified") is not True:
        print(f"BASELINE BROKEN: {NAME} did not verify: {baseline}")
        return 2
    escaped = []
    for name, marker, forge in ATTACKS:
        cell = run_attack(name, forge)
        reason = caught(name, marker, cell)
        if reason is None:
            print(f"caught {name}: {cell['failed']}")
        else:
            print(f"ESCAPED {name}: {reason}")
            escaped.append(name)
    print(f"{len(ATTACKS) - len(escaped)}/{len(ATTACKS)} attacks caught")
    if escaped:
        print(f"ATTACKS ESCAPED: {', '.join(escaped)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
