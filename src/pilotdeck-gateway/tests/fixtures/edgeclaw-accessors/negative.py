#!/usr/bin/env python3
"""Reversible product mutations; every case must compile and fail its runtime
assertion against the real-TS oracle. Run serially from a cleanly tested
worktree. Never mutates expected values.
"""
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/accessors.rs'
helpers = source.parent.parent / 'file_memory' / 'helpers.rs'
storage = source.parent.parent / 'storage.rs'
file_memory = source.parent.parent / 'file_memory.rs'
tests = root / 'src/pilotdeck-gateway/tests/edgeclaw_accessors.rs'
replay = 'ts_databases_and_file_tree_replay_repository_accessors'
home = 'repository_home_fallback_matches_ts_homedir'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
# name, file, original, mutation, test
cases = [
    ('close', storage, '        if guard.is_none() {\n            return Err(StorageError::Closed);\n        }\n        if let Some(conn)', '        if guard.is_none() { return Ok(()); }\n        if let Some(conn)', replay),
    ('accessor', source, '        &self.workspace_memory\n', '        &self.global_user_memory\n', replay),
    ('mode', source, '        &self.repository.mode\n', '        "single"\n', replay),
    ('catalog-order', helpers, '            crate::project_files::node_compare_utf16(&collator, &a, &b)', '            a.units().cmp(b.units())', replay),
    ('newest-first', helpers, '            crate::project_files::node_compare_utf16(\n                &collator,\n                &JsString::from(bt),\n                &JsString::from(at),\n            )', '            crate::project_files::node_compare_utf16(\n                &collator,\n                &JsString::from(at),\n                &JsString::from(bt),\n            )', replay),
    ('external-path', source, '"external:{}:{path}"', '"wrong:{}:{path}"', replay),
    ('include-external', source, '        Some(value) => js_truthy(value),', '        Some(_) => true,', replay),
    ('global-filter', source, '        if text(options, "scope") == "project" {', '        if false {', replay),
    ('global-prefix', helpers, 'format!("global/{path}")', 'path', replay),
    ('repair', source, '        self.workspace_memory.repair_manifests()', '        Ok(JsValue::Undefined)', replay),
    ('cache-refresh', source, '        let mut next = HashMap::new();', '        if !cache.is_empty() { return Ok(cache.values().cloned().collect()); }\n        let mut next = HashMap::new();', replay),
    # listGlobalMemoryEntries must not forward projectId/includeTmp to the store.
    ('global-whitelist', source, 'fn global_entry_input(options: &JsObject, kinds: Option<Vec<JsValue>>) -> JsObject {\n    let mut input = JsObject::new();', 'fn global_entry_input(options: &JsObject, kinds: Option<Vec<JsValue>>) -> JsObject {\n    let mut input = options.clone();', replay),
    # Dropping every forwarded option (the opposite extreme) must also fail:
    # numeric limit 2.5/NaN, offset 1 and query "Probe three" change the result.
    ('global-drop', source, '''    if let Some(query) = options.get("query").filter(|v| js_truthy(v)) {
        put(&mut input, "query", query.clone());
    }
    if let Some(JsValue::Number(limit)) = options.get("limit") {
        put(&mut input, "limit", JsValue::Number(*limit));
    }
    if let Some(JsValue::Number(offset)) = options.get("offset") {
        put(&mut input, "offset", JsValue::Number(*offset));
    }
''', '', replay),
    # Numeric forwarding alone: with the 6-entry probe store, dropping
    # limit/offset changes limit 2.5 (2 vs 6) and NaN ([] vs 6).
    ('global-limit-drop', source, '''    if let Some(JsValue::Number(limit)) = options.get("limit") {
        put(&mut input, "limit", JsValue::Number(*limit));
    }
    if let Some(JsValue::Number(offset)) = options.get("offset") {
        put(&mut input, "offset", JsValue::Number(*offset));
    }
''', '', replay),
    # NaN is `typeof number` and must be forwarded; treating it as absent makes
    # the 6-entry probe return all entries instead of the oracle's [].
    ('global-limit-nan-drop', source, '    if let Some(JsValue::Number(limit)) = options.get("limit") {\n        put(&mut input, "limit", JsValue::Number(*limit));\n    }', '    if let Some(JsValue::Number(limit)) = options.get("limit").filter(|v| !v.as_number().is_some_and(f64::is_nan)) {\n        put(&mut input, "limit", JsValue::Number(*limit));\n    }', replay),
    # A truthy non-string query must reach the store and throw from the eager
    # normalizeWhitespace. Falling back to no query returns every entry instead.
    ('query-typeerror', helpers, '''        Some(_) => {
            return Err(FileMemoryError::Type(
                "(value ?? \\"\\").replace is not a function".into(),
            ));
        }''', '        Some(_) => return Ok(None),', replay),
    # includeDeprecated is a JS truthy OR on the caller option: 1/"yes"/[]/{}
    # include deprecated entries. Boolean-only matching silently drops them.
    ('include-deprecated', file_memory, 'truthy(input, "includeDeprecated")', 'flag(input, "includeDeprecated")', replay),
    # The accessor drops only falsy query values. Forwarding 0 as well must now
    # throw (not silently return entries) on both accessor paths.
    ('project-query-raw', source, '''    if let Some(query) = options.get("query").filter(|v| js_truthy(v)) {
        put(&mut input, "query", query.clone());
    }
    put(&mut input, "limit", JsValue::Number(5000.0));''', '''    if let Some(query) = options.get("query") {
        put(&mut input, "query", query.clone());
    }
    put(&mut input, "limit", JsValue::Number(5000.0));''', replay),
    ('global-query-raw', source, '''    if let Some(query) = options.get("query").filter(|v| js_truthy(v)) {
        put(&mut input, "query", query.clone());
    }
    if let Some(JsValue::Number(limit)) = options.get("limit") {''', '''    if let Some(query) = options.get("query") {
        put(&mut input, "query", query.clone());
    }
    if let Some(JsValue::Number(limit)) = options.get("limit") {''', replay),
    # Non-array kinds throws TypeError in TS before the scope check.
    ('kinds-typeerror', source, '        Some(_) => Err(FileMemoryError::Type(\n            "options.kinds?.filter is not a function".into(),\n        )),', '        Some(_) => Ok(None),', replay),
    # Project kinds is forwarded to the store's `new Set(...)`: numbers throw.
    ('project-kinds-typeerror', source, '        Some(JsValue::Number(number)) => Err(not_iterable(format!(\n            "number {}",\n            crate::js_value::number_string(*number)\n        ))),', '        Some(value @ JsValue::Number(_)) => Ok(value.clone()),', replay),
    # Boolean and object kinds have their own TypeError arms; each must be
    # exercised by a mutation, not only one family representative.
    ('project-kinds-bool', source, '        Some(JsValue::Bool(value)) => Err(not_iterable(format!("boolean {value}"))),', '        Some(value @ JsValue::Bool(_)) => Ok(value.clone()),', replay),
    ('project-kinds-object', source, '        Some(JsValue::Object(_)) => Err(not_iterable("object".into())),', '        Some(value @ JsValue::Object(_)) => Ok(value.clone()),', replay),
    # TS hardcodes limit 5000/offset 0 for readable entries.
    ('limit-override', source, '    put(&mut input, "limit", JsValue::Number(5000.0));\n    put(&mut input, "offset", JsValue::Number(0.0));', '    put(\n        &mut input,\n        "limit",\n        options\n            .get("limit")\n            .cloned()\n            .unwrap_or(JsValue::Number(5000.0)),\n    );\n    put(&mut input, "offset", JsValue::Number(0.0));', replay),
    # os.homedir() falls back to passwd when HOME is unset.
    ('home-fallback', source, '    let output = Command::new("/usr/bin/id").arg("-P").output().ok()?;', '    let output = Command::new("/usr/bin/true").output().ok()?;', home),
    # A verifier timeout must signal the whole owned group, not just the leader.
    ('group-kill', tests, '            let group = rustix::process::kill_process_group(\n                rustix::process::Pid::from_raw(child.id() as i32).expect("child pid"),\n                rustix::process::Signal::KILL,\n            );', '            let group: std::io::Result<()> = Ok(());', 'subprocess_timeout_kills_the_whole_process_group'),
]
originals = {p: p.read_bytes() for _, p, _, _, _ in cases}
try:
    for name, path, before, after, test in cases:
        text = originals[path].decode()
        assert text.count(before) == 1, (name, text.count(before))
        path.write_text(text.replace(before, after))
        try:
            slots = subprocess.run([str(root / 'scripts/cargo-slots.sh')],capture_output=True,text=True)
            while slots.returncode == 1:
                time.sleep(15)
                slots = subprocess.run([str(root / 'scripts/cargo-slots.sh')],capture_output=True,text=True)
            if slots.returncode != 0:
                raise RuntimeError('cargo slots unavailable: '+slots.stdout+slots.stderr)
            cmd = ['cargo','test','--manifest-path',str(root/'src/Cargo.toml'),'-p','pilotdeck-gateway','--test','edgeclaw_accessors',test,'--','--exact']
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
