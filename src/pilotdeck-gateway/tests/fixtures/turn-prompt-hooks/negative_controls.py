#!/usr/bin/env python3
"""#741: commit first; all selected controls are accounted for, including harness failures."""
import json
import os
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
# The shared suite owns the guarded mutation scope and all cargo children.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases
from _negative_common import mutate

prompt = root / 'src/pilotdeck-gateway/src/runtime/turn/runner_prompt.rs'
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
oracle = Path(__file__).with_name('oracle.json')
unit = 'runtime::turn::runner::tests::prompts::'
host = 'runtime::project::factory::tests::interaction_host::prompt_hooks::'
cases = [
    ('wiring', live, '.with_prompt_hooks(prompt_hooks);', '.with_prompt_hooks(None);', None, host+'prompt_host_pass', '#741 prompt live context forwarded'),
    ('gate', live, 'if self.preflight < 3 {', 'if self.preflight < 2 {', None, host+'prompt_host_pass', '#741 prompt live submitted'),
    ('execute-context', live, 'let messages = runner.messages().to_vec();', 'let messages = self.input.loop_messages();', None, host+'prompt_host_pass', '#741 prompt live context forwarded'),
    ('abort-context', live, '.map(|runner| runner.messages().to_vec())', '.map(|_| self.input.loop_messages())', None, host+'prompt_host_context_abort', '#741 prompt live context forwarded'),
    ('block-gate', live, 'if let Some(error) = runner.prompt_block_error() {', 'if let Some(error) = runner.prompt_block_error().filter(|_| false) {', None, host+'prompt_host_block', '#741 prompt live execute gate'),
    ('accepted-first', prompt, 'self.require(Phase::Accepted)?;', 'let _ = Phase::Accepted;', None, unit+'before_accept', '#741 prompt requires accepted input'),
    ('submitted-once', prompt, 'if self.prompt_submitted {', 'if false {', None, unit+'pending', '#741 prompt duplicate rejected'),
    ('block-effect', prompt, '.any(|e| matches!(e, HookEffect::Block { .. }))', '.any(|_| false)', None, unit+'block', '#741 prompt loop input'),
    ('hook-context', prompt, 'self.messages.extend(messages.iter().cloned());', 'let _ = messages;', None, unit+'context', '#741 prompt loop input'),
    ('dispatch-reject', prompt, 'result.map_err(|e| RecorderError::State(e.message))?', 'result.unwrap_or_default()', None, unit+'reject', ['#741 prompt rejection', '#741 prompt loop input']),
    ('cancel-pending', prompt, '_ = cancel.cancelled() => LifecycleDispatchResult::default(),', '_ = std::future::pending::<()>() => LifecycleDispatchResult::default(),', None, unit+'abort_pending', '#741 prompt pending bound'),
    ('cancel-signal', prompt, 'signal.abort();', 'let _ = signal;', None, unit+'drop_pending', '#741 prompt signal on cancel only'),
    ('text-coercion', prompt, 'crate::js_value::to_string_units_checked(text)', 'crate::js_value::to_string_units_checked(&JsValue::Null)', None, unit+'blocks_coerce', '#741 prompt events'),
    ('null-block', prompt, 'if matches!(block, JsValue::Null | JsValue::Undefined) {', 'if false {', None, unit+'blocks_null', ['#741 prompt rejection', '#741 prompt loop input']),
    ('regeneration', oracle, '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"', '"sourceCommit": "0000000000000000000000000000000000000000"', None, unit+'regenerates_identically', '#741 prompt fixture regeneration'),
]
if sys.argv[1:] == ['--audit']:
    for _, path, old, new, scope, _, _ in cases:
        mutate(path.read_text(), scope, [(old, new)])
    print(json.dumps([{'mutation': c[0], 'test': c[5], 'assertion': c[6]} for c in cases], indent=2))
    raise SystemExit()
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
    raise SystemExit('caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
names = set(sys.argv[2:])
if names - {c[0] for c in cases}: raise SystemExit('unknown mutation')
chosen = [c for c in cases if not names or c[0] in names]
out = Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=False)
raise SystemExit(run_cases(root, [Case(n,p,o,v,t,a,s) for n,p,o,v,s,t,a in chosen], out, env, mutate, timeout=600, baseline='HEAD'))
