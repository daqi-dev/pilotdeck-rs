#!/usr/bin/env python3
"""Bounded integrity controls; commit sources first, caller owns the target."""
import json
import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
from negative_process import tree_lock
from negative_suite import Case, bind_module_file, declares_test, run_cases
from _negkit import non_head
from _negative_common import mutate

live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
tests = live.with_name('live_stream_tests.rs')
host = root / 'src/pilotdeck-gateway/src/host/execution.rs'
oracle = fixtures / 'turn-runner/oracle.json'
# name, source, before, after, function scope, full test name, assertion anchor
cases = [
    ('loop-prefix', live,
     'wire && *state == IntegrityState::Initial', 'false && wire && *state == IntegrityState::Initial',
     'observe_source', 'runtime::turn::live::stream_tests::real_ts_sidecar_stream_starts_with_loop_events', 'real sidecar full host sequence'),
    ('host-not-loop', live,
     'wire && *state == IntegrityState::Initial', '*state == IntegrityState::Initial',
     'observe_source', 'runtime::turn::live::tests::host_lifecycle_cannot_justify_a_sidecar_final', 'host status is not loop evidence'),
    # #996-pr2 fix-r5: the migrated leak/prompt switches made the closure a
    # multi-line if/else chain, which the scoped mutate() single-line rule
    # rejects. Guard the whole any() head instead; a false predicate is the
    # same mutant as any(|_| false) (host lifecycle kinds never match).
    ('host-lifecycle-leak', live,
     'if super::runner::host_lifecycle_events().iter().any(|kind| {', 'if false && super::runner::host_lifecycle_events().iter().any(|kind| {',
     'observe_source', 'runtime::turn::live::tests::host_lifecycle_cannot_justify_a_sidecar_final', 'host lifecycle leak must be rejected'),
    ('host-failure-not-loop', live,
     'current == IntegrityState::Failed && *self.wire_events.lock().unwrap() > 0', 'current == IntegrityState::Failed',
     'observe_terminal', 'runtime::turn::live::tests::host_lifecycle_cannot_justify_a_sidecar_final', 'host failure is not loop evidence'),
    ('early-final', live,
     'Some("success") => current == IntegrityState::Running,', 'Some("success") => current != IntegrityState::Failed,',
     'observe_terminal', 'runtime::turn::live::stream_tests::real_ts_sidecar_early_final', '#748 early-final lifecycle rejection reason'),
    ('delivered-suffix', live,
     'if !outcome.is_some_and', 'if false && !outcome.is_some_and',
     'finish', 'runtime::turn::live::stream_tests::slow_consumer_with_a_blocked_callback_still_exits_bounded', 'integrity failure terminal must be error'),
    ('completed-leak', live,
     'if event_type.equals_str("turn_completed") {', 'if false && event_type.equals_str("turn_completed") {',
     'observe_source', 'runtime::turn::live::stream_tests::forwarding::forward_completed', '#741 pr5d leaked completed must be rejected'),
    ('failure-sticky', live,
     'wire && *state == IntegrityState::Initial', 'wire && *state != IntegrityState::Completed',
     'observe_source', 'runtime::turn::live::stream_tests::forwarding::forward_event_warning', 'forward event order/payload'),
    ('wire-completed-suppression', host,
     'if !event.final_event && text_is(event.payload.get("type"), "turn_completed") { continue; }', 'if false && !event.final_event && text_is(event.payload.get("type"), "turn_completed") { continue; }',
     None, 'runtime::turn::live::stream_tests::real_ts_sidecar_completed_warning', 'real sidecar full host sequence'),
    ('sequence-duplicate', host,
     'if event.sequence.value() != next_sequence {', 'if false && event.sequence.value() != next_sequence {',
     None, 'runtime::turn::live::stream_tests::real_ts_sidecar_duplicate', 'integrity rejection catch order'),
    ('sequence-gap', host,
     'if event.sequence.value() != next_sequence {', 'if false && event.sequence.value() != next_sequence {',
     None, 'runtime::turn::live::stream_tests::real_ts_sidecar_gap', 'integrity rejection catch order'),
    ('golden-regeneration', oracle,
     '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"', '"sourceCommit": "0000000000000000000000000000000000000000"',
     None, 'runtime::turn::live::stream_tests::stream_integrity_regenerates_identically', 'turn-runner/oracle.json differs'),
]


def audit():
    # #1406 review: bind each bound test to the one file that declares its
    # module. Concatenating the three bodies would accept a same-named short
    # function from another module. Longest prefix first: forwarding:: lives
    # inside stream_tests::.
    modules = (('runtime::turn::live::stream_tests::forwarding::', live.with_name('forward_tests.rs'), 'case!'),
               ('runtime::turn::live::stream_tests::', tests, None),
               ('runtime::turn::live::tests::', live, None))
    # #2795 fix-r3（sibling P1 同族跟改，manager-gateway fix-r2 先例）：锚点核对
    # 也要持树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能把变异体
    # 留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败是拒跑（2），
    # 不是宿主会读成 rc 1 的裸 traceback。audit 是 --audit 独立模式，
    # 与下方变异轮无同进程嵌套。
    try:
        with tree_lock(root, timeout=60):
            sources = {path: path.read_text() for _, path, _ in modules}
            macros = {path: macro for _, path, macro in modules}
            for name, path, old, new, scope, test, panic in cases:
                mutate(path.read_text(), scope, [(old, new)])
                # #1890: one shared binding rule (this copy required the spaced
                # `fn name(` spelling and carried the macro form inline).
                file, short = bind_module_file(test, [(prefix, source) for prefix, source, _ in modules])
                if not declares_test(sources[file], short, macros[file]):
                    raise ValueError('bound test not found: ' + test)
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        print(f'--audit tree lock or read failed: '
              f'{type(error).__name__}: {error}', file=sys.stderr)
        raise SystemExit(2)
    return [{'mutation': n, 'test': t, 'panic': a} for n, _, _, _, _, t, a in cases]


if sys.argv[1:] == ['--audit']:
    print(json.dumps(audit(), indent=2))
    raise SystemExit()
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by caller')
if not env.get('PDRS_BASELINE'):
    raise SystemExit('PDRS_BASELINE must be supplied by caller')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
           CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
chosen = cases[:1] if sys.argv[2:] == ['--sample'] else cases
if sys.argv[2:] and sys.argv[2:] != ['--sample']:
    names = set(sys.argv[2:])
    unknown = names - {row[0] for row in cases}
    if unknown:
        raise SystemExit(f'unknown controls: {sorted(unknown)}')
    chosen = [row for row in cases if row[0] in names]
out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=False)
# #1540: a green run against a pinned non-HEAD baseline is NON-HOST EVIDENCE
# (the host gate only trusts HEAD), so it exits 42 instead of 0.
baseline_source, baseline, baseline_sha = non_head.gate(root)
raise SystemExit(non_head.non_head_exit(
    baseline_source, baseline_sha,
    code=run_cases(root,
        [Case(name, path, old, new, test, panic, scope)
         for name, path, old, new, scope, test, panic in chosen],
        out, env, mutate, timeout=600, baseline=baseline),
    evidence=out))
