#!/usr/bin/env python3
"""#1325 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.

Rows bind one mutation to one lib test (run with `--exact`); the kill evidence
is that test's own panic payload carrying its `#1325` message. Six rows bind
`runtime::cron::manager_tests::six_methods_replay_the_ts_gold`, the seventh
binds `...::global_list_caps_the_merged_run_page_at_the_nullish_default`. The
replay needs no Node: it reads the committed oracle bytes, so every row runs on
both platforms. The gold rows pin: the unconfigured gate's error code,
requireProjectKey's missing-key message, updateTask's revision increment, the
unknown-unit delay fold (NaN * → RangeError, not the "positive finite" error),
the running-task update reason, and the global list merge's
`slice(0, input.limit ?? 50)` — including the limit-0 empty page that the
store's own `slice(-0)` quirk would fill again. The seventh row pins the
nullish default itself: the oracle's `list-global-merge` only uses the explicit
limits 2 and 0, so no gold cell can tell 50 apart from "every run".
"""
from pathlib import Path
import json
import os
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
manager = root / 'src/pilotdeck-gateway/src/runtime/cron/manager.rs'
projection = root / 'src/pilotdeck-gateway/src/runtime/cron/projection.rs'
tests = root / 'src/pilotdeck-gateway/src/runtime/cron/manager_tests.rs'
oracle = root / 'src/pilotdeck-gateway/tests/fixtures/native-cron/oracle.json'
REPLAY = 'runtime::cron::manager_tests::six_methods_replay_the_ts_gold'
NULLISH_REPLAY = (
    'runtime::cron::manager_tests::'
    'global_list_caps_the_merged_run_page_at_the_nullish_default'
)
mutations = [
    # The unconfigured gate reports the wrong DialogGatewayError code, so the
    # createTask unconfigured cell's recorded `code` no longer matches.
    ('gate-code-swapped', projection,
     '    controller.ok_or_else(|| {\n'
     '        CronError::gateway(\n'
     '            "CAPABILITY_UNAVAILABLE",\n'
     '            "Cron runtime is not configured on this gateway.",\n'
     '        )\n'
     '    })',
     '    controller.ok_or_else(|| {\n'
     '        CronError::gateway(\n'
     '            "RUNTIME_UNAVAILABLE",\n'
     '            "Cron runtime is not configured on this gateway.",\n'
     '        )\n'
     '    })',
     REPLAY,
     '#1325 unconfigured createTask error differs from the TS gold'),
    # A missing projectKey reports a different message than
    # requireProjectKey's, so the first create-errors cell diverges.
    ('create-project-key-message', manager,
     '        JsValue::Undefined | JsValue::Null => Err(CronError::error(JsString::from(\n'
     '            "Cron task creation requires a projectKey.",\n'
     '        ))),',
     '        JsValue::Undefined | JsValue::Null => Err(CronError::error(JsString::from(\n'
     '            "Cron task creation needs a projectKey.",\n'
     '        ))),',
     REPLAY,
     '#1325 gold create-errors step 0 (createTask) error differs from the TS gold'),
    # The successful update stops bumping revision, so the lifecycle cell's
    # recorded `revision: 1` update result ages to 0.
    ('update-revision-not-bumped', manager,
     '                    ("revision", JsValue::Number(revision_of(current) + 1.0)),',
     '                    ("revision", JsValue::Number(revision_of(current))),',
     REPLAY,
     '#1325 gold create-lifecycle step 2 (updateTask) differs from the TS gold'),
    # An unknown delay unit folds as "minute" instead of NaN, so the
    # `unit: "minutes"` cell creates a task where the TS gold records the
    # `RangeError: Invalid time value` (the fold the gold caught live).
    ('delay-unknown-unit-folds', manager,
     '        Some(unit) if unit.equals_str("day") => Some(DelayUnit::Day),\n'
     '        _ => None,\n'
     '    };',
     '        Some(unit) if unit.equals_str("day") => Some(DelayUnit::Day),\n'
     '        _ => Some(DelayUnit::Minute),\n'
     '    };',
     REPLAY,
     '#1325 gold create-delay-fold step 1 (createTask): expected an error'),
    # A running task's update failure reports the wrong reason.
    ('update-running-reason-swapped', manager,
     '                reason.set(Some("running"));',
     '                reason.set(Some("stopped"));',
     REPLAY,
     '#1325 gold update-cases step 5 (updateTask) differs from the TS gold'),
    # The global list merge drops its `slice(0, input.limit ?? 50)`, so the
    # limit-2 page keeps the per-project pools' four runs (and limit 0 would
    # refill from the store's `slice(-0)` quirk).
    ('list-global-limit-ignored', manager,
     '            runs.truncate(end);',
     '            let _ = end;',
     REPLAY,
     '#1325 gold list-global-merge step 3 (listTasks) differs from the TS gold'),
    # The nullish arm falls back to the whole merged page, so
    # `{includeHistory: true}` with no limit returns 60 runs instead of 50. No
    # gold cell can see this (the oracle's list-global-merge only uses the
    # explicit limits 2 and 0), so this row binds the nullish-default test.
    ('list-global-nullish-default-ignored', manager,
     '                JsValue::Undefined | JsValue::Null => js_slice_end(runs.len(), 50.0),',
     '                JsValue::Undefined | JsValue::Null => runs.len(),',
     NULLISH_REPLAY,
     '#1325 global list nullish limit default is not capped at 50'),
]


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check_anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        print('\n'.join(errors) if errors else f'#1325 {len(mutations)} unique anchors checked')
        return int(bool(errors))
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    if not args:
        raise SystemExit('provide NEW_OUT and optional mutation names, or --check')
    names = args[1:]
    selected = [row for row in mutations if not names or row[0] in names]
    if not selected or (names and len(selected) != len(names)):
        raise SystemExit('unknown or duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    summary = []
    guarded = list(dict.fromkeys([manager, projection, tests, oracle, Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py']))

    def save(name, text):
        with (out / name).open('x') as stream:
            stream.write(text)

    def run(label, test, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        save(label + '.slots.log', slot.stdout + slot.stderr)
        # negative_suite needs libtest's named result lines, including positives.
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib']
        if compile:
            command.append('--no-run')
        else:
            command += [test, '--', '--exact', '--show-output']
        save(label + '.command.json', json.dumps(command) + '\n')
        try:
            result = run_owned(command, cwd=root / 'src', env=env, timeout=300)
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes)
                              else value or '' for value in (error.stdout, error.stderr))
            save(label + '.log', raw + f'\n{type(error).__name__}: {error}\n')
            raise
        save(label + '.log', result.stdout + result.stderr)
        return result

    for row in selected:
        name, path, old, new, test, assertion = row
        item = dict(name=name, test=test, assertion=assertion,
                    status='INVALID', reason='not executed', compiled=False)
        try:
            # No caller-supplied baseline, dirty-file snapshot, or alternate git.
            with mutating_sources(root, guarded) as originals:
                original = originals[path].decode()
                check_anchor(row, original)
                if not compile_only and not passed(run(name + '-positive', test), test):
                    raise RuntimeError('bound positive control did not pass')
                try:
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', test, compile=True).returncode != 0:
                        raise RuntimeError('mutant failed compilation; not a runtime kill')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='cargo test --no-run passed')
                    else:
                        case = Case(name, path, old, new, test, assertion)
                        status, reason = classify(run(name + '-negative', test), case)
                        item.update(status=status, reason=reason)
                finally:
                    # Bytes come only from the HEAD-validated guard; it also
                    # restores and verifies every guarded source on exit.
                    path.write_bytes(originals[path])
                if not passed(run(name + '-restored', test), test):
                    raise RuntimeError('bound restored control did not pass')
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        summary.append(item)
        print(f"{name}: {item['status']}: {item['reason']}", flush=True)
    save('summary.json', json.dumps(summary, indent=2) + '\n')
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED', 'SKIPPED')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not summary
               or not counts[success]
               or any(row['status'] not in (success, 'SKIPPED') for row in summary))


if __name__ == '__main__':
    raise SystemExit(main())
