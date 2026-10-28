#!/usr/bin/env python3
# anchor-selfcheck: unsupported two-field rows with name-dependent ordered replacements on intermediate text in submit
"""Runtime counterexamples for cancellation before/during host creation."""
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases
from _negative_common import mutate, check_mutations

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
lazy = 'host = async { host_factory(request.identity.clone(), recorder.clone(), signal).await } => host?,'
# pr5f added a second select in submit. Bind the whole factory select exactly
# once; neither the client gate nor its cancellation priority is mutated.
factory_select = """                let (client, inner, lease, manifest, interaction_events, prompt_hooks, lifecycle) = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        return Ok(ExecuteResult::Aborted { terminal: None });
                    }
                    // select constructs every branch before polling; keep the
                    // factory call lazy so a pre-cancel never creates resources.
                    host = async { host_factory(request.identity.clone(), recorder.clone(), signal).await } => host?,
                };"""
mutations = [
    ('eager-factory', lazy, 'host = host_factory(request.identity.clone(), recorder.clone(), signal) => host?,'),
    ('factory-before-cancel', factory_select,
     factory_select.replace(lazy, '').replace('biased;', 'biased; ' + lazy)),
    ('ignore-factory-cancel', factory_select,
     factory_select.replace('_ = cancellation.cancelled() =>', '_ = std::future::pending::<()>() =>')),
]
if sys.argv[1:] == ['--check']:
    raise SystemExit(check_mutations((n, source, None, [(o, v)]) for n, o, v in mutations))
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
names = set(sys.argv[2:])
if names - {m[0] for m in mutations}:
    raise SystemExit('unknown mutation')
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
test = 'runtime::turn::live::stream_tests::live_factory_pre_aborted'
cases = [Case(n, source, o, v, test,
              ['#741k pre-cancel factory must return aborted', '#741k factory call must be lazy'])
         for n, o, v in mutations if not names or n in names]
raise SystemExit(run_cases(root, cases, out, env, mutate,
                           baseline=env.get('PDRS_NEGATIVE_BASELINE', 'HEAD')))
