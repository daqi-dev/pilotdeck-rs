#!/usr/bin/env python3
"""Negative controls for the #929 external skills management port.

Every mutation edits product source in place through the shared
``mutating_sources`` guard and is bound to one exact named test plus the
semantic assertion that must fail.  Each case is classified KILLED / SURVIVED /
INVALID, all cases run to completion, and any non-KILLED result exits non-zero.

The source tree must equal ``HEAD`` for the guard to run.  In the delivery
sandbox the patch is uncommitted, so the coordinator runs this after committing:

  CARGO_TARGET_DIR=/path/to/target python3 \
    src/pilotdeck-gateway/tests/fixtures/skills-external/negative_controls.py \
    _tmp/<date>-issue-929/negative-controls

Table rows follow anchor_selfcheck's ``(name, path, before, after, ...)``
layout so the static preflight can resolve every anchor.
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
binding = root / "src/pilotdeck-gateway/src/skills/binding.rs"
external = root / "src/pilotdeck-gateway/src/skills/external.rs"

MUTATIONS = {
    "external-branch-removed": (
        binding,
        """            SkillsBinding::External => {
                let binding = ExternalSkillBinding::from_value(&binding)?;
                ExternalSkillsPort::new(binding, self.external.clone())
                    .call(method, input)
                    .await
            }""",
        """            SkillsBinding::External => Err(disabled()),""",
        "skills::tests::facade_routes_each_method_to_the_selected_port",
        ["#929 external List must reach the module host"],
    ),
    "snapshot-binding-ignored": (
        binding,
        "        let binding = (self.binding_for)(project_key)?;",
        """        let _ = project_key;
        let binding = (self.binding_for)(None)?;""",
        "skills::external_tests::facade_routes_each_project_to_its_snapshot_binding",
        ["#929 project A must hit its own module host"],
    ),
    "provider-code-projected": (
        external,
        "    SkillError::provider_failure(code, message, retryability, details)",
        """    let _ = code;
    SkillError::provider_failure(
        "MODULE_CALL_FAILED",
        message,
        retryability,
        details,
    )""",
        "skills::external_tests::external_skills_oracle_replays_requests_and_outcomes",
        ["#929 case list-provider-error outcome must match real TS"],
    ),
    "correlation-check-removed": (
        external,
        """    let correlated = in_reply_to.equals_str(message_id)
        && match object.get("requestId") {
            None => true,
            Some(value) if value.is_undefined() => true,
            Some(value) => value
                .as_string()
                .is_some_and(|value| value.equals_str(request_id)),
        };""",
        """    let correlated = true;
    let _ = (in_reply_to, message_id, request_id);""",
        "skills::external_tests::external_skills_oracle_replays_requests_and_outcomes",
        ["#929 case correlation-mismatch outcome must match real TS"],
    ),
    "timeout-code-projected": (
        external,
        '                ModuleTransportError::Timeout => SkillError::transport_failure(\n                    "MODULE_TIMEOUT",',
        '                ModuleTransportError::Timeout => SkillError::transport_failure(\n                    "MODULE_TRANSPORT_UNAVAILABLE",',
        "skills::external_tests::timeout_maps_to_module_timeout",
        ["#929 timeout keeps MODULE_TIMEOUT"],
    ),
    "result-object-check-removed": (
        external,
        "        if result.as_object().is_none() {",
        "        if matches!(result, JsValue::Null) {",
        "skills::external_tests::external_skills_oracle_replays_requests_and_outcomes",
        ["#929 case result-not-object outcome must match real TS"],
    ),
    "body-deadline-not-shared": (
        external,
        """            let body = match tokio::time::timeout_at(
                deadline,
                crate::fetch_redirect::read_body(""",
        """            let body = match tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_secs(30),
                crate::fetch_redirect::read_body(""",
        "skills::external_tests::stalled_body_is_bounded_by_the_request_timeout",
        ["#929 stalled body must return within timeoutMs instead of waiting forever"],
    ),
    "manifest-failure-not-shared": (
        external,
        """                    *flight
                        .result
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result.clone());""",
        """                    *flight
                        .result
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Ok(()));""",
        "skills::external_tests::concurrent_manifest_failure_is_shared_by_every_waiter",
        ["#929 concurrent waiter must receive the same manifest failure"],
    ),
    "bom-not-stripped": (
        external,
        "        let text = text.strip_prefix('\\u{feff}').unwrap_or(&text);",
        "        let text: &str = &text;",
        "skills::external_tests::bom_is_stripped_from_manifest_and_response",
        ["#929 BOM-prefixed manifest and response must parse"],
    ),
    "body-error-mapped-to-transport": (
        external,
        "                Ok(Err(_)) => Vec::new(),",
        "                Ok(Err(_)) => return Err(ModuleTransportError::Unavailable),",
        "skills::external_tests::truncated_success_body_is_a_protocol_error",
        ["#929 truncated 2xx body keeps the protocol code"],
    ),
    "provider-message-lossy": (
        external,
        "    let message = nonempty(error.get(\"message\")).unwrap_or_else(|| code.clone());",
        """    let message = nonempty(error.get("message")).unwrap_or_else(|| code.clone());
    let message = JsString::from(String::from_utf16_lossy(message.units()));""",
        "skills::external_tests::provider_error_keeps_unpaired_surrogates",
        ["#929 provider message keeps the unpaired surrogate"],
    ),
    "non-response-validation-skipped": (
        external,
        """    pilotdeck_protocol::module::validate_module_message(value)
        .map_err(|error| SkillError::protocol_failure(error.message))?;
    let object = value
        .as_object()
        .ok_or_else(|| SkillError::protocol_failure("Module message must be an object."))?;
    if object
        .get("kind")
        .and_then(JsValue::as_string)
        .is_none_or(|kind| !kind.equals_str("response"))
    {
        return Err(SkillError::protocol_failure(
            "Module call did not return a response envelope.",
        ));
    }""",
        """    let object = value
        .as_object()
        .ok_or_else(|| SkillError::protocol_failure("Module message must be an object."))?;
    if object
        .get("kind")
        .and_then(JsValue::as_string)
        .is_none_or(|kind| !kind.equals_str("response"))
    {
        return Err(SkillError::protocol_failure(
            "Module call did not return a response envelope.",
        ));
    }
    pilotdeck_protocol::module::validate_module_message(value)
        .map_err(|error| SkillError::protocol_failure(error.message))?;""",
        "skills::external_tests::non_response_messages_get_the_full_protocol_validation",
        ['#929 protocol validation message for {"kind":"error"}'],
    ),
    "owner-cancel-leaves-flight": (
        external,
        """        if self.settled {
            return;
        }""",
        """        if true {
            return;
        }""",
        "skills::external_tests::cancelled_manifest_owner_releases_its_waiters",
        ["#929 waiter must recover when the in-flight manifest owner is cancelled (bounded)"],
    ),
    "timer-clamp-removed": (
        external,
        "            timeout: crate::runtime::network::node_timer_duration(self.binding.timeout_ms),",
        "            timeout: Duration::try_from_secs_f64(self.binding.timeout_ms / 1000.0)\n                .unwrap_or(Duration::MAX),",
        "skills::external_tests::oversized_timeout_uses_the_node_timer_clamp",
        ["#929 timeoutMs 2147483648 must schedule the Node timer delay 1ms"],
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
    selected = ["external-branch-removed"] if args.sample else (args.names or list(MUTATIONS))
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
