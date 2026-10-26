#!/usr/bin/env python3
"""Runtime counterexamples for cancellation before/during host creation."""
import json
import os
from functools import partial
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from _negative_common import mutate, run

root = Path(__file__).resolve().parents[5]
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--lib', 'runtime::turn::live::stream_tests::live_']
run = partial(run, root=root, out=out, command=command, env=env)
lazy = 'host = async { host_factory(request.identity.clone(), recorder.clone()).await } => host?,'
mutations = [
    ('eager-factory', [(lazy, 'host = host_factory(request.identity.clone(), recorder.clone()) => host?,')]),
    ('factory-before-cancel', [('biased;', 'biased; ' + lazy), (lazy, '')]),
    ('ignore-factory-cancel', [('_ = cancellation.cancelled() =>', '_ = std::future::pending::<()>() =>')]),
]
# Priority mutation needs one simultaneous replacement, not a second anchor
# search after inserting an identical line. All anchors are unique initially.
with mutating_sources(root, [source], baseline=env.get('PDRS_NEGATIVE_BASELINE', 'HEAD')) as originals:
    original = originals[source].decode()
    positive = run('positive')
    assert positive.returncode == 0
    for name in ['live_factory_pre_aborted', 'live_factory_pre_disposed',
                 'live_interaction_factory_pre_aborted', 'live_interaction_factory_pre_disposed',
                 'live_factory_pending_abort', 'live_factory_pending_dispose',
                 'live_factory_pending_drop', 'live_factory_cancelled_waiter_resumes']:
        assert f'::{name} ... ok' in positive.stdout, name
    for name, replacements in mutations:
        if name == 'factory-before-cancel':
            changed = mutate(original, 'submit', [(lazy, '')])
            changed = mutate(changed, 'submit', [('biased;', 'biased; ' + lazy)])
        else:
            changed = mutate(original, 'submit', replacements)
        source.write_text(changed)
        result = run(name)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        assert 'panicked at' in result.stdout + result.stderr, name
        assert '::live_factory_pre_aborted ... FAILED' in result.stdout, name
        source.write_bytes(originals[source])
        print(f'{name}: runtime RED; source restored', flush=True)
    assert run('restored').returncode == 0
(out / 'summary.json').write_text(json.dumps({'mutations':len(mutations),'runtimeFailures':len(mutations),'restoredPass':True},indent=2)+'\n')
