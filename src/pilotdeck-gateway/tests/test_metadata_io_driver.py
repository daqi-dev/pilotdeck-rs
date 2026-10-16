"""Toolchain starts are mocked; watchdog controls run only this Python interpreter."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('metadata_io_driver', Path(__file__).with_name('check_metadata_io.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class EnvironmentControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        (self.root / 'rust-toolchain.toml').write_text('[toolchain]\nchannel="1.99"\n')
        self.home = self.root / 'original-home'
        (self.home / '.cargo').mkdir(parents=True)
        self.toolchain = self.home / '.rustup/toolchains/1.99-mock-host'
        (self.toolchain / 'bin').mkdir(parents=True)
        for name in ('cargo', 'rustc', 'rustdoc', 'cargo-clippy', 'clippy-driver', 'rustfmt'):
            path = self.toolchain / 'bin' / name
            path.write_text('mock only; never execute\n')
            path.chmod(0o755)
        lib = self.toolchain / 'lib/rustlib'
        (lib / 'mock-host/lib').mkdir(parents=True)
        (lib / 'components').write_text('\n'.join(name+'-mock-host' for name in
            ('cargo', 'rustc', 'rust-std', 'clippy-preview', 'rustfmt-preview')))
        self.original = dict(HOME=str(self.home), PATH='/original/bin', RUSTC_WRAPPER='untrusted-wrapper',
                             RUSTC_WORKSPACE_WRAPPER='untrusted-workspace-wrapper',
                             CARGO_BUILD_RUSTC_WRAPPER='untrusted-build-wrapper',
                             CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='untrusted-build-workspace-wrapper',
                             RUSTUP_TOOLCHAIN='missing-inherited-toolchain')

    def test_node_home_never_flows_into_cargo_and_executables_are_explicit(self):
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('no process allowed')) as process:
            preflight = driver.toolchain_preflight(self.toolchain, self.original, self.root)
            cargo, node = driver.environments(self.original, preflight, self.root/'scratch', self.root/'target')
            self.assertEqual(cargo['HOME'], str(self.home))
            self.assertEqual(node['HOME'], str(self.root/'scratch/isolation/home'))
            self.assertNotEqual(cargo['HOME'], node['HOME'])
            self.assertEqual(cargo['CARGO_HOME'], str(self.home/'.cargo'))
            self.assertEqual(cargo['RUSTUP_HOME'], str(self.home/'.rustup'))
            self.assertEqual(cargo['RUSTUP_AUTO_INSTALL'], '0')
            self.assertEqual(cargo['RUSTC'], str(self.toolchain/'bin/rustc'))
            self.assertEqual(preflight['binaries']['cargo']['path'], str(self.toolchain/'bin/cargo'))
            for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                        'CARGO_BUILD_RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER'):
                self.assertEqual(cargo[key], '')
            self.assertNotIn('RUSTUP_TOOLCHAIN', cargo)
            process.assert_not_called()

    def test_original_home_spelling_is_preserved_even_when_it_is_a_symlink(self):
        alias = self.root/'home-alias'
        alias.symlink_to(self.home, target_is_directory=True)
        original = dict(self.original, HOME=str(alias))
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('no process allowed')) as process:
            preflight = driver.toolchain_preflight(self.toolchain, original, self.root)
            cargo, _ = driver.environments(original, preflight, self.root/'scratch', self.root/'target')
            self.assertEqual(cargo['HOME'], str(alias))
            process.assert_not_called()

    def test_missing_installation_is_rejected_before_any_proxy(self):
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('proxy started')) as process:
            with self.assertRaises((FileNotFoundError, RuntimeError)):
                driver.toolchain_preflight(self.toolchain/'missing', self.original, self.root)
            process.assert_not_called()

    def test_incident_or_redirected_installation_is_rejected_without_starting_it(self):
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('proxy started')) as process:
            outside = self.root / 'scratch/downloaded'
            outside.mkdir(parents=True)
            with self.assertRaises(RuntimeError):
                driver.toolchain_preflight(outside, self.original, self.root)
            cargo = self.toolchain/'bin/cargo'
            cargo.unlink()
            cargo.symlink_to(self.root/'nonoriginal-proxy')
            with self.assertRaises(RuntimeError):
                driver.toolchain_preflight(self.toolchain, self.original, self.root)
            process.assert_not_called()

    def test_missing_component_fails_before_any_proxy(self):
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('proxy started')) as process:
            (self.toolchain/'lib/rustlib/components').write_text('cargo-mock-host\n')
            with self.assertRaises(RuntimeError):
                driver.toolchain_preflight(self.toolchain, self.original, self.root)
            process.assert_not_called()

    def test_remaining_preflight_failures_never_start_a_process(self):
        changes = ('relative-home', 'missing-cargo-home', 'wrong-channel', 'missing-std', 'not-executable')
        for change in changes:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temp:
                # Each branch gets an independent mock installation.
                case = EnvironmentControls('test_missing_component_fails_before_any_proxy')
                case.setUp()
                self.addCleanup(case.doCleanups)
                original = dict(case.original)
                if change == 'relative-home': original['HOME'] = 'relative'
                if change == 'missing-cargo-home': original['CARGO_HOME'] = str(Path(temp)/'absent')
                if change == 'wrong-channel': (case.root/'rust-toolchain.toml').write_text('[toolchain]\nchannel="1.98"\n')
                if change == 'missing-std': (case.toolchain/'lib/rustlib/mock-host/lib').rmdir()
                if change == 'not-executable': (case.toolchain/'bin/cargo').chmod(0o644)
                with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('no process allowed')) as process:
                    with self.assertRaises((OSError, RuntimeError)):
                        driver.toolchain_preflight(case.toolchain, original, case.root)
                    process.assert_not_called()

    def test_environment_ignores_rustflags_cargo_and_node_injection(self):
        original = dict(self.original, RUSTFLAGS='--bad-flag', CARGO_ENCODED_RUSTFLAGS='bad',
                        CARGO_BUILD_RUSTFLAGS='bad', CARGO_INCREMENTAL='bad', CARGO_HOME_POISON='bad',
                        LD_PRELOAD='spy', DYLD_INSERT_LIBRARIES='spy', NODE_OPTIONS='--require spy', NODE_PATH='spy')
        pf = driver.toolchain_preflight(self.toolchain, original, self.root)
        cargo, node = driver.environments(original, pf, self.root/'scratch', self.root/'target', '/fixed/node', '/fixed/git')
        for key in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_RUSTFLAGS', 'CARGO_INCREMENTAL',
                    'CARGO_HOME_POISON', 'LD_PRELOAD', 'DYLD_INSERT_LIBRARIES', 'NODE_OPTIONS', 'NODE_PATH'):
            self.assertNotIn(key, cargo)
            self.assertNotIn(key, node)
        self.assertIn('/fixed', node['PATH'])
        self.assertNotIn('CARGO_HOME', node)
        self.assertNotIn('RUSTUP_HOME', node)

    def test_source_inventory_is_exactly_the_twelve_required_files(self):
        expected = {
            'src/pilotdeck-gateway/src/metadata.rs', 'src/pilotdeck-gateway/src/metadata/tests.rs',
            'src/pilotdeck-gateway/src/pairing/tests.rs', 'src/pilotdeck-gateway/tests/record_metadata_io.mts',
            'src/pilotdeck-gateway/tests/check_metadata_io.py', 'src/pilotdeck-gateway/tests/test_metadata_io_driver.py',
            'src/pilotdeck-gateway/tests/baseline_source_guard.mjs', 'src/pilotdeck-gateway/tests/test_baseline_source_guard.mjs',
            '.github/workflows/ci.yml', 'rust-toolchain.toml', 'src/Cargo.toml', 'src/Cargo.lock',
        }
        self.assertEqual(len(driver.SOURCE_FILES), 12)
        self.assertEqual(set(driver.SOURCE_FILES), expected)

    def test_target_output_overlap_file_and_nonempty_target_are_rejected_without_writes(self):
        target = self.root/'new-target'
        for output in (target, target/'inside', self.root):
            with self.assertRaisesRegex(RuntimeError, 'disjoint'):
                driver.validate_output_target(target, output)
            self.assertFalse(target.exists())
        target.write_text('owned sentinel')
        with self.assertRaisesRegex(RuntimeError, 'directory'):
            driver.validate_output_target(target, self.root/'separate')
        self.assertEqual(target.read_text(), 'owned sentinel')
        target.unlink();target.mkdir();(target/'sentinel').write_text('keep')
        with self.assertRaisesRegex(RuntimeError, 'fresh target required'):
            driver.validate_output_target(target,self.root/'separate')
        self.assertEqual((target/'sentinel').read_text(),'keep')

    def test_sources_missing_or_changed_are_not_silent_none(self):
        with patch.object(driver, 'SOURCE_FILES', ('source.rs',)):
            with self.assertRaises(RuntimeError): driver.source_snapshot(self.root)
            source = self.root/'source.rs'
            source.write_text('before')
            before = driver.source_snapshot(self.root)
            source.write_text('after')
            self.assertNotEqual(before, driver.source_snapshot(self.root))

    def test_tool_and_rustup_postflight_changes_are_observable(self):
        before = driver.rustup_snapshot(self.home/'.rustup')
        (self.toolchain/'bin/rustc').write_text('changed mock bytes')
        after = driver.rustup_snapshot(self.home/'.rustup')
        self.assertNotEqual(before, after)
        (self.home/'.rustup/downloads').mkdir()
        self.assertNotEqual(after, driver.rustup_snapshot(self.home/'.rustup'))

    def test_receipt_missing_duplicate_extra_wrong_or_empty_ids_fail(self):
        ids = {str(n) for n in range(10)}
        good = ''.join('MARK '+str(n)+'\n' for n in range(10))
        driver.check_receipts(good, 'MARK', ids)
        for broken in ('', good.rsplit('MARK ', 1)[0], good+'MARK 0\n', good.replace('MARK 9', 'MARK 0'), good.replace('MARK 9', 'MARK other')):
            with self.subTest(broken=broken), self.assertRaises(RuntimeError):
                driver.check_receipts(broken, 'MARK', ids)
        with self.assertRaises(RuntimeError): driver.check_receipts(good, 'MARK', ids-{str(9)}|{''})

    def test_restore_requires_receipt_owner_count_and_real_modes(self):
        import json
        scratch = self.root/'restore'; scratch.mkdir()
        (scratch/'.metadata-oracle-owner').write_text('owner')
        permissions = []
        for n in range(3):
            path = scratch/str(n); path.mkdir(); path.chmod(0o755)
            permissions.append(dict(path=str(path), mode=0o755))
        fixture = self.root/'oracle.json'
        plan=dict(owner='owner', scratch=str(scratch), permissions=permissions)
        fixture.write_text(json.dumps(plan))
        receipt = Path(str(fixture)+'.cleanup.json')
        good = dict(owner='owner', restored=3)
        with self.assertRaises(FileNotFoundError): driver.check_restore(fixture,plan)
        for data in ({}, dict(good, owner='wrong'), dict(good, restored=0), dict(good, restored=True)):
            receipt.write_text(json.dumps(data))
            with self.assertRaises(RuntimeError): driver.check_restore(fixture,plan)
        receipt.write_text(json.dumps(good)); driver.check_restore(fixture,plan)
        Path(permissions[0]['path']).chmod(0o111)
        with self.assertRaises(RuntimeError): driver.check_restore(fixture,plan)
        Path(permissions[0]['path']).chmod(0o755)
        (scratch/'.metadata-oracle-owner').write_text('wrong')
        with self.assertRaises(RuntimeError): driver.check_restore(fixture,plan)

    def test_postflight_rejects_source_binary_component_and_cache_changes(self):
        for change in ('source', 'binary', 'components', 'rustup-entry', 'node-cache'):
            with self.subTest(change=change):
                case = EnvironmentControls('test_missing_component_fails_before_any_proxy'); case.setUp()
                try:
                    source=case.root/'source.rs'; source.write_text('original')
                    with patch.object(driver, 'SOURCE_FILES', ('source.rs',)):
                        pf=driver.toolchain_preflight(case.toolchain,case.original,case.root)
                        before=driver.source_snapshot(case.root); rustup=driver.rustup_snapshot(case.home/'.rustup')
                        node=case.toolchain/'bin/rustfmt'; node_sha=driver.sha(node); node_home=case.root/'node-home';node_home.mkdir()
                        driver.check_postflight({},case.root,case.toolchain,case.original,pf,before,rustup,node,node_sha,node_home)
                        if change=='source': source.write_text('changed')
                        if change=='binary': (case.toolchain/'bin/rustc').write_text('changed binary')
                        if change=='components': (case.toolchain/'lib/rustlib/components').write_text('cargo-mock-host')
                        if change=='rustup-entry': (case.home/'.rustup/tmp').mkdir()
                        if change=='node-cache': (node_home/'.rustup').mkdir()
                        with self.assertRaises((OSError,RuntimeError)):
                            driver.check_postflight({},case.root,case.toolchain,case.original,pf,before,rustup,node,node_sha,node_home)
                finally: case.doCleanups()

    def test_workspace_git_is_rejected_before_execution(self):
        shim=self.root/'git';shim.write_text('never execute');shim.chmod(0o755)
        with patch.object(driver.subprocess,'Popen',side_effect=AssertionError('no process')) as process:
            with self.assertRaises(RuntimeError):driver.external_git(shim,(self.root,))
            process.assert_not_called()

    def test_protected_outputs_are_rejected_without_writes(self):
        before = {str(p.relative_to(self.home)) for p in self.home.rglob('*')}
        for suffix in ('.pilotdeck', '.cargo', '.rustup', 'ws/PilotDeck-rust-program-20261009'):
            destination = self.home/suffix/'new'
            self.assertFalse(os.path.lexists(destination))
            with self.assertRaises(RuntimeError): driver.reject_protected_paths((destination,), self.home)
            self.assertFalse(os.path.lexists(destination))
        alias = self.root/'protected-alias';alias.symlink_to(self.home/'.rustup',target_is_directory=True)
        with self.assertRaises(RuntimeError): driver.reject_protected_paths((alias/'new',),self.home)
        self.assertFalse(os.path.lexists(alias/'new'))
        self.assertEqual(before,{str(p.relative_to(self.home)) for p in self.home.rglob('*')})


class FailureRestoreControls(unittest.TestCase):
    setUp = EnvironmentControls.setUp
    def test_main_failure_still_restores_and_noop_exit_zero_fails(self):
        import contextlib
        import io
        import json
        for scenario in ('replay-failure', 'noop-restore', 'source-head-failure', 'source-head-timeout', 'source-head-spawn-error', 'source-head-empty', 'source-head-polluted', 'source-head-short', 'permission-target-replaced', 'permission-mode-replaced', 'fixture-deleted', 'record-failure', 'record-timeout', 'success'):
            with self.subTest(scenario=scenario):
                output = self.root / scenario
                pf = driver.toolchain_preflight(self.toolchain, self.original, self.root)
                names = []
                permissions, decoys = [], []
                def fake_run(command, cwd, environment, log, **kwargs):
                    name = log.stem; names.append(name)
                    text = ''
                    code, reason = 0, None
                    fixture = output / 'metadata-io.json'
                    if name == 'record':
                        scratch = output / 'recording'
                        (scratch / '.metadata-oracle-owner').write_text('owner')
                        for n in range(1 if scenario.startswith('record-') else 3):
                            path = scratch / str(n); path.mkdir(); path.chmod(0o111)
                            permissions.append(dict(path=str(path), mode=0o755))
                        data=dict(baselineCommit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',owner='owner',scratch=str(scratch),
                                  permissions=permissions,cases=[dict(id=str(n),status='RUN') for n in range(10)])
                        text=''.join('PDRS_METADATA_PERMISSION '+json.dumps(dict(
                            baselineCommit=data['baselineCommit'],owner='owner',scratch=str(scratch),permission=item))+'\n' for item in permissions)
                        if scenario.startswith('record-'):
                            code,reason = (9,None) if scenario=='record-failure' else (-9,'timeout')
                        else:
                            serialized=json.dumps(data);fixture.write_text(serialized)
                            text+='PDRS_METADATA_ORACLE '+json.dumps(serialized)+'\n'
                    if name == 'replay':
                        if scenario == 'replay-failure': code = 9
                        text = 'test result: ok. 3 passed; 0 failed; 2 ignored;\n'
                    if name == 'native-retained':
                        text = 'test result: ok. 2 passed; 0 failed; 0 ignored;\n'
                        text += ''.join('PDRS_NATIVE_METADATA_OK '+str(n)+'\n' for n in range(10))
                        if scenario in ('permission-target-replaced','permission-mode-replaced'):
                            data=json.loads(fixture.read_text())
                            if scenario=='permission-target-replaced':
                                for n in range(3):
                                    path=output/'recording'/('decoy-'+str(n));path.mkdir();path.chmod(0o700)
                                    decoys.append(path)
                                data['permissions']=[dict(path=str(path),mode=0o755) for path in decoys]
                            else:data['permissions']=[dict(item,mode=0o111) for item in permissions]
                            fixture.write_text(json.dumps(data))
                        if scenario=='fixture-deleted':fixture.unlink()

                    if name in ('replay', 'native-retained'):
                        text += ''.join('PDRS_METADATA_PAIR_OK '+str(n)+'\n' for n in range(10))
                    if name == 'source-head':
                        self.assertTrue(kwargs['discard_stderr'])
                        text = 'a'*40+'\n'
                        if scenario == 'source-head-failure': code = 9
                        if scenario == 'source-head-timeout': code, reason = -9, 'timeout'
                        if scenario == 'source-head-spawn-error': raise OSError('mock Git spawn denied')
                        if scenario == 'source-head-empty': text = ''
                        if scenario == 'source-head-polluted': text = 'warning\n'+'a'*40+'\n'
                        if scenario == 'source-head-short': text = 'a'*39+'\n'
                    if name == 'restore':
                        if scenario != 'noop-restore':
                            trusted=json.loads(command[-1])
                            self.assertEqual(trusted['permissions'],permissions)
                            for item in trusted['permissions']: Path(item['path']).chmod(item['mode'])
                        # A plausible receipt plus exit0 still cannot hide wrong modes.
                        Path(str(fixture)+'.cleanup.json').write_text(json.dumps(dict(owner='owner', restored=len(permissions))))
                    log.write_text(text)
                    return dict(exit=code, reason=reason, reaped=True, log=str(log),stdout=text)
                with patch.dict(driver.os.environ, self.original, clear=True), \
                     patch.object(driver, 'toolchain_preflight', return_value=pf), \
                     patch.object(driver, 'source_snapshot', return_value={}), \
                     patch.object(driver, 'rustup_snapshot', return_value={}), \
                     patch.object(driver.shutil, 'which', return_value=str(self.toolchain/'bin/cargo')), \
                     patch.object(driver, 'run_bounded', side_effect=fake_run), \
                     patch.object(driver, 'check_postflight'), \
                     patch.object(driver, 'external_git', side_effect=lambda executable, roots: executable), \
                     patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('no external process')), \
                     contextlib.redirect_stdout(io.StringIO()):
                    code = driver.main(['--baseline',str(self.root), '--output',str(output),
                        '--target',str(self.root/'new-target'), '--toolchain',str(self.toolchain)])
                result = json.loads((output/'verification.json').read_text())
                self.assertEqual(code, 0 if scenario=='success' else 1)
                self.assertEqual(result['success'], scenario=='success')
                if scenario=='success':self.assertEqual(result['sourceHead'],'a'*40)
                self.assertEqual(names[-2:], ['restore','source-head'])
                if scenario.startswith('record-'):
                    self.assertEqual(names,['record','restore','source-head'])
                if scenario == 'replay-failure':
                    self.assertEqual(names, ['record', 'replay', 'restore', 'source-head'])
                    for item in permissions:self.assertEqual(os.stat(item['path']).st_mode & 0o777, 0o755)
                elif scenario == 'noop-restore':
                    self.assertIn('permission not restored', result['error'])
                    for item in permissions:Path(item['path']).chmod(0o755)
                elif scenario.startswith('source-head-'):
                    expected = {'source-head-failure':'exit=9', 'source-head-timeout':'reason=timeout',
                                'source-head-spawn-error':'mock Git spawn denied', 'source-head-empty':'forty lowercase hex',
                                'source-head-polluted':'forty lowercase hex','source-head-short':'forty lowercase hex'}[scenario]
                    self.assertIn(expected,result['error'])
                    self.assertIsNone(result['sourceHead'])
                if scenario in ('permission-target-replaced','permission-mode-replaced','fixture-deleted'):
                    self.assertTrue(result['oracleGuard']['restored'])
                    self.assertFalse(result['oracleGuard']['artifactUnchanged'])
                if scenario != 'noop-restore':
                    for item in permissions:self.assertEqual(os.stat(item['path']).st_mode & 0o777,0o755)
                for path in decoys:self.assertEqual(os.stat(path).st_mode & 0o777,0o700)
                if scenario not in ('replay-failure','record-failure','record-timeout'):
                    native = next(r for r in result['runs'] if r['name']=='native-retained')
                    replay = next(r for r in result['runs'] if r['name']=='replay')
                    self.assertEqual(native['environment']['PDRS_METADATA_IO_LIVE'],str(output/'metadata-io.json'))
                    self.assertNotIn('PDRS_METADATA_IO_LIVE',replay['environment'])


class WatchdogControls(unittest.TestCase):
    def test_real_python_success_timeout_and_hard_output_limit(self):
        import sys
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            cases=[('success', 'print("ok")', 5, None),
                   ('timeout', 'import time; time.sleep(5)', 0.05, 'timeout'),
                   ('output', 'import os; os.write(1,b"x"*1000000)', 5, 'output limit'),
                   ('exact-cap', 'import os; os.write(1,b"x"*128)', 5, None),
                   ('cap-plus-one', 'import os; os.write(1,b"x"*129)', 5, 'output limit')]
            for name,code,deadline,reason in cases:
                log=root/(name+'.log')
                result=driver.run_bounded([sys.executable,'-I','-c',code],root,{},log,deadline=deadline,limit=128)
                self.assertEqual(result['reason'],reason)
                self.assertTrue(result['reaped'])
                self.assertLessEqual(log.stat().st_size,128)
                if reason is None:self.assertEqual(result['exit'],0)
                if name in ('exact-cap','cap-plus-one'):self.assertEqual(log.read_bytes(),b'x'*128)

    def test_exited_leader_descendants_are_killed_with_and_without_stdout(self):
        import subprocess
        import sys
        import time
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            for holds_stdout,reason in ((True,'timeout'),(False,'surviving descendants')):
                pidfile=root/('held.pid' if holds_stdout else 'closed.pid')
                child_code='import time; time.sleep(60)'
                leader=('import subprocess,sys,pathlib; '
                    'child=subprocess.Popen([sys.executable,"-I","-c",'+repr(child_code)+']'
                    + (')' if holds_stdout else ',stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)')
                    + '; pathlib.Path('+repr(str(pidfile))+').write_text(str(child.pid))')
                result=driver.run_bounded([sys.executable,'-I','-c',leader],root,{},
                    root/(pidfile.stem+'.log'),deadline=0.2)
                self.assertEqual(result['reason'],reason)
                self.assertTrue(result['reaped'])
                pid=int(pidfile.read_text())
                state=''
                for _ in range(100):
                    state=subprocess.run(['/bin/ps','-o','stat=','-p',str(pid)],capture_output=True,text=True).stdout.strip()
                    if not state or state.startswith('Z'):break
                    time.sleep(0.01)
                self.assertTrue(not state or state.startswith('Z'),f'descendant still running: {pid} {state}')
                print(f'descendant stdout={holds_stdout}: leaderReaped={result["reaped"]}, reason={reason}, childState={state or "absent"}')

    def test_identity_stdout_is_not_polluted_by_stderr(self):
        import sys
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            code='import os; os.write(2,b"warning on stderr\\n"); os.write(1,b"'+('a'*40)+'\\n")'
            merged=driver.run_bounded([sys.executable,'-I','-c',code],root,{},root/'merged.log')
            self.assertEqual(merged['exit'],0)
            self.assertIn('warning on stderr',merged['stdout'])
            self.assertIsNone(driver.re.fullmatch('[0-9a-f]{40}',merged['stdout'].strip()))
            result=driver.run_bounded([sys.executable,'-I','-c',code],root,{},root/'head.log',discard_stderr=True)
            self.assertEqual(result['exit'],0)
            self.assertIsNone(result['reason'])
            self.assertEqual(result['stdout'],'a'*40+'\n')
            self.assertEqual((root/'head.log').read_text(),'a'*40+'\n')

    def test_exit_between_poll_and_kill_is_expected(self):
        from types import SimpleNamespace
        with patch.object(driver.os,'killpg',side_effect=ProcessLookupError) as killpg:
            self.assertTrue(driver.stop_group(SimpleNamespace(pid=123)))
            killpg.assert_called_once_with(123,driver.signal.SIGKILL)


if __name__ == '__main__':
    unittest.main()
