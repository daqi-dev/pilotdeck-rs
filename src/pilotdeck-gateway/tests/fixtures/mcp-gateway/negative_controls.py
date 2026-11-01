#!/usr/bin/env python3
"""Negative controls for the #1215 U6 MCP control gateway adapter.

Every mutation edits product source in place through the shared
``mutating_sources`` guard and is bound to one exact named lib test plus the
semantic assertion that must fail. Each case is classified KILLED / SURVIVED /
INVALID, all cases run to completion, and any non-KILLED result exits non-zero.

The source tree must equal ``HEAD`` for the guard to run. In the delivery
sandbox the patch is uncommitted, so the coordinator runs this after committing:

  CARGO_TARGET_DIR=/path/to/target python3 \\
    src/pilotdeck-gateway/tests/fixtures/mcp-gateway/negative_controls.py \\
    _tmp/<date>-issue-1215/negative-controls

Rows follow anchor_selfcheck's ``MUTATIONS`` dict layout
``name: (path, before, after, test, [markers])`` so the static preflight can
resolve every anchor. A mutation may list several markers; ``cases_for`` turns
each marker into its own ``Case``, so every marker must fail under the mutation
or the row is INVALID.
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases

root = Path(__file__).resolve().parents[5]
assert os.environ.get("CARGO_TARGET_DIR"), "caller must set CARGO_TARGET_DIR"
gateway = root / "src/pilotdeck-gateway/src/runtime/mcp/gateway.rs"
sdk = root / "src/pilotdeck-gateway/src/runtime/mcp/sdk.rs"
session_handle = root / "src/pilotdeck-gateway/src/runtime/session/handle.rs"

REPLAY = "runtime::mcp::gateway_tests::mcp_gateway_replays_the_real_ts_oracle"
DISPATCH = "runtime::mcp::gateway_tests::mcp_gateway_dispatch_returns_none_for_non_control_methods"


def _response(case_id):
    return f"#1215 case {case_id}: the reconstructed WS response must match TS"


def _events(case_id):
    return f"#1215 case {case_id}: the event stream must match TS"


# Every marker names the exact assertion message that must turn red.
MUTATIONS = {
    # The SESSION_BUSY gate: removing it lets all four methods run their
    # registry callback on a busy session, so every busy row's envelope,
    # event stream and post state all diverge from the TS bytes.
    "busy-gate-removed": (
        gateway,
        "        if self.options.owner.has_active_turn(session_key) {",
        "        if false && self.options.owner.has_active_turn(session_key) {",
        REPLAY,
        [
            _response("set-busy"),
            _response("toggle-busy"),
            _response("reconnect-busy"),
            _response("override-busy-valid"),
        ],
    ),
    # The post-success router close. The registry callback already closes on
    # its own happy path, so the kill comes from the rows where only the
    # component closes: diagnostic-only sets (no evict) and every override
    # (the registry never closes there, and the close-fault row stops
    # failing).
    "close-after-success-removed": (
        gateway,
        """        self.options
            .owner
            .close(session_key)
            .await
            .map_err(|error| SdkMcpError::plain("Error", JsString::from(error.message)))""",
        """        let _ = session_key;
        Ok(())""",
        REPLAY,
        [
            _events("set-invalid-config"),
            _events("override-auto"),
            _response("override-close-fault"),
        ],
    ),
    # The shared `!input.sessionKey?.trim() || !input.serverName?.trim()`
    # validation of the three per-server controls. The registry layer
    # re-validates `set_mcp_servers` itself, so the control binds to the
    # toggle/reconnect/override rows whose missing-field envelopes differ
    # from the registry's own errors.
    "control-validation-relaxed": (
        gateway,
        """        let session_key = required_trimmed(
            input.get("sessionKey"),
            "sessionKey",
            "INVALID_MCP_SERVER",
            "sessionKey and serverName are required.",
        )?;
        required_trimmed(
            input.get("serverName"),
            "serverName",
            "INVALID_MCP_SERVER",
            "sessionKey and serverName are required.",
        )?;
        Ok(session_key)""",
        """        Ok(input
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default())""",
        REPLAY,
        [
            _response("toggle-missing-server-name"),
            _response("toggle-blank-server-name"),
            _response("override-missing-fields"),
            _response("reconnect-missing-fields"),
            _response("toggle-missing-session-key"),
            _response("toggle-blank-session-key"),
            _response("reconnect-missing-session-key"),
            _response("override-missing-session-key"),
        ],
    ),
    # The WS-layer `{ ok: true }` shape of the two void controls.
    "ok-true-shape-removed": (
        gateway,
        """    let mut object = JsObject::new();
    object.insert("ok".into(), JsValue::Bool(true));
    JsValue::Object(object)""",
        "    JsValue::Null",
        REPLAY,
        [
            _response("toggle-off"),
            _response("reconnect-success"),
        ],
    ),
    # The mode check must precede the busy gate; swapping them answers the
    # busy-with-invalid-mode row with SESSION_BUSY instead of
    # INVALID_MCP_PERMISSION_MODE.
    "override-mode-after-busy": (
        gateway,
        """        if !mode_ok {
            let mut units = "Unsupported MCP permission mode: "
                .encode_utf16()
                .collect::<Vec<_>>();
            units.extend(js_value::to_string_units(&mode).map_err(range_error)?);
            return Err(SdkMcpError::gateway(
                "INVALID_MCP_PERMISSION_MODE",
                JsString::from_utf16(units),
            ));
        }
        self.reject_active_turn(
            &session_key,
            "Cannot change MCP permission mode while a turn is active.",
        )?;""",
        """        self.reject_active_turn(
            &session_key,
            "Cannot change MCP permission mode while a turn is active.",
        )?;
        if !mode_ok {
            let mut units = "Unsupported MCP permission mode: "
                .encode_utf16()
                .collect::<Vec<_>>();
            units.extend(js_value::to_string_units(&mode).map_err(range_error)?);
            return Err(SdkMcpError::gateway(
                "INVALID_MCP_PERMISSION_MODE",
                JsString::from_utf16(units),
            ));
        }""",
        REPLAY,
        [
            _response("override-busy-invalid-mode"),
        ],
    ),
    # The dispatch entry must return None for everything that is not one of
    # the four controls; the wider method table owns those.
    "unknown-method-dispatched": (
        gateway,
        "            _ => return None,",
        "            _ => Ok(JsValue::Undefined),",
        DISPATCH,
        [
            "#1215 dispatch must not own non-control method mcp_server_status",
        ],
    ),
    # The close-fault rows of the golden answer `Failed to dispose agent
    # handle.`, produced by the real #1011 LiveSessionHandle aggregation
    # over the fake's distinct `fixture dispose failure` rejection (fix-r1
    # P2-3: the TS fixture does the same through the real AgentHandle). A
    # passthrough aggregate — the single-failure semantics of
    # `RuntimeError::aggregate` — would leak the fake's distinct text into
    # both close-fault envelopes.
    "dispose-aggregation-bypassed": (
        session_handle,
        """    } else {
        Err(RuntimeError {
            message: message.into(),
            causes: failures,
            js_error: None,
        })
    }
}""",
        """    } else {
        Err(failures.into_iter().next().unwrap())
    }
}""",
        REPLAY,
        [
            _response("set-close-fault"),
            _response("override-close-fault"),
        ],
    ),
    # `require_input` must keep `undefined` distinct from `null`: a frame
    # without `params` answers "...of undefined...", a `params: null` frame
    # "...of null..." (fix-r1 P2-2 probe). Dropping the Undefined arm makes
    # the omitted-params row replay with the registry's INVALID_SESSION_KEY
    # envelope instead of the TypeError the TS bytes carry.
    "undefined-params-conflated": (
        sdk,
        """        JsValue::Undefined | JsValue::Null => Err(SdkMcpError::type_error(format!(
            "Cannot read properties of {} (reading '{}')",
            input_kind(input),
            field
        ))),""",
        """        JsValue::Null => Err(SdkMcpError::type_error(format!(
            "Cannot read properties of {} (reading '{}')",
            input_kind(input),
            field
        ))),""",
        REPLAY,
        [
            _response("set-params-omitted"),
        ],
    ),
}


def mutate(original, _scope, replacements):
    for old, new in replacements:
        count = original.count(old)
        if count != 1:
            raise ValueError(f"expected one mutation anchor, got {count}")
        original = original.replace(old, new, 1)
    return original


def cases_for(names):
    bound = []
    for name in names:
        source, old, new, test, markers = MUTATIONS[name]
        for marker in markers:
            bound.append(Case(name, source, old, new, test, marker))
    return bound


def check_anchors(selected):
    stale = []
    for name in selected:
        source, old = MUTATIONS[name][0], MUTATIONS[name][1]
        count = source.read_text().count(old)
        if count != 1:
            stale.append((name, source.relative_to(root).as_posix(), count))
    if stale:
        for name, path, count in stale:
            print(f"STALE ANCHOR {name}: {count} matches in {path}")
        print("STALE ANCHORS: refusing to run")
        raise SystemExit(2)
    print(f"{len(selected)} anchors: ok")


def compile_cases(selected, out):
    """Apply each mutation once, prove `cargo test --no-run`, restore."""
    results = []
    try:
        with mutating_sources(root, list(dict.fromkeys(MUTATIONS[name][0] for name in selected))) as originals:
            for name in selected:
                source, old, new = MUTATIONS[name][:3]
                status, reason = "INVALID", ""
                try:
                    source.write_text(mutate(originals[source].decode(), None, [(old, new)]))
                    slot = wait_for_slot(root / "scripts/cargo-slots.sh")
                    (out / f"{name}-compile.slots.log").write_text(slot.stdout + slot.stderr)
                    result = run_owned(
                        ["cargo", "test", "--offline", "--manifest-path", "src/Cargo.toml",
                         "-p", "pilotdeck-gateway", "--lib", "--no-run"],
                        cwd=root, env=os.environ.copy(), timeout=1800,
                    )
                    (out / f"{name}-compile.log").write_text(result.stdout + result.stderr)
                    status = "COMPILED" if result.returncode == 0 else "INVALID"
                    reason = f"cargo exit={result.returncode}"
                except Exception as error:
                    reason = f"{type(error).__name__}: {error}"
                finally:
                    source.write_bytes(originals[source])
                results.append({"mutation": name, "status": status, "reason": reason})
                print(f"{name}: {status}: {reason}", flush=True)
    except Exception as error:
        for name in selected[len(results):]:
            results.append({"mutation": name, "status": "INVALID", "reason": str(error)})
        for row in results:
            row.update(status="INVALID", reason=row["reason"] + f"; source guard: {error}")
    (out / "compile-summary.json").write_text(json.dumps(results, indent=2) + "\n")
    counts = {state: sum(row["status"] == state for row in results)
              for state in ("COMPILED", "INVALID")}
    print("compile summary: " + ", ".join(f"{key}={value}" for key, value in counts.items()))
    return int(not results or any(row["status"] != "COMPILED" for row in results))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path, nargs="?")
    parser.add_argument("names", nargs="*", choices=sorted(MUTATIONS))
    parser.add_argument("--sample", action="store_true")
    parser.add_argument("--compile-only", action="store_true")
    parser.add_argument("--check-anchors", action="store_true")
    args = parser.parse_args()
    selected = ["busy-gate-removed"] if args.sample else (args.names or list(MUTATIONS))
    if args.check_anchors:
        check_anchors(selected)
        return 0
    assert args.out is not None, "provide the output directory"
    args.out.mkdir(parents=True, exist_ok=False)
    check_anchors(selected)
    compiled = compile_cases(selected, args.out)
    if args.compile_only:
        return compiled
    runtime = run_cases(
        root,
        cases_for(selected),
        args.out,
        os.environ.copy(),
        mutate,
        timeout=1800,
    )
    return int(compiled != 0 or runtime != 0)


if __name__ == "__main__":
    raise SystemExit(main())
