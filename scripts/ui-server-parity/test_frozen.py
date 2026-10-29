# Isolated bootstrap: verify the source loader before executing local code.
import os as _boot_os
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (fix-r3 P1, same defect as #701 fix-r2). Nonexistent sys.path entries skip.
if any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.isdir(root)
                and _boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))
        for entry in _boot_sys.path):
    print('{"verdict":"FAIL","errors":["apparatus directory on sys.path; run the entry file directly with -I"]}')
    raise SystemExit(1)
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
import unittest
import json
import os
from pathlib import Path
import py_compile
import shutil
import subprocess
import sys
import tempfile
from unittest.mock import patch
from check_frozen import self_test, required_files, ROOT, SPEC
from common import apparatus_hashes
from apparatus import ApparatusUnsafe, EXP008_FILES, scan, git_admission


def copy_apparatus(root):
    shutil.copytree(ROOT / 'scripts/ui-server-parity', root / 'scripts/ui-server-parity')
    shutil.copytree(ROOT / 'scripts/exp008', root / 'scripts/exp008')
    shutil.copytree(ROOT / 'specs/ui-server-parity', root / 'specs/ui-server-parity')
    shutil.copyfile(ROOT / 'rust-toolchain.toml', root / 'rust-toolchain.toml')
    subprocess.run(['git', 'init', '-q', str(root)], check=True)
    subprocess.run(['git', '-C', str(root), 'add', 'scripts', 'specs', 'rust-toolchain.toml'], check=True)



class FrozenControls(unittest.TestCase):
    def test_capture_binds_all_registered_files_except_approval_manifest(self):
        hashes = apparatus_hashes()
        self.assertNotIn('specs/ui-server-parity/FROZEN.md', hashes)
        self.assertIn('specs/ui-server-parity/human-approvers.txt', hashes)
        self.assertTrue({str(p.relative_to(ROOT)) for p in required_files(ROOT, SPEC)} <= set(hashes))

    def test_human_allowlist_tampering_fails_checker_preflight(self):
        from check_frozen import check
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            copy_apparatus(root)
            folder = root / 'specs/ui-server-parity'
            (folder / 'human-approvers.txt').write_text('zhengdaqi\nunauthorized-human\n')
            report = check(folder, root=root)
            self.assertEqual(report['verdict'], 'FAIL', report)
            self.assertTrue(any('human-approvers.txt' in error for error in report['errors']))
            result = subprocess.run([sys.executable, '-I', str(root / 'scripts/ui-server-parity/check_frozen.py')],
                                    cwd=root, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(json.loads(result.stdout)['verdict'], 'FAIL')
            self.assertIn('human-approvers.txt', result.stderr)

    def test_DS_Store_regular_file_is_ignored_by_all_entrypoints_and_hashes(self):
        import common
        entrypoints = ['check_frozen.py', 'run.py', 'calibrate.py', 'check_ruler.py',
                       'compare.py', 'check_coverage.py', 'benchmark_clock.py',
                       'summarize_ruler.py', 'controls/run_all.py']
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            copy_apparatus(root)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=Fixture',
                            '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture', '--no-verify'], check=True)
            with patch.object(common, 'ROOT', root):
                original = apparatus_hashes()
                for name in ('scripts/ui-server-parity/.DS_Store',
                             'scripts/ui-server-parity/controls/.DS_Store', 'scripts/exp008/.DS_Store'):
                    (root / name).write_bytes(b'not executable')
                self.assertEqual(apparatus_hashes(), original)
            self.assertEqual(git_admission(root), subprocess.check_output(
                ['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip())
            for entry in entrypoints:
                result = subprocess.run([sys.executable, '-I', str(root / 'scripts/ui-server-parity' / entry), '--help'],
                                        cwd=root, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, (entry, result.stderr))
            store = root / 'scripts/exp008/.DS_Store'
            store.unlink()
            for kind in ('directory', 'symlink', 'fifo'):
                if kind == 'directory':
                    store.mkdir()
                elif kind == 'symlink':
                    store.symlink_to(root / 'missing')
                else:
                    os.mkfifo(store)
                with self.subTest(kind=kind), self.assertRaises(ApparatusUnsafe):
                    scan(store.parent)
                store.rmdir() if kind == 'directory' else store.unlink()
            (store.parent / '.DS_Store.py').write_text('injected')
            with self.assertRaisesRegex(ApparatusUnsafe, 'untracked apparatus'):
                scan(store.parent)

    def test_toolchain_inputs_are_admitted_and_hashed(self):
        import common
        from check_frozen import check
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            copy_apparatus(root)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=Fixture',
                            '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture', '--no-verify'], check=True)
            with patch.object(common, 'ROOT', root):
                self.assertIn('rust-toolchain.toml', apparatus_hashes())
            path = root / 'rust-toolchain.toml'
            path.write_text('[toolchain]\nchannel="injected"\n')
            self.assertEqual(check(root / 'specs/ui-server-parity', root=root)['verdict'], 'FAIL')
            with self.assertRaisesRegex(ApparatusUnsafe, 'dirty apparatus'):
                git_admission(root)
            rogue = root / 'rust-toolchain'
            rogue.write_text('injected')
            with self.assertRaisesRegex(ApparatusUnsafe, 'untracked/missing toolchain'):
                git_admission(root)

    def test_positive_draft_and_negative_manifest_controls(self):
        report = self_test()
        self.assertEqual(report['verdict'], 'PASS', report)

    def test_checker_and_capture_hashes_reject_the_same_unsafe_tree(self):
        from check_frozen import check
        import common
        for kind in ('symlink-directory', 'symlink-file', 'pyc', 'cache-directory', 'json-file', 'exp008-json', 'exp008-textwrap'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                device = root / 'scripts/ui-server-parity'
                device.mkdir(parents=True)
                (root / 'scripts/exp008').mkdir()
                payload = 'raise RuntimeError(\"unregistered\")'
                outside = root / 'outside'
                outside.mkdir()
                (outside / 'payload.py').write_text('injected')
                if kind == 'symlink-directory':
                    (device / 'json').symlink_to(outside, target_is_directory=True)
                elif kind == 'symlink-file':
                    (device / 'json.py').symlink_to(outside / 'payload.py')
                elif kind == 'pyc':
                    (device / 'common.pyc').write_bytes(b'forged')
                elif kind == 'cache-directory':
                    (device / '__pycache__').mkdir()
                elif kind == 'json-file':
                    (device / 'json.py').write_text(payload + 'print(\"{\\\"verdict\\\":\\\"PASS\\\"}\")\nraise SystemExit(0)\n')
                else:
                    (root / 'scripts/exp008' / ('json.py' if kind == 'exp008-json' else 'textwrap.py')).write_text(payload)
                with self.subTest(kind=kind), patch.object(common, 'ROOT', root):
                    self.assertEqual(check(root / 'specs/ui-server-parity', root=root)['verdict'], 'FAIL')
                    with self.assertRaises(ApparatusUnsafe):
                        apparatus_hashes()

    def test_entrypoints_reject_injection_before_json_or_local_import_without_B(self):
        # All payloads live in disposable copies, never in the real apparatus.
        entrypoints = ['check_frozen.py', 'run.py', 'calibrate.py', 'check_ruler.py',
                       'compare.py', 'check_coverage.py', 'benchmark_clock.py',
                       'summarize_ruler.py', 'controls/run_all.py']
        for kind in ('symlink-directory', 'symlink-file', 'pyc', 'cache-directory', 'json-file', 'exp008-json', 'exp008-textwrap'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                device = root / 'scripts/ui-server-parity'
                copy_apparatus(root)
                marker = root / 'INJECTION_EXECUTED'
                payload = f'from pathlib import Path\nPath({str(marker)!r}).write_text("injected")\nMAX_BRACKET_NS = 10**12\n'
                external = root / '_tmp/jsonpkg'
                external.mkdir(parents=True)
                (external / '__init__.py').write_text(payload)
                if kind == 'symlink-directory':
                    (device / 'json').symlink_to(external, target_is_directory=True)
                elif kind == 'symlink-file':
                    (device / 'json.py').symlink_to(external / '__init__.py')
                elif kind == 'pyc':
                    cache = device / '__pycache__'
                    cache.mkdir()
                    cfile = cache / f'common.{sys.implementation.cache_tag}.pyc'
                    py_compile.compile(str(external / '__init__.py'), cfile=str(cfile), doraise=True)
                    # Forge a matching timestamp/size header, so -B alone would load it.
                    import struct
                    stat = (device / 'common.py').stat()
                    data = cfile.read_bytes()
                    cfile.write_bytes(data[:8] + struct.pack('<II', int(stat.st_mtime), stat.st_size) + data[16:])
                elif kind == 'cache-directory':
                    (device / '__pycache__').mkdir()
                elif kind == 'json-file':
                    (device / 'json.py').write_text(payload + 'print(\"{\\\"verdict\\\":\\\"PASS\\\"}\")\nraise SystemExit(0)\n')
                else:
                    (root / 'scripts/exp008' / ('json.py' if kind == 'exp008-json' else 'textwrap.py')).write_text(payload)
                env = {key: value for key, value in os.environ.items()
                       if key not in ('PYTHONDONTWRITEBYTECODE', 'PYTHONPATH')}
                if kind == 'pyc':
                    # Control: this is executable bytecode, and -B does not prevent loading it.
                    probe = 'import importlib.machinery, sys; m = importlib.machinery.SourcelessFileLoader("payload", sys.argv[1]).load_module(); assert m.MAX_BRACKET_NS == 10**12'
                    result = subprocess.run([sys.executable, '-I', '-c', probe, str(cfile)],
                                            env=env, cwd=root, text=True, capture_output=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(marker.exists())
                    marker.unlink()
                for entry in entrypoints:
                    with self.subTest(kind=kind, entry=entry):
                        result = subprocess.run([sys.executable, '-I', str(device / entry), '--help'],
                                                env=env, cwd=root, text=True, capture_output=True, timeout=10)
                        self.assertEqual(result.returncode, 1, result.stderr)
                        self.assertEqual(json.loads(result.stdout)['verdict'], 'FAIL')
                        self.assertFalse(marker.exists(), result.stderr)

    def test_new_B1_dependencies_are_bound_and_registered(self):
        required = {str(p.relative_to(ROOT)) for p in required_files(ROOT, SPEC)}
        hashes = apparatus_hashes()
        for name in EXP008_FILES:
            path = 'scripts/exp008/' + name
            self.assertIn(path, required)
            self.assertIn(path, hashes)

    def test_source_preflight_never_creates_bytecode_even_without_B(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            device = root / 'scripts/ui-server-parity'
            copy_apparatus(root)
            env = {key: value for key, value in os.environ.items()
                   if key not in ('PYTHONDONTWRITEBYTECODE', 'PYTHONPATH')}
            result = subprocess.run([sys.executable, '-I', str(device / 'check_frozen.py'), '--self-test'],
                                    env=env, cwd=root, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['verdict'], 'PASS')
            self.assertFalse(list(device.rglob('__pycache__')))

    def test_isolation_ignores_python_environment_and_rejects_nonisolated_entry(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            shadow = root / 'shadow'
            shadow.mkdir()
            marker = root / 'ENVIRONMENT_EXECUTED'
            (shadow / 'json.py').write_text(f'open({str(marker)!r}, "w").write("injected")\n')
            cache = root / 'external-cache'
            env = dict(os.environ, PYTHONPATH=str(shadow), PYTHONHOME=str(root / 'fake-home'),
                       PYTHONPYCACHEPREFIX=str(cache), PYTHONUSERBASE=str(shadow))
            isolated = subprocess.run([sys.executable, '-I', str(ROOT / 'scripts/ui-server-parity/check_frozen.py')],
                                      env=env, capture_output=True, text=True, timeout=10)
            # The manifest may be a valid draft (2/DRAFT) or a registered
            # freeze (0/PASS); isolation is the invariant either way.
            draft = '待定标' in (SPEC / 'FROZEN.md').read_text().splitlines()[0]
            self.assertEqual(isolated.returncode, 2 if draft else 0, isolated.stderr)
            self.assertEqual(json.loads(isolated.stdout)['verdict'], 'DRAFT' if draft else 'PASS')
            self.assertFalse(marker.exists())
            self.assertFalse(cache.exists())
            for flags in ([], ['-E', '-s'], ['-I', '-X', f'pycache_prefix={cache}']):
                result = subprocess.run([sys.executable, *flags, str(ROOT / 'scripts/ui-server-parity/check_frozen.py')],
                                        env={k: v for k, v in os.environ.items() if not k.startswith('PYTHON')},
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(json.loads(result.stdout)['verdict'], 'FAIL')
                self.assertFalse(marker.exists())

    def test_git_admission_rejects_dirty_apparatus_and_changed_HEAD(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            copy_apparatus(root)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=Fixture',
                            '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture', '--no-verify'], check=True)
            head = git_admission(root)
            self.assertEqual(git_admission(root, head), head)
            with self.assertRaisesRegex(ApparatusUnsafe, 'HEAD changed'):
                git_admission(root, '0' * 40)
            path = root / 'scripts/exp008/run.py'
            original = path.read_bytes()
            path.write_bytes(original + b'\n# worker drift\n')
            with self.assertRaisesRegex(ApparatusUnsafe, 'dirty apparatus'):
                git_admission(root, head)
            path.write_bytes(original)
            rogue = root / 'scripts/exp008/textwrap.py'
            rogue.write_text('raise RuntimeError("shadow")')
            with self.assertRaisesRegex(ApparatusUnsafe, 'untracked apparatus'):
                git_admission(root, head)
            rogue.unlink()
            # Git status includes staged edits too, even with no unstaged diff.
            path.write_bytes(original + b'\n# staged drift\n')
            subprocess.run(['git', '-C', str(root), 'add', str(path)], check=True)
            with self.assertRaisesRegex(ApparatusUnsafe, 'dirty apparatus'):
                git_admission(root, head)

    def test_checked_source_loader_rejects_changed_bytes_before_execution(self):
        from apparatus import SourceLoader, manifest_pins
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            copy_apparatus(root)
            path = root / 'scripts/ui-server-parity/common.py'
            marker = root / 'CHANGED_SOURCE_EXECUTED'
            path.write_text(f'open({str(marker)!r}, "w").write("injected")')
            module = type(sys)('changed_source')
            with self.assertRaisesRegex(ApparatusUnsafe, 'changed source hash'):
                SourceLoader(path, root, manifest_pins(root)).exec_module(module)
            self.assertFalse(marker.exists())

    def test_b1_load_restores_path_and_reconcile_child_is_isolated(self):
        before = sys.path[:]
        import run
        self.assertEqual(sys.path, before)
        self.assertNotIn(str(ROOT / 'scripts/exp008'), sys.path)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            dependency = root / 'exp008'
            shutil.copytree(ROOT / 'scripts/exp008', dependency)
            marker = root / 'RECONCILE_SHADOW_EXECUTED'
            (dependency / 'json.py').write_text(f'open({str(marker)!r}, "w").write("injected")\n')
            result = subprocess.run([sys.executable, '-I', str(dependency / 'reconcile_routes.py'), '--help'],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(marker.exists())

    def test_documented_tests_use_direct_isolated_files(self):
        readme = (ROOT / 'scripts/ui-server-parity/README.md').read_text()
        block = readme.split('离线验证（不监听）：', 1)[1].split('```sh', 1)[1].split('```', 1)[0]
        self.assertNotIn('discover', block,
                         '#1023-1: unittest discover reinserts the apparatus directory')
        self.assertNotIn('-m unittest', block,
                         '#1023-1: module invocation cannot be guarded before the import')
        tests = sorted(path.name for path in (ROOT / 'scripts/ui-server-parity').glob('test_*.py'))
        self.assertTrue(tests, '#1023-1: the test inventory must not be empty')
        for name in tests:
            self.assertIn(f'python3 -I scripts/ui-server-parity/{name}', block,
                          f'#1023-1: {name} needs an explicit isolated file command')

    def test_entrypoints_reject_reinserted_apparatus_directory(self):
        apparatus = ROOT / 'scripts/ui-server-parity'
        entries = [path for path in sorted(apparatus.rglob('*.py'))
                   if '# Isolated bootstrap' in path.read_text()]
        self.assertGreaterEqual(len(entries), 20, '#1023-2: guard inventory must not be empty')
        probe = ('import runpy, sys\n'
                 'sys.path.insert(0, sys.argv[1])\n'
                 'runpy.run_path(sys.argv[2], run_name="__main__")\n')
        for path in entries:
            relative = path.relative_to(ROOT)
            with self.subTest(entry=str(relative)):
                result = subprocess.run(
                    [sys.executable, '-I', '-c', probe, str(apparatus), str(path), '--help'],
                    cwd=ROOT, capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 1,
                                 f'#1023-2: {relative} must reject a reinserted apparatus directory')
                self.assertIn('apparatus directory on sys.path', result.stdout,
                              f'#1023-2: {relative} must fail at the guard, not a later import')

    def test_entry_guards_compare_directory_identity(self):
        apparatus = ROOT / 'scripts/ui-server-parity'
        entries = [path for path in sorted(apparatus.rglob('*.py'))
                   if '# Isolated bootstrap' in path.read_text()]
        self.assertGreaterEqual(len(entries), 20, '#1023-5: guard inventory must not be empty')
        for path in entries:
            relative = path.relative_to(ROOT)
            with self.subTest(entry=str(relative)):
                text = path.read_text()
                head = text[:text.index('raise SystemExit(1)',
                                        text.index('apparatus directory on sys.path'))]
                self.assertIn('_boot_os.path.samefile', head,
                              f'#1023-5: {relative} must compare directory identity, not spelling')
                self.assertNotIn('realpath', head,
                                 f'#1023-5: {relative} must not compare realpath strings')

    def test_entry_guards_reject_alias_spellings(self):
        apparatus = ROOT / 'scripts/ui-server-parity'
        entries = [path for path in sorted(apparatus.rglob('*.py'))
                   if '# Isolated bootstrap' in path.read_text()]
        self.assertGreaterEqual(len(entries), 20, '#1023-6: guard inventory must not be empty')
        probe = ('import runpy, sys\n'
                 'sys.path.insert(0, sys.argv[1])\n'
                 'runpy.run_path(sys.argv[2], run_name="__main__")\n')
        marker = '#1023-6: the guard must reject an analogous directory spelling'
        with tempfile.TemporaryDirectory(prefix='b2-alias-') as temp:
            link = Path(temp) / 'linked-apparatus'
            link.symlink_to(apparatus, target_is_directory=True)
            aliases = [link]
            case = apparatus.with_name(apparatus.name.upper())
            if case.is_dir():
                try:
                    same_case = os.path.samefile(case, apparatus)
                except OSError:
                    same_case = False
                if same_case:
                    aliases.append(case)
            for path in entries:
                relative = path.relative_to(apparatus)
                label = str(path.relative_to(ROOT))
                forms = ([(alias, path) for alias in aliases]
                         + [(apparatus, alias / relative) for alias in aliases])
                for on_path, entry in forms:
                    with self.subTest(entry=label, spelling=str(on_path)):
                        result = subprocess.run(
                            [sys.executable, '-I', '-c', probe, str(on_path),
                             str(entry), '--help'],
                            cwd=ROOT, capture_output=True, text=True, timeout=60)
                        self.assertEqual(
                            result.returncode, 1,
                            f'{marker}: {label} via {on_path} was admitted '
                            f'(rc={result.returncode})')
                        self.assertIn(
                            'apparatus directory on sys.path', result.stdout,
                            f'{marker}: {label} via {on_path} failed later than the guard: '
                            + (result.stdout + result.stderr)[:200])

    def test_discover_form_is_rejected_before_importing_a_test(self):
        result = subprocess.run(
            [sys.executable, '-I', '-B', '-m', 'unittest', 'discover',
             '-s', str(ROOT / 'scripts/ui-server-parity'), '-p', 'test_frozen.py'],
            cwd=ROOT, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 1, '#1023-3: discover must be rejected')
        self.assertIn('apparatus directory on sys.path', result.stdout + result.stderr,
                      '#1023-3: the guard must name discover reinsertion, not a later failure')

    def test_every_test_file_runs_by_direct_isolated_entry(self):
        for path in sorted((ROOT / 'scripts/ui-server-parity').glob('test_*.py')):
            result = subprocess.run([sys.executable, '-I', str(path), '-h'],
                                    cwd=ROOT, capture_output=True, text=True, timeout=120)
            self.assertEqual(result.returncode, 0,
                             f'#1023-4: {path.name} must run as an explicit file: ' + result.stderr)
            self.assertIn('usage:', (result.stdout + result.stderr).lower(),
                          f'#1023-4: {path.name} must reach unittest.main(), not just exit 0')


if __name__ == '__main__':
    unittest.main()
