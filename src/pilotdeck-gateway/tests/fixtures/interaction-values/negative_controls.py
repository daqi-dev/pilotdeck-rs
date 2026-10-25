"""Single-writer mutations through the shared tree lock; assertion failures only.

``negative_process.mutating_sources`` holds the same-tree flock and verifies the
committed bytes before the first write, so a killed predecessor's residue is a
refusal instead of a silently captured ``original``. Cargo runs through
``run_owned`` (fresh process group, total deadline) and a cargo slot is awaited
before each source write. Evidence goes in a new output folder; every mutant is
restored byte-for-byte and its SHA-256 is retained.
"""
import hashlib
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/runtime/interaction/value.rs'
bus = source.with_name('bus.rs')
reconnect = source.with_name('reconnect.rs')
mutations = [
    ('owner-stale', reconnect, '    if !state\n        .requests\n        .iter()\n        .any(|r| r.token == token && &r.owner == owner)\n    {\n        return;\n    }\n', ''),
    ('owner-create', bus, 'state.replay_owners.insert(owner.clone());', ''),
    ('owner-disconnect', reconnect, '!state.replay_owners.contains(owner)', '!state.requests.iter().any(|r| &r.owner == owner)'),
    ('owner-remove', reconnect, '    if !state.requests.iter().any(|r| &r.owner == owner) {\n        state.replay_owners.remove(owner);', '    if !state.requests.iter().any(|r| &r.owner == owner) {'),
    ('owner-dispose', reconnect, '        state.requests.retain(|request| &request.owner != owner);\n        state.replay_owners.remove(owner);', '        state.requests.retain(|request| &request.owner != owner);'),
    ('owner-clear', reconnect, 'state.replay_owners.clear();', ''),
    ('wire-normalize', source, 'replay_payload(Some(value.clone())).unwrap_or(JsValue::Undefined)', 'value.clone()'),
    ('unsupported', source, 'values.push(JsValue::Undefined)', 'values.push(JsValue::Null)'),
    ('date-invalid', source, '!ms.is_finite() || ms.abs() > 8.64e15', 'ms.abs() > 8.64e15'),
    ('date-range', source, '!ms.is_finite() || ms.abs() > 8.64e15', '!ms.is_finite()'),
    ('date-clip', source, 'format_epoch_iso(*ms as i64)', 'format_epoch_iso(ms.floor() as i64)'),
    ('date-iso', source, 'format_epoch_iso(*ms as i64)', 'format_epoch_iso(0)'),
    ('bigint', source, 'JsValue::String(decimal.clone().into())', 'JsValue::Null'),
    ('bigint-empty', source, 'digits.is_empty()', 'false'),
    ('bigint-digits', source, '!digits.bytes().all(|b| b.is_ascii_digit())', 'false'),
    ('bigint-canonical', source, '(digits.starts_with(\'0\') && decimal != "0")', 'false'),
    ('wire-identity', source, '| ProducerValue::Value(JsValue::Array(_) | JsValue::Object(_))', ''),
    ('identity', source, 'values.push("[Circular]".into());', 'values.push(JsValue::Null);'),
    ('object-order', source, 'fields.sort_by_key(|(key, _)| {\n                            (key.array_index().is_none(), key.array_index())\n                        });', 'fields.reverse();'),
    ('array-undefined', source, 'if v.is_undefined() { JsValue::Null } else { v }', 'v'),
    ('object-undefined', source, '!key.equals_str("__proto__") && !child.is_undefined()', '!key.equals_str("__proto__")'),
    ('object-proto', source, '!key.equals_str("__proto__") && !child.is_undefined()', '!child.is_undefined()'),
    ('proto-visit', source, 'let mut fields: Vec<_> = fields.iter().collect();', 'let mut fields: Vec<_> = fields.iter().filter(|(k, _)| !k.equals_str("__proto__")).collect();'),
    ('object-fields', source, 'let mut fields: Vec<_> = fields.iter().collect();', 'let mut fields: Vec<_> = fields.iter().filter(|_| false).collect();'),
    ('array-fields', source, 'work.push(Work::Array(items.len()));', 'work.push(Work::Array(0));'),
    ('root-omit', source, 'values.pop().filter(|value| !value.is_undefined())', 'values.pop()'),
    ('invalid-reference', source, 'let node = nodes.get(index).ok_or_else(|| {', 'let node = Some(nodes.get(index).unwrap_or(&ProducerValue::Function)).ok_or_else(|| {'),
    ('entry-bypass', bus, '|| to_interaction_replay_value(nodes, root)', '|| { let _ = (nodes, root); Ok(None) }'),
    ('validation-order', bus, 'self.register_with_payload(owner, entry, || to_interaction_replay_value(nodes, root))',
     'let payload = to_interaction_replay_value(nodes, root)?; self.register_with_payload(owner, entry, || Ok(payload))'),
]
selected = set(sys.argv[2:])
mutations = [mutation for mutation in mutations
             if not selected or mutation[0] in selected]
assert len({name for name, *_ in mutations}) == len(mutations), 'duplicate mutation name'
assert not selected or len(mutations) == len(selected), 'unknown mutation name'
paths = sorted({path for _name, path, _old, _new in mutations}, key=str)
env = dict(os.environ, CARGO_TARGET_DIR=str(root / '_build/cargo-target'))
command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib','interaction_producer_','--','--nocapture']

with mutating_sources(root, paths) as snapshots:
    # All anchors are checked against the locked, HEAD-verified snapshots before
    # the first write: a stale anchor aborts with the tree still pristine.
    for name, path, old, new in mutations:
        text = snapshots[path].decode()
        assert text.count(old) == 1, (name, text.count(old))
        with (out / (name + '.mutation')).open('x') as f:
            f.write(str(path.relative_to(root)) + '\n' + old + '\n=>\n' + new + '\n')
    for name, path, old, new in mutations:
        original = snapshots[path]
        text = original.decode()
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
            (out / (name + '.slots.log')).write_text(slot.stdout + slot.stderr)
            path.write_text(text.replace(old, new))
            result = run_owned(command, cwd=root, env=env, timeout=180)
            log = result.stdout + result.stderr
            (out / (name + '.log')).write_text(log)
            assert result.returncode == 101 and 'test result: FAILED.' in log and 'panicked at' in log, name
            print(name + ': KILLED (assertion killed)', flush=True)
        finally:
            path.write_bytes(original)
            assert path.read_bytes() == original
            with (out / (name + '.restored-sha256')).open('x') as f:
                f.write(hashlib.sha256(original).hexdigest()+'\n')
# All mutations restored and checked by mutating_sources; compile failures never count.
slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
(out / 'restored.slots.log').write_text(slot.stdout + slot.stderr)
result = run_owned(command, cwd=root, env=env, timeout=180)
log = result.stdout + result.stderr
(out / 'restored.log').write_text(log)
assert result.returncode == 0 and 'test result: ok.' in log, 'restored positive control failed'
print('restored: PASS',flush=True)
