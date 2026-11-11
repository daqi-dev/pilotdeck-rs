#!/usr/bin/env python3
"""#1087 negative controls for the live-history slot contract.

Every control mutates one committed source (or oracle), runs the single bound
libtest with the caller's CARGO_TARGET_DIR, and restores the original bytes
under ``mutating_sources``. The runner never stops at the first failure: each
row is recorded KILLED / SURVIVED / INVALID with a reason, the summary.json is
published once after every guarded byte was restored and verified, and the
shared ``negative_selection.verdict_exit`` contract keeps exit 0 only for a
nonempty all-KILLED run.

The kill judge is the shared #813 criterion (``negative_panic``): the bound
test's own ``FAILED`` status line plus the marker inside that test's own panic
payload. The run filter is a bare suffix (no ``--exact``), so the full
module-qualified name is resolved from the run's own status lines and must be
unique — an ambiguous or absent match is a fail-closed INVALID row, never a
guess. A marker printed to stdout or another test's panic is not evidence
(#1087, the cargo-side form of #818).
"""
import json
import os
import re
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_selection import verdict_exit
from negative_suite import passed

root = Path(__file__).resolve().parents[5]
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
baseline = os.environ.get('NEGATIVE_BASELINE') or 'HEAD'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
fixtures = Path(__file__).resolve().parents[1]
controls = [
    ('unreserved-history', live, 'state.active = Some(cancellation.clone());', 'let _ = cancellation.clone();', 'live_history_slot_matrix', 'concurrent submit must reject before reading history'),
    ('empty-history', live, 'input.history = (self.history)();', 'input.history = (self.history)(); input.history.clear();', 'live_history_slot_matrix', 'new owner reads fresh history'),
    ('disposed-read', live, 'return Err(RuntimeError::new("Agent session disposed."));', 'let _ = (self.history)(); return Err(RuntimeError::new("Agent session disposed."));', 'live_history_slot_matrix', 'history slot matrix must match real TS'),
    ('rejected-read', live, 'if state.active.is_some() {', 'if state.active.is_some() { let _ = (self.history)();', 'live_history_slot_matrix', 'history slot matrix must match real TS'),
    ('gates-extra-row', fixtures/'turn-gates/oracle.json', '"matrix": [', '"matrix": [{"name":"unconsumed"},', 'gates_oracle_fully_consumed', 'oracle rows must be fully consumed'),
    ('gates-duplicate-row', fixtures/'turn-gates/oracle.json', '"matrix": [', '"matrix": [{"name":"start-failure"},', 'gates_oracle_fully_consumed', 'oracle rows must be fully consumed'),
    ('recording-extra-row', fixtures/'live-recording/oracle.json', '"matrix": [', '"matrix": [{"name":"unconsumed"},', 'live_recording_oracle_fully_consumed', 'oracle rows must be fully consumed'),
    ('recording-duplicate-row', fixtures/'live-recording/oracle.json', '"matrix": [', '"matrix": [{"name":"start-cancel"},', 'live_recording_oracle_fully_consumed', 'oracle rows must be fully consumed'),
]
selected = set(sys.argv[2:])
if not selected <= {c[0] for c in controls}:
    unknown = sorted(selected - {c[0] for c in controls})
    raise SystemExit(f'unknown negative-control names: {unknown}')
chosen = [c for c in controls if not selected or c[0] in selected]

def run(name, test):
    slots = wait_for_slot(root/'scripts/cargo-slots.sh', timeout=900)
    (out/f'{name}.slots.log').write_text(slots.stdout+slots.stderr)
    result = run_owned(['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib',test,'--','--test-threads=1'], cwd=root, env=env, timeout=900)
    (out/f'{name}.log').write_text(result.stdout+result.stderr)
    return result


def full_names(log, test):
    """Full libtest names whose last segment is the bare filter ``test``.

    The run filters by substring (no ``--exact``), so the status lines carry
    the module-qualified names; the suffix resolves to exactly one of them or
    the row is fail-closed INVALID (#1087).
    """
    names = set()
    for line in log.split('\n'):
        match = re.match(r'^test (\S+) \.\.\. (?:ok|FAILED|ignored)$', line)
        if match and (match.group(1) == test
                      or match.group(1).endswith('::' + test)):
            names.add(match.group(1))
    return sorted(names)


def classify(result, test, anchor):
    """KILLED only on the bound test's own FAILED line plus its panic payload."""
    log = result.stdout + result.stderr
    names = full_names(log, test)
    if len(names) != 1:
        return 'INVALID', (f'cannot resolve the bound test name: '
                            f'{len(names)} matches for {test}')
    if passed(result, names[0]):
        return 'SURVIVED', 'bound test still passed'
    if result.returncode != 0 and named_test_killed(log, names[0], anchor):
        return 'KILLED', 'bound test failed at its assertion'
    return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing anchor)'


results = []
with mutating_sources(root, list(dict.fromkeys(c[1] for c in chosen)), baseline=baseline) as originals:
    for name, path, old, new, test, anchor in chosen:
        status, reason = 'INVALID', 'not executed'
        try:
            positive = run(name+'-positive', test)
            positive_names = full_names(positive.stdout+positive.stderr, test)
            if (positive.returncode != 0 or len(positive_names) != 1
                    or not passed(positive, positive_names[0])):
                status, reason = 'INVALID', 'positive control not green before mutation'
            else:
                source = originals[path].decode()
                if source.count(old) != 1:
                    status, reason = ('INVALID',
                                      f'stale anchor: {source.count(old)} matches')
                else:
                    try:
                        path.write_text(source.replace(old, new, 1))
                        result = run(name, test)
                        status, reason = classify(result, test, anchor)
                    finally:
                        path.write_bytes(originals[path])
                    restored = run(name+'-restored', test)
                    if restored.returncode != 0:
                        status, reason = 'INVALID', reason + '; restored run not green'
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        results.append({'mutation': name, 'test': test, 'assertion': anchor,
                        'status': status, 'reason': reason})
        print(f'{name}: {status}: {reason}', flush=True)
counts = {state: sum(row['status'] == state for row in results)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print(json.dumps(counts), flush=True)
# #1087 fix-r1: one atomic rename, never a truncating write_text. The publish
# already happens only after every guarded byte was restored and verified; the
# write channel itself must also never leave an empty or half-written
# summary.json behind (#1903 rule).
(out/'summary.json.tmp').write_text(json.dumps(results, indent=2)+'\n')
(out/'summary.json.tmp').replace(out/'summary.json')
raise SystemExit(verdict_exit(results, key='status'))
