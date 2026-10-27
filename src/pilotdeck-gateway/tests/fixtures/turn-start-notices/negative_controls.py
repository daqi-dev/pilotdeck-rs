#!/usr/bin/env python3
"""Commit first. Every mutation runs through the shared guarded suite."""
import json
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
# negative_suite owns with mutating_sources, run_owned and wait_for_slot;
# no local backup/restore or alternate baseline is permitted here.
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases
from _negative_common import mutate

runner = root / 'src/pilotdeck-gateway/src/runtime/turn/runner.rs'
live = runner.with_name('live.rs')
events = root / 'src/pilotdeck-gateway/src/runtime/project/interaction_events.rs'
oracle = Path(__file__).with_name('oracle.json')
prefix = 'runtime::turn::runner::tests::notices::'
cases = [
    ('wiring', live, 'let events = handler.ports().events;', 'let events = None;', None,
     'runtime::project::factory::tests::interaction_host::host_next_turn_surfaces_the_pending_hook_discard_notice', '#741 notice live start order'),
    ('notify', runner, 'self.interaction_events = events;', 'self.interaction_events = None; let _ = events;', 'with_interaction_events', prefix+'start_success', '#741 notice start output'),
    ('start-order', runner, 'let mut events = vec![event];', 'let mut events = Vec::new(); let _ = event;', 'start', prefix+'start_success', '#741 notice start output'),
    ('after-durable', runner, 'self.io(|r, s, t| r.start_turn(s, t)).await?;', 'if let Some(port) = &self.interaction_events { let _ = port.host_started(); } self.io(|r, s, t| r.start_turn(s, t)).await?;', 'start', prefix+'start_failure', '#741 notice failed write did not consume'),
    ('once', events, 'std::mem::take(&mut turn.notices)', 'turn.notices.clone()', None, prefix+'both_start_paths_consume_once', '#741 notice both paths once'),
    ('stale', events, 'check(&generation, &turn).map(|()| take_notices(&mut turn))', 'Ok(take_notices(&mut turn))', 'host_started', prefix+'stale', '#741 notice stale rejected'),
    ('closed', events, 'check(&generation, &turn).map(|()| take_notices(&mut turn))', 'Ok(take_notices(&mut turn))', 'host_started', prefix+'closed', '#741 notice closed rejected'),
    ('buffer-isolation', events, 'check(&generation, &turn).map(|()| take_notices(&mut turn))', 'check(&generation, &turn).map(|()| std::mem::take(&mut turn.events))', 'host_started', prefix+'buffered_events_are_not_stolen', '#741 notice unrelated buffer preserved'),
    ('regeneration', oracle, '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"', '"sourceCommit": "0000000000000000000000000000000000000000"', None, prefix+'regenerates_identically', '#741 notice fixture regeneration'),
]

if sys.argv[1:] == ['--audit']:
    for _, path, old, new, scope, _, _ in cases:
        mutate(path.read_text(), scope, [(old, new)])
    print(json.dumps([{'mutation': c[0], 'test': c[5], 'panic': c[6]} for c in cases], indent=2))
    raise SystemExit()
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
    raise SystemExit('caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
chosen = cases[:1] if sys.argv[2:] == ['--sample'] else cases
if sys.argv[2:] and sys.argv[2:] != ['--sample']:
    names = set(sys.argv[2:])
    if names - {c[0] for c in cases}: raise SystemExit('unknown mutation')
    chosen = [c for c in cases if c[0] in names]
out = Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=False)
raise SystemExit(run_cases(root, [Case(n,p,o,v,t,a,s) for n,p,o,v,s,t,a in chosen], out, env, mutate, timeout=600, baseline='HEAD'))
