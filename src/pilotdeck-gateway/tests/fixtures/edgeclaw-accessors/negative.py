#!/usr/bin/env python3
"""Reversible product mutations; every case must compile and fail the TS replay.
Run serially from a cleanly tested worktree. Never mutates expected values.
"""
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/accessors.rs'
storage = source.parent.parent / 'storage.rs'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
cases = [
    ('close', storage, '        if guard.is_none() {\n            return Err(StorageError::Closed);\n        }\n        if let Some(conn)', '        if guard.is_none() { return Ok(()); }\n        if let Some(conn)'),
    ('accessor', source, '        &self.workspace_memory\n', '        &self.global_user_memory\n'),
    ('mode', source, '        &self.repository.mode\n', '        "single"\n'),
    ('catalog-order', source, '            node_compare_utf16(&collator, &a, &b)', '            a.units().cmp(b.units())'),
    ('newest-first', source, 'node_compare_utf16(&collator, &JsString::from(bt), &JsString::from(at))', 'node_compare_utf16(&collator, &JsString::from(at), &JsString::from(bt))'),
    ('external-path', source, '"external:{}:{path}"', '"wrong:{}:{path}"'),
    ('include-external', source, '!matches!(options.get("includeExternal"), Some(JsValue::Bool(false)))', 'true'),
    ('global-filter', source, 'text(options, "scope") == "project" || user == Some(false)', 'user == Some(false)'),
    ('global-prefix', source, 'format!("global/{path}")', 'path'),
    ('repair', source, '        self.workspace_memory.repair_manifests()', '        Ok(JsValue::Undefined)'),
    ('cache-refresh', source, '        let mut next = HashMap::new();', '        if !cache.is_empty() { return Ok(cache.values().cloned().collect()); }\n        let mut next = HashMap::new();'),
]
originals = {p: p.read_bytes() for _, p, _, _ in cases}
cmd = ['cargo','test','--manifest-path',str(root/'src/Cargo.toml'),'-p','pilotdeck-gateway','--test','edgeclaw_accessors','ts_databases_and_file_tree_replay_repository_accessors','--','--exact']
try:
    for name, path, before, after in cases:
        text = originals[path].decode()
        assert text.count(before) == 1, (name, text.count(before))
        path.write_text(text.replace(before, after))
        try:
            slots = subprocess.run(['/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh'],capture_output=True,text=True)
            while slots.returncode == 1:
                time.sleep(15)
                slots = subprocess.run(['/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh'],capture_output=True,text=True)
            if slots.returncode != 0:
                raise RuntimeError('cargo slots unavailable: '+slots.stdout+slots.stderr)
            run = subprocess.run(cmd,cwd=root,capture_output=True,text=True,timeout=180)
            log = run.stdout+run.stderr
            (out/f'{name}.log').write_text(slots.stdout+slots.stderr+log)
            assert run.returncode != 0 and 'test result: FAILED.' in log and 'could not compile' not in log, name+' did not fail a runtime assertion'
            lines = [line for line in log.splitlines() if 'failed:' in line or 'panicked at' in line or 'test result:' in line]
            print(name+': '+ ' | '.join(lines),flush=True)
        finally:
            path.write_bytes(originals[path])
finally:
    for path, data in originals.items():
        assert path.read_bytes() == data, 'restoration failed: '+str(path)
