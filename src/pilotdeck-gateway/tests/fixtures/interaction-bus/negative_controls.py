#!/usr/bin/env python3
"""Temporary product mutations; require an oracle assertion failure, then restore.

Run with no other writers/builds in this worktree. Evidence goes in a new output
folder; original source bytes and hashes are verified after every mutation.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/runtime/interaction'
# Each independent mutant is applied to the original, never cumulatively.
mutations = [
    ('late-cross-thread-response', 'bus.rs',
     '&& (e.active.load(Ordering::SeqCst)\n                        || e.settled_by == Some(std::thread::current().id()))', ''),
    ('cross-thread-bulk', 'bus.rs',
     'stored\n        .settled_by\n        .is_none_or(|thread| thread == std::thread::current().id())', 'true'),
    ('same-thread-rejected-consume', 'bus.rs',
     '|| e.settled_by == Some(std::thread::current().id())', '|| false'),
    ('nested-reject-map', 'bus.rs',
     '        let mut cursor = 0;\n        loop {\n            // TS holds',
     '        let mut cursor = 0;\n        loop {\n            if !self.port.state.lock().unwrap().buckets.iter().any(|b| Arc::ptr_eq(&b.entries, &entries)) { break; }\n            // TS holds'),
    ('cancel-other-pending', 'bus.rs', '.retain(|e| e.token != self.token);', '.retain(|_| false);'),
    ('close-last-binding', 'reconnect.rs',
     '        state.last.retain(|(key, _)| key != owner);\n    }\n    /// Like TS',
     '        // mutant: keep last binding\n    }\n    /// Like TS'),
    ('peer-duplicate-error', 'bus.rs', '" is already pending for ".into()', '" is pending for ".into()'),
    ('peer-duplicate-check', 'bus.rs',
     '&r.owner == owner && r.kind == self.kind() && r.entry.request_id == entry.request_id',
     'false && &r.owner == owner && r.kind == self.kind() && r.entry.request_id == entry.request_id'),
    ('reject-window-final-delete-removed', 'bus.rs',
     '                    state\n                        .buckets\n                        .retain(|b| b.bus != self.id || &b.owner != owner);',
     '                    // mutant: keep the owner bucket after the exhausted loop'),
    ('reject-window-delete-before-loop', 'bus.rs',
     '        let Some(entries) = entries else {\n            return;\n        };\n        let mut cursor = 0;',
     '        let Some(entries) = entries else {\n            return;\n        };\n'
     '        self.port\n            .state\n            .lock()\n            .unwrap()\n            .buckets\n            .retain(|b| b.bus != self.id || &b.owner != owner);\n'
     '        let mut cursor = 0;'),
    ('injected-port-ignored', 'coordinator.rs',
     'let reconnect_port = options.reconnect_port.unwrap_or_default();',
     'let reconnect_port = InteractionReconnectPort::default();'),
    ('injected-grants-ignored', 'coordinator.rs',
     'permission_grants: options.permission_grants.unwrap_or_default(),',
     'permission_grants: Arc::new(GatewaySessionPermissionRuleSetRegistry::default()),'),
    ('injected-elicitation-ignored', 'coordinator.rs',
     '            elicitation_bus: options\n                .elicitation_bus\n                .unwrap_or_else(|| GatewayElicitationBus::new(reconnect_port.clone())),',
     '            elicitation_bus: GatewayElicitationBus::new(reconnect_port.clone()),'),
    ('bash-carriage-return', 'coordinator.rs', '10 | 13 | 0x2028 | 0x2029', '10 | 0x2028 | 0x2029'),
    ('deny-dispose-detached', 'bus.rs',
     '.find(|e| e.token > cursor && bulk_available(e))',
     '.find(|e| e.token > cursor && e.active.load(Ordering::SeqCst) && bulk_available(e))'),
    ('duplicate-error', 'bus.rs', ' is already pending for session ', ' is pending for session '),
    ('empty-id', 'bus.rs', 'if owner.is_empty() || entry.request_id.is_empty() {', 'if false {'),
    ('cancel-removes-pending', 'bus.rs', '                .retain(|e| e.token != self.token);', '                .retain(|_| true);'),
    ('consume-session-isolation', 'bus.rs', '.position(|b| b.bus == self.id && &b.owner == owner)?;', '.position(|b| b.bus == self.id)?;'),
    ('unknown-id', 'bus.rs', '&e.entry.request_id == request\n', 'true\n'),
    ('stale-response', 'bus.rs', 'if binding.is_some_and(|binding| {', 'if false && binding.is_some_and(|binding| {'),
    ('active-after-consume', 'bus.rs', '        stored.active.store(false, Ordering::SeqCst);\n        reconnect::remove', '        stored.active.store(true, Ordering::SeqCst);\n        reconnect::remove'),
    ('reject-live-reentry', 'bus.rs', '.find(|e| e.token > cursor && bulk_available(e))', '.find(|e| e.token > cursor && bulk_available(e) && !e.entry.request_id.equals_str("reentered"))'),
    ('rejection-text', 'bus.rs', '(entry.reject)(reason.clone());', '(entry.reject)("wrong_reason".into());'),
    ('permission-close-deny', 'bus.rs', '("decision", "deny".into()),', '("decision", "allow".into()),'),
    ('question-dispose-reject', 'bus.rs', '            if P {\n                self.deny', '            if true {\n                self.deny'),
    ('default-disposal-text', 'bus.rs', '"gateway_disposed".into()', '"disposed".into()'),
    ('preserve-pending-turn', 'coordinator.rs', 'let preserve = disconnected\n', 'let preserve = false && disconnected\n'),
    ('elicitation-observer', 'coordinator.rs', 'if let Some(observer) = &self.on_delivered {', 'if let Some(observer) = &None::<DeliveredObserver> {'),
    ('permission-decision-extras', 'coordinator.rs', 'self.remember.unwrap_or(JsValue::Undefined)', 'JsValue::Undefined'),
    ('close-clears-rules', 'coordinator.rs', 'self.permission_grants.close_session(owner);', '// deliberately retain session rules'),
    ('empty-grant', 'coordinator.rs', 'if rule.tool_name.is_empty() {', 'if false {'),
    ('grant-alias', 'coordinator.rs', '("Read", "read_file"),', '("Read", "Read"),'),
    ('close-callback-order', 'coordinator.rs', 'self.elicitation_bus.reject_session(owner, reason);\n        self.permission_bus.deny_session(owner, reason);', 'self.permission_bus.deny_session(owner, reason);\n        self.elicitation_bus.reject_session(owner, reason);'),
    ('reconnect-binds-pending', 'reconnect.rs', 'request.binding = Some(next.clone());', 'request.binding = None;'),
    ('disconnect-releases-request-binding', 'reconnect.rs', 'request.binding = None;', '// deliberately keep the old request binding'),
    ('reconnect-generation', 'reconnect.rs', 'next.generation <= last.generation', 'next.generation < last.generation'),
    ('binding-validation-text', 'reconnect.rs', 'Interaction connection generation must be a non-negative safe integer.', 'Invalid generation.'),
    ('replay-undefined-object', 'reconnect.rs', '!key.equals_str("__proto__") && !v.is_undefined()', '!key.equals_str("__proto__")'),
    ('replay-undefined-array', 'reconnect.rs', 'Work::Visit(JsValue::Undefined) => values.push(JsValue::Null),', 'Work::Visit(JsValue::Undefined) => values.push(JsValue::Bool(true)),'),
    ('replay-nonfinite', 'reconnect.rs', 'Work::Visit(JsValue::Number(n)) if !n.is_finite() => values.push(JsValue::Null),', 'Work::Visit(JsValue::Number(n)) if !n.is_finite() => values.push(JsValue::Number(42.0)),'),
    ('replay-prototype', 'reconnect.rs', '!key.equals_str("__proto__") && !v.is_undefined()', '!v.is_undefined()'),
]
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
env = os.environ.copy()
env.setdefault('CARGO_TARGET_DIR', str(root / 'target'))
summary = []
with mutating_sources(root, sorted({source / filename for _, filename, _, _ in mutations})) as snapshots:
    for name, filename, old, new in mutations:
        path = source / filename
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        expected = 2 if name in ['reject-live-reentry', 'deny-dispose-detached'] else 1
        assert content.count(old) == expected, (name, content.count(old))
        changed = new.join(content.rsplit(old, 1)) if name == 'deny-dispose-detached' else content.replace(old, new, 1)
        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(content.splitlines(True), changed.splitlines(True), fromfile=filename, tofile=filename)))
        try:
            # Required resource check; do not interpret the output as a concurrency
            # guarantee if the sandbox denies ps. Its raw output is retained.
            while True:
                slots = subprocess.run([str(root / 'scripts/cargo-slots.sh')], cwd=root, capture_output=True, text=True)
                (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)
                if slots.returncode != 1:
                    assert slots.returncode == 0
                    break
                time.sleep(5)
            path.write_text(changed)
            # Each mutation names the test whose assertion it must turn red.
            test = {
                'late-cross-thread-response': 'interaction_reject_cross_thread_response_and_bulk_have_one_winner',
                'cross-thread-bulk': 'interaction_reject_cross_thread_response_and_bulk_have_one_winner',
                'reject-window-final-delete-removed': 'interaction_register_racing_reject_window_matches_ts_map_semantics',
                'reject-window-delete-before-loop': 'interaction_register_racing_reject_window_matches_ts_map_semantics',
                'injected-port-ignored': 'interaction_injected_options_share_the_supplied_components',
                'injected-grants-ignored': 'interaction_injected_options_share_the_supplied_components',
                'injected-elicitation-ignored': 'interaction_injected_options_share_the_supplied_components',
            }.get(name, 'interaction_real_ts_stepwise_oracle')
            result = run_owned(['cargo','test','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib',
                f'runtime::interaction::tests::{test}'], cwd=root, env=env, timeout=240)
            log = result.stdout + result.stderr
            (out / f'{name}.log').write_text(log)
            killed = result.returncode == 101 and f'{test} ... FAILED' in log and ('assertion `left == right` failed' in log or 'late response:' in log)
            failure = next((line.strip() for line in log.splitlines() if 'assertion `left == right` failed' in line or 'late response:' in line), '')
            summary.append(dict(name=name, killed=killed, returncode=result.returncode, failure=failure, sourceSha256=before))
            print(f'{name}: {"KILLED" if killed else "NOT KILLED"} rc={result.returncode} {failure}', flush=True)
            assert killed, f'{name}: compile/signal failure or surviving mutant is not evidence'
        finally:
            path.write_bytes(original)
            assert hashlib.sha256(path.read_bytes()).hexdigest() == before
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2)+'\n')
    print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte',flush=True)
