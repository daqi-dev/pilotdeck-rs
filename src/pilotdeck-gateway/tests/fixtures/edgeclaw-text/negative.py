#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-text.

Every case must compile and fail a runtime oracle assertion; the script restores
the original bytes in `finally` and never touches oracle.json.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
text = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/text.rs'
helpers = text.parent / 'file_memory' / 'helpers.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
out.mkdir(parents=True, exist_ok=True)
cases = [
    # Change the sha1 truncation: every id and external id collapses.
    ('hash-truncation', helpers,
     'format!("{:x}", Sha1::digest(value.as_bytes()))[..10].into()',
     'format!("{:x}", Sha1::digest(value.as_bytes()))[..9].into()',
     'ids_match_ts'),
    # Change an output section header: the rendered recall context no longer
    # matches the TS bytes even though parsing still works.
    ('section-header', text, '("## User Profile", &sections.user_profile)',
     '("## User", &sections.user_profile)', 'recall_context_matches_ts'),
    # Drop the final JS trim: leading/trailing whitespace survives normalizeText.
    ('trim-semantics', text,
     'JsString::from_utf16(js_trim_units(&out).to_vec())',
     'JsString::from_utf16(out)',
     'text_helpers_match_ts'),
    # Compare General paths without path.resolve: a trailing `/.` differs.
    ('general-decision', text,
     '''    let general = general_workspace_dir_in(&home?);
    Ok(normalize_workspace_path(workspace_dir)?.units()
        == normalize_workspace_path(&general)?.units())''',
     '''    let general = general_workspace_dir_in(&home?);
    Ok(workspace_dir.units() == general.units())''',
     'general_workspace_match_ts'),
    # Swallow a failed cwd lookup back into the empty-string fallback.
    ('cwd-fallback', text,
     '''    let cwd = cwd?;
    Ok(resolve_units(&cwd.to_string_lossy(), workspace_path))''',
     '''    let cwd = cwd.unwrap_or_default();
    Ok(resolve_units(&cwd.to_string_lossy(), workspace_path))''',
     'normalize_workspace_path_reports_a_missing_cwd'),
    # Swallow a failed homedir lookup into "not the General workspace".
    ('homedir-false', text,
     '''    let general = general_workspace_dir_in(&home?);
    Ok(normalize_workspace_path(workspace_dir)?.units()''',
     '''    let Ok(home) = home else { return Ok(false) };
    let general = general_workspace_dir_in(&home);
    Ok(normalize_workspace_path(workspace_dir)?.units()''',
     'general_workspace_reports_a_missing_homedir'),
    # Reintroduce recursion just before the explicit stack: 1e5 nesting
    # overflows the thread stack instead of returning.
    ('recursive-decode', text,
     '''pub fn decode_escaped_unicode_value(value: &JsValue, decode_common_escapes: bool) -> JsValue {
    // Explicit stack: the TS baseline recurses, so a deeply nested JSON value''',
     '''pub fn decode_escaped_unicode_value(value: &JsValue, decode_common_escapes: bool) -> JsValue {
    if let JsValue::Array(items) = value {
        if let Some(first) = items.first() {
            return JsValue::Array(vec![decode_escaped_unicode_value(first, decode_common_escapes)]);
        }
    }
    // Explicit stack: the TS baseline recurses, so a deeply nested JSON value''',
     'decode_escaped_unicode_value_handles_100k_nesting'),
    # Render null array elements as the literal "null" instead of joining empty.
    ('array-join', text.parent / '..' / '..' / '..' / 'js_value.rs',
     '''                if !matches!(item, JsValue::Undefined | JsValue::Null) {
                    next = Some(item);
                }''',
     '''                next = Some(item);''',
     'trace_i18n_matches_ts'),
    # Keep route= metadata lines: they must be stripped before rendering.
    ('route-strip', text,
     '''        if line.equals_str("## ClawXMemory Recall")
            || starts_with(&line, "route=")
            || line.equals_str(''',
     '''        if line.equals_str("## ClawXMemory Recall")
            || line.equals_str(''',
     'recall_context_matches_ts'),
    # Break the empty-session fallback.
    ('l0-fallback', text,
     'JsString::from("session")', 'JsString::from("s")', 'ids_match_ts'),
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
with mutating_sources(root, sorted({path for _, path, _, _, _ in cases})) as originals:
    try:
        for name, path, before, after, test in cases:
            source = originals[path].decode()
            assert source.count(before) == 1, (name, source.count(before))
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(source.replace(before, after))
            try:
                cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_text', test, '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=300)
                log = run.stdout + run.stderr
                (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                failed = 'test result: FAILED.' in log or (
                    'has overflowed its stack' in log and 'fatal runtime error' in log)
                assert run.returncode != 0 and failed and 'could not compile' not in log, \
                    name + ' did not fail a runtime assertion'
                lines = [line for line in log.splitlines()
                         if 'failed:' in line or 'panicked at' in line or 'test result:' in line
                         or 'overflowed its stack' in line]
                print(name + ': ' + ' | '.join(lines), flush=True)
            finally:
                path.write_bytes(originals[path])
    finally:
        for path, data in originals.items():
            path.write_bytes(data)
            assert path.read_bytes() == data, 'restoration failed: ' + str(path)
