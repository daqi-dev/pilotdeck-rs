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
from common import (Blocked, CleanupBlocked, HEAD, OPENSSL_CONFIG, SCRIPT, SCENARIOS, baseline_path, environment, env_summary, fixture,
                    hashes, initial_tree, output_path, parity_lock, save, snapshot)
from compare import coalesce, compare, comparison_view, PROBE_CHECKS
from controls import run_controls
from run_round import baseline_identity, capture, check_budget, main, rust_isolation, sandbox_profile


def bind_policy(run):
    p = run['provenance']
    iso = run['isolation']
    iso['profileText'] = sandbox_profile(Path(run['runtime']), Path(p['baseline']['path']), p['executable']['path'], run['side'])
    iso['profileSha256'] = hashlib.sha256(iso['profileText'].encode()).hexdigest()
    iso['profilePath'] = '/synthetic/' + run['side'] + '.sb'
    iso['probeProfileSha256'] = hashlib.sha256(sandbox_profile(
        Path(run['runtime']), Path(p['baseline']['path']), p['node']['path'], 'ts').encode()).hexdigest()


def as_rust(path):
    run = json.loads((path / 'round.json').read_text())
    expected = {'side': 'rust', 'executable': {'path': '/synthetic/rust-candidate', 'sha256': '1' * 64},
                'sourceHead': 'a' * 40, 'sourcePath': '/synthetic/rust-source'}
    run['side'] = 'rust'
    run['provenance'].update(expected)
    run['isolation']['nodeExec'] = {'denied': True, 'status': 'DONE', 'timedOut': False, 'exitCode': 71, 'stdoutHex': '',
        'stderrHex': b'sandbox-exec: execvp(): Operation not permitted'.hex(),
        'audit': {'allStopped': True, 'remaining': [], 'errors': []}}
    bind_policy(run)
    save(path / 'round.json', run)
    for scenario in SCENARIOS:
        p = path / 'cases' / scenario['id'] / 'record.json'
        row = json.loads(p.read_text())
        row['executedArgv'] = [expected['executable']['path']] + scenario['argv']
        row['sandboxArgv'] = ['/usr/bin/sandbox-exec', '-f', run['isolation']['profilePath'], *row['executedArgv']]
        row['profileSha256'] = run['isolation']['profileSha256']
        save(p, row)
    return expected


def write_stream(case, stream, data):
    events = [r for r in json.loads((case / 'events.json').read_text()) if r['stream'] != stream]
    if data:
        events.append({'stream': stream, 'hex': data.hex()})
    (case / f'{stream}.bin').write_bytes(data)
    save(case / 'events.json', events)


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
           'provenance': {'side': 'ts', 'sourcePath': '/synthetic/baseline', 'opensslConfig': OPENSSL_CONFIG,
                          'node': {'path': '/synthetic/unit-test-only', 'sha256': '0' * 64}, 'executable': {'path': '/synthetic/unit-test-only', 'sha256': '0' * 64},
                          'sourceHead': HEAD, 'baseline': {'sourceHead': HEAD, 'entrySha256': '0' * 64,
                              'path': '/synthetic/baseline', 'files': {'src/fixture.ts': '0' * 64},
                              'trees': {name: '0' * 64 for name in ('src', 'dist', 'packages', 'node_modules', 'skills')}}}}
    run['isolation']['probe'].update({key: True for key in PROBE_CHECKS})
    bind_policy(run)
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
            if stream == 'stdout' and scenario['id'].startswith('help-'):
                data = (SCRIPT / 'help.txt').read_bytes()
            (folder / f'{stream}.bin').write_bytes(data)
            if data:
                events.append({'stream': stream, 'hex': data.hex()})
        save(folder / 'events.json', events)
        save(folder / 'record.json', {'status': 'DONE', 'timedOut': False, 'exitCode': scenario['exit'],
             'argv': scenario['argv'], 'stdinHex': scenario['stdinHex'], 'cwd': str(runtime / 'project'),
             'executedArgv': ['/synthetic/unit-test-only', '/synthetic/baseline/dist/src/cli/pilotdeck.js'] + scenario['argv'],
             'sandboxArgv': ['/usr/bin/sandbox-exec', '-f', run['isolation']['profilePath'], '/synthetic/unit-test-only',
                             '/synthetic/baseline/dist/src/cli/pilotdeck.js'] + scenario['argv'],
             'profileSha256': run['isolation']['profileSha256'],
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
        for row in result['controls']:
            self.assertIn(row['expectedPairRule'], row['observedRules'])

    def test_exact_ts_warning_prefix_only_once_without_mutating_raw_evidence(self):
        source = self.source()
        original = self.root / 'original'
        shutil.copytree(source, original)
        warning = (b'(node:123) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n'
                   b'(Use `node --trace-warnings ...` to show where the warning was created)\n')
        case = source / 'cases/flag-missing'
        data = warning + (case / 'stderr.bin').read_bytes()
        write_stream(case, 'stderr', data)
        # Split both lines across pipe reads, with stdout interspersed. Removal
        # must preserve all remaining stream order and coalesce newly adjacent reads.
        events = [('stderr', warning[:30]), ('stdout', b'a'), ('stderr', warning[30:]),
                  ('stdout', b'b'), ('stderr', b'product')]
        stripped, view = comparison_view('ts', warning + b'product', events)
        self.assertEqual(stripped, b'product')
        self.assertEqual(view, [('stdout', b'ab'), ('stderr', b'product')])
        self.assertEqual(compare(original, source)['status'], 'PASS')
        self.assertEqual((case / 'stderr.bin').read_bytes(), data)
        self.assertEqual(comparison_view('ts', warning * 2, [('stderr', warning * 2)])[0], warning)
        self.assertEqual(comparison_view('rust', warning, [('stderr', warning)])[0], warning)

    def test_product_experimental_warning_and_nonprefix_sqlite_are_compared(self):
        source = self.source()
        for name, extra in (
            ('product-name', b'(node:12) ExperimentalWarning: product-canary\n'),
            ('product-code', b'(node:12) [ExperimentalWarning] ProductWarning: product-canary\n'),
            ('nonprefix-sqlite', b'product\n(node:12) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n(Use `node --trace-warnings ...` to show where the warning was created)\n'),
            ('almost-sqlite', b'(node:12) ExperimentalWarning: SQLite is an experimental feature and might change at any time\r\n(Use `node --trace-warnings ...` to show where the warning was created)\n')):
            with self.subTest(name=name):
                target = self.root / name
                shutil.copytree(source, target)
                write_stream(target / 'cases/help-long', 'stderr', extra)
                result = compare(source, target)
                self.assertIn('help-long.stderr', [r['rule'] for r in result['failures']])
                self.assertEqual(comparison_view('ts', extra, [('stderr', extra)])[0], extra)

    def test_rust_sqlite_warning_is_a_difference(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        warning = (b'(node:1) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n'
                   b'(Use `node --trace-warnings ...` to show where the warning was created)\n')
        write_stream(source / 'cases/help-long', 'stderr', warning)
        self.assertEqual(compare(source, target, expected)['status'], 'PASS')
        write_stream(target / 'cases/help-long', 'stderr', warning)
        result = compare(source, target, expected)
        self.assertIn('help-long.stderr', [r['rule'] for r in result['failures']])

    def test_help_middle_drift_rejected_even_when_both_sides_agree(self):
        source = self.source()
        case = source / 'cases/help-long'
        write_stream(case, 'stdout', (case / 'stdout.bin').read_bytes() + b'undocumented command\n')
        result = compare(source, source)
        self.assertIn('help-long.oracle.stdout', [r['rule'] for r in result['failures']])

    def test_rust_evidence_requires_independent_binary_and_source_binding(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        self.assertEqual(compare(source, target, expected)['status'], 'PASS')
        for field, bad in (('executable', {'path': '/another/candidate', 'sha256': '2' * 64}),
                           ('sourceHead', 'b' * 40), ('sourcePath', '/another/source'), ('side', 'ts')):
            with self.subTest(field=field):
                result = compare(source, target, {**expected, field: bad})
                self.assertIn('round.rust-binding', [r['rule'] for r in result['failures']])
        self.assertIn('round.rust-binding', [r['rule'] for r in compare(source, target)['failures']])
        self.assertIn('round.expected-rust-side', [r['rule'] for r in compare(source, source, expected)['failures']])

    def test_side_relabel_and_ts_provenance_in_rust_cannot_pass(self):
        source = self.source()
        target = self.root / 'relabel'
        shutil.copytree(source, target)
        path = target / 'round.json'
        run = json.loads(path.read_text())
        run['side'] = 'rust'
        save(path, run)
        result = compare(source, target)
        self.assertIn('round.provenance-side', [r['rule'] for r in result['failures']])
        # Even changing the tag cannot make the Node executable a Rust source.
        expected = as_rust(target)
        run = json.loads(path.read_text())
        run['provenance']['executable'] = run['provenance']['node']
        save(path, run)
        self.assertIn('round.rust-source', [r['rule'] for r in compare(source, target, expected)['failures']])

    def test_baseline_dependency_and_skills_identity_must_match(self):
        source = self.source()
        for field in ('node_modules', 'skills'):
            target = self.root / field
            shutil.copytree(source, target)
            path = target / 'round.json'
            run = json.loads(path.read_text())
            run['provenance']['baseline']['trees'][field] = '1' * 64
            save(path, run)
            self.assertIn('round.baseline', [r['rule'] for r in compare(source, target)['failures']])
            del run['provenance']['baseline']['trees'][field]
            save(path, run)
            self.assertIn('round.baseline-trees', [r['rule'] for r in compare(source, target)['failures']])

    def test_installed_manifest_covers_hidden_dependencies_content_skills_and_links(self):
        baseline = self.root / 'baseline'
        for name in ('src', 'dist/src/cli', 'packages', 'node_modules/.pnpm/pkg', 'skills'):
            (baseline / name).mkdir(parents=True)
        (baseline / 'package.json').write_text('{}')
        (baseline / 'dist/src/cli/pilotdeck.js').write_text('entry')
        dependency = baseline / 'node_modules/.pnpm/pkg/index.js'
        dependency.write_text('original')
        with patch('run_round.git_head', return_value=HEAD):
            first = baseline_identity(baseline)
            dependency.write_text('modified')
            second = baseline_identity(baseline)
            self.assertNotEqual(first['trees']['node_modules'], second['trees']['node_modules'])
            (baseline / 'skills/new.md').write_text('skill')
            third = baseline_identity(baseline)
            self.assertNotEqual(second['trees']['skills'], third['trees']['skills'])
            # pnpm hardlinks are legal; their contents still participate.
            os.link(dependency, baseline / 'node_modules/hardlink.js')
            link = baseline / 'node_modules/pkg'
            link.symlink_to('.pnpm/pkg', target_is_directory=True)
            fourth = baseline_identity(baseline)
            self.assertNotEqual(third['trees']['node_modules'], fourth['trees']['node_modules'])
            (baseline / 'apps/desktop').mkdir(parents=True)
            (baseline / 'node_modules/desktop').symlink_to('../apps/desktop')
            fifth = baseline_identity(baseline)
            self.assertNotEqual(fourth['trees']['node_modules'], fifth['trees']['node_modules'])
            policy = sandbox_profile(self.root / 'runtime', baseline, '/installed/node', 'ts')
            self.assertNotIn(str(baseline / 'apps'), policy)
            link.unlink()
            link.symlink_to(self.root / 'outside')
            with self.assertRaises(Blocked):
                baseline_identity(baseline)

    def test_openssl_is_controlled_and_profiles_separate_exec_and_reads(self):
        runtime, baseline = self.root / 'runtime', self.root / 'baseline'
        fixture(runtime)
        self.assertEqual((runtime / 'openssl.cnf').read_bytes(), b'')
        self.assertEqual(environment(runtime, SCENARIOS[0])['OPENSSL_CONF'], str(runtime / 'openssl.cnf'))
        ts = sandbox_profile(runtime, baseline, '/opt/homebrew/bin/node', 'ts')
        rust = sandbox_profile(runtime, baseline, '/candidate/pilotdeck', 'rust')
        self.assertIn('(allow process-exec (literal "/candidate/pilotdeck"))', rust)
        for policy in (ts, rust):
            self.assertNotIn('/opt/homebrew/etc/openssl@3', policy)
            self.assertNotIn('(allow file-read-metadata)', policy)
            self.assertNotIn('opendirectoryd', policy)
        self.assertIn(str(baseline / 'node_modules'), ts)
        self.assertNotIn(str(baseline), rust)
        self.assertNotIn('homebrew', rust)
        self.assertNotIn('/opt/homebrew/bin/node', rust)

    def test_probe_private_read_and_metadata_denials_required(self):
        source = self.source()
        path = source / 'round.json'
        run = json.loads(path.read_text())
        for key in ('privateReadDenied', 'outsideMetadataDenied'):
            run['isolation']['probe'][key] = False
            save(path, run)
            self.assertIn('round.probe', [r['rule'] for r in compare(source, source)['failures']])
            run['isolation']['probe'][key] = True

    def test_rust_node_denial_probe_rejects_success_incomplete_and_timeout(self):
        profile = self.root / 'rust.sb'
        profile.write_text('synthetic policy')
        good = {'status': 'DONE', 'timedOut': False, 'exitCode': 71,
                'audit': {'allStopped': True, 'remaining': [], 'errors': []}}
        denied = {'stdout': b'', 'stderr': b'sandbox-exec: execvp(): Operation not permitted\n'}
        with patch('run_round.capture', return_value=(good, denied, [])):
            result = rust_isolation(self.root, self.root, profile, '/node', {})
            self.assertTrue(result['nodeExec']['denied'])
        for changes, streams in (({'status': 'ERROR'}, denied), ({'timedOut': True}, denied),
                                 ({'exitCode': 0}, {'stdout': b'v22\n', 'stderr': b''}),
                                 ({}, {'stdout': b'', 'stderr': b'sandbox_apply: Operation not permitted'})):
            with self.subTest(changes=changes, streams=streams):
                with patch('run_round.capture', return_value=({**good, **changes}, streams, [])):
                    with self.assertRaises(Blocked):
                        rust_isolation(self.root, self.root, profile, '/node', {})

    def test_rust_incomplete_node_denial_evidence_is_rejected(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        path = target / 'round.json'
        run = json.loads(path.read_text())
        run['isolation']['nodeExec']['status'] = 'ERROR'
        save(path, run)
        self.assertIn('round.rust-node-exec', [r['rule'] for r in compare(source, target, expected)['failures']])

    def test_sandbox_wrapper_and_policy_mutations_rejected(self):
        source = self.source()
        path = source / 'cases/help-long/record.json'
        row = json.loads(path.read_text())
        row['sandboxArgv'] = row['executedArgv']
        save(path, row)
        self.assertIn('help-long.sandbox-binding', [r['rule'] for r in compare(source, source)['failures']])
        path = source / 'round.json'
        run = json.loads(path.read_text())
        run['isolation']['profileText'] += '(allow default)\n'
        run['isolation']['profileSha256'] = hashlib.sha256(run['isolation']['profileText'].encode()).hexdigest()
        save(path, run)
        self.assertIn('round.profile-binding', [r['rule'] for r in compare(source, source)['failures']])

    def test_batch_budget_includes_three_rounds_and_rejects_overflow(self):
        check_budget(60, True)  # Current 10 cases: 1800 seconds plus reserve.
        with patch('run_round.SCENARIOS', [None] * 30):
            with self.assertRaises(Blocked):
                check_budget(60, True)
        with patch('run_round.SCENARIOS', [None] * 31):
            with self.assertRaises(Blocked):
                check_budget(1, False)

    def test_ts_executed_argv_rejects_warning_suppression(self):
        source = self.source()
        self.assertEqual(compare(source, source)['status'], 'PASS')
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario['id']):
                path = source / 'cases' / scenario['id'] / 'record.json'
                record = json.loads(path.read_text())
                original = record['executedArgv'][:]
                record['executedArgv'].insert(1, '--disable-warning=ExperimentalWarning')
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
                expected = as_rust(target)
                self.assertEqual(compare(source, target, expected)['status'], 'PASS')
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
                result = compare(source, target, expected)
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

    def test_lock_guard_and_directory_symlinks_do_not_redirect_writes(self):
        lock = self.root / 'lock'
        original = self.root / 'original'
        original.write_text('preserve')
        lock.with_name('lock.guard').symlink_to(original)
        with self.assertRaises(OSError):
            with parity_lock(lock):
                self.fail('symlink guard accepted')
        self.assertEqual(original.read_text(), 'preserve')
        lock.with_name('lock.guard').unlink()
        original_dir = self.root / 'original-dir'
        original_dir.mkdir()
        lock.symlink_to(original_dir)
        with self.assertRaises(Blocked):
            with parity_lock(lock):
                self.fail('symlink lock accepted')
        self.assertEqual(list(original_dir.iterdir()), [])

    def test_new_lock_permissions_are_private(self):
        lock = self.root / 'lock'
        with parity_lock(lock):
            self.assertEqual(lock.stat().st_mode & 0o777, 0o700)
            self.assertEqual((lock / 'owner.json').stat().st_mode & 0o777, 0o600)

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
            profile = sandbox_profile(self.root / 'runtime', self.root / 'baseline', '/bin/true', 'ts')
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
