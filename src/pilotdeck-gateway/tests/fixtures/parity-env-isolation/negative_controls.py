#!/usr/bin/env python3
"""#2513 Python-only source mutants; run only after the coordinator commit.

Guard under test (issue #2513, from the #1354 deepseek r3 sibling#2 review):
  - scripts/parity/round.py's host-environment cleanup tuple must strip
    PILOTDECK_GATEWAY_MODE along with the other six PILOTDECK_* deployment
    variables before the env reaches the run.py child.  The offline control
    scripts/parity/controls/host_env_isolation.py captures the env round.py
    passes to R.spawn and fails with the 「PILOTDECK_GATEWAY_MODE 泄漏」
    assertion when the variable survives.

Execution environment: this fixture is run by the coordinator on a host where
the PilotDeck baseline is discoverable (PDRS_BASELINE exported or the
rev-parse fallback; the offline control only needs g03_compare to import).
mutating_sources requires every guarded source to equal HEAD, so a candidate
branch must be committed before the run; the control subprocess is bounded
(run_owned) and every cargo launch inside negative_process goes through the
shared slot gate.

Every mutated source equals HEAD before its window opens (mutating_sources);
each mutant compiles before its control runs; stale anchors abort instead of
being recorded as SURVIVED.  No shadow repository or HEAD exemption is used.
The positive-only control (no mutation) is judged PASS/FAIL.  Both tables are
pinned to the static rosters EXPECTED_MUTATIONS / EXPECTED_POSITIVES
(registered()), so a dropped or renamed row is a startup refusal, not a
smaller green run.
"""
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, tree_lock, write_source_atomic
from negative_selection import claim_dir, refuse, registered

root = Path(__file__).resolve().parents[5]
mutations = [
    # Removing the #2513 tuple entry makes the host-exported
    # PILOTDECK_GATEWAY_MODE reach the run.py child environment; the offline
    # host-env control must fail with its leak assertion.
    ('parity-round-gateway-mode-strip', root / 'scripts/parity/round.py',
     "'PILOTDECK_AGENT_LOOP_TCP_PORT',\n                'PILOTDECK_GATEWAY_MODE'):",
     "'PILOTDECK_AGENT_LOOP_TCP_PORT'):",
     'control_round_host_env_isolation'),
]

positives = [
    ('parity-round-host-env-isolation', 'control_round_host_env_isolation'),
]

# Static rosters: the denominators of this run are literals in the source, not
# len(table) — a dropped or renamed row is a startup refusal, never a smaller
# green run (#1666; negative_selection.registered).
EXPECTED_MUTATIONS = [
    'parity-round-gateway-mode-strip',
]
EXPECTED_POSITIVES = [
    'parity-round-host-env-isolation',
]


def run_control(scratch, label):
    command = [sys.executable, '-I', str(root / 'scripts/parity/controls/host_env_isolation.py')]
    result = run_owned(command, cwd=str(root), timeout=600)
    for suffix, value in (('stdout', result.stdout), ('stderr', result.stderr),
                          ('exit', f'{result.returncode}\n')):
        (scratch / f'{label}.{suffix}').write_text(value, encoding='utf-8')
    return result


def control_round_host_env_isolation(scratch):
    """The offline host-env control must pass on the guarded source."""
    result = run_control(scratch, 'host-env')
    log = result.stdout + result.stderr
    if result.returncode != 0:
        if 'PILOTDECK_GATEWAY_MODE 泄漏' not in log:
            raise RuntimeError('host_env_isolation failed without the leak marker '
                              f'(exit={result.returncode}): {log.strip()}')
        raise AssertionError('#2513 PILOTDECK_GATEWAY_MODE 泄漏进 run.py 子进程环境：'
                             f'round.py 清理元组的变异体被当场抓住 (exit={result.returncode})')


RUNNERS = {
    'control_round_host_env_isolation': control_round_host_env_isolation,
}


def scratch_dir(out, name):
    directory = out / name
    directory.mkdir(parents=True)
    return directory


def main():
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py <fresh output directory> [mutation names...]')
    try:
        registered(mutations, EXPECTED_MUTATIONS)
        registered(positives, EXPECTED_POSITIVES)
    except ValueError as error:
        refuse(f'control roster drifted: {error}')
    out = claim_dir(Path(sys.argv[1]).resolve())
    selected = sys.argv[2:]
    if len(set(selected)) != len(selected):
        refuse(f'repeated mutation name in the selection: {selected}')
    chosen = [item for item in mutations if not selected or item[0] in selected]
    if selected and set(selected) != {item[0] for item in chosen}:
        refuse('#2513 unknown mutation name')
    results = []
    # The whole run (mutation windows, restored checks, positive checks) holds
    # the same-tree lock: every control reads or executes the live tree, and a
    # concurrent runner between windows would judge moving bytes.
    with tree_lock(root):
        for name, path, before, after, control_name in chosen:
            control = RUNNERS[control_name]
            status, reason, compiled = 'INVALID', 'not run', False
            try:
                with mutating_sources(root, [path]) as originals:
                    text = originals[path].decode()
                    if text.count(before) != 1:
                        raise RuntimeError(f'#2513 stale anchor {name}: expected 1, found {text.count(before)}')
                    control(scratch_dir(out, name + '-positive'))  # must pass on the unmutated source
                    mutated = text.replace(before, after, 1)
                    compile(mutated, str(path), 'exec')
                    compiled = True
                    write_source_atomic(root, path, mutated.encode())
                    try:
                        control(scratch_dir(out, name + '-negative'))
                    except AssertionError as error:
                        message = str(error)
                        if '#2513' not in message:
                            raise RuntimeError(f'control asserted without its marker: {message}')
                        status, reason = 'KILLED', message
                    else:
                        status, reason = 'SURVIVED', 'control passed on the mutant'
                control(scratch_dir(out, name + '-restored'))  # restoration verified on the live tree
            except Exception as error:
                status, reason = 'INVALID', f'{type(error).__name__}: {error}'
            results.append(dict(name=name, status=status, reason=reason, compiled=compiled))
            print(f'{name}: {status}: {reason}', flush=True)
        for name, control_name in positives:
            control = RUNNERS[control_name]
            status, reason = 'FAIL', 'not run'
            try:
                control(scratch_dir(out, name))
                status, reason = 'PASS', 'isolation observably in force on the committed tree'
            except Exception as error:
                reason = f'{type(error).__name__}: {error}'
            results.append(dict(name=name, status=status, reason=reason, compiled=False))
            print(f'{name}: {status}: {reason}', flush=True)
    counts = {}
    for row in results:
        counts[row['status']] = counts.get(row['status'], 0) + 1
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results, 'counts': counts},
                                    ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    temporary.replace(out / 'summary.json')
    print(json.dumps(counts), flush=True)
    bad = [row for row in results
           if (row['status'] not in ('KILLED', 'PASS'))]
    return int(bool(bad) or not results)


if __name__ == '__main__':
    raise SystemExit(main())
