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
from negative_suite import ReceiptRun, judge, passed

SOURCE = ROOT / 'src/pilotdeck-gateway/tests/live_e2e.rs'
BINDINGS = {
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
    for name, (test, assertion) in BINDINGS.items():
        if source.count(f'mutants::on("{name}")') != 1 or f'"{assertion}"' not in source or test not in source:
            raise RuntimeError('#748 stale switch/assertion binding: ' + name)
    if args.check:
        print(f'#748 {len(BINDINGS)} feature switches and assertion bindings: OK')
        return 0
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise RuntimeError('#748 output and caller CARGO_TARGET_DIR required')
    selected = args.selected or list(BINDINGS)
    if len(set(selected)) != len(selected) or any(name not in BINDINGS for name in selected):
        raise RuntimeError('#748 unknown or repeated selection')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    env.pop('PDRS_MUTANT', None)
    env.pop('LIVE_E2E_MUTANT', None)
    env.pop(ReceiptRun.ENV, None)
    env.pop(ReceiptRun.PARENT_ENV, None)
    guarded = [SOURCE, ROOT / 'src/pilotdeck-gateway/src/mutants.rs']
    guarded += [p for p in FIXTURE.rglob('*') if p.is_file() and '__pycache__' not in p.parts]
    guarded += [ROOT / 'src/pilotdeck-gateway/src' / name for name in [
        'host/mod.rs', 'host/execution.rs', 'runtime/session/factory.rs',
        'runtime/turn/live.rs', 'runtime/turn/runner.rs', 'runtime/turn/runner_recording.rs',
        'runtime/turn/host_factory.rs', 'runtime/turn/submit_host.rs', 'runtime/turn/submit.rs']]
    guarded += [FIXTURE.parent / 'negative_suite.py', FIXTURE.parent / 'negative_process.py',
                ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs', ROOT / 'src/pilotdeck-gateway/src/test_process.rs',
                ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs', ROOT / 'src/pilotdeck-gateway/tests/fixtures/_shared/baseline.mts']
    results = []

    def run(label, test, run_env, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (output / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway', '--features', 'pdrs-mutants', '--test', 'live_e2e']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=180)
        (output / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded):
            for name in selected:
                test, assertion = BINDINGS[name]
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
            test, assertion = BINDINGS[name]
            results.append(dict(name=name, test=test, assertion=assertion, status='INVALID', reason=reason))
    counts = {state: sum(row['status'] == state for row in results) for state in ('KILLED','SURVIVED','INVALID','COMPILED')}
    (output / 'summary.json').write_text(json.dumps({'controls':results,'counts':counts}, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
