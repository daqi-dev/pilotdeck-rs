#!/usr/bin/env python3
"""#978 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.
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
source = root / 'src/pilotdeck-gateway/src/runtime/background.rs'
output = root / 'src/pilotdeck-gateway/src/runtime/background/output.rs'
tests = root / 'src/pilotdeck-gateway/tests/background_persist.rs'
mutations = [
    ('completion-never-published', source,
     '        entry.done.send_replace(true);\n    }\n}\n\n/// `exitCode` as a wire value:',
     '        entry.done.send_replace(false);\n    }\n}\n\n/// `exitCode` as a wire value:',
     'test_snapshot_persistence_and_recovery', '#978 snapshot completion deadline'),
    ('spill-wait-outcome', source,
     '            () = entry.wait_done() => "completed",',
     '            () = entry.wait_done() => "unknown",',
     'test_output_spill_write_and_read', '#978 spill wait outcome'),
    ('snapshot-wait-outcome', source,
     '            () = entry.wait_done() => "completed",',
     '            () = entry.wait_done() => "unknown",',
     'test_snapshot_persistence_and_recovery', '#978 snapshot wait outcome'),
    ('spill-never-written', output,
     '            spill.append(chunk.to_vec());',
     '            let _ = spill; // #978 negative: suppress accepted disk writes',
     'test_output_spill_write_and_read', '#978 spill write deadline'),
    ('spill-extra-byte', output,
     '                        .write_all(&chunk)',
     '                        .write_all(&[chunk.as_slice(), b"!"].concat())',
     'test_output_spill_write_and_read', '#978 exact spill file length'),
    ('producer-failed', source,
     '                    } else if code == Some(0) {\n                        TaskStatus::Completed',
     '                    } else if false && code == Some(0) {\n                        TaskStatus::Completed',
     'test_output_spill_write_and_read', '#978 spill producer completed successfully'),
    ('snapshot-producer-failed', source,
     '                    } else if code == Some(0) {\n                        TaskStatus::Completed',
     '                    } else if false && code == Some(0) {\n                        TaskStatus::Completed',
     'test_snapshot_persistence_and_recovery', '#978 snapshot producer completed successfully'),
    ('start-pid-missing', source,
     '                task.pid = pid;',
     '                task.pid = None;',
     'test_unsettled_task_restored_as_cancelled', '#978 start acknowledged the child PID'),
    ('spill-count-incomplete', source,
     '            state.task.output_bytes = state.output.total;\n            let task = &state.task;',
     '            state.task.output_bytes = 0;\n            let task = &state.task;',
     'test_output_spill_write_and_read', '#978 spill producer delivered all bytes'),
    ('spill-read-skipped', output,
     '        let path = &self.spill.as_ref()?.path;',
     '        if offset == 0 { return None; }\n        let path = &self.spill.as_ref()?.path;',
     'test_output_spill_write_and_read', '#978 spill prefix is not truncated'),
    ('completed-status-lost', source,
     '            let mut task = task;\n\n            if was_unsettled {',
     '            let mut task = task;\n'
     '            if task.status == TaskStatus::Completed { task.status = TaskStatus::Cancelled; }\n\n'
     '            if was_unsettled {',
     'test_snapshot_persistence_and_recovery', '#978 restored completed status'),
    ('cancelled-pid-lost', source,
     '            let mut task = task;\n\n            if was_unsettled {',
     '            let mut task = task;\n            task.pid = None;\n\n            if was_unsettled {',
     'test_unsettled_task_restored_as_cancelled', '#978 restored cancelled PID'),
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
        print('\n'.join(errors) if errors else f'#978 {len(mutations)} unique anchors checked')
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
    guarded = list(dict.fromkeys([source, output, tests, Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py']))

    def save(name, text):
        with (out / name).open('x') as stream:
            stream.write(text)

    def run(label, test, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        save(label + '.slots.log', slot.stdout + slot.stderr)
        # negative_suite needs libtest's named result lines, including positives.
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway',
                   '--test', 'background_persist']
        command += ['--no-run'] if compile else [test, '--', '--exact', '--show-output']
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
        item = dict(mutation=name, test=test, assertion=assertion,
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
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    return int(any(row['status'] != ('COMPILED' if compile_only else 'KILLED') for row in summary))


if __name__ == '__main__':
    raise SystemExit(main())
