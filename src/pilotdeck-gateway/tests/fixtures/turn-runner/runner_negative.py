#!/usr/bin/env python3
"""Runtime mutations of the durable runner; always restore the owned tree."""
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
source = root / 'src/pilotdeck-gateway/src/runtime/turn/runner.rs'
env = os.environ.copy()
env['CARGO_TARGET_DIR'] = str(root / '_build/cargo-target')
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--lib', 'runtime::turn::runner::tests']
mutations = [
    ('start-durable', 'start', [('self.io(|r, s, t| r.start_turn(s, t)).await?;', '')]),
    ('accepted-before-write', 'accept', [('.record_accepted_input(s, t, written, input.metadata)?', '.record_accepted_input(s, t, written, input.metadata).unwrap_or_default()')]),
    ('accepted-boundary', 'accept', [('JsValue::Array(accepted)', 'JsValue::Array(self.messages.clone())')]),
    ('synthetic-durable', 'accept', [('.chain(input.synthetic)', '.chain(std::iter::empty())')]),
    ('history-loop-only', 'accept', [('self.messages.extend(durable);', 'self.messages = durable;')]),
    ('status-required', 'loop_failed', [('if !self.failure_status {', 'if false {')]),
    ('status-dedup', 'loop_failed', [('if !self.failure_status {', 'if true {')]),
    ('complete-durable', 'complete', [('self.io(move |r, s, t| r.complete_turn(s, t, written))', 'async { let _ = written; Ok::<(), RecorderError>(()) }')]),
    ('catch-complete', 'fail_inner', [('self.io(move |r, s, t| r.complete_turn(s, t, written))', 'async { let _ = written; Ok::<(), RecorderError>(()) }')]),
    ('terminal-mark', 'complete', [('self.phase = Phase::Completed;', 'self.phase = Phase::Accepted;')]),
    ('catch-terminal-mark', 'fail_inner', [('self.phase = Phase::Completed;', 'self.phase = Phase::Accepted;')]),
    ('worker-lease', 'io', [('drop(lease);', ''), ('let result = write(&recorder, &session, &turn);', 'drop(lease); let result = write(&recorder, &session, &turn);')]),
    ('aborted-reason', 'fail_inner', [('error.get("code") == Some(&"agent_aborted".into())', 'false')]),
    ('nullish-hint', 'status', [('.filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))', '')]),
    ('visible-error-details', 'accept', [('"SessionEventValidationError".into()', '"Error".into()')]),
    ('phase-guard', 'require', [('if self.phase == phase {', 'if true {')]),
    ('catch-phase-guard', 'fail_inner', [('!matches!(self.phase, Phase::Started | Phase::Accepted)', 'false')]),
    ('identity-turn-guard', 'complete', [('|| result.get("turnId") != Some(&JsValue::String(self.turn.clone()))', '|| false')]),
    ('identity-guard', 'complete', [('if result.get("sessionId")', 'if false && result.get("sessionId")')]),
    ('best-effort-status', 'status', [('.await;', '.await.expect("mutant status failure");')]),
]

if sys.argv[2:]:
    selected = set(sys.argv[2:])
    assert selected <= {row[0] for row in mutations}, 'unknown mutation'
    mutations = [row for row in mutations if row[0] in selected]
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
with mutating_sources(root, [source], baseline=os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')) as originals:
    original = originals[source].decode()
    assert run('positive').returncode == 0
    for name, scope, replacements in mutations:
        source.write_text(mutate(original, scope, replacements))
        result = run(name)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        assert 'panicked at' in result.stdout + result.stderr, name
        source.write_bytes(originals[source])
        print(f'{name}: runtime RED; source restored', flush=True)
    assert run('restored').returncode == 0
(out / 'summary.json').write_text(json.dumps({'mutations':len(mutations),'runtimeFailures':len(mutations),'restoredPass':True},indent=2)+'\n')
