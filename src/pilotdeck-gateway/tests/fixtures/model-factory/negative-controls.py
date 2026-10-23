#!/usr/bin/env python3
"""Serial, bounded source mutations; restore bytes even on failure. No scratch target."""
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[5]
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
expected_target = root / 'src/target'
if Path(os.environ['CARGO_TARGET_DIR']).resolve() != expected_target:
    raise SystemExit(f'use the launcher target {expected_target}')
registry = root / 'src/pilotdeck-gateway/src/runtime/model/provider_registry.rs'
family = root / 'src/pilotdeck-gateway/src/runtime/project/model.rs'
sources = {path: path.read_bytes() for path in (registry, family)}

controls = [
    ('new-invocation-gets-old-generation', registry,
     'let previous = state.entries.insert(id, entry.clone());',
     'let previous = state.entries.get(&id).cloned();\n        if previous.is_none() { state.entries.insert(id, entry.clone()); }',
     'provider_replacement_captures_accepted_generation_and_drains'),
    ('dispose-before-drain', registry, '            entry.drained().await;', '// NEGATIVE: do not wait for accepted invocations.',
     'provider_replacement_captures_accepted_generation_and_drains'),
    ('http-cancellation-not-forwarded', registry,
     '''        Box::pin(http::stream_model(
            self.transport.as_ref(),
            request,
            &self.config,
            options,
            sink,
        ))''',
     '''        Box::pin(async move {
            let options = StreamOptions { abort: None, ..options.clone() };
            http::stream_model(self.transport.as_ref(), request, &self.config, &options, sink).await
        })''', 'factory_reload_inflight_cancel_and_drain_match_ts'),
    ('registration-removes-replacement', registry,
     '''if registry
            .entries
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.entry))''',
     'if true', 'provider_replacement_captures_accepted_generation_and_drains'),
    ('replacement-changes-map-dispose-order', registry,
     'entries.sort_by_key(|entry| entry.order);', 'entries.sort_by_key(|entry| entry.generation);',
     'registry_errors_ordered_disposal_and_complete_lease_match_ts'),
    ('complete-releases-before-settle', registry,
     'lease.provider().complete(request, abort).await',
     'let provider = lease.provider().clone(); drop(lease); provider.complete(request, abort).await',
     'registry_errors_ordered_disposal_and_complete_lease_match_ts'),
    ('factory-skips-durable-recorder', family,
     '''            self.model_config.clone(),
            recorder,
            options,''',
     '''            self.model_config.clone(),
            { let _ = recorder; None },
            options,''', 'factory_reload_inflight_cancel_and_drain_match_ts'),

    ('provider-validation-forgets-js-bom', registry, '| 0xFEFF', '',
     'registry_errors_ordered_disposal_and_complete_lease_match_ts'),
    ('native-family-registers-no-providers', family,
     'for provider in providers {', 'for provider in providers.into_iter().take(0) {',
     'native_default_family_metadata_recording_and_http_abort_match_ts'),
    ('factory-ignores-project-router-defaults', family,
     'project_router_config(&snapshot)', 'RouterConfig::from_snapshot(&snapshot)',
     'native_default_family_metadata_recording_and_http_abort_match_ts'),
]

try:
    requested = set(sys.argv[2:])
    if requested - {control[0] for control in controls}:
        raise RuntimeError('unknown control name')
    for name, path, before, after, test in controls:
        if requested and name not in requested:
            continue
        target = ['--test', 'model_factory']
        if test.startswith('lib:'):
            target = ['--lib']
            test = test.removeprefix('lib:')
        original = sources[path].decode()
        if original.count(before) != 1:
            raise RuntimeError(f'{name}: mutation anchor count {original.count(before)}')
        while True:
            slot = subprocess.run([str(root / 'scripts/cargo-slots.sh')], cwd=root, timeout=10)
            if slot.returncode == 0:
                break
            if slot.returncode != 1:
                raise RuntimeError('cargo slot preflight failed')
            time.sleep(5)
        path.write_text(original.replace(before, after))
        try:
            with (output / f'{name}.log').open('wb') as log:
                result = subprocess.run(['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'), '--offline', '-p', 'pilotdeck-gateway', *target, test, '--', '--exact', '--test-threads=1'], cwd=root, stdout=log, stderr=subprocess.STDOUT, timeout=100)
            contents = (output / f'{name}.log').read_text()
            if result.returncode == 0 or f'test {test} ... FAILED' not in contents or 'could not compile' in contents:
                raise RuntimeError(f'{name}: not a test assertion failure (exit {result.returncode})')
            print(f'NEGATIVE_PASS {name} exit={result.returncode}', flush=True)
        finally:
            path.write_bytes(sources[path])
finally:
    for path, original in sources.items():
        path.write_bytes(original)
        if hashlib.sha256(path.read_bytes()).digest() != hashlib.sha256(original).digest():
            raise RuntimeError(f'restore failed: {path}')
    print('RESTORED source SHA-256 matches originals', flush=True)
