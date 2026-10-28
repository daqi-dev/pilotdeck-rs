#!/usr/bin/env python3
"""Temporary apparatus mutations; require a selftest assertion failure, then restore.

Run with no other writers in this worktree. Evidence goes in a new output
folder; original source bytes and hashes are verified after every mutation.

Every row turns off one injection (or forges one observable the selftest
asserts on) and names the case whose dedicated ``#969`` assertion must go
red, so a surviving mutant can never pass for evidence. The baseline row is
the positive control: the same case must pass with no mutation applied.

The mutated files are new in this branch: until they are committed,
``mutating_sources`` refuses them (they are not in HEAD yet), which is the
refusal working as designed. The coordinator runs this script post-commit.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

root = Path(__file__).resolve().parents[5]
source = Path(__file__).resolve().parent
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)

# The case each mutant must turn red, and the exact assertion message that has
# to appear in the run log; anything else (timeout, crash, unrelated error)
# does not count as a kill.
TESTS = {
    'baseline': ('text-passthrough', None),
    'mitm-drop-disabled': ('text-drop', 'dropped frame must be recorded as not delivered'),
    'mitm-duplicate-disabled': ('text-duplicate', 'TS client must reject the repeated sequence'),
    'mitm-early-final-disabled': ('text-early-final', 'early-final rewrite must mark the frame final/completed'),
    'mitm-double-delivery': ('text-passthrough', 'passthrough must not surface a client error'),
    'expectation-drop-weakened': ('text-drop', 'TS client must reject the sequence gap'),
    'expectation-terminal-weakened': ('text-early-final', 'TS client must accept the premature final as its terminal'),
    'driver-tool-payload-forged': ('tool-passthrough', 'read tool must return the sandbox file payload'),
    'driver-abort-never-fires': ('cancel-passthrough', 'driver exited 1'),
}

# Each independent mutant is applied to the original, never cumulatively.
# The baseline row keeps every source untouched: it proves the same runner
# path passes without a mutation.
mutations = [
    ('baseline', None, None, None),
    ('mitm-drop-disabled', 'stdio_mitm.py',
     'forward_count = 0 if "drop" in actions else 2 if "duplicate" in actions else 1',
     'forward_count = 1 if "drop" in actions else 2 if "duplicate" in actions else 1'),
    ('mitm-duplicate-disabled', 'stdio_mitm.py',
     '2 if "duplicate" in actions else 1',
     '1 if "duplicate" in actions else 1'),
    ('mitm-early-final-disabled', 'stdio_mitm.py',
     'if "early_final" in actions:',
     'if False and "early_final" in actions:'),
    ('mitm-double-delivery', 'stdio_mitm.py',
     'forward_count = 0 if "drop" in actions else 2 if "duplicate" in actions else 1',
     'forward_count = 2'),
    ('expectation-drop-weakened', 'selftest.py',
     'MISMATCH_DROP = "Sidecar event sequence mismatch: expected 1, got 2."',
     'MISMATCH_DROP = "Sidecar event sequence mismatch: expected 1, got 3."'),
    ('expectation-terminal-weakened', 'selftest.py',
     'check(result["terminal"] is not None and result["terminal"]["stopReason"] == "mitm_premature_final",',
     'check(result["terminal"] is not None and result["terminal"]["stopReason"] == "completed",'),
    ('driver-tool-payload-forged', 'driver.mts',
     "const text = readFileSync(path, 'utf8');",
     "const text = 'forged';"),
    ('driver-abort-never-fires', 'driver.mts',
     "if (scenario === 'cancel' && evidence.abortAfterEvents === null && event.type === 'model_event') {",
     "if (false && scenario === 'cancel' && evidence.abortAfterEvents === null && event.type === 'model_event') {"),
]
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
env = os.environ.copy()
assert env.get('PDRS_BASELINE'), 'PDRS_BASELINE must point at the read-only TS baseline root'
summary = []
# Run every mutation before concluding anything; the verdict comes last.
with mutating_sources(root, sorted({source / filename for _, filename, _, _ in mutations if filename})) as snapshots:
    for name, filename, old, new in mutations:
        path = source / filename
        original = snapshots[path] if filename else None
        before = hashlib.sha256(original).hexdigest() if original else None
        content = original.decode() if original else None
        if old:
            assert content.count(old) == 1, (name, content.count(old))
            changed = content.replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=filename, tofile=filename)))
        try:
            case, bound = TESTS[name]
            if old:
                path.write_text(changed)
            result = run_owned([sys.executable, str(source / 'selftest.py'),
                                str(out / f'{name}-evidence'), '--cases', case],
                               cwd=root, env=env, timeout=300)
            log = result.stdout + result.stderr
            (out / f'{name}.log').write_text(log)
            if name == 'baseline':
                killed = result.returncode == 0
            else:
                killed = result.returncode == 1 and bound in log
            summary.append(dict(name=name, killed=killed, returncode=result.returncode, bound=bound))
            print(f'{name}: {"KILLED" if killed else "NOT KILLED"} rc={result.returncode}', flush=True)
        finally:
            if original is not None:
                path.write_bytes(original)
                assert hashlib.sha256(path.read_bytes()).hexdigest() == before
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte', flush=True)
survivors = [row['name'] for row in summary if not row['killed']]
assert not survivors, f'mutations that did not reproduce their bound failure: {survivors}'
