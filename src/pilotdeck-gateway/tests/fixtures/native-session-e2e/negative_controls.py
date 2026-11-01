#!/usr/bin/env python3
"""#1140 e2e mutations: guarded source runs or read-only memory rehearsals."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import json
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import re
import unittest
from unittest.mock import patch

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_process import target_environment

session = root / 'scripts/gateway-parity/session_domain.py'
common = root / 'scripts/gateway-parity/common.py'
native = root / 'scripts/gateway-parity/native_evidence.py'
collector = root / 'scripts/native-witness/collect.py'
fixture = Path(__file__).with_name('test_e2e.py')
# Each row mutates one #1140 fix-r1 addition and names the e2e test whose
# dedicated assertion message must fire (the named FAIL the classifier pins):
# the scoring-side shapes in session_domain.py and the recorder/contract-side
# shapes in collect.py, so neither the collector's output nor the frozen audit
# can silently drift away from the P1 recorder contract.
cases = [
    ('decoder-frame-for-send-rows', session, "        require(len(decoded) == 1\n                and decoded[0].get('raw' if row['direction'] == 'send' else 'frame') == row['frame'],\n                'OBSERVER_WIRE', 'wire observation disagrees with recorder')\n", "        require(len(decoded) == 1\n                and decoded[0].get('frame') == row['frame'],\n                'OBSERVER_WIRE', 'wire observation disagrees with recorder')\n", 'test_e2e_scores_real_capture'),
    ('drop-required-provider-score', session, "        if case in required and case not in covered:\n            surfaces.append('provider.requests')\n", '        pass\n', 'test_e2e_data_control_empty_ledger'),
    ('record-state-batch-crash', session, "        projected = {'applicable': True, 'events': events, 'state': record.get('state'),\n", "        projected = {'applicable': True, 'events': events, 'state': record['state'],\n", 'test_e2e_data_control_missing_record_state'),
    ('executed-includes-not-run', session, "            cell['executedCases'] = [row['case'] for row in native['verdicts']\n                                     if row['status'] != 'NOT_RUN']\n", "            cell['executedCases'] = [row['case'] for row in native['verdicts']]\n", 'test_e2e_data_control_observation_null'),
    ('skip-structure-type-check', session, "    if not (isinstance(observation['frames'], list)\n            and all(isinstance(event, dict) and isinstance(event.get('type'), str)\n                    for event in observation['frames'])):\n        return 'frames'\n", "    if not isinstance(observation['frames'], list):\n        return 'frames'\n", 'test_e2e_data_control_frames_without_type'),
    ('hellos-unfiltered', session, "    hellos = [r['frame'] for r in observer['rows']\n              if isinstance(r['frame'], dict) and r['frame'].get('type') == 'hello_ok']\n", "    hellos = [r['frame'] for r in observer['rows'] if r['frame'].get('type') == 'hello_ok']\n", 'test_e2e_scores_real_capture'),
    ('drop-wire-shape-require', session, "        require(isinstance(row.get('wire'), dict), 'OBSERVER_WIRE',\n                'observer rows must carry wire metadata '\n                '(opcode/fin/rsv/masked/lengthCode/payloadLength)')\n        wire_frame(row)\n", '        wire_frame(row)\n', 'test_e2e_data_control_row_without_wire'),
    ('send-frame-as-object', collector, "                          'frame': payload.decode(), 'connection': 1})\n", "                          'frame': json.loads(payload), 'connection': 1})\n", 'test_e2e_capture_schema'),
    ('drop-row-wire-metadata', collector, "        self.rows.append({'direction': 'send', 'wireHex': frame.hex(),\n                          'wire': {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': True,\n                                   'lengthCode': 126 if 126 <= size <= 65535 else 127 if size > 65535 else size,\n                                   'payloadLength': size},\n                          'frame': payload.decode(), 'connection': 1})\n", "        self.rows.append({'direction': 'send', 'wireHex': frame.hex(),\n                          'frame': payload.decode(), 'connection': 1})\n", 'test_e2e_capture_schema'),
    ('drop-contract-connections', collector, "                    'externalEndpoints': [f'127.0.0.1:{port + 1}'],\n                    'connections': 1,\n", "                    'externalEndpoints': [f'127.0.0.1:{port + 1}'],\n", 'test_collector_profile_connections'),
    ('hardcode-registry-applicable', native, "'applicable': bool(events or record)", "'applicable': True", 'test_e2e_scores_real_capture'),
    ('observation-key-batch-crash', session, "    observed = {row['case']: row.get('observation') for row in rows}\n", "    observed = {row['case']: row['observation'] for row in rows}\n", 'test_e2e_data_control_observation_missing'),
    ('drop-driver-binding-crosscheck', session, "    require(isinstance(driver, dict) and driver.get('binding') == contract['binding'], 'BINDING',\n            'driver-recorded hello binding disagrees with the observer hello record')\n", '    pass\n', 'test_e2e_data_control_forged_binding_value'),
    ('drop-driver-binding-record', collector, "    w.save(staging / 'driver.json', driver)", "    w.save(staging / 'driver.json', dict(driver, binding=None))", 'test_e2e_capture_schema'),
    ('shrink-process-roles', collector, "                    'processRoles': (['host'] + ['sidecar'] * len(cases)) if session_domain else ['host', 'sidecar'],\n", "                    'processRoles': ['host', 'sidecar'],\n", 'test_collector_profile_roles'),
    ('drop-window-attribution', native, 'need(windows == [0] + [1] * len(cases),', 'need(True,', 'test_e2e_data_control_sidecar_misallocated'),
    ('drop-unwitnessed-sources', collector, '                        sources.append(source)', "                        if row['stimulus']['status'] == 'WITNESSED':\n                            sources.append(source)", 'test_unwitnessed_stimuli_are_not_executed'),
    ('drop-unwitnessed-transcript-path', collector, '                        sources.append(source)', "                        sources.append(dict(source, transcriptPath='') if row['stimulus']['status'] != 'WITNESSED' else source)", 'test_unwitnessed_stimuli_are_not_executed'),
    ('unwitnessed-counts-executed', session, "            cell['executedCases'] = [row['case'] for row in native['verdicts']\n                                     if row['status'] != 'NOT_RUN']\n", "            cell['executedCases'] = [row['case'] for row in native['verdicts']]\n", 'test_unwitnessed_stimuli_are_not_executed'),
    ('drop-unwitnessed-candidate', session, "            cell['cases'] = native['verdicts']", "            cell['cases'] = [] if all(row['status'] == 'NOT_RUN' for row in native['verdicts']) else native['verdicts']", 'test_unwitnessed_stimuli_are_not_executed'),
]
messages = {
    'decoder-frame-for-send-rows': '#1140 e2e real capture scores 72/72 per-case PASS',
    'drop-required-provider-score': '#1140 r10 empty provider ledger fails required cases without shrinking denominator',
    'record-state-batch-crash': '#1140 e2e a missing run-record state is a per-case FAIL',
    'executed-includes-not-run': '#1140 e2e a NOT_RUN case is not executed',
    'skip-structure-type-check': '#1140 e2e frames without a type are a structure rejection',
    'hellos-unfiltered': '#1140 e2e real capture scores 72/72 per-case PASS',
    'drop-wire-shape-require': '#1140 e2e a recorder row without wire is rejected',
    'send-frame-as-object': '#1140 e2e sent rows record the payload text',
    'drop-row-wire-metadata': '#1140 e2e recorder rows carry wire metadata',
    'drop-contract-connections': '#1140 session profile connections',
    'hardcode-registry-applicable': '#1140 e2e real capture scores 72/72 per-case PASS',
    'observation-key-batch-crash': '#1140 e2e a row without the observation key scores NOT_RUN, not a batch KeyError',
    'drop-driver-binding-crosscheck': '#1140 e2e a forged hello binding value is rejected on BINDING',
    'drop-driver-binding-record': '#1140 e2e the driver hello record matches the observer hello binding',
    'shrink-process-roles': '#1140 session profile roles',
    'drop-window-attribution': '#1140 e2e a balanced sidecar misallocation is rejected on NATIVE_PROCESS',
    'drop-unwitnessed-sources': '#1368 NOT_RUN retains all raw originals and missing target reasons',
    'drop-unwitnessed-transcript-path': '#1368 NOT_RUN retains all raw originals and missing target reasons',
    'unwitnessed-counts-executed': '#1368 unwitnessed stimuli never count as executed',
    'drop-unwitnessed-candidate': '#1368 NOT_RUN retains exactly one native candidate',
}


def bound(test):
    classname = 'LifecycleTests' if test.startswith('test_collector_profile_') else 'E2ETests'
    return classname + '.' + test


def preflight():
    # Read-only: this never executes a mutant or bypasses the HEAD guard.
    errors = []
    for name, path, before, after, _ in cases:
        text = path.read_text()
        if text.count(before) != 1:
            errors.append(f'STALE ANCHOR {name}: {text.count(before)} matches')
        else:
            try:
                compile(text.replace(before, after, 1), str(path), 'exec')
            except SyntaxError as error:
                errors.append(f'INVALID compile {name}: {error}')
    if errors:
        print('\n'.join(errors), file=sys.stderr)
        raise SystemExit(2)
    print(f'#1140 fix-r1..fix-r3 {len(cases)} unique anchors; every Python mutant compiles')


def passed(result):
    log = result.stdout + result.stderr
    return result.returncode == 0 and re.search(r'^Ran 1 test in ', log, re.M) is not None and re.search(r'^OK$', log, re.M) is not None


def classify(result, test, marker):
    log = result.stdout + result.stderr
    if passed(result):
        return 'SURVIVED'
    if (result.returncode == 1 and re.search(r'^FAIL: ' + re.escape(test)
            + r' \((?:__main__|bound_tests)\.' + re.escape(bound(test).split('.')[0]) + r'(?:\.' + re.escape(test) + r')?\)$', log, re.M)
            and re.search(r'^AssertionError:[^\n]*' + re.escape(marker) + r'[^\n]*$', log, re.M)
            and re.search(r'^Ran 1 test in ', log, re.M) and 'FAILED (failures=1)' in log):
        return 'KILLED'
    return 'INVALID'


def memory_run(name):
    """Rehearse Python-only mutations without writing dirty/new sources."""
    _, path, before, after, test = next(row for row in cases if row[0] == name)
    changed = path.read_bytes().replace(before.encode(), after.encode(), 1)
    compile(changed, str(path), 'exec')
    original_read = Path.read_bytes
    original_code = importlib.machinery.SourceFileLoader.get_code
    def read(p):
        return changed if p.resolve() == path else original_read(p)
    def code(loader, fullname):
        return compile(changed, str(path), 'exec') if Path(loader.path).resolve() == path else original_code(loader, fullname)
    with patch.object(Path, 'read_bytes', read), patch.object(importlib.machinery.SourceFileLoader, 'get_code', code):
        spec = importlib.util.spec_from_file_location('bound_tests', fixture)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        suite = unittest.defaultTestLoader.loadTestsFromName(bound(test), module)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def execution_environment():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by caller')
    return target_environment(root)


def main():
    if sys.argv[1:2] == ['--memory-run']:
        return memory_run(sys.argv[2])
    preflight()
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    output, baseline, only, memory = None, 'HEAD', None, False
    arguments = sys.argv[1:]
    position = 0
    while position < len(arguments):
        argument = arguments[position]
        if output is None and not argument.startswith('--'):
            output = argument
        elif argument == '--case' and position + 1 < len(arguments):
            only = arguments[position + 1]
            position += 1
        elif argument == '--baseline' and position + 1 < len(arguments):
            baseline = arguments[position + 1]
            position += 1
        elif argument == '--memory':
            memory = True
        else:
            raise SystemExit('usage: negative_controls.py NEW_OUTPUT [--case NAME] '
                             '[--baseline TREE] [--memory] | --check-anchors')
        position += 1
    if output is None:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT [--case NAME] '
                         '[--baseline TREE] [--memory] | --check-anchors')
    env = execution_environment()
    selected = cases if only is None else [row for row in cases if row[0] == only]
    if not selected:
        raise SystemExit('unknown negative case')
    out = Path(output).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []
    def run(name, test, mutant=None):
        # The bound test executes Python; the host Rust preflight below is
        # recorded separately for each mutation as required by the taskbook.
        command = ([sys.executable, '-I', __file__, '--memory-run', mutant] if mutant else
                   [sys.executable, '-I', str(fixture), bound(test)])
        result = run_owned(command,
                            cwd=root, env=env, timeout=300)
        (out / (name + '.log')).write_text(result.stdout + result.stderr)
        return result
    def one(row):
        name, path, before, after, test = row
        status, reason, compiled = 'INVALID', '', False
        try:
            text = path.read_text()
            if text.count(before) != 1:
                raise ValueError('STALE ANCHOR ' + name)
            changed = text.replace(before, after, 1)
            compile(changed, str(path), 'exec')
            if not passed(run(name + '-positive', test)):
                raise ValueError('positive bound test did not pass')
            if not memory:
                path.write_text(changed)
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
            (out / (name + '-slots.log')).write_text(slot.stdout + slot.stderr)
            build = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                               '-p', 'pilotdeck-gateway', '--test', 'native_session_e2e', '--no-run'],
                              cwd=root, env=env, timeout=900)
            (out / (name + '-compile.log')).write_text(build.stdout + build.stderr)
            if build.returncode != 0:
                raise ValueError('no-run failed')
            compiled = True
            status = classify(run(name + '-negative', test, name if memory else None), test, messages[name])
            reason = {'KILLED': 'named assertion matched', 'SURVIVED': 'bound test passed',
                      'INVALID': 'exit/test identity/assertion marker did not match'}[status]
        except Exception as error:
            status, reason = 'INVALID', str(error)
        finally:
            if not memory:
                path.write_bytes(originals[path])
        try:
            if not passed(run(name + '-restored', test)):
                status, reason = 'INVALID', reason + '; restored test failed'
        except Exception as error:
            status, reason = 'INVALID', reason + '; restore: ' + str(error)
        results.append({'name': name, 'status': status, 'compiled': compiled, 'reason': reason,
                        'test': bound(test), 'marker': messages[name]})
    try:
        if memory:
            for row in selected:
                one(row)
        else:
            paths = [session, common, native, collector, fixture, Path(__file__).resolve()]
            with mutating_sources(root, paths, baseline=baseline) as originals:
                for row in selected:
                    one(row)
    except Exception as error:
        for row in results:
            row.update(status='INVALID', compiled=False, reason='guard failure: ' + str(error))
        if not results:
            results = [{'name': name, 'status': 'INVALID', 'compiled': False, 'reason': 'guard refusal: ' + str(error)} for name, *_ in selected]
    # Publish only after mutating_sources has verified restoration.
    temporary = out / 'summary.json.tmp'
    summary = {'mode': 'memory-only' if memory else 'guarded-source', 'controls': results}
    temporary.write_text(json.dumps(summary, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(summary, indent=2))
    return 0 if len(results) == len(selected) and all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
