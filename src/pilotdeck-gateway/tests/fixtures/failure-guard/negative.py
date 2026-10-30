#!/usr/bin/env python3
"""M6 failure-guard mutations; keep all controls and validate submit oracle steps."""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _negkit.contract import Case
from _negkit import submit as adapter

fixture = Path(__file__).resolve().parent
workspace = fixture.parents[3]
repo = workspace.parent
turn = workspace / 'pilotdeck-gateway/src/runtime/turn'
guard = turn / 'failure_guard.rs'
submit = turn / 'submit.rs'
submit_oracle = json.loads((fixture.parent / 'turn-submit/oracle.json').read_text())['cases']

pure = 'runtime::turn::failure_guard::tests::real_ts_failure_guard_and_classify'
replay = 'runtime::turn::submit::tests::real_ts_submit_scenarios'

def submit_marker(case, index, step):
    return adapter.submit_marker(submit_oracle, case, index, step)

def guard_trip_marker(case, action):
    return (f'#813 failure guard trip: {case} action {action}: '
            'trip result bytes must match')

def guard_message_marker(case, action):
    return (f'#813 failure guard message: {case} action {action}: '
            'trip message bytes must match')

# #813 fix-r8: every mutant names the complete message of the one assertion
# that must fail. There is no `case: step` / `failure guard` fallback: those
# prefixes also matched another assertion in the same test (sibling P1).
def mutation(name, path, before, after, case=None, marker=None):
    if path.read_text().count(before) != 1:
        raise ValueError(f'{name}: anchor not unique')
    assert marker is not None, (name, 'needs an explicit target assertion message')
    assert marker.startswith('#813 '), (name, 'marker must be a #813 assertion message', marker)
    assert marker not in ('#813 failure guard', '#813 assertion'), (name, marker)
    return dict(name=name, path=path, before=before, after=after, test=replay if case else pure,
        case=case, marker=marker, target=marker)

accumulate = 'accumulate-success-does-not-reset-and-trip-latches'
model_window = 'model-window-and-reset'
longest = 'longest-prefix-shared-across-subagent'
mutants = [
    mutation('tool-threshold-plus-one', guard, '*failures < limit', '*failures < limit + 1.0',
        marker=guard_trip_marker(accumulate, 4)),
    mutation('tool-threshold-minus-one', guard, '*failures < limit', '*failures < limit - 1.0',
        marker=guard_trip_marker(accumulate, 2)),
    mutation('model-threshold-plus-one', guard, 'self.model_failures < limit',
        'self.model_failures < limit + 1.0', marker=guard_trip_marker(model_window, 3)),
    mutation('model-threshold-minus-one', guard, 'self.model_failures < limit',
        'self.model_failures < limit - 1.0', marker=guard_trip_marker(model_window, 2)),
]
mutants += [
    mutation('counted-classification-wrong', guard, '"tool_unavailable",', '"permission_denied",',
        marker=guard_trip_marker(accumulate, 4)),
    mutation('prefix-shortest-not-longest', guard, '.max_by_key(|(unit, _)| unit.units().len())', '.min_by_key(|(unit, _)| unit.units().len())',
        marker=guard_trip_marker(longest, 2)),
    mutation('exact-unit-skipped', guard, 'if let Some(limit) = limits.get_key(name)', 'if let Some(limit) = limits.get_key(name).filter(|_| false)',
        marker=guard_trip_marker(accumulate, 4)),
    mutation('success-clears-window', guard, 'if !is(result, "type", "error") {', 'if !is(result, "type", "error") { self.tool_failures.clear();',
        marker=guard_trip_marker(accumulate, 4)),
    # #813 fix-r10: the disabled latch cannot show on a fresh guard
    # (`tripped` is already false, so `&& false` is a no-op). It first
    # shows after the accumulate trip: action 6 re-counts mcp__db__exact
    # and returns a second trip where the gold is null.
    mutation('trip-latch-ignored', guard, 'if self.tripped || ok ||', 'if (self.tripped && false) || ok ||',
        marker=guard_trip_marker(accumulate, 6)),
    mutation('trip-message-changed', guard, '次，已自动停止，请稍后重试', '次，已自动停止，请重试',
        marker=guard_message_marker(accumulate, 4)),
    mutation('disabled-policy-observed', submit, '.filter(|config| truthy(field(config, "enabled")))', '.filter(|config| { let _ = config; true })', 'guard-disabled',
        submit_marker('guard-disabled', 2, 'terminal')),
    mutation('model-not-observed', submit, '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "turn_continued") && let Some(trip) = guard.observe(&event)', 'guard-model-trip',
        submit_marker('guard-model-trip', 2, 'terminal')),
    mutation('tool-not-observed', submit, '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "tool_result") && let Some(trip) = guard.observe(&event)', 'guard-tool-trip',
        submit_marker('guard-tool-trip', 2, 'terminal')),
    mutation('subagent-not-observed', submit, '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "subagent_tool_result") && let Some(trip) = guard.observe(&event)', 'guard-subagent-trip',
        submit_marker('guard-subagent-trip', 2, 'terminal')),
    mutation('trip-still-admitted', submit, 'let _ = handle.abort(&format!("run_policy:{code}").into());', 'let _ = (&handle, &code);', 'guard-model-trip',
        submit_marker('guard-model-trip', 2, 'terminal')),
    mutation('trip-status-not-recorded', submit, 'let _ = self.host.record_failure(run, &record).await;\n        publisher.gateway_event(object([', 'let _ = record;\n        publisher.gateway_event(object([', 'guard-tool-trip',
        submit_marker('guard-tool-trip', 2, 'terminal')),
    mutation('record-error-propagated', submit, 'let _ = self.host.record_failure(run, &record).await;\n        publisher.gateway_event(object([', 'self.host.record_failure(run, &record).await?;\n        publisher.gateway_event(object([', 'guard-trip-status-record-failed',
        submit_marker('guard-trip-status-record-failed', 2, 'terminal')),
    mutation('model-metadata-dropped', submit, 'build_guard_trip_message(trip, model.as_ref())', 'build_guard_trip_message(trip, model.as_ref().filter(|_| false))', 'guard-model-trip',
        submit_marker('guard-model-trip', 2, 'terminal')),
    mutation('trip-abort-reason-wrong', submit, 'format!("run_policy:{code}")', 'format!("policy:{code}")', 'guard-tool-trip',
        submit_marker('guard-tool-trip', 2, 'terminal')),
]
mutants += [
    mutation('guard-wire-order-changed', submit, '("runId", JsValue::String(run.run_id.clone())),\n            ("event", "run_policy_stopped".into()),', '("event", "run_policy_stopped".into()),\n            ("runId", JsValue::String(run.run_id.clone())),', 'guard-model-trip',
        submit_marker('guard-model-trip', 2, 'terminal')),
    mutation('next-window-keeps-model-count', submit, '.map(FailureGuard::new);', '.map(|config| { let mut guard = FailureGuard::new(config); if run.run_id.equals_str("r2") { let _ = guard.on_model_failure(); } guard });', 'guard-reset-next-turn',
        submit_marker('guard-reset-next-turn', 4, 'second')),
]
# Take the real configuration snapshot before refresh rather than after it.
submit_text = submit.read_text()
start = submit_text.index('        let _ = self.host.refresh().await;')
end = submit_text.index('        let handle = self', start)
block = submit_text[start:end]
config_start = block.index('        let mut failure_guard = self')
before_refresh = block[config_start:] + block[:config_start]
mutants.append(mutation('config-snapshotted-before-refresh', submit, block, before_refresh, 'guard-refresh-enables',
    submit_marker('guard-refresh-enables', 2, 'terminal')))


def build():
    oracle = json.loads((fixture / 'oracle.json').read_text())['cases']
    entries = [adapter.Mutation(Case(name, test, '#1110 positive ' + name,
                                     kind='positive', positive_tests=(test,)))
               for name, test in [('baseline-pure', pure), ('baseline-submit', replay)]]
    for control in mutants:
        binding = Case(control['name'], control['test'], control['marker'])
        entries.append(adapter.Mutation(binding, control['path'], control['before'],
                                        control['after'], control['case']))
    adapter.check(entries, oracle)
    return entries, oracle


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    try:
        entries, oracle = build()
        if args.check:
            print(f'failure-guard: {len(entries) - 2} mutations + 2 baselines; anchors OK')
            return 0
        if not args.output:
            parser.error('output is required unless --check')
        return adapter.run(entries, oracle, repo, args.output, args.selected, os.environ)
    except Exception as error:
        print(f'refused: {type(error).__name__}: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
