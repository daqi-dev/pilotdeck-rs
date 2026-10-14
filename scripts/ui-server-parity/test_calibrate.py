"""Calibration safety tests with fake runner output; never build or listen."""
# Isolated bootstrap: verify the source loader before executing local code.
import os as _boot_os
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
import hashlib as _boot_hashlib
import subprocess as _boot_subprocess
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_os.path.isfile(_boot_manifest):
        with open(_boot_manifest) as _boot_file:
            _boot_pins = [line.split()[0] for line in _boot_file
                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]
        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:
            raise ValueError('apparatus bootstrap hash mismatch')
    else:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
from datetime import datetime, timezone
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import calibrate
from calibrate import Batch, gate_report
from common import ClockRefused, node_modules_hash, source_hashes
from controls.run_all import clock_evidence


class CalibrationControls(unittest.TestCase):
    def setUp(self):
        # The real worktree is below a second repository selector. Production
        # must reject that layout; these unit fixtures have an independent cwd.
        caller = tempfile.TemporaryDirectory()
        self.addCleanup(caller.cleanup)
        patcher = patch('apparatus.Path.cwd', return_value=Path(caller.name).resolve())
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_exit_two_is_not_enough_to_pass_ruler_gate(self):
        good = dict(verdict='PENDING_REVIEW', calibration_eligible=True, comparison_mode='calibration',
                    self_compare=True, pairs=[{'verdict': 'PASS'} for _ in range(10)],
                    per_rule_negative=True, same_bad_rejected=True, errors=[], independent_review=None,
                    proxy_compare={'verdict': 'PASS'})
        gate_report(good, proxy=True)
        for mutation in ({'verdict': 'REFUSED'}, {'comparison_mode': 'historical-replay'},
                         {'pairs': [{'verdict': 'FAIL'}] * 10}, {'pairs': []},
                         {'proxy_compare': {'verdict': 'FAIL'}}, {'calibration_eligible': False}):
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                gate_report({**good, **mutation}, proxy=True)

    def batch(self, tmp):
        root = Path(tmp)
        (root / 'attempts').mkdir()
        state = dict(head='test-head', baseline='/readonly/baseline', utc_day=calibrate.utc_day(), runs={},
                     pinned={'apparatus': {}, 'source': {'node': 'a' * 64, 'baseline': 'b' * 64,
                                                       'node_modules': 'd' * 64,
                                                       'recorder_proxy': 'c' * 64}})
        return Batch(root, state)

    def fake_run(self, command, **kwargs):
        slot = Path(command[command.index('--out') + 1])
        (slot / 'direct').mkdir(parents=True)
        (slot / 'direct/run-meta.json').write_text('{"clock_regression_check":{"status":"REFUSED"}}')
        (slot / 'result.json').write_text('{"status":"REFUSED"}')
        return type('Result', (), {'returncode': 2})()

    def test_refused_run_archived_and_only_named_slot_retried(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            with patch.object(batch, 'guard'), patch('calibrate.subprocess.run', side_effect=self.fake_run) as run:
                for retry in (False, True):
                    with self.assertRaisesRegex(ValueError, 'retry ONLY this run'):
                        batch.capture('ref1', retry=retry)
                self.assertEqual(run.call_count, 2)
            self.assertEqual(set(batch.state['runs']), {'ref1'})
            self.assertEqual(batch.state['runs']['ref1']['status'], 'REFUSED')
            attempts = batch.state['attempts']
            self.assertNotEqual(attempts[0]['archive'], attempts[1]['archive'])
            for entry in attempts:
                self.assertEqual(json.loads((Path(entry['archive']) / 'result.json').read_text())['status'], 'REFUSED')
            self.assertFalse(batch.slot.exists())

    def test_success_is_archived_once_and_resume_never_recaptures_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)

            def success(command, **kwargs):
                slot = Path(command[command.index('--out') + 1])
                (slot / 'direct').mkdir(parents=True)
                (slot / 'result.json').write_text('{"status":"PASS"}')
                return type('Result', (), {'returncode': 0})()

            with patch.object(batch, 'guard'), patch.object(batch, 'admit', return_value={'run_id': 'fresh'}), \
                    patch('calibrate.subprocess.run', side_effect=success) as run:
                batch.capture('ref1')
                batch.capture('ref1')
                self.assertEqual(run.call_count, 1)
            self.assertEqual(batch.state['attempts'][0]['archive'], batch.state['runs']['ref1']['archive'])
            self.assertTrue(Path(batch.state['attempts'][0]['archive']).is_dir())
            self.assertFalse(batch.slot.exists())

    def test_retry_cannot_replace_pool_after_scoring_or_overwrite_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            batch.state['runs'] = {'ref1': {'status': 'REFUSED'}, 'ts1': {'status': 'PASS'}}
            with self.assertRaisesRegex(ValueError, 'pool already consumed'):
                batch.capture('ref1', retry=True)
            batch.state['runs']['ref1']['status'] = 'PASS'
            with self.assertRaisesRegex(ValueError, 'exact run'):
                batch.capture('ref1', retry=True)
            with self.assertRaisesRegex(ValueError, 'earlier slots'):
                batch.capture('ref3')

    def test_resume_rejects_day_or_source_change(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            with patch('calibrate.git_admission'), patch.object(batch, 'verify_build'), patch('calibrate.utc_day', return_value='other-day'), self.assertRaisesRegex(ValueError, 'UTC midnight'):
                batch.guard()
            with patch('calibrate.git_admission'), patch.object(batch, 'verify_build'), patch.object(batch, 'pin', return_value={}), self.assertRaisesRegex(ValueError, 'source changed'):
                batch.guard()

    def test_pass_capture_with_missing_or_bad_clock_is_not_admitted(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            directory = Path(tmp) / 'ref1/direct'
            directory.mkdir(parents=True)
            for meta in ({}, {'clock_regression_check': {'schema': 1, 'status': 'REFUSED', 'reason': 'wide bracket'}}):
                (directory / 'run-meta.json').write_text(json.dumps(meta))
                with self.assertRaises(ClockRefused):
                    batch.admit('ref1')

    def test_source_path_day_and_overlap_are_checked_on_admission(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            directory = Path(tmp) / 'ref2/direct'
            directory.mkdir(parents=True)
            start = int(datetime.now(timezone.utc).timestamp() * 1000)
            meta = dict(baseline_path=batch.state['baseline'], wall_start_ms=start, wall_end_ms=start + 1000,
                        clock_regression_check=clock_evidence(start, start + 1000))
            (directory / 'run-meta.json').write_text(json.dumps(meta))
            identity = dict(source_sha256=batch.state['pinned']['source'], capture_utc_day=batch.state['utc_day'],
                            fixture_path=str(batch.slot / 'direct'), run_id='fresh', recorder_pid=200,
                            recording_sha256='e' * 64, wall_start_ms=start, wall_end_ms=start + 1000)
            with patch('calibrate.capture_identity', return_value=identity):
                self.assertEqual(batch.admit('ref2'), identity)
            for mutation in ({'capture_utc_day': 'other-day'}, {'fixture_path': '/another/slot'},
                             {'source_sha256': {}}):
                with patch('calibrate.capture_identity', return_value={**identity, **mutation}), self.assertRaises(ValueError):
                    batch.admit('ref2')
            old = {**identity, 'run_id': 'previous', 'recorder_pid': 100, 'recording_sha256': 'f' * 64}
            batch.state['runs']['ref1'] = {'status': 'PASS', 'identity': old}
            with patch('calibrate.capture_identity', return_value=identity), self.assertRaisesRegex(ValueError, 'overlap'):
                batch.admit('ref2')

    def test_space_preflight_uses_projected_total_and_disk_free(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'reference/direct').mkdir(parents=True)
            (root / 'reference/direct/recording.jsonl').write_text('x')
            free = type('Disk', (), {'free': 50_000_000_000})()
            with patch('calibrate.size', side_effect=[12_000_000, 29_000_000_000]), \
                    patch('calibrate.shutil.disk_usage', return_value=free), self.assertRaisesRegex(ValueError, 'space budget'):
                calibrate.space_check(root, root / 'reference')
            with patch('calibrate.size', side_effect=[12_000_000, 16_000_000_000]), \
                    patch('calibrate.shutil.disk_usage', return_value=free):
                report = calibrate.space_check(root, root / 'reference')
            self.assertEqual(report['projected_bytes'], 18_468_000_000)

    def test_fresh_target_build_ignores_forged_old_binary_and_detects_new_drift(self):
        import os
        import shutil
        import subprocess
        import hashlib
        if not shutil.which('cargo'):
            self.skipTest('Cargo unavailable')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            import subprocess
            (root / 'rust-toolchain.toml').write_bytes((calibrate.ROOT / 'rust-toolchain.toml').read_bytes())
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            subprocess.run(['git', '-C', str(root), 'add', 'rust-toolchain.toml'], check=True)
            manifest = root / 'scripts/exp008/proxy/Cargo.toml'
            manifest.parent.mkdir(parents=True)
            manifest.write_text('[package]\nname="pdrs-exp008-proxy"\nversion="0.1.0"\nedition="2021"\n')
            source = manifest.parent / 'src/main.rs'
            source.parent.mkdir()
            source.write_text('fn main() { println!("FRESH"); }\n')
            old_target = root / '_build/exp010-target'
            env = dict(os.environ, CARGO_HOME=str(root / '_build/cargo-home'), CARGO_TARGET_DIR=str(old_target))
            subprocess.run(['cargo', 'generate-lockfile', '--offline', '--manifest-path', str(manifest)],
                           env=env, check=True, capture_output=True)
            subprocess.run(['cargo', 'build', '--locked', '--offline', '--manifest-path', str(manifest)],
                           env=env, check=True, capture_output=True)
            old_binary = old_target / 'debug/pdrs-exp008-proxy'
            # Cargo may copy the deps executable back to debug even when fresh.
            # Forge both outputs without changing the source/fingerprint timestamps.
            outputs = [old_binary] + [p for p in (old_target / 'debug/deps').glob('pdrs_exp008_proxy-*')
                                      if p.suffix == '']
            for output in outputs:
                old_stat = output.stat()
                output.write_text('#!/bin/sh\necho FORGED\n')
                output.chmod(0o755)
                os.utime(output, ns=(old_stat.st_atime_ns, old_stat.st_mtime_ns))
            subprocess.run(['cargo', 'build', '--locked', '--offline', '--manifest-path', str(manifest)],
                           env=env, check=True, capture_output=True)
            self.assertEqual(subprocess.check_output([str(old_binary)], text=True).strip(), 'FORGED')
            batch_path = root / '_tmp/batch'
            batch_path.mkdir(parents=True)
            batch = self.batch(batch_path)
            batch.state.update(resource_root=str(root), size_reference=str(root))
            with patch.object(calibrate, 'ROOT', root), patch('calibrate.space_check'):
                batch.build()
                batch.verify_build()
                self.assertEqual(subprocess.check_output([str(batch.binary)], text=True).strip(), 'FRESH')
                self.assertEqual(batch.state['build']['sha256'], hashlib.sha256(batch.binary.read_bytes()).hexdigest())
                self.assertNotEqual(batch.state['build']['sha256'], hashlib.sha256(old_binary.read_bytes()).hexdigest())
                self.assertEqual(batch.state['build']['target_dir'], str(batch_path / 'cargo-target'))
                import run as runner
                standalone = root / '_tmp/standalone'
                standalone.mkdir()
                with patch.object(runner, 'ROOT', root), patch.object(runner, 'BINARY', None):
                    runner.build(standalone / 'cargo-target')
                    self.assertEqual(subprocess.check_output([str(runner.BINARY)], text=True).strip(), 'FRESH')
                    self.assertTrue((standalone / 'cargo-home').is_dir())
                    self.assertIn('rustc', json.loads((standalone / 'toolchain.json').read_text()))
                toolchain = batch.state['build']['toolchain']
                batch.state['build']['toolchain'] = {'rustc': 'FORGED', 'cargo': 'FORGED'}
                with self.assertRaisesRegex(ValueError, 'toolchain changed'):
                    batch.verify_build()
                batch.state['build']['toolchain'] = toolchain
                with self.assertRaisesRegex(ValueError, 'target already exists'):
                    batch.build()
                original = batch.binary.read_bytes()
                batch.binary.write_bytes(b'FORGED')
                with self.assertRaisesRegex(ValueError, 'build missing/changed'):
                    batch.verify_build()
                batch.binary.write_bytes(original)
                batch.state['build'].update(target_dir=str(old_target), binary=str(old_binary),
                                             sha256=hashlib.sha256(old_binary.read_bytes()).hexdigest())
                with self.assertRaisesRegex(ValueError, 'build missing/changed'):
                    batch.verify_build()
            print('CONTROL Cargo old target=FORGED; fresh batch target=FRESH; old/reused/changed batch build=FAIL')

    def test_batch_guard_refuses_HEAD_change_before_capture(self):
        from apparatus import ApparatusUnsafe
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            with patch('calibrate.git_admission', side_effect=ApparatusUnsafe('batch HEAD changed')), \
                    patch.object(batch, 'pin') as pin, self.assertRaisesRegex(ValueError, 'HEAD changed'):
                batch.guard()
            pin.assert_not_called()

    def test_integrity_admission_failure_reports_FAIL_exit_1(self):
        import contextlib
        import io
        from apparatus import ApparatusUnsafe
        output = io.StringIO()
        with patch('sys.argv', ['calibrate.py', '--preflight']), \
                patch('calibrate.git_admission', side_effect=ApparatusUnsafe('dirty apparatus')), \
                contextlib.redirect_stderr(output):
            self.assertEqual(calibrate.main(), 1)
        self.assertEqual(json.loads(output.getvalue())['verdict'], 'FAIL')

    def test_node_modules_tree_binds_bytes_paths_types_and_link_targets(self):
        import os
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = root / 'node_modules'
            tree.mkdir()
            file = tree / 'ws.js'
            file.write_bytes(b'original')
            outside = root / 'outside'
            outside.write_bytes(b'one')
            link = tree / 'link'
            link.symlink_to(outside)
            original = node_modules_hash(root)
            outside.write_bytes(b'two')
            self.assertEqual(node_modules_hash(root), original)  # not followed
            for mutation in ('bytes', 'rename', 'empty-directory', 'link-target', 'file-type'):
                with self.subTest(mutation=mutation):
                    if mutation == 'bytes':
                        file.write_bytes(b'forged')
                    elif mutation == 'rename':
                        file.rename(tree / 'renamed')
                    elif mutation == 'empty-directory':
                        (tree / 'empty').mkdir()
                    elif mutation == 'link-target':
                        link.unlink()
                        link.symlink_to(root / 'missing')
                    else:
                        file.unlink()
                        file.symlink_to(outside)
                    self.assertNotEqual(node_modules_hash(root), original)
                    if mutation == 'rename':
                        (tree / 'renamed').rename(file)
                    elif mutation == 'empty-directory':
                        (tree / 'empty').rmdir()
                    elif mutation == 'link-target':
                        link.unlink()
                        link.symlink_to(outside)
                    else:
                        file.unlink()
                        file.write_bytes(b'original')
            fifo = tree / 'pipe'
            os.mkfifo(fifo)
            with self.assertRaisesRegex(ValueError, 'nonregular node_modules'):
                node_modules_hash(root)

    def test_batch_pin_and_resume_reject_node_modules_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            batch.state['node_modules'] = {'sha256': 'd' * 64}
            original = batch.state['pinned']['source']
            with patch('calibrate.apparatus_hashes', return_value={}), \
                    patch('calibrate.source_hashes', return_value=original):
                self.assertEqual(batch.pin(), batch.state['pinned'])
            with patch('calibrate.git_admission'), patch.object(batch, 'verify_build'), \
                    patch('calibrate.source_hashes', return_value={**original, 'node_modules': 'e' * 64}), \
                    self.assertRaisesRegex(ValueError, 'node_modules changed'):
                batch.guard()
            del batch.state['node_modules']
            with patch('calibrate.source_hashes', return_value=original), self.assertRaisesRegex(ValueError, 'absent batch binding'):
                batch.pin()

    def test_real_source_binding_rejects_node_modules_bytes_change(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = self.batch(tmp)
            baseline = Path(tmp) / 'baseline'
            for name in ('src', 'dist', 'ui/server', 'node_modules/ws'):
                (baseline / name).mkdir(parents=True)
            for name in ('package.json', 'pnpm-lock.yaml', 'ui/package.json'):
                (baseline / name).write_text('{}')
            dependency = baseline / 'node_modules/ws/index.js'
            dependency.write_text('original')
            batch.binary.parent.mkdir(parents=True)
            batch.binary.write_bytes(b'recorder')
            batch.state['baseline'] = str(baseline)
            batch.state['node_modules'] = {'sha256': node_modules_hash(baseline)}
            with patch('calibrate.apparatus_hashes', return_value={}):
                batch.state['pinned'] = batch.pin()
                previous = batch.state['pinned']['source']
                dependency.write_text('forged observations')
                changed = source_hashes(baseline, batch.binary)
                self.assertEqual(changed['baseline'], previous['baseline'])
                self.assertNotEqual(changed['node_modules'], previous['node_modules'])
                with patch('calibrate.git_admission'), patch.object(batch, 'verify_build'), \
                        self.assertRaisesRegex(ValueError, 'node_modules changed'):
                    batch.guard()
                dependency.unlink()
                (baseline / 'node_modules/ws').rmdir()
                (baseline / 'node_modules').rmdir()
                with self.assertRaisesRegex(ValueError, 'source unavailable/changed'):
                    batch.pin()

    def test_cargo_config_injections_fail_before_any_process(self):
        from apparatus import cargo_config_admission, cargo_process
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            cwd, home = root / 'batch', root / 'batch/cargo-home'
            home.mkdir(parents=True)
            paths = [root / '.cargo/config', root / '.cargo/config.toml',
                     cwd / '.cargo/config.toml', home / '.cargo/config.toml',
                     home / 'config', home / 'config.toml', home / 'credentials.toml']
            for path in paths:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('[build]\nrustc-wrapper="injected"\n')
                with self.subTest(path=path), patch('apparatus.run_tool') as run, \
                        self.assertRaisesRegex(ValueError, 'forbidden'):
                    cargo_process(['cargo', '-V'], cwd, home, cwd / 'target')
                run.assert_not_called()
                path.unlink()
            # A broken link is an injection too, even though is_file() is false.
            (root / '.cargo/config.toml').symlink_to(root / 'absent')
            with self.assertRaisesRegex(ValueError, 'forbidden'):
                cargo_config_admission(cwd, home)

    def test_cargo_allowlist_removes_environment_and_PATH_injections(self):
        from apparatus import cargo_environment
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            injections = {'RUSTFLAGS': '--cfg forged', 'CARGO_ENCODED_RUSTFLAGS': 'forged',
                          'CARGO_BUILD_RUSTC_WRAPPER': '/injected', 'RUSTC_WRAPPER': '/injected',
                          'RUSTC_WORKSPACE_WRAPPER': '/injected', 'RUSTC': '/injected',
                          'RUSTUP_TOOLCHAIN': 'injected', 'LD_PRELOAD': '/injected',
                          'DYLD_INSERT_LIBRARIES': '/injected', 'CC': '/injected',
                          'CARGO_HOME': '/injected', 'PATH': '/injected'}
            for key, value in injections.items():
                with self.subTest(key=key), patch.dict('os.environ', {key: value}):
                    env = cargo_environment(root, root / 'home', root / 'target')
                    self.assertNotEqual(env.get(key), value)
                    self.assertNotIn('/injected', env['PATH'])
                    self.assertEqual(set(env), {'HOME', 'PATH', 'CARGO_HOME', 'CARGO_TARGET_DIR',
                                                'RUSTUP_HOME', 'RUSTUP_AUTO_INSTALL', 'RUSTUP_TOOLCHAIN'})

    def test_cargo_seed_copies_only_index_and_compressed_cache(self):
        from apparatus import prepare_cargo
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            seed = root / 'seed'
            for name in ('registry/index/entry', 'registry/cache/package.crate',
                         'registry/src/serde_json/build.rs', 'git/checkouts/build.rs',
                         'config.toml', 'credentials.toml'):
                file = seed / name
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text('injected')
            home = root / 'cargo-home'
            prepare_cargo(root, home, root / 'target', seed)
            files = {str(p.relative_to(home)) for p in home.rglob('*') if p.is_file()}
            self.assertEqual(files, {'registry/index/entry', 'registry/cache/package.crate'})
            with self.assertRaises(FileExistsError):
                prepare_cargo(root, home, root / 'target2', seed)
            (seed / 'registry/cache/link.crate').symlink_to(seed / 'config.toml')
            with self.assertRaisesRegex(ValueError, 'nonregular Cargo cache'):
                prepare_cargo(root, root / 'home2', root / 'target3', seed)

    def test_cached_crate_checksum_injection_fails_before_Cargo(self):
        import hashlib
        from apparatus import cargo_process
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            manifest = root / 'Cargo.toml'
            manifest.write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
            crate = root / 'home/registry/cache/index/serde_json-1.0.151.crate'
            crate.parent.mkdir(parents=True)
            original = b'locked archive bytes'
            crate.write_bytes(original)
            manifest.with_name('Cargo.lock').write_text(
                'version=4\n[[package]]\nname="serde_json"\nversion="1.0.151"\n'
                'source="registry+https://github.com/rust-lang/crates.io-index"\n'
                'checksum="' + hashlib.sha256(original).hexdigest() + '"\n')
            command = ['cargo', 'build', '--locked', '--offline', '--manifest-path', str(manifest)]
            with patch('apparatus.run_tool') as run:
                cargo_process(command, root, root / 'home', root / 'target')
                run.assert_called_once()
                run.reset_mock()
                crate.write_bytes(original + b'FORGED')
                with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                    cargo_process(command, root, root / 'home', root / 'target')
                run.assert_not_called()

    def test_nested_dependencies_and_workspace_packages_share_digest(self):
        from common import dependency_roots
        import os
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            for name in ('node_modules/.pnpm/cors/node_modules/cors', 'ui/node_modules',
                         'packages/sdk/dist', 'packages/declared/dist', 'packages/excluded/dist', 'local/pkg/dist',
                         'arbitrary/deep/node_modules/pkg'):
                (root / name).mkdir(parents=True)
            (root / 'package.json').write_text('{"workspaces":{"packages":["ui","packages/*","!packages/excluded"]}}')
            for name in ('ui', 'packages/sdk', 'packages/declared', 'packages/excluded', 'local/pkg'):
                (root / name / 'package.json').write_text('{}')
            (root / 'packages/sdk/dist/package.json').write_text('{}')
            cors = root / 'node_modules/.pnpm/cors/node_modules/cors/index.js'
            cors.write_text('original cors')
            sdk = root / 'packages/sdk/dist/index.js'
            sdk.write_text('original sdk')
            declared = root / 'packages/declared/dist/index.js'
            declared.write_text('original declared')
            local = root / 'local/pkg/dist/index.js'
            local.write_text('original linked package')
            deep = root / 'arbitrary/deep/node_modules/pkg/index.js'
            deep.write_text('original arbitrary depth')
            ui_link = root / 'ui/node_modules/cors'
            ui_link.symlink_to('../../node_modules/.pnpm/cors/node_modules/cors')
            (root / 'node_modules/sdk').symlink_to('../packages/sdk')
            (root / 'node_modules/local').symlink_to('../local/pkg')
            # Directory links are recorded, not descended (including a cycle).
            (root / 'cycle').symlink_to(root)
            inventory = dependency_roots(root)
            self.assertIn('arbitrary/deep/node_modules', inventory['node_modules'])
            self.assertIn('node_modules/.pnpm/cors/node_modules', inventory['node_modules'])
            self.assertEqual(inventory['workspace_packages'],
                             ['local/pkg', 'packages/declared', 'packages/sdk', 'ui'])
            original = node_modules_hash(root)
            for file in (sdk, declared, local, deep):
                before = file.read_bytes()
                file.write_bytes(before + b'FORGED')
                self.assertNotEqual(node_modules_hash(root), original, file)
                file.write_bytes(before)
            ui_link.unlink()
            ui_link.mkdir()
            (ui_link / 'index.js').write_text('real directory replacement')
            self.assertNotEqual(node_modules_hash(root), original)
            (ui_link / 'index.js').unlink()
            ui_link.rmdir()
            ui_link.symlink_to('../../packages/sdk')
            self.assertNotEqual(node_modules_hash(root), original)
            # A node_modules directory symlink still binds its raw/resolved target.
            linked = root / 'other/node_modules'
            linked.parent.mkdir()
            linked.symlink_to('../node_modules')
            changed = node_modules_hash(root)
            linked.unlink()
            linked.symlink_to('../ui/node_modules')
            self.assertNotEqual(node_modules_hash(root), changed)
            special = root / 'packages/sdk/pipe'
            os.mkfifo(special)
            with self.assertRaisesRegex(ValueError, 'nonregular node_modules'):
                node_modules_hash(root)

    def test_node_ancestor_modules_fail_and_record_warning(self):
        from common import node_ancestor_admission
        import io
        import contextlib
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            baseline = root / 'baseline'
            baseline.mkdir()
            for kind in ('directory', 'broken-link'):
                modules = root / 'node_modules'
                modules.mkdir() if kind == 'directory' else modules.symlink_to(root / 'missing')
                errors = io.StringIO()
                with contextlib.redirect_stderr(errors), self.assertRaisesRegex(ValueError, 'ancestor node_modules'):
                    node_ancestor_admission(baseline)
                self.assertIn('WARNING', errors.getvalue())
                modules.rmdir() if kind == 'directory' else modules.unlink()

    def test_node_children_use_allowlist_and_ignore_preload_and_search_path(self):
        import run
        import os
        import subprocess
        import shutil
        from apparatus import node_environment
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            marker = root / 'NODE_INJECTION_EXECUTED'
            preload = root / 'preload.cjs'
            preload.write_text('require("fs").writeFileSync(' + json.dumps(str(marker)) + ', "injected")')
            with patch.dict(os.environ, {'NODE_PATH': str(root), 'NODE_OPTIONS': '--require=' + str(preload),
                                         'LD_PRELOAD': '/fake', 'PARITY_ATTACK': '1', 'HTTP_PROXY': '/fake'}):
                for env in (node_environment(), run.build_env(root, 'probe'),
                            run.build_env(root, 'enumerate', {'PARITY_BASELINE': str(root)})):
                    self.assertNotIn('NODE_PATH', env)
                    self.assertNotIn('NODE_OPTIONS', env)
                    self.assertNotIn('HTTP_PROXY', env)
                    self.assertNotIn('PARITY_ATTACK', env)
                    self.assertNotIn('LD_PRELOAD', env)
                    result = subprocess.run([shutil.which('node'), '-e',
                                             'console.log(process.env.NODE_PATH || "CLEAN")'],
                                            env=env, capture_output=True, text=True, check=True)
                    self.assertEqual(result.stdout.strip(), 'CLEAN')
            self.assertFalse(marker.exists())

    def test_rust_ancestor_selectors_fail_before_process_or_directory_creation(self):
        from apparatus import cargo_process, prepare_cargo
        import os
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            cwd, home = root / 'outer/batch', root / 'cache/home'
            cwd.mkdir(parents=True)
            home.mkdir(parents=True)
            paths = [cwd / 'rust-toolchain', cwd.parent / 'rust-toolchain.toml',
                     home / 'rust-toolchain.toml', home.parent / 'rust-toolchain',
                     root / 'rust-toolchain.toml']
            for selector in paths:
                for contents in ('[toolchain]\nchannel="1.98"\n', '[toolchain]\npath="/fake"\n'):
                    selector.write_text(contents)
                    with patch('apparatus.run_tool') as proc, self.assertRaisesRegex(ValueError, 'ancestor toolchain'):
                        cargo_process(['cargo', '-V'], cwd, home, cwd / 'target')
                    proc.assert_not_called()
                    with self.assertRaisesRegex(ValueError, 'ancestor toolchain'):
                        prepare_cargo(cwd, home, cwd / 'target', root / 'seed')
                    self.assertFalse((cwd / 'target').exists())
                    selector.unlink()
            selector = home.parent / 'rust-toolchain.toml'
            selector.symlink_to(root / 'absent')
            with self.assertRaisesRegex(ValueError, 'ancestor toolchain'):
                cargo_process(['rustc', '-vV'], cwd, home, cwd / 'target')
            selector.unlink()
            # The Python caller cwd also matters even when Cargo cwd is separate.
            selector = root / 'rust-toolchain'
            selector.write_text('1.98')
            with patch('apparatus.Path.cwd', return_value=root), self.assertRaisesRegex(ValueError, 'ancestor toolchain'):
                cargo_process(['cargo', '-V'], calibrate.ROOT, calibrate.ROOT / '_tmp/home', root / 'target')

    def test_root_path_selector_and_mismatched_rustc_release_fail(self):
        from apparatus import cargo_identity, cargo_environment
        import subprocess
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            selector = root / 'rust-toolchain.toml'
            selector.write_text('[toolchain]\npath="/fake"\n')
            subprocess.run(['git', '-C', str(root), 'add', 'rust-toolchain.toml'], check=True)
            with patch('apparatus.run_tool') as proc, self.assertRaisesRegex(ValueError, 'path= forbidden'):
                cargo_identity(root, root / 'home', root / 'target', root=root)
            proc.assert_not_called()
            selector.write_text('[toolchain]\nchannel="1.99"\n')
            env = cargo_environment(root, root / 'home', root / 'target', root=root)
            self.assertEqual(env['RUSTUP_TOOLCHAIN'], '1.99')
            for release in ('1.98.0', '1.99.1', '1.99.0-nightly', ''):
                with patch('apparatus.cargo_process', return_value=type('Result', (), {'stdout': 'release: ' + release})()), \
                        self.assertRaisesRegex(ValueError, 'release differs'):
                    cargo_identity(root, root / 'home', root / 'target', root=root)
