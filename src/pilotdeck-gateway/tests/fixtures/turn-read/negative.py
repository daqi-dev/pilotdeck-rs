#!/usr/bin/env python3
"""Temporarily mutate the owned implementation, require assertion failure, restore.

No tests/oracle are changed. Run alone: an exclusive directory lock prevents
another mutation run. Every mutation and the original SHA are recorded before
writing. finally restores the exact bytes, including on failed controls.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
repo = rust.parent
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
source = rust / 'pilotdeck-gateway/src/runtime/turn/read.rs'
registry_source = rust / 'pilotdeck-gateway/src/storage/run_registry.rs'
originals = {source: source.read_bytes(), registry_source: registry_source.read_bytes()}
lock = repo / '_tmp/turn-read-mutation.lock'
lock.mkdir(parents=True)
env = dict(os.environ, CARGO_NET_OFFLINE='true')
# Reuse the tree's own build directory (CARGO_TARGET_DIR from the caller, else src/target);
# a separate repo/target is a second full ~30 GiB build of the gateway.
env.setdefault('CARGO_TARGET_DIR', str(rust / 'target'))
prefix = 'runtime::turn::read::tests::'
mutants = [
    ('attempts-3-to-2', source,
     'pub const HISTORY_EPOCH_ATTEMPTS: usize = 3;', 'pub const HISTORY_EPOCH_ATTEMPTS: usize = 2;',
     'read_epoch_never_settles_error'),
    ('attempts-3-to-4', source,
     'pub const HISTORY_EPOCH_ATTEMPTS: usize = 3;', 'pub const HISTORY_EPOCH_ATTEMPTS: usize = 4;',
     'read_epoch_never_settles_error'),
    ('final-error-text', source,
     '"Session changed during transcript synchronization; retry the snapshot.",',
     '"Session changed; retry the snapshot.",',
     'read_epoch_never_settles_error'),
    ('epoch-retry-removed', source,
     '        if !same_turn_epoch(&before, &stream) {',
     '        if false && !same_turn_epoch(&before, &stream) {',
     'read_replaced_run_during_read_retries'),
    ('history-error-retried', source,
     '        let history = history.read(input).await?;',
     '        let history = match history.read(input).await { Ok(history) => history, Err(_) => continue };',
     'read_history_error_propagates_without_retry'),
    ('not-configured-message', source,
     '"read_session_messages is not configured. Wire `readSessionMessages` via createLocalGateway.",',
     '"read_session_messages is not wired.",',
     'read_not_configured'),
    ('merge-stream-first', source,
     '    let mut merged = history.as_object().cloned().unwrap_or_default();\n    merged.insert("stream".into(), stream.to_json());',
     '    let mut merged = JsObject::new();\n    merged.insert("stream".into(), stream.to_json());\n'
     '    for (key, value) in history.as_object().cloned().unwrap_or_default().entries().iter() {\n'
     '        merged.insert(key.clone(), value.clone());\n    }',
     'read_stable_active_merges_history_and_stream'),
    ('merge-keeps-stale-stream', source,
     '    merged.insert("stream".into(), stream.to_json());',
     '    if merged.get("stream").is_none() {\n        merged.insert("stream".into(), stream.to_json());\n    }',
     'read_history_stream_key_replaced_in_place'),
    ('cursor-validation-removed', source,
     '        && !is_non_negative_safe_integer(value)',
     '        && false',
     'run_events_validation_and_not_found'),
    ('limit-validation-removed', source,
     '    if let Some(value) = limit\n        && !is_valid_limit(value)\n    {',
     '    if let Some(value) = limit\n        && false\n    {',
     'run_events_validation_and_not_found'),
    ('limit-bound-501', source,
     '(1.0..=500.0).contains(&n)', '(1.0..=501.0).contains(&n)',
     'run_events_validation_and_not_found'),
    ('run-not-found-message', source,
     'units.extend(" is not registered.".encode_utf16());',
     'units.extend(" is not in the registry.".encode_utf16());',
     'run_events_validation_and_not_found'),
    ('gap-off-by-one', source,
     'after.as_number().is_some_and(|a| a > last)', 'after.as_number().is_some_and(|a| a > last + 1.0)',
     'run_events_paging'),
    ('gap-number-only', source,
     'last_seq_to_number(last)',
     'last_seq_to_number(&JsValue::Number(last.as_number().unwrap_or(f64::NAN)))',
     'run_events_non_numeric_last_seq'),
    ('toprimitive-error-swallowed', source,
     'if object.get_property("toString").is_some() {',
     'if false && object.get_property("toString").is_some() {',
     'run_events_toprimitive_throw_last_seq'),
    ('deep-array-gate-reintroduced', source,
     'Some(JsValue::Array(items)) => frames.push((items.iter(), false)),',
     'Some(JsValue::Array(items)) => {\n'
     '                if frames.len() >= js_value::MAX_ARRAY_DEPTH {\n'
     '                    return Err(NotPrimitive);\n'
     '                }\n'
     '                frames.push((items.iter(), false));\n'
     '            }',
     'run_events_deep_array_last_seq'),
    # r5 (A7.2-pr7 gate5 P2a): the two r4 negative controls for the
    # pathological-depth probe, previously only run tree-out, now reproducible
    # in the suite. Both target the parent test; the spin outlives its 10 s
    # deadline, the recursive join overflows the child's stack at 100,000
    # layers. Both must compile — only the probe may fail.
    ('lastseq-conversion-spin', source,
     'fn last_seq_to_number(value: &JsValue) -> Result<f64, NotPrimitive> {\n    match value {',
     'fn last_seq_to_number(value: &JsValue) -> Result<f64, NotPrimitive> {\n'
     '    let spin = std::time::Instant::now();\n'
     '    while spin.elapsed() < std::time::Duration::from_secs(30) { std::hint::spin_loop(); }\n'
     '    match value {',
     'run_events_pathological_depth_last_seq_is_bounded'),
    ('join-string-recursive', source,
     'fn join_string(value: &JsValue) -> Result<String, NotPrimitive> {\n    let mut out = Vec::new();',
     'fn join_string(value: &JsValue) -> Result<String, NotPrimitive> {\n'
     '    if let JsValue::Array(items) = value {\n'
     '        let mut out = String::new();\n'
     '        let mut separate = false;\n'
     '        for item in items {\n'
     '            if separate {\n'
     '                out.push_str(",");\n'
     '            }\n'
     '            separate = true;\n'
     '            if !matches!(item, JsValue::Undefined | JsValue::Null) {\n'
     '                out.push_str(&join_string(item)?);\n'
     '            }\n'
     '        }\n'
     '        return Ok(out);\n'
     '    }\n'
     '    let mut out = Vec::new();',
     'run_events_pathological_depth_last_seq_is_bounded'),
    ('nextseq-dropped', source,
     '    if let Some(last) = events.last() {\n        result.insert("nextSeq".into(), field(last, "seq").clone());\n    }',
     '    let _ = events.last();',
     'run_events_paging'),
    ('runid-surrogate-dropped', source,
     'units.extend_from_slice(run_id.units());',
     'units.extend(run_id.to_utf8().unwrap_or_default().encode_utf16());',
     'run_not_found_lone_surrogate_runid'),
    ('record-drops-projectkey', source,
     '    if let Some(project) = record\n        .get("projectKey")\n        .filter(|value| !value.is_undefined())\n    {',
     '    if let Some(project) = record\n        .get("projectKey_")\n        .filter(|value| !value.is_undefined())\n    {',
     'run_get_maps_durable_records'),
    ('record-drops-result', source,
     'if let Some(result) = record.get("result").filter(|value| !value.is_undefined()) {',
     'if let Some(result) = record.get("result_").filter(|value| !value.is_undefined()) {',
     'run_get_maps_durable_records'),
    ('reattach-returns-none', source,
     '    run_get(registry, run).await\n}',
     '    let _ = (registry, run);\n    Ok(None)\n}',
     'run_get_maps_durable_records'),
    ('capability-unavailable-dropped', source,
     ') -> Result<Option<JsValue>, TurnReadError> {\n    let Some(registry) = registry else {\n'
     '        return Err(TurnReadError::dialog(\n            "CAPABILITY_UNAVAILABLE",\n'
     '            "Persistent run registry is unavailable.",\n        ));\n    };',
     ') -> Result<Option<JsValue>, TurnReadError> {\n    let Some(registry) = registry else {\n'
     '        return Ok(None);\n    };',
     'run_capability_unavailable'),
    ('registry-error-swallowed', source,
     'let record = registry.get(run).await.map_err(registry_error)?;',
     'let record = registry.get(run).await.unwrap_or(None);',
     'run_registry_failure_propagates'),
    ('registry-error-name-plain', source,
     'name: error.name().to_string(),', 'name: "Error".to_string(),',
     'run_registry_failure_propagates'),
    ('corrupt-snapshot-name-flattened', registry_source,
     'Self::Named(name, _) => name,', 'Self::Named(_, _) => "Error",',
     'run_corrupt_snapshot_errors'),
]
summary = []
try:
    for name, path, before, after, test in mutants:
        assert originals[path].decode().count(before) == 1, (name, 'anchor is not unique')
    (output / 'negative-original-sha256.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(b).hexdigest() for p,b in originals.items()}, indent=2)+'\n')
    for name, path, before, after, test in [('baseline', None, None, None, '')] + mutants:
        if path:
            assert path.read_bytes() == originals[path], f'{path} changed outside this run'
            path.write_text(originals[path].decode().replace(before, after, 1))
        slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True)
        while slots.returncode == 1:
            time.sleep(30)
            slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True)
        command = ['cargo','test','--offline','--manifest-path',str(rust / 'Cargo.toml'),'-p','pilotdeck-gateway','--lib',prefix+test,'--','--nocapture']
        process = subprocess.Popen(command, cwd=repo, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True)
        try:
            raw, _ = process.communicate(timeout=240)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            raw, _ = process.communicate()
        finally:
            if path: path.write_bytes(originals[path])
        valid = (process.returncode == 0 and 'test result: ok' in raw) if name == 'baseline' else (
            process.returncode == 101 and 'test result: FAILED' in raw and 'could not compile' not in raw and prefix+test in raw
            and ('panicked at' in raw or 'assertion' in raw))
        (output / f'negative-{name}.log').write_text('COMMAND '+ ' '.join(command)+'\n'+slots.stdout+slots.stderr+raw+f'\nexit={process.returncode}\n')
        summary.append(dict(name=name, file=str(path.relative_to(repo)) if path else None, test=prefix+test, before=before, after=after, exit=process.returncode, valid=valid))
        (output/'negative-summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2)+'\n')
        print(f'{name}: {"PASS" if valid else "INVALID"} exit={process.returncode}', flush=True)
        if not valid: raise SystemExit(1)
finally:
    for path, original in originals.items():
        # These paths belong exclusively to this run while the lock exists.
        path.write_bytes(original)
        assert path.read_bytes() == original
    lock.rmdir()
print(f'{len(mutants)} compiled implementation mutants killed; original bytes restored', flush=True)
