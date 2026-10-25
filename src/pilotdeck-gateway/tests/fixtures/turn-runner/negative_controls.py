#!/usr/bin/env python3
"""Each completion mutation must fail a runtime oracle assertion, then recover."""
import json
import os
import re
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/storage/recorder.rs'
env = os.environ.copy()
env['CARGO_TARGET_DIR'] = str(root / '_build/cargo-target')
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--test', 'recorder_completion']
# Scope first, then require one indentation-independent statement anchor.
mutations = [
    ('dedup', 'complete_turn', [('if inner.completed_turn_id.as_ref() == Some(turn_id) {', 'if false {')]),
    ('active-required', 'complete_turn', [('self.require_recording(&inner, turn_id)?;', '')]),
    ('step-first', 'complete_turn', [('self.complete_step(&mut inner, session_id, turn_id, outcome)?;', 'let _ = outcome;')]),
    ('error-outcome', 'complete_turn', [('kind.equals_str("error") => "failed"', 'kind.equals_str("error") => "completed"')]),
    ('abort-outcome', 'complete_turn', [('kind.equals_str("aborted") => "aborted"', 'kind.equals_str("aborted") => "completed"')]),
    ('retryable', 'complete_turn', [('let mut draft = draft("turn_result");', 'inner.completed_turn_id = Some(turn_id.clone()); let mut draft = draft("turn_result");')]),
    ('remove-active', 'complete_turn', [('inner.turns.remove(turn_id);', '')]),
    ('failed-start-reset', 'start_turn', [('let draft = draft("turn_started");', 'inner.completed_turn_id = None; let draft = draft("turn_started");')]),
    ('reset-start', 'start_turn', [('inner.completed_turn_id = None;', '')]),
    ('reset-restore', 'restore', [('inner.completed_turn_id = None;', '')]),
]
def mutate(original, scope, replacements):
    methods = list(re.finditer(r'^    (?:pub(?:\([^)]*\))? )?(?:async )?fn (\w+)', original, re.M))
    matches = [i for i, method in enumerate(methods) if method[1] == scope]
    assert len(matches) == 1, (scope, matches)
    i = matches[0]
    start = methods[i].start()
    end = methods[i + 1].start() if i + 1 < len(methods) else len(original)
    body = original[start:end]
    for old, new in replacements:
        assert old == old.lstrip() and '\n' not in old, old
        assert body.count(old) == 1, (scope, old, body.count(old))
        body = body.replace(old, new)
    return original[:start] + body + original[end:]

def run(name):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)
    result = run_owned(command, cwd=root, env=env, timeout=900)
    (out / f'{name}.log').write_text(result.stdout + result.stderr)
    return result
baseline = os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')
with mutating_sources(root, [source], baseline=baseline) as originals:
    original = originals[source].decode()
    assert run('positive').returncode == 0
    for name, scope, replacements in mutations:
        source.write_text(mutate(original, scope, replacements))
        result = run(name)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        assert 'assertion `left == right` failed' in result.stdout, name
        source.write_bytes(originals[source])
        print(f'{name}: runtime RED; source restored', flush=True)
    assert run('restored').returncode == 0
(out / 'summary.json').write_text(json.dumps({'mutations':len(mutations),'runtimeFailures':len(mutations),'restoredPass':True},indent=2)+'\n')
