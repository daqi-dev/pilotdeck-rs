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
shared slot gate.  The mutation window over scripts/parity/round.py holds the
shared parity lease a live round of the same tree also takes (#2513 fix-r1
review P1-3), so the two can never interleave on mutated bytes.

Every mutated source equals HEAD before its window opens (mutating_sources);
each mutant compiles before its control runs; stale anchors abort instead of
being recorded as SURVIVED.  No shadow repository or HEAD exemption is used.
The positive-only control (no mutation) is judged PASS/FAIL.  Both tables are
pinned to the static rosters EXPECTED_MUTATIONS / EXPECTED_POSITIVES
(registered()), so a dropped or renamed row is a startup refusal, not a
smaller green run.  ``--check`` re-validates the anchors, the mutant
compilations and the rosters without opening a window (CI runs it; the full
kill round needs a committed tree and stays on the host chain).

Startup refusals follow the shared contract (``specs/negative-controls``):
the roster, the selection (unknown or repeated name) and the caller target
are all refused (exit 2) *before* the evidence directory is claimed, so a
refused run never leaves a directory a host would read as its verdict
(#2513 fix-r1 review P2#2: claim_dir used to run before the selection
refusals).
"""
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, tree_lock, write_source_atomic
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered)

root = Path(__file__).resolve().parents[5]

# Discovery anchors (#2133 register, test_negative_discovery_anchors.TABLES):
# the literal keys neg-for-diff.sh derives for this table's dependencies.  The
# two imported helpers are the whole key set — both mutation targets are
# spelled as full inline paths, which the discovery grep finds in this file's
# own text without an anchor.
DISCOVERY_ANCHORS = ('fixtures/negative_process.py',
                     'fixtures/negative_selection.py')

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

# The KILLED judge below greps the control entry's own leak marker, and the
# entry must carry the #2513 tag; --check binds both so a rename there cannot
# silently downgrade every kill to INVALID.
CONTROL_ENTRY = root / 'scripts/parity/controls/host_env_isolation.py'
CONTROL_MARKERS = ('PILOTDECK_GATEWAY_MODE 泄漏', '#2513')


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


def check_errors():
    """The ``--check`` verdict: anchors, mutants, bindings and rosters intact."""
    errors = []
    for name, path, before, after, control_name in mutations:
        text = path.read_text(encoding='utf-8')
        if text.count(before) != 1:
            errors.append(f'{name}: anchor occurs {text.count(before)} times')
        if before == after:
            errors.append(f'{name}: anchor equals replacement')
        else:
            try:
                compile(text.replace(before, after, 1), str(path), 'exec')
            except SyntaxError as error:
                errors.append(f'{name}: mutant does not compile: {error}')
        if control_name not in RUNNERS:
            errors.append(f'{name}: unknown control {control_name}')
    for name, control_name in positives:
        if control_name not in RUNNERS:
            errors.append(f'{name}: unknown control {control_name}')
    entry = CONTROL_ENTRY.read_text(encoding='utf-8')
    for marker in CONTROL_MARKERS:
        if marker not in entry:
            errors.append(f'control entry lost its {marker!r} marker')
    return errors


def scratch_dir(out, name):
    directory = out / name
    directory.mkdir(parents=True)
    return directory


def main():
    # The rosters are validated before any branch, so the --check static gate
    # (CI) refuses roster drift — a renamed or dropped row with a stale
    # EXPECTED_* must not print "anchors OK" (the sdk-mcp-session-tools
    # precedent).
    try:
        registered(mutations, EXPECTED_MUTATIONS)
        registered(positives, EXPECTED_POSITIVES)
    except ValueError as error:
        refuse(f'control roster drifted: {error}')
    if sys.argv[1:2] == ['--check']:
        if len(sys.argv) > 2:
            refuse('#2513 --check takes no output directory and no mutation names')
        errors = check_errors()
        if errors:
            raise SystemExit('#2513 ' + '; '.join(errors))
        print(f'#2513 {len(mutations)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        refuse('usage: negative_controls.py --check | '
               'fresh output directory [mutation names...]')
    # A flag-shaped first argument is a misspelled mode, not an evidence
    # directory: before this guard a typo'd --check spelling was mkdir'ed and
    # the run went on to mutate the live tree (#2513 fix-r1 review P2#3).
    if sys.argv[1].startswith('-'):
        refuse(f'unknown option {sys.argv[1]!r} (modes: --check)')
    # Every caller-side refusal fires before the claim, so a refused run
    # leaves no evidence directory behind (specs/negative-controls shared
    # contract; #2513 fix-r1 review P2#2 — claim_dir used to run first).
    selected = sys.argv[2:]
    if len(set(selected)) != len(selected):
        refuse(f'repeated mutation name in the selection: {selected}')
    chosen = [item for item in mutations if not selected or item[0] in selected]
    if selected and set(selected) != {item[0] for item in chosen}:
        unknown = sorted(set(selected) - {item[0] for item in chosen})
        refuse(f'#2513 unknown mutation name: {unknown}')
    if missing_target():
        refuse('#2513 set CARGO_TARGET_DIR to the caller-owned build tree before running')
    refuse_unusable_target()
    out = claim_dir(Path(sys.argv[1]).resolve())
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
