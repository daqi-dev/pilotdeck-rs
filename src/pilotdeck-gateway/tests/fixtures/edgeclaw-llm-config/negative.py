#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-llm-config.

Every case must compile and fail a runtime oracle assertion; the original
bytes are restored in `finally`. This file is new, so `mutating_sources`
refuses to run until the package is committed; the coordinator runs it on the
host after the commit.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
config = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/config.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
out.mkdir(parents=True, exist_ok=True)
cases = [
    # Default provider: the last env fallback disappears.
    ('provider-default', config,
     '        JsString::from("edgeclaw_memory"),\n', '        JsString::from(""),\n',
     'llm_config_matches_ts'),
    # First failure must stay cached: make a successful read cache `null`.
    ('openclaw-cache-null', config,
     '        *cached = Some(value.clone());\n        value\n',
     '        *cached = Some(None);\n        value\n',
     'llm_config_matches_ts'),
    # Model selection must also match `name`, not only `id`.
    ('model-name-match', config,
     'field(Some(entry), "id") == *model || field(Some(entry), "name") == *model',
     'field(Some(entry), "id") == *model',
     'llm_config_matches_ts'),
    # `EDGECLAW_MEMORY_MODEL` must feed the modelRef parser.
    ('env-model-ref', config,
     '        env_trim(env, "EDGECLAW_MEMORY_MODEL"),\n', '        JsString::default(),\n',
     'llm_config_matches_ts'),
    # Selection baseUrl strips trailing slashes.
    ('selection-trailing-slash', config,
     'value.units()[end - 1] == u16::from(b\'/\')', 'value.units()[end - 1] == u16::from(b\' \')',
     'llm_config_matches_ts'),
    # Environment-shaped apiKey values expand from the injected env.
    ('api-key-env', config,
     '        if looks_like_env_var_name(&configured)\n', '        if false && looks_like_env_var_name(&configured)\n',
     'llm_config_matches_ts'),
    # `Math.floor` on the interval, not ceiling.
    ('merge-floor', config,
     'Some(JsValue::Number(value)) => value.floor().max(0.0)',
     'Some(JsValue::Number(value)) => value.ceil().max(0.0)',
     'llm_config_matches_ts'),
    # Header values are stored untrimmed.
    ('sanitize-header-value', config,
     '            out.insert(key, JsValue::String(text.clone()));',
     '            out.insert(key, JsValue::String(trimmed(Some(text))));',
     'llm_config_matches_ts'),
    # Model headers override provider headers.
    ('headers-model-wins', config,
     '''    for source in [
        provider.and_then(|provider| provider.get_str("headers")),
        model.and_then(|model| model.get_str("headers")),
    ]''',
     '''    for source in [
        model.and_then(|model| model.get_str("headers")),
        provider.and_then(|provider| provider.get_str("headers")),
    ]''',
     'llm_config_matches_ts'),
    # The apiType default is `openai-responses`.
    ('api-type-default', config,
     '        JsString::from("openai-responses"),\n', '        JsString::from("anthropic"),\n',
     'llm_config_matches_ts'),
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
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_llm_config', test, '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=600)
                log = run.stdout + run.stderr
                (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                failed = 'test result: FAILED.' in log
                assert run.returncode != 0 and failed and 'could not compile' not in log, \
                    name + ' did not fail a runtime assertion'
                lines = [line for line in log.splitlines()
                         if 'failed:' in line or 'panicked at' in line or 'test result:' in line]
                print(name + ': ' + ' | '.join(lines), flush=True)
            finally:
                path.write_bytes(originals[path])
    finally:
        for path, data in originals.items():
            path.write_bytes(data)
            assert path.read_bytes() == data, 'restoration failed: ' + str(path)
