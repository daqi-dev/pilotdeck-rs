#!/usr/bin/env python3
"""Serial behavior mutations; restores exact source bytes even on failure."""
import hashlib
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[5]
evidence = Path(sys.argv[1]).resolve()
evidence.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
original = source.read_text()
prefix = 'runtime::session::owner::tests::hooks::'
active, idle, other = 'active_user_turn_matches_ts', 'idle_hook_matches_ts_with_turn_interleaving', 'non_idle_hooks_are_silent'
mutations = [
    ('no-active', '&& self.0.turns.admission.has_active_turn(key)', '&& (self.0.turns.admission.has_active_turn(key) && false)', active),
    ('no-admission', '&& self.0.turns.admission.has_active_turn(key)', '&& (self.0.turns.admission.has_active_turn(key) || true)', active),
    ('always-on', '!key.units().starts_with(always_on.units())', '(key.units().starts_with(always_on.units()) || true)', active),
    ('cron', '!key.units().starts_with(cron.units())', '(key.units().starts_with(cron.units()) || true)', active),
    ('project', '&& record.context.project_key.as_ref() == Some(project)', '&& (record.context.project_key.as_ref() == Some(project) || true)', active),
    ('no-hook', 'if reason == EvictionReason::Idle', 'if false && reason == EvictionReason::Idle', idle),
    ('all-reasons', 'if reason == EvictionReason::Idle', 'if (reason == EvictionReason::Idle || true)', other),
    ('dispose-error', 'if reason == EvictionReason::Idle', 'if reason == EvictionReason::Idle && result.is_ok()', idle),
    ('early-snapshot', '        self.emit(key, reason);\n        if reason', '        if reason', idle),
    ('timestamp', 'last_used_at: record.last_used,', 'last_used_at: (self.0.options.now_ms)(),', idle),
    ('context', 'context: record.context,', 'context: SessionContext { allowed_tools: None, ..record.context },', idle),
    ('array-length', 'messages.len() as f64', '(messages.len() + 1) as f64', idle),
    ('utf16-length', 'text.units().len() as f64', 'text.to_utf8().unwrap().len() as f64', idle),
    ('inherited-messages', 'snapshot.as_object()?.get_property("messages")?', 'snapshot.get("messages")?', idle),
    ('inherited-length', '.get_property("length")', '.get("length")', idle),
    ('object-length', '.filter(|v| !v.is_undefined())\n                        .cloned()', '.filter(|v| !v.is_undefined()).map(|_| JsValue::Number(0.0))', idle),
    ('undefined', '.filter(|v| !v.is_undefined())', '', idle),
    ('missing-count', '            .flatten();', '            .flatten().or(Some(JsValue::Number(0.0)));', idle),
    ('snapshot-panic', '            .ok()\n            .flatten();', '            .expect("snapshot failed");', idle),
    ('disabled-eager', '        self.emit(key, reason);', '        self.emit(key, reason);\n        if self.0.options.on_session_idle_evict.is_none() { record.handle.snapshot(); }', idle),
    ('early-callback', '.dispose(format!("session_{label}").into())\n            .await;', '.dispose(format!("session_{label}").into());', idle),
]
env = dict(os.environ, CARGO_TARGET_DIR=str(root / '_build/cargo-target'))
def run(label, test, red=False):
    with (evidence / (label + '.log')).open('x') as log:
        subprocess.run(['bash', 'scripts/cargo-slots.sh'], cwd=root, stdout=log, stderr=subprocess.STDOUT, check=True)
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib', prefix + test, '--', '--nocapture']
        result = subprocess.run(command, cwd=root / 'src', env=env, stdout=log, stderr=subprocess.STDOUT, timeout=600)
    output = (evidence / (label + '.log')).read_text()
    assert ('running 1 test' if test else 'running 3 tests') in output, output[-3000:]
    assert result.returncode == (101 if red else 0), output[-3000:]
    assert ('test result: FAILED' if red else 'test result: ok') in output, output[-3000:]
    print(label + ': ' + ('behavior failure detected' if red else 'PASS'), flush=True)
run('baseline', '')
for name, before, after, test in mutations:
    if len(sys.argv) > 2 and name not in sys.argv[2:]:
        continue
    assert original.count(before) == 1, (name, original.count(before))
    changed = original.replace(before, after)
    if name == 'early-snapshot':
        changed = changed.replace('        result\n    }\n\n    fn emit', '        self.emit(key, reason);\n        result\n    }\n\n    fn emit')
    if name == 'early-callback':
        changed = changed.replace('        result\n    }\n\n    fn emit', '        result.await\n    }\n\n    fn emit')
    (evidence / (name + '.mutation.txt')).write_text(before + '\n=>\n' + after)
    try:
        source.write_text(changed)
        run(name, test, True)
    finally:
        assert source.read_text() == changed, 'concurrent edit: refuse to overwrite'
        source.write_text(original)
run('restored', '')
(evidence / 'restored.sha256').write_text(hashlib.sha256(source.read_bytes()).hexdigest() + '\n')
