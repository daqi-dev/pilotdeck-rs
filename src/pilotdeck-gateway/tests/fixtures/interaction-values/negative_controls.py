"""Single-writer mutations; assertion failures only, with byte-exact restoration."""
import hashlib
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(exist_ok=False)
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
env = dict(os.environ, CARGO_TARGET_DIR=str(root / '_build/cargo-target'))
command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib','interaction_producer_','--','--nocapture']
selected = set(sys.argv[2:])
for name, path, old, new in mutations:
    if selected and name not in selected:
        continue
    original = path.read_bytes()
    text = original.decode()
    assert text.count(old) == 1, (name, text.count(old))
    with (out / (name + '.mutation')).open('x') as f:
        f.write(str(path.relative_to(root)) + '\n' + old + '\n=>\n' + new + '\n')
    try:
        path.write_text(text.replace(old,new))
        with (out / (name + '.log')).open('x') as log:
            subprocess.run(['bash','scripts/cargo-slots.sh'],cwd=root,stdout=log,stderr=log,check=True)
            result = subprocess.run(command,cwd=root,env=env,stdout=log,stderr=log,timeout=180)
        output = (out / (name + '.log')).read_text()
        assert result.returncode == 101 and 'test result: FAILED.' in output and 'panicked at' in output, name
        print(name + ': assertion killed', flush=True)
    finally:
        path.write_bytes(original)
        assert path.read_bytes() == original
        with (out / (name + '.restored-sha256')).open('x') as f:
            f.write(hashlib.sha256(original).hexdigest()+'\n')
# All mutations restored before the positive run; compile failures never count.
with (out / 'restored.log').open('x') as log:
    subprocess.run(command,cwd=root,env=env,stdout=log,stderr=log,timeout=180,check=True)
print('restored: PASS',flush=True)
