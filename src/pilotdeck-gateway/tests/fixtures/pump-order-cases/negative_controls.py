#!/usr/bin/env python3
"""Run all selected #855/#741i controls, then summarize; commit sources first.

`gate-order` attaches the pump before the #832 durable gates (each of the three
gate scenarios must catch it on its own); `wake-recheck` removes the post-wake
`stopped` re-check; `final-overtakes` lets the final skip the
reserved-slot predecessor it must publish behind. A mutant only counts as
killed when the exact named test panicked with its bound semantic assertion: a compile
failure, a gate/cleanup timeout or any other test's panic does not.

Table rows follow the anchor_selfcheck layout `(name, path, before, after,
...)` so the static preflight can resolve every anchor to its source file.
"""
import os
import argparse
import json
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
assert os.environ.get('CARGO_TARGET_DIR'), 'caller must set CARGO_TARGET_DIR'
path = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
mutations = {
    'gate-order': (
        path,
        '''                // No connection or execute can start until both durable gates commit.
                let Ok(messages) = execute_wait.await else {
                    return Ok(ExecuteResult::Aborted { terminal: None });
                };
                request
                    .payload
                    .insert("messages".into(), JsValue::Array(messages));
                let client = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Ok(ExecuteResult::Aborted { terminal: None }),
                    client = client => client?,
                };
                pending_host.handoff();
                let identity = request.identity.clone();
                handler
                    .with_pump(
                        client.execute_cancellable(request, handler.clone(), cancellation),
                        identity,
                    )
                    .await''',
        '''                // No connection or execute can start until both durable gates commit.
                let identity = request.identity.clone();
                let gated = handler.clone();
                let mut gated_request = request;
                let gated_cancellation = cancellation;
                let gated_pending_host = pending_host;
                handler
                    .with_pump(
                        Box::pin(async move {
                            let Ok(messages) = execute_wait.await else {
                                return Ok(ExecuteResult::Aborted { terminal: None });
                            };
                            gated_request
                                .payload
                                .insert("messages".into(), JsValue::Array(messages));
                            let client = tokio::select! {
                                biased;
                                _ = gated_cancellation.cancelled() => return Ok(ExecuteResult::Aborted { terminal: None }),
                                client = client => client?,
                            };
                            gated_pending_host.handoff();
                            client
                                .execute_cancellable(gated_request, gated.clone(), gated_cancellation)
                                .await
                        }),
                        identity,
                    )
                    .await''',
        'gates_pump_',
        [('gates_pump_reject_never_starts_the_pump', '#855 [reject]'),
         ('gates_pump_cancel_never_starts_the_pump', '#855 [cancel]'),
         ('gates_pump_open_pins_the_start_order', '#855 [open]')],
    ),
    'wake-recheck': (
        path,
        '                    if self.forwarding.lock().unwrap().stopped || Instant::now() < self.forwarding.lock().unwrap().deadline() { continue; }',
        '                    if Instant::now() < self.forwarding.lock().unwrap().deadline() { continue; }',
        'host_pump_final_orders_behind_the_reserved_slot',
        [('host_pump_final_orders_behind_the_reserved_slot',
          '#855: the probe batch must publish')],
    ),
    'final-overtakes': (
        path,
        '''        let delivery = {
            let mut forwarding = self.forwarding.lock().unwrap();
            if final_event {
                forwarding.reserve()
            } else {''',
        '''        let delivery = {
            let mut forwarding = self.forwarding.lock().unwrap();
            if final_event {
                let mut overtaking = forwarding.reserve();
                overtaking.previous = None;
                overtaking
            } else {''',
        'host_pump_final_orders_behind_the_reserved_slot',
        [('host_pump_final_orders_behind_the_reserved_slot',
          '#855: the final must stay parked')],
    ),
}
# Keep the old mutation names/anchors for callers; each gate cell binds one
# complete libtest name and specific semantic assertion, never a setup/timeout.
mutations['model-prefix-lost'] = (
    path,
    '                        permit.send((event, wire, receipt));',
    '''                        let mut event = event;
                        if let JsValue::Object(ref mut object) = event {
                            if object.get("type") == Some(&"model_request_started".into()) {
                                object.insert("type".into(), "model_event".into());
                            }
                        }
                        permit.send((event, wire, receipt));''',
    'gates_pump_open_pins_the_start_order',
    [('gates_pump_open_pins_the_start_order',
      '#741i [open]: model_request_started must precede turn_completed')],
)

# A legal but wrong event name must fail at the new strict host-prefix check,
# independently in both the cancelled and the successful gate cells.
mutations['submitted-prefix-lost'] = (
    path,
    '    fn deliver_recorded(&mut self, event: JsValue) -> Result<Option<JsValue>, RuntimeError> {',
    '''    fn deliver_recorded(&mut self, mut event: JsValue) -> Result<Option<JsValue>, RuntimeError> {
        if let JsValue::Object(ref mut object) = event {
            if object.get("type") == Some(&"user_prompt_submitted".into()) {
                object.insert("type".into(), "model_event".into());
            }
        }''',
    'gates_pump_',
    [('gates_pump_cancel_never_starts_the_pump',
      '#741k [cancel]: user_prompt_submitted must follow input_accepted'),
     ('gates_pump_open_pins_the_start_order',
      '#741k [open]: user_prompt_submitted must follow input_accepted')],
)

from negative_suite import Case, run_cases


def mutate(original, _scope, replacements):
    for old, new in replacements:
        if original.count(old) != 1:
            raise ValueError(f'expected one mutation anchor, got {original.count(old)}')
        original = original.replace(old, new, 1)
    return original


def bound_cases(selected):
    bound = []
    for name in selected:
        source, old, new, _test_filter, checks = mutations[name]
        for test, marker in checks:
            if name == 'gate-order':
                cell = {'gates_pump_reject_never_starts_the_pump': 'reject',
                        'gates_pump_cancel_never_starts_the_pump': 'cancel',
                        'gates_pump_open_pins_the_start_order': 'open'}[test]
                marker = [f'#855 [{cell}]: no pump entry while the gate holds',
                          f'#855 [{cell}]: the pump must start only after the gate commits']
                if cell == 'reject':
                    marker.append('#855 [reject]: the pump must never start, not even before the failure terminal')
                elif cell == 'cancel':
                    marker.append('#855 [cancel]: the pump must never start, not even before the abort terminal')
                else:
                    marker.append('#855 [open]: the pump must not tick at gate open')
            module = 'gates_tests' if test.startswith('gates_') else 'host_pump_tests'
            full_test = f'runtime::turn::live::stream_tests::{module}::{test}'
            bound.append(Case(name + '-' + test if len(checks) > 1 else name,
                              source, old, new, full_test, marker))
    return bound


def compile_cases(selected, out):
    """Compile every distinct mutation before runtime controls; always summarize."""
    results = []
    try:
        with mutating_sources(root, [path]) as originals:
            for name in selected:
                source, old, new, _test_filter, _checks = mutations[name]
                status, reason = 'INVALID', ''
                try:
                    source.write_text(mutate(originals[source].decode(), None, [(old, new)]))
                    slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
                    (out / f'{name}.slots.log').write_text(slot.stdout + slot.stderr)
                    result = run_owned(['cargo', 'test', '--offline', '--manifest-path',
                                        'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib', '--no-run'],
                                       cwd=root, env=os.environ.copy(), timeout=900)
                    (out / f'{name}.log').write_text(result.stdout + result.stderr)
                    status = 'COMPILED' if result.returncode == 0 else 'INVALID'
                    reason = f'cargo exit={result.returncode}'
                except Exception as error:
                    reason = f'{type(error).__name__}: {error}'
                finally:
                    source.write_bytes(originals[source])
                results.append({'mutation': name, 'status': status, 'reason': reason})
                print(f'{name}: {status}: {reason}', flush=True)
    except Exception as error:
        for name in selected[len(results):]:
            results.append({'mutation': name, 'status': 'INVALID', 'reason': str(error)})
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + f'; source guard: {error}')
    (out / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')
    return int(not results or any(row['status'] != 'COMPILED' for row in results))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('names', nargs='*', choices=None)
    parser.add_argument('--sample', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    args = parser.parse_args()
    unknown = set(args.names) - set(mutations)
    if unknown:
        parser.error(f'unknown controls: {sorted(unknown)}')
    selected = ['model-prefix-lost'] if args.sample else args.names or list(mutations)
    args.out.mkdir(parents=True, exist_ok=False)
    if args.compile_only:
        return compile_cases(selected, args.out)
    return run_cases(root, bound_cases(selected), args.out, os.environ.copy(), mutate)


if __name__ == '__main__':
    raise SystemExit(main())
