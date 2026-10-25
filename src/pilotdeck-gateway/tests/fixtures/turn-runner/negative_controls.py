#!/usr/bin/env python3
"""Each completion mutation must fail a runtime oracle assertion, then recover."""
import json
import os
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
mutations = [
    ('dedup', 'if inner.completed_turn_id.as_ref() == Some(turn_id) {', 'if false {'),
    ('active-required', '        self.require_recording(&inner, turn_id)?;\n        let outcome', '        let outcome'),
    ('step-first', '        self.complete_step(&mut inner, session_id, turn_id, outcome)?;', '        let _ = outcome;'),
    ('error-outcome', 'kind.equals_str("error") => "failed"', 'kind.equals_str("error") => "completed"'),
    ('abort-outcome', 'kind.equals_str("aborted") => "aborted"', 'kind.equals_str("aborted") => "completed"'),
    ('retryable', '        self.append(session_id, turn_id, draft)?;\n        inner.turns.remove(turn_id);',
     '        inner.completed_turn_id = Some(turn_id.clone());\n        self.append(session_id, turn_id, draft)?;\n        inner.turns.remove(turn_id);'),
    ('remove-active', '        inner.turns.remove(turn_id);', '        // mutant retains active turn'),
    ('failed-start-reset', '        self.append(session_id, turn_id, draft)?;\n        inner.completed_turn_id = None;',
     '        inner.completed_turn_id = None;\n        self.append(session_id, turn_id, draft)?;'),
    ('reset-start', '        inner.completed_turn_id = None;\n        inner.turns.insert', '        inner.turns.insert'),
    ('reset-restore', '        inner.compactions.clear();\n        inner.completed_turn_id = None;', '        inner.compactions.clear();'),
]
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
    for name, old, new in mutations:
        assert original.count(old) == 1, (name, original.count(old))
        source.write_text(original.replace(old, new))
        result = run(name)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        assert 'assertion `left == right` failed' in result.stdout, name
        source.write_bytes(originals[source])
    assert run('restored').returncode == 0
(out / 'summary.json').write_text(json.dumps({'mutations':len(mutations),'runtimeFailures':len(mutations),'restoredPass':True},indent=2)+'\n')
