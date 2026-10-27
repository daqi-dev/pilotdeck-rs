#!/usr/bin/env python3
"""Session-composition propagation controls; process every selected mutant first.

fix-r1 (#885): a deleted-cwd ToolResultBudget failure must cross
ContextStorageResources -> TurnHostFactory -> LiveSessionAdapter with the
original uv_cwd message, not the `Host execution failed:` host wrapper.
--check validates anchors without touching product sources. Execution requires
the caller's CARGO_TARGET_DIR and the default HEAD guard.
"""
import difflib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import target_hits

ROOT = Path(__file__).resolve().parents[5]
FACTORY = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory.rs'
LIVE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
PREFIX = 'runtime::turn::host_factory::tests::'
WORKER = 'session_cwd_environment_worker'  # host_factory_tests.rs's bounded worker
TEST = 'session_composition_uv_cwd_error_reaches_stream_unwrapped'
SESSION_KIND = '.map_err(|error| HostError::new(HostErrorKind::Session, error.message))?'
SESSION_BRANCH = '''Err(error) if error.kind == HostErrorKind::Session => {
                Err(RuntimeError::new(error.message))
            }'''
MUTATIONS = [
    ('session-kind', FACTORY, SESSION_KIND,
     '.map_err(|error| HostError::new(HostErrorKind::Protocol, error.message))?',
     [(TEST, '#885 composition failure must preserve the TS uv_cwd message on the first poll')]),
    ('live-prefix', LIVE, SESSION_BRANCH,
     '''Err(error) if error.kind == HostErrorKind::Session => {
                Err(map_host_error(error))
            }''',
     [(TEST, '#885 composition failure must preserve the TS uv_cwd message on the first poll')]),
]


def write_new(path, value):
    with path.open('x') as stream:
        stream.write(value)


def check_anchor(mutation):
    name, source, old, new, targets = mutation
    count = source.read_text().count(old)
    if count != 1 or old == new or not targets or any(not t or not m for t, m in targets):
        raise RuntimeError(f'{name}: invalid anchor/targets (count={count})')


def run(out, label, env):
    slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600, interval=15)
    write_new(out / f'{label}.slots.log', slots.stdout + slots.stderr)
    command = ['cargo', 'test', '--offline', '-q', '-p', 'pilotdeck-gateway', '--lib', PREFIX + TEST,
               '--', '--nocapture', '--format=pretty', '--test-threads=1']
    write_new(out / f'{label}.command.json', json.dumps({
        'command': command, 'cwd': str(ROOT / 'src'),
        'CARGO_TARGET_DIR': env['CARGO_TARGET_DIR']}, indent=2) + '\n')
    result = run_owned(command, cwd=ROOT / 'src', env=env, timeout=600)
    log = result.stdout + result.stderr
    write_new(out / f'{label}.log', log)
    return result, log


def passed(result, log):
    return (result.returncode == 0 and f'1 passed; 0 failed; 0 ignored;' in log
            and log.count('PDRS_VERIFIED_CHILD ') == 1)


def classify(result, log, targets):
    if passed(result, log):
        return 'SURVIVED', 'propagation test still passed'
    # The shared judgment binds the target to its own evidence: the outer
    # libtest failures list names it and its own panic block carries its
    # dedicated message. The parent's message embeds the worker's whole
    # captured output (test_process.rs:137), so the worker's own panic line
    # must not terminate the parent block (#889).
    hit, unrelated = target_hits(result, PREFIX, targets, WORKER)
    if (result.returncode == 101 and 'test result: FAILED.' in log
            and 'error: could not compile' not in log
            and not unrelated and len(hit) == len(targets)):
        return 'KILLED', 'named runtime assertions failed'
    return 'INVALID', 'missing named runtime failure; compile/setup/timeout is not a kill'


def main():
    selected = sys.argv[2:] if sys.argv[1:] != ['--check'] else []
    mutations = [m for m in MUTATIONS if not selected or m[0] in selected]
    if not mutations or (selected and len(mutations) != len(selected)):
        raise SystemExit('unknown/duplicate mutation')
    if sys.argv[1:] == ['--check']:
        errors = []
        for mutation in mutations:
            try:
                check_anchor(mutation)
            except Exception as error:
                errors.append(str(error))
        if errors:
            raise SystemExit('\n'.join(errors))
        print(f'{len(mutations)} unique anchors checked')
        return
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    if len(sys.argv) < 2:
        raise SystemExit('usage: session_negative.py <new-output-directory> [mutation ...]')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    # The default is HEAD; NEGATIVE_BASELINE only lets a pre-submit sample
    # point at a temporary commit-tree object without moving HEAD.
    baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
    paths = [FACTORY, LIVE, Path(__file__).resolve()]
    results = []
    for mutation in mutations:
        name, source, old, new, targets = mutation
        status, reason, restored = 'INVALID', 'not run', False
        try:
            check_anchor(mutation)
            with mutating_sources(ROOT, paths, baseline=baseline) as originals:
                original = originals[source].decode()
                if original.count(old) != 1:
                    raise RuntimeError('stale anchor after locking')
                positive, log = run(out, f'{name}-positive', env)
                if not passed(positive, log):
                    raise RuntimeError('positive propagation test did not pass with its receipt')
                changed = original.replace(old, new, 1)
                write_new(out / f'{name}.patch', ''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True),
                    fromfile=str(source.relative_to(ROOT)), tofile=str(source.relative_to(ROOT)))))
                source.write_text(changed)
                negative, log = run(out, f'{name}-negative', env)
                status, reason = classify(negative, log, targets)
            with mutating_sources(ROOT, paths, baseline=baseline):
                result, log = run(out, f'{name}-restored', env)
                restored = passed(result, log)
            if not restored:
                status, reason = 'INVALID', 'restored propagation test did not pass'
        except Exception as error:
            status, reason = 'INVALID', str(error)
        row = dict(name=name, status=status, reason=reason, restoredPass=restored, targets=targets)
        results.append(row)
        write_new(out / f'{name}.result.json', json.dumps(row, indent=2) + '\n')
        print(f'{name}: {status}: {reason}', flush=True)
    write_new(out / 'summary.json', json.dumps(results, indent=2) + '\n')
    counts = {status: sum(r['status'] == status for r in results)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps(counts), flush=True)
    if counts['SURVIVED'] or counts['INVALID']:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
