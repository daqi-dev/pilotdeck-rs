#!/usr/bin/env python3
"""Runtime mutations of the durable runner; always restore the owned tree."""
import json
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/runtime/turn/runner.rs'
env = os.environ.copy()
env['CARGO_TARGET_DIR'] = str(root / '_build/cargo-target')
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--lib', 'runtime::turn::runner::tests']
mutations = [
    ('start-durable', '        self.io(|r, s, t| r.start_turn(s, t)).await?;', '', 1),
    ('accepted-before-write', '.await\n        {\n            let error = object([', '.await.or(Ok::<(), RecorderError>(()))\n        {\n            let error = object([', 1),
    ('accepted-boundary', 'JsValue::Array(accepted)', 'JsValue::Array(self.messages.clone())', 1),
    ('synthetic-durable', '.chain(input.synthetic)', '.chain(std::iter::empty())', 1),
    ('history-loop-only', 'self.messages.extend(durable);', 'self.messages = durable;', 1),
    ('status-required', 'if !self.failure_status {', 'if false {', 1),
    ('status-dedup', 'if !self.failure_status {', 'if true {', 1),
    ('complete-durable', 'self.io(move |r, s, t| r.complete_turn(s, t, written)).await {', 'Ok::<(), RecorderError>(()) {', 1),
    ('catch-complete', 'self.io(move |r, s, t| r.complete_turn(s, t, written))\n            .await?;', 'let _ = written;', 1),
    ('terminal-mark', 'self.phase = Phase::Completed;', 'self.phase = Phase::Accepted;', 2),
    ('worker-lease', 'let result = write(&recorder, &session, &turn);\n            drop(lease);', 'drop(lease);\n            let result = write(&recorder, &session, &turn);', 1),
    ('aborted-reason', 'error.get("code") == Some(&"agent_aborted".into())', 'false', 1),
    ('nullish-hint', '.filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))', '', 1),
    ('visible-error-details', '"SessionEventValidationError".into()', '"Error".into()', 1),
    ('phase-guard', 'if self.phase == phase {', 'if true {', 1),
    ('catch-phase-guard', '!matches!(self.phase, Phase::Started | Phase::Accepted)', 'false', 1),
    ('identity-turn-guard', '|| result.get("turnId") != Some(&JsValue::String(self.turn.clone()))', '|| false', 1),
    ('identity-guard', 'if result.get("sessionId")', 'if false && result.get("sessionId")', 1),
    ('best-effort-status', '.await;\n        self.event("agent_status", fields)', '.await.expect("mutant status failure");\n        self.event("agent_status", fields)', 1),
]
if sys.argv[2:]:
    selected = set(sys.argv[2:])
    assert selected <= {row[0] for row in mutations}, 'unknown mutation'
    mutations = [row for row in mutations if row[0] in selected]
def run(name):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)
    result = run_owned(command, cwd=root, env=env, timeout=900)
    (out / f'{name}.log').write_text(result.stdout + result.stderr)
    return result
with mutating_sources(root, [source], baseline=os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')) as originals:
    original = originals[source].decode()
    assert run('positive').returncode == 0
    for name, old, new, count in mutations:
        assert original.count(old) == count, (name, original.count(old))
        source.write_text(original.replace(old, new))
        result = run(name)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        assert 'panicked at' in result.stdout + result.stderr, name
        source.write_bytes(originals[source])
        print(f'{name}: runtime RED; source restored', flush=True)
    assert run('restored').returncode == 0
(out / 'summary.json').write_text(json.dumps({'mutations':len(mutations),'runtimeFailures':len(mutations),'restoredPass':True},indent=2)+'\n')
