#!/usr/bin/env python3
# anchor-selfcheck: unsupported feature-gated controls, no textual source replacement
"""#748 HEAD-only controls. --check validates every switch and bound assertion.

No source bytes are changed: pdrs-mutants gates the integration crate's switches.
The source guard still requires all guarded inputs to equal HEAD. Run this after
committing, with an explicit CARGO_TARGET_DIR and a fresh output directory.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[5]
FIXTURE = Path(__file__).resolve().parent
sys.path.insert(0, str(FIXTURE.parent))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, declares_test, judge, passed
from negative_source import assertion_marker_present

SOURCE = ROOT / 'src/pilotdeck-gateway/tests/live_e2e.rs'
BINDINGS = {
    'live-derived-run-count': ('live_early_final', '#748 early-final run identity replacement count'),
    'live-derived-error-count': ('live_early_final', '#748 early-final error replacement count'),
    'live-derived-event-shape': ('live_early_final', '#748 early-final explicit error event structure'),
    'live-derived-transcript-shape': ('live_early_final', '#748 early-final explicit error transcript structure'),
    'live-event-payload': ('live_text', '#748 GatewayEvent payload bytes'),
    'live-client-payload': ('live_ts_client_payload', '#748 TS client payload bytes'),
    'live-metadata': ('live_text', '#748 native session metadata payloads'),
    'live-transcript': ('live_text', '#748 durable transcript payloads'),
    'live-time-shape': ('live_text', '#748 raw timestamp shape'),
    'live-owner-pid': ('live_text', '#748 raw owner pid'),
    'live-fingerprint': ('live_text', '#748 original request fingerprint'),
    'live-registry': ('live_text', '#748 registry terminal record'),
    'live-cancel': ('live_cancel_stream', '#748 provider interrupted'),
    'live-injection': ('live_drop', '#748 injection executed'),
    'live-reap': ('live_text', '#748 child group reaped'),
    'live-provenance-list': ('live_artifact_provenance', '#748 exact provenance inventory'),
    'live-mitm-hash': ('live_artifact_provenance', '#748 fixed apparatus hash'),
    'live-golden-hash': ('live_artifact_provenance', '#748 golden byte hash'),
}

LIB_BINDINGS = {
    'live-terminal-case-inventory': ('runtime::turn::live::tests::terminal_message_boundaries_match_ts', '#748 exact terminal boundary inventory', 'live.rs', 'live.rs'),
    'live-terminal-falsy-final': ('runtime::turn::live::tests::terminal_message_boundaries_match_ts', '#748 TS terminal message boundary', 'live.rs', 'live.rs'),
    'live-diagnostic-message': ('host::tests::close_drains_stderr_exit_tail_before_aborting_pump', '#748 stderr stays in bounded diagnostic field', '../../host/mod.rs', '../../host/tests.rs'),
    'live-message-field-classification': ('runtime::turn::live::stream_tests::progress_message_text_is_not_a_canonical_message', '#748 progress message text bypasses canonical classification', 'live.rs', 'live_stream_tests.rs'),
    'live-context-detached': ('runtime::turn::submit::tests::context_budget_delayed_write_precedes_terminal_and_eof', '#748 context status must settle before native terminal', 'submit.rs', 'submit_tests.rs'),
    'live-context-unbounded': ('runtime::turn::submit::tests::pending_context_budget_status_cannot_hold_turn_completion', '#748 context status wait ends at abort budget', 'submit.rs', 'submit_tests.rs'),
    'live-run-mode-ask': ('runtime::turn::submit_host::tests::prepare_preserves_ask_run_mode', '#748 explicit ask run mode survives normalization', 'submit_host.rs', 'submit_host_tests.rs'),
    'live-early-final-witness': ('runtime::turn::live::stream_tests::success_final_requires_loop_completion_witness', '#748 success final requires loop completion witness', 'live.rs', 'live_stream_tests.rs'),
    'live-malformed-durable': ('runtime::turn::live::stream_tests::bound_malformed_durable_messages_fail_before_yield_and_persist_terminal', '#748 malformed message never yielded', 'live.rs', 'live_stream_tests.rs'),
    'live-durable-before-yield': ('runtime::turn::live::stream_tests::wire_durable_messages_match_ts_yield_before_callback', '#748 TS durable message is yielded before callback', 'live.rs', 'live_stream_tests.rs'),
}
ALL_BINDINGS = {**BINDINGS, **{name: (row[0], row[1]) for name, row in LIB_BINDINGS.items()}}
# 纯文本绑定：没有 mutants::on 开关可挂，靠 capture.py 与 test_acceptance.py
# 双向锚定，任一侧的那段绑定被单独删掉都响亮失败。
CAPTURE_GUARD = '\n    if isinstance(sys.exception(), RuntimeError):\n        return\n'
CAPTURE_FINALLY = '\n    finally:\n        if process is not None:\n            reclaim(process, output)\n'
TEXT_BINDINGS = {
    'capture-second-signal': ('capture.py', CAPTURE_GUARD, 'test_acceptance.py', "'capture-secondary-signal'"),
    'capture-reclaim-in-finally': ('capture.py', CAPTURE_FINALLY, 'test_acceptance.py', 'def test_capture_second_signal_preserves_cleanup'),
}
# 清单自己也要有分母：整张表被删掉时循环空转、`+ 0 text bindings: OK` 会假绿。
EXPECTED_TEXT_BINDINGS = 2
TURN = ROOT / 'src/pilotdeck-gateway/src/runtime/turn'
# #1885: `negative_process.mutating_sources` rewrites every guarded path on exit
# with a plain `write_bytes` and no atomic publish, so a SIGKILL inside that
# window truncates whatever the set carries. These controls change no source
# bytes at all, so the set must stay empty: the old whole-fixture glob put every
# golden (`oracle.json`, `expected/*.json`, `capture.py`) at that risk for
# nothing. The workspace-equals-HEAD proof is unaffected -- `mutating_sources`
# runs it over the whole `src/` input tree regardless of this list.
RESTORE_SET = ()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*', choices=None)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    args = parser.parse_args()
    source = SOURCE.read_text()
    declared = set(re.findall(r'mutants::on\("([^"]+)"\)', source))
    if declared != set(BINDINGS):
        raise RuntimeError(f'#748 stale switch bindings: source={declared}, runner={set(BINDINGS)}')
    # #2303 (条 4): every binding is audited before the refusal, and a
    # scanner refusal (ValueError) becomes one named INVALID record per row
    # instead of a raw traceback on the first failure -- the session-factory
    # / session-drop-release audit shape.
    invalid = []
    for name, (test, assertion) in BINDINGS.items():
        try:
            # #2303 (条 5): the assertion marker must live in code, never in
            # a comment -- the raw-text scan let a commented-out copy of the
            # assertion satisfy the binding after the real one was deleted.
            # #2303 (条 17): the bound cell must be declared, not merely
            # mentioned -- `cell!` rows here, plain fns for the last two.
            if (source.count(f'mutants::on("{name}")') != 1
                    or not assertion_marker_present(source, f'"{assertion}"')
                    or not declares_test(source, test, 'cell!')):
                raise ValueError('stale switch/assertion binding')
        except Exception as error:
            invalid.append(f'{name}: {type(error).__name__}: {error}')
    for name, (test, assertion, production, tests) in LIB_BINDINGS.items():
        try:
            if (TURN / production).read_text().count(f'mutants::on("{name}")') != 1:
                raise ValueError('stale production switch')
            text = (TURN / tests).read_text()
            # #2303 (条 17): the old short-name "occurs anywhere" scan accepted
            # a noise carrier (a comment, an unrelated mention); the shared
            # declaration rule requires the fn form in the bound module file.
            # #2303 (条 5): the assertion marker must live in code, never in
            # a comment.
            if (not declares_test(text, test.rsplit('::', 1)[1])
                    or not assertion_marker_present(text, f'"{assertion}"')):
                raise ValueError('stale production test binding')
        except Exception as error:
            invalid.append(f'{name}: {type(error).__name__}: {error}')
    if invalid:
        raise RuntimeError('#748 binding audit INVALID rows: ' + '; '.join(invalid))
    # #1194/#1398: the feature switches above cannot see a plain-Python guard,
    # so bind it by text here. Behaviour is proven by
    # test_acceptance.py::test_capture_second_signal_preserves_cleanup.
    if len(TEXT_BINDINGS) != EXPECTED_TEXT_BINDINGS:
        raise RuntimeError(f'#748 text binding inventory drifted: {len(TEXT_BINDINGS)} != {EXPECTED_TEXT_BINDINGS}')
    for name, (source_file, anchor, probe_file, probe) in TEXT_BINDINGS.items():
        if (FIXTURE / source_file).read_text().count(anchor) != 1:
            raise RuntimeError(f'#748 {name}: guard anchor missing or duplicated in {source_file}')
        if (FIXTURE / probe_file).read_text().count(probe) != 1:
            raise RuntimeError(f'#748 {name}: binding probe missing or duplicated in {probe_file}')
    if args.check:
        # #1885: prove-failable pin. Re-broadening the restore set to the
        # fixture tree (or to any product path) puts files this runner never
        # writes back into the write-back window; the check is on the constant
        # the run actually uses, so restoring the old list turns this red.
        if RESTORE_SET:
            raise RuntimeError(
                '#1885 feature switches rewrite no bytes; restore set must stay empty: '
                + ', '.join(str(path) for path in RESTORE_SET))
        print(f'#748 {len(ALL_BINDINGS)} feature switches and assertion bindings + {len(TEXT_BINDINGS)} text bindings: OK')
        return 0
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('#748 output and caller CARGO_TARGET_DIR required')
    selected = args.selected or list(ALL_BINDINGS)
    if len(set(selected)) != len(selected) or any(name not in ALL_BINDINGS for name in selected):
        raise RuntimeError('#748 unknown or repeated selection')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    env.pop('PDRS_MUTANT', None)
    env.pop('LIVE_E2E_MUTANT', None)
    env.pop(ReceiptRun.ENV, None)
    env.pop(ReceiptRun.PARENT_ENV, None)
    # #1885: this process holds the exclusive tree lock for the whole run, so a
    # child asking for the shared read lease would block on its own parent until
    # the 180 s harness timeout. The runner is the writer for the bytes its
    # children read, which is exactly what the bypass asserts.
    env['PDRS_NEGATIVE_LEASE_HELD_ROOT'] = str(ROOT)
    # #1885: these controls change no source bytes -- `pdrs-mutants` gates the
    # switches at compile time -- so the restore set is empty and the goldens
    # (`oracle.json`, `expected/*.json`, `capture.py`) never enter the
    # write-back window. `mutating_sources` still takes the tree lock and still
    # proves the workspace inputs equal HEAD; whichever file a SIGKILL had
    # truncated is not one this runner ever wrote.
    guarded = list(RESTORE_SET)
    results = []

    def run(label, test, run_env, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (output / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway', '--features', 'pdrs-mutants']
        command += ['--lib'] if '::' in test else ['--test', 'live_e2e']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=180)
        (output / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded):
            for name in selected:
                test, assertion = ALL_BINDINGS[name]
                item = dict(name=name, test=test, assertion=assertion, status='INVALID', reason='not run')
                try:
                    changed = dict(env, PDRS_MUTANT=name)
                    compiled = run(name + '-compile', test, changed, True)
                    if compiled.returncode:
                        raise RuntimeError('mutant did not compile; not a runtime kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='all feature branches compiled')
                    else:
                        if not passed(run(name + '-positive', test, env), test):
                            raise RuntimeError('positive bound test failed')
                        receipt = ReceiptRun(output, name, [(test, assertion)])
                        result = run(name + '-negative', test, receipt.environment(changed))
                        killed, reason, hits = judge(result, '', [(test, assertion)], receipt)
                        item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'), reason=reason, hits=hits)
                        if not passed(run(name + '-restored', test, env), test):
                            raise RuntimeError('restored bound test failed')
                        item['restoredPass'] = True
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                results.append(item)
                print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except Exception as error:
        # #979: loss of the source/restoration guard invalidates prior successes.
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name in selected[len(results):]:
            test, assertion = ALL_BINDINGS[name]
            results.append(dict(name=name, test=test, assertion=assertion, status='INVALID', reason=reason))
    counts = {state: sum(row['status'] == state for row in results) for state in ('KILLED','SURVIVED','INVALID','COMPILED')}
    (output / 'summary.json').write_text(json.dumps({'controls':results,'counts':counts}, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
