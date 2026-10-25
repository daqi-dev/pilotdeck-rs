#!/usr/bin/env python3
"""Issue #680 negative controls: remove each dynamic ids-shape check and require
the real-TS idAudit replays to fail at runtime, then restore and require pass.

Anchors stay short and unique so a rustfmt change elsewhere cannot silently
break the mutation: the first `assert text.count(old) == 1` is the guard.
The tree lock is taken before reading `original` and held through the final
restore, so a concurrent negative runner can never capture mutated bytes.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

parser = argparse.ArgumentParser()
parser.add_argument('--out', required=True)
args = parser.parse_args()
root = Path(__file__).resolve().parents[5]
os.chdir(root)
out = Path(args.out).resolve()
out.mkdir(parents=True, exist_ok=True)
path = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/entries.rs'
file_memory = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
mutations = [
    # `batch_ids` non-array branch: 42/{}/true must not collapse to an empty list.
    ('container-check', path,
     '        _ => Err(FileMemoryError::Type("ids.filter is not a function".into())),\n',
     '        _ => Ok(&[]),\n'),
    # `batch_ids` undefined/null branches: they must not become empty batches.
    ('container-nullish-check', path,
     '        JsValue::Undefined => Err(FileMemoryError::Type(\n'
     '            "Cannot read properties of undefined (reading \'filter\')".into(),\n'
     '        )),\n'
     '        JsValue::Null => Err(FileMemoryError::Type(\n'
     '            "Cannot read properties of null (reading \'filter\')".into(),\n'
     '        )),\n',
     '        JsValue::Undefined => Ok(&[]),\n'
     '        JsValue::Null => Ok(&[]),\n'),
    # `mutation_ids` non-string branch: 42/{}/[] must not silently become "".
    ('element-check', path,
     '            _ => Err(FileMemoryError::Type(\n'
     '                "relativePath.replace is not a function".into(),\n'
     '            )),\n',
     '            _ => Ok(JsString::from("")),\n'),
    # undefined/null elements must not be treated as empty strings.
    ('nullish-element-check', path,
     '            JsValue::Undefined => Err(FileMemoryError::Type(\n'
     '                "Cannot read properties of undefined (reading \'replace\')".into(),\n'
     '            )),\n'
     '            JsValue::Null => Err(FileMemoryError::Type(\n'
     '                "Cannot read properties of null (reading \'replace\')".into(),\n'
     '            )),\n',
     '            JsValue::Undefined => Ok(JsString::from("")),\n'
     '            JsValue::Null => Ok(JsString::from("")),\n'),
    # ["valid", 5] must reject 5 before the valid id is written.
    ('element-order-check', path,
     '        .iter()\n        .map(|value| match value {\n',
     '        .iter()\n        .take(1)\n        .map(|value| match value {\n'),
    # [null, 5] must report the first element (null), not the later number.
    ('element-reverse-check', path,
     '        .iter()\n        .map(|value| match value {\n',
     '        .iter()\n        .rev()\n        .map(|value| match value {\n'),
    # A genuinely empty array is an empty batch, not a shape error.
    ('empty-array-check', path,
     '        JsValue::Array(items) => Ok(items),\n',
     '        JsValue::Array(items) if !items.is_empty() => Ok(items),\n'),
    # getMemoryRecordsByIds filters non-string elements; it must not panic or
    # keep them as ids.
    ('get-element-filter-check', path,
     '            .filter_map(JsValue::as_string)\n            .cloned()\n',
     '            .map(|value| value.as_string().unwrap())\n            .cloned()\n'),
    # Empty batches repair MEMORY.md. Changing the repaired bytes while keeping
    # the public result must fail the strict diskAfter comparison; without that
    # check the old diskUnchanged:false boolean would still pass.
    ('repair-manifest-bytes', file_memory,
     '                self.write_bytes("MEMORY.md", next)?;\n',
     '                self.write_bytes("MEMORY.md", format!("{next}\\n"))?;\n'),
    # The added archiveTmpEntries batches must be live: the current-project
    # mode rejects before reading ids, so change that observable error text.
    ('archive-error-text', file_memory,
     '                    "archive_tmp is not supported in EdgeClaw current-project memory mode".into(),\n',
     '                    "archive_tmp unsupported".into(),\n'),
]
env = os.environ.copy()
command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '--locked', '--offline',
           '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_entries', '--',
           'malformed_batch_ids', '--nocapture']
rows = []
restored_pass = False
with mutating_sources(root, [path, file_memory]) as originals:
    try:
        for name, target, old, new in mutations:
            text = originals[target].decode()
            assert text.count(old) == 1, (name, text.count(old))
            slot = wait_for_slot(root/'scripts/cargo-slots.sh')
            if slot.returncode:
                raise RuntimeError('cargo slots full')
            target.write_text(text.replace(old, new))
            try:
                run = run_owned(command, env=env, cwd=root, timeout=300)
            finally:
                target.write_bytes(originals[target])
            log = slot.stdout + slot.stderr + '$ ' + ' '.join(command) + '\n' + run.stdout + run.stderr
            (out / (name + '.log')).write_text(log)
            assert run.returncode != 0 and 'test result: FAILED' in log and 'could not compile' not in log, log[-2000:]
            failures = [line.strip() for line in log.splitlines() if 'panicked at' in line or 'left:' in line or 'right:' in line]
            rows.append({'name': name, 'exit': run.returncode, 'failures': failures})
            print(name + ': killed at runtime; ' + '; '.join(failures[:2]), flush=True)
        slot = wait_for_slot(root/'scripts/cargo-slots.sh')
        run = run_owned(command, env=env, cwd=root, timeout=300)
        log = slot.stdout + slot.stderr + '$ ' + ' '.join(command) + '\n' + run.stdout + run.stderr
        (out / 'restored.log').write_text(log)
        restored_pass = run.returncode == 0 and 'test result: ok. 2 passed;' in log
    finally:
        try:
            for target, content in originals.items():
                target.write_bytes(content)
            restored_bytes = all(target.read_bytes() == content
                                 for target, content in originals.items())
            restored = {str(target.relative_to(root)): hashlib.sha256(content).hexdigest()
                        for target, content in originals.items()}
        except OSError:
            restored = {}
            restored_bytes = False
        (out / 'summary.json').write_text(json.dumps({
            'results': rows,
            'restoredSha256': restored,
            'restoredPass': bool(restored_pass and restored_bytes),
            'restoredBytes': restored_bytes,
            'complete': len(rows) == len(mutations),
        }, indent=2) + '\n')
        if not restored_bytes:
            raise RuntimeError('negative_ids sources were not restored to the original bytes')
assert restored_pass, 'restored run failed'
assert len(rows) == len(mutations), 'partial negative suite'
