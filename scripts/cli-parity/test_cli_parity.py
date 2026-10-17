"""Contract tests use clearly synthetic recordings, never calibration evidence."""
import hashlib
from contextlib import nullcontext, redirect_stdout
import io
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import (Blocked, CleanupBlocked, HEAD, SCENARIOS, baseline_path, environment, env_summary, fixture,
                    hashes, initial_tree, output_path, parity_lock, save, snapshot)
from compare import coalesce, compare
from controls import run_controls
from run_round import capture, main, sandbox_profile


def synthetic_round(path, runtime):
    path.mkdir()
    (path / 'cases').mkdir()
    run = {'schema': 1, 'kind': 'cli-d0-recording', 'origin': 'synthetic-unit-fixture', 'side': 'ts', 'status': 'DONE',
           'runtime': str(runtime), 'scenarios': [s['id'] for s in SCENARIOS], 'apparatus': hashes(),
           'isolation': {'status': 'PASS', 'errors': [], 'exitCode': 0, 'profileSha256': '0' * 64,
                         'audit': {'allStopped': True, 'remaining': [], 'errors': []},
                         'driverProcess': {'pid': 1},
                         'probe': {'status': 'PASS', 'homeMatches': True, 'outsideReadDenied': True,
                                   'outsideWriteDenied': True, 'listenDenied': True, 'connectDenied': True,
                                   'childDenied': True, 'runtimeWritable': True, 'stdinIsTTY': False,
                                   'osHomedir': str(runtime / 'home')}},
           'provenance': {'executable': {'path': '/synthetic/unit-test-only', 'sha256': '0' * 64},
                          'sourceHead': HEAD, 'baseline': {'sourceHead': HEAD, 'entrySha256': '0' * 64,
                              'path': '/synthetic/baseline', 'files': {'src/fixture.ts': '0' * 64}}}}
    save(path / 'round.json', run)
    for scenario in SCENARIOS:
        folder = path / 'cases' / scenario['id']
        folder.mkdir()
        fixture(folder / 'tree')
        save(folder / 'initial.json', initial_tree())
        if scenario['id'] in ('version-default', 'empty-nontty', 'unknown-command'):
            marker = folder / 'tree/pilot/.legacy-bundled-skills-migrated-v1'
            marker.write_bytes(b'completed\n')
            marker.chmod(0o600)
        save(folder / 'final.json', snapshot(folder / 'tree'))
        events = []
        for stream in ('stdout', 'stderr'):
            data = scenario.get(stream, scenario.get(stream + 'Prefix', '') + 'synthetic\n').encode()
            (folder / f'{stream}.bin').write_bytes(data)
            if data:
                events.append({'stream': stream, 'hex': data.hex()})
        save(folder / 'events.json', events)
        save(folder / 'record.json', {'status': 'DONE', 'timedOut': False, 'exitCode': scenario['exit'],
             'argv': scenario['argv'], 'stdinHex': scenario['stdinHex'], 'cwd': str(runtime / 'project'),
             'executedArgv': ['/synthetic/unit-test-only', '--disable-warning=ExperimentalWarning',
                              '/synthetic/baseline/dist/src/cli/pilotdeck.js'] + scenario['argv'],
             'env': env_summary(environment(runtime, scenario)), 'pid': 123,
             'audit': {'allStopped': True, 'remaining': [], 'errors': []},
             'processTree': [{'pid': 123, 'ppid': 1, 'pgid': 123, 'state': 'Z', 'command': 'synthetic'}]})


class ContractTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='d0-test-')
        self.root = Path(self.temp.name).resolve()
        # Production has no fixture acceptance switch, including in either CLI.
        self.origin_patch = patch('compare.RECORDING_ORIGIN', 'synthetic-unit-fixture')
        self.origin_patch.start()

    def tearDown(self):
        self.origin_patch.stop()
        self.temp.cleanup()

    def source(self):
        source = self.root / 'source'
        synthetic_round(source, self.root / 'fixed-runtime')
        return source

    def test_all_five_mutations_detected_on_synthetic_fixture(self):
        result = run_controls(self.source(), self.root / 'mutants')
        self.assertEqual(result['status'], 'PASS', result)
        self.assertEqual(len(result['controls']), 5)

    def test_ts_executed_argv_requires_experimental_warning_flag(self):
        source = self.source()
        self.assertEqual(compare(source, source)['status'], 'PASS')
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario['id']):
                path = source / 'cases' / scenario['id'] / 'record.json'
                record = json.loads(path.read_text())
                original = record['executedArgv'][:]
                record['executedArgv'].remove('--disable-warning=ExperimentalWarning')
                save(path, record)
                result = compare(source, source)
                self.assertEqual(result['status'], 'FAIL')
                self.assertEqual({row['rule'] for row in result['failures']},
                                 {f'{scenario["id"]}.executed-argv'})
                record['executedArgv'] = original
                save(path, record)

    def test_expected_disk_additions_require_exact_complete_tree(self):
        source = self.source()
        self.assertEqual(compare(source, source)['status'], 'PASS')
        for name in ('version-default', 'empty-nontty', 'unknown-command'):
            for mutation in ('extra', 'missing', 'content', 'size', 'mode', 'type',
                             'missing-fixture', 'changed-fixture'):
                with self.subTest(scenario=name, mutation=mutation):
                    target = self.root / f'{name}-{mutation}'
                    shutil.copytree(source, target)
                    case = target / 'cases' / name
                    marker = case / 'tree/pilot/.legacy-bundled-skills-migrated-v1'
                    if mutation == 'extra':
                        (case / 'tree/pilot/.unexpected').write_bytes(b'extra')
                    elif mutation == 'missing':
                        marker.unlink()
                    elif mutation == 'content':
                        marker.write_bytes(b'Completed\n')  # Same length; SHA-256 must differ.
                    elif mutation == 'size':
                        marker.write_bytes(b'completed\n\n')
                    elif mutation == 'mode':
                        marker.chmod(0o644)
                    elif mutation == 'type':
                        marker.unlink()
                        marker.mkdir(mode=0o600)
                    elif mutation == 'missing-fixture':
                        (case / 'tree/project/keep.txt').unlink()
                    else:
                        (case / 'tree/project/keep.txt').write_bytes(b'changed')
                    # Keep manifests valid: the independent oracle must reject even
                    # two equally wrong recordings, not just stale cached metadata.
                    save(case / 'final.json', snapshot(case / 'tree'))
                    result = compare(target, target)
                    self.assertEqual(result['status'], 'FAIL')
                    self.assertEqual({row['rule'] for row in result['failures']},
                                     {f'{name}.oracle.disk'})

    def test_missing_both_rounds_cannot_pass(self):
        self.assertEqual(compare(self.root, self.root)['status'], 'FAIL')

    def test_production_rejects_synthetic_calibration(self):
        source = self.source()
        with patch('compare.RECORDING_ORIGIN', 'live-os-sandbox'):
            self.assertEqual(compare(source, source)['status'], 'FAIL')
            self.assertEqual(run_controls(source, self.root / 'controls')['status'], 'BLOCKED')

    def test_unknown_side_cannot_bypass_oracles(self):
        source = self.source()
        path = source / 'round.json'
        run = json.loads(path.read_text())
        run['side'] = 'anything'
        save(path, run)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_stub_isolation_is_rejected(self):
        source = self.source()
        path = source / 'round.json'
        run = json.loads(path.read_text())
        run['isolation'] = {'status': 'PASS'}
        save(path, run)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_symlink_cannot_mutate_original_evidence(self):
        source = self.source()
        original = source / 'cases/help-long/stdout.bin'
        retained = self.root / 'original.bin'
        original.rename(retained)
        before = retained.read_bytes()
        original.symlink_to(retained)
        self.assertEqual(run_controls(source, self.root / 'controls')['status'], 'BLOCKED')
        self.assertEqual(retained.read_bytes(), before)
        self.assertFalse((self.root / 'controls').exists())

    def test_case_directory_symlink_rejected(self):
        source = self.source()
        folder = source / 'cases/help-long'
        retained = self.root / 'original-case'
        folder.rename(retained)
        folder.symlink_to(retained, target_is_directory=True)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_extra_root_symlink_cannot_be_used_as_control_report(self):
        source = self.source()
        original = self.root / 'original.json'
        original.write_text('retain this evidence\n')
        before = original.read_bytes()
        (source / 'comparison.json').symlink_to(original)
        self.assertEqual(run_controls(source, self.root / 'controls')['status'], 'BLOCKED')
        self.assertEqual(original.read_bytes(), before)
        self.assertFalse((self.root / 'controls').exists())

    def test_cyclic_symlink_is_a_failed_recording(self):
        source = self.source()
        path = source / 'cases/help-long/tree/project/cycle'
        path.symlink_to('cycle')
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_controls_cannot_copy_inside_source(self):
        source = self.source()
        with self.assertRaises(ValueError):
            run_controls(source, source / 'controls')
        self.assertFalse((source / 'controls').exists())

    def test_rust_pair_comparison_catches_all_five_mutations(self):
        source = self.source()
        for name in ('stdout', 'stderr', 'exitCode', 'drop-disk', 'extra-file'):
            with self.subTest(name=name):
                target = self.root / name
                shutil.copytree(source, target)
                path = target / 'round.json'
                run = json.loads(path.read_text())
                run['side'] = 'rust'
                save(path, run)
                for scenario in SCENARIOS:
                    p = target / 'cases' / scenario['id'] / 'record.json'
                    row = json.loads(p.read_text())
                    row['executedArgv'] = ['/synthetic/unit-test-only'] + scenario['argv']
                    save(p, row)
                self.assertEqual(compare(source, target)['status'], 'PASS')
                case = target / 'cases/help-long'
                if name in ('stdout', 'stderr'):
                    (case / f'{name}.bin').write_bytes(b'changed\x00\xff')
                    events = [r for r in json.loads((case / 'events.json').read_text()) if r['stream'] != name]
                    events.append({'stream': name, 'hex': b'changed\x00\xff'.hex()})
                    save(case / 'events.json', events)
                elif name == 'exitCode':
                    row = json.loads((case / 'record.json').read_text())
                    row[name] = 73
                    save(case / 'record.json', row)
                else:
                    if name == 'drop-disk':
                        (case / 'tree/project/keep.txt').unlink()
                    else:
                        (case / 'tree/project/extra').write_bytes(b'extra')
                    save(case / 'final.json', snapshot(case / 'tree'))
                result = compare(source, target)
                self.assertEqual(result['status'], 'FAIL')
                field = 'disk' if name in ('drop-disk', 'extra-file') else name
                self.assertIn(f'help-long.{field}', [r['rule'] for r in result['failures']])

    def test_missing_scenario_on_both_sides_cannot_pass(self):
        source = self.source()
        (source / 'cases/empty-nontty').rename(source / 'cases/renamed')
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_stale_disk_manifest_and_hidden_extra_file(self):
        source = self.source()
        (source / 'cases/help-long/tree/.hidden').write_bytes(b'\x00\xff')
        result = compare(source, source)
        self.assertTrue(any('disk.manifest' in row['rule'] for row in result['failures']))

    def test_changed_fixture_rejected_even_if_both_sides_agree(self):
        source = self.source()
        path = source / 'cases/help-long/initial.json'
        save(path, {})
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_exit_and_cleanup_are_not_truthy_checks(self):
        source = self.source()
        path = source / 'cases/help-long/record.json'
        record = json.loads(path.read_text())
        record['exitCode'] = False
        record['audit']['allStopped'] = 'true'
        save(path, record)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_unknown_env_and_injected_parent_secrets_rejected(self):
        with patch.dict(os.environ, {'NODE_OPTIONS': '--require /evil', 'API_KEY': 'secret',
                                      'HTTP_PROXY': 'http://secret', 'SSH_AUTH_SOCK': '/socket'}):
            env = environment(self.root, SCENARIOS[0])
        self.assertTrue({'NODE_OPTIONS', 'API_KEY', 'HTTP_PROXY', 'SSH_AUTH_SOCK'}.isdisjoint(env))
        source = self.source()
        path = source / 'cases/help-long/record.json'
        record = json.loads(path.read_text())
        record['env']['API_KEY'] = 'secret'
        save(path, record)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_token_summary_does_not_disclose_value(self):
        env = environment(self.root, next(s for s in SCENARIOS if s['token']))
        summary = env_summary(env)
        self.assertNotIn(env['PILOTDECK_SDK_AUTH_TOKEN'], json.dumps(summary))
        self.assertEqual(summary['PILOTDECK_SDK_AUTH_TOKEN']['sha256'], hashlib.sha256(env['PILOTDECK_SDK_AUTH_TOKEN'].encode()).hexdigest())

    def test_baseline_precedence_and_no_fallback_on_missing_explicit(self):
        for name in ('explicit', 'env'):
            entry = self.root / name / 'dist/src/cli/pilotdeck.js'
            entry.parent.mkdir(parents=True)
            entry.write_text('fixture')
        env = {'PDRS_BASELINE': str(self.root / 'env')}
        self.assertEqual(baseline_path(self.root / 'explicit', env), self.root / 'explicit')
        self.assertEqual(baseline_path(None, env), self.root / 'env')
        with self.assertRaises(Blocked):
            baseline_path(self.root / 'missing', env)
        with patch('common.ROOT', self.root):
            with self.assertRaises(Blocked):
                baseline_path(None, {})
            default = self.root / 'resources/PilotDeck'
            entry = default / 'dist/src/cli/pilotdeck.js'
            entry.parent.mkdir(parents=True)
            entry.write_text('fixture')
            self.assertEqual(baseline_path(None, {'PDRS_BASELINE': ''}), default)

    def test_lock_is_exclusive_and_existing_owner_is_retained(self):
        lock = self.root / 'lock'
        with parity_lock(lock):
            original = (lock / 'owner.json').read_bytes()
            with self.assertRaises(Blocked):
                with parity_lock(lock):
                    self.fail('second owner acquired lock')
            self.assertEqual((lock / 'owner.json').read_bytes(), original)
        self.assertFalse(lock.exists())

    def test_cleanup_failure_retains_owned_lock(self):
        lock = self.root / 'lock'
        with self.assertRaises(CleanupBlocked):
            with parity_lock(lock):
                raise CleanupBlocked('synthetic failure')
        self.assertTrue((lock / 'owner.json').is_file())

    def test_failed_preflight_cannot_start_any_product_round(self):
        output = self.root / 'blocked-run'
        with patch('sys.argv', ['run_round.py', '--node', sys.executable]), \
             patch('run_round.output_path', return_value=output), \
             patch('run_round.baseline_path', return_value=self.root / 'baseline'), \
             patch('run_round.baseline_identity', return_value={'sourceHead': HEAD}), \
             patch('run_round.sandbox_profile', return_value='synthetic policy'), \
             patch('run_round.parity_lock', return_value=nullcontext({'pid': 1})), \
             patch('run_round.preflight', side_effect=Blocked('synthetic EPERM')), \
             patch('run_round.run_round') as product, redirect_stdout(io.StringIO()):
            self.assertEqual(main(), 2)
            product.assert_not_called()
        self.assertEqual(json.loads((output / 'summary.json').read_text())['status'], 'BLOCKED')

    def test_snapshot_does_not_follow_links_or_open_fifo(self):
        root = self.root / 'tree'
        fixture(root)
        (root / 'project/link').symlink_to('keep.txt')
        self.assertEqual(snapshot(root)['project/link']['type'], 'symlink')
        (root / 'project/escape').symlink_to('../../../outside')
        with self.assertRaises(ValueError):
            snapshot(root)
        (root / 'project/escape').unlink()
        os.mkfifo(root / 'fifo')
        with self.assertRaises(ValueError):
            snapshot(root)

    def test_snapshot_detects_mode_and_empty_directory(self):
        root = self.root / 'tree'
        fixture(root)
        self.assertEqual(snapshot(root), initial_tree())
        (root / 'project/empty').mkdir()
        (root / 'project/keep.txt').chmod(0o400)
        tree = snapshot(root)
        self.assertIn('project/empty', tree)
        self.assertEqual(tree['project/keep.txt']['mode'], 0o400)
        root.chmod(0o755)
        self.assertEqual(snapshot(root)['.']['mode'], 0o755)

    def test_chunk_boundaries_ignored_but_interstream_order_preserved(self):
        row = lambda stream, data: {'stream': stream, 'hex': data.hex()}
        a = [row('stdout', b'\x00'), row('stdout', b'\xff\n'), row('stderr', b'e')]
        b = [row('stdout', b'\x00\xff\n'), row('stderr', b'e')]
        self.assertEqual(coalesce(a), coalesce(b))
        self.assertNotEqual(coalesce(a), coalesce(list(reversed(b))))

    def test_stream_truncation_rejected(self):
        source = self.source()
        (source / 'cases/help-long/stdout.bin').write_bytes(b'')
        result = compare(source, source)
        self.assertTrue(any('stdout.events' in row['rule'] for row in result['failures']))

    def test_output_path_rejects_external_path(self):
        with self.assertRaises(Blocked):
            output_path(self.root / 'outside')

    def test_profile_only_runtime_is_writable(self):
        with patch('run_round.sys.platform', 'darwin'):
            profile = sandbox_profile(self.root / 'runtime', self.root / 'baseline', '/bin/true', '/bin/true')
        self.assertIn('(deny default)', profile)
        self.assertIn('(deny network*)', profile)
        self.assertIn('(deny process-fork)', profile)
        self.assertIn(f'(allow file-write* (subpath "{self.root}/runtime"))', profile)

    def test_capture_timeout_kills_process_group(self):
        # A trusted Python fixture tests cleanup only; not an unconfined product run.
        with patch('run_round.process_table', return_value=[]):
            record, streams, _ = capture([sys.executable, '-I', '-c', 'import time; time.sleep(30)'],
                                         {'PATH': '/usr/bin:/bin'}, self.root, b'', .1)
        self.assertTrue(record['timedOut'])
        self.assertTrue(record['audit']['allStopped'])
        self.assertEqual(record['status'], 'ERROR')
        with self.assertRaises(ProcessLookupError):
            os.killpg(record['pid'], 0)

    def test_capture_interrupt_still_records_cleanup(self):
        with patch('run_round.process_table', side_effect=KeyboardInterrupt('synthetic cancellation')):
            record, _, _ = capture([sys.executable, '-I', '-c', 'import time; time.sleep(30)'],
                                   {'PATH': '/usr/bin:/bin'}, self.root, b'', 1)
        self.assertEqual(record['status'], 'ERROR')
        self.assertTrue(record['audit']['allStopped'])
        with self.assertRaises(ProcessLookupError):
            os.killpg(record['pid'], 0)


if __name__ == '__main__':
    unittest.main()
