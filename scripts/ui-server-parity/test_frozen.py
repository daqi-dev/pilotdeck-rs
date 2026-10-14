# Admit source-only apparatus before json or any local module can be imported.
import os as _boot_os
import sys as _boot_sys
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
with open(_boot_source, 'rb') as _boot_file:
    exec(compile(_boot_file.read(), _boot_source, 'exec'), _boot_namespace)
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
from apparatus import ApparatusUnsafe, EXP008_FILES, scan


class FrozenControls(unittest.TestCase):
    def test_capture_binds_all_registered_files_except_approval_manifest(self):
        hashes = apparatus_hashes()
        self.assertNotIn('specs/ui-server-parity/FROZEN.md', hashes)
        self.assertTrue({str(p.relative_to(ROOT)) for p in required_files(ROOT, SPEC)} <= set(hashes))

    def test_positive_draft_and_negative_manifest_controls(self):
        report = self_test()
        self.assertEqual(report['verdict'], 'PASS', report)

    def test_checker_and_capture_hashes_reject_the_same_unsafe_tree(self):
        from check_frozen import check
        import common
        for kind in ('symlink-directory', 'symlink-file', 'pyc', 'cache-directory'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                device = root / 'scripts/ui-server-parity'
                device.mkdir(parents=True)
                (root / 'scripts/exp008').mkdir()
                outside = root / 'outside'
                outside.mkdir()
                (outside / 'payload.py').write_text('injected')
                if kind == 'symlink-directory':
                    (device / 'json').symlink_to(outside, target_is_directory=True)
                elif kind == 'symlink-file':
                    (device / 'json.py').symlink_to(outside / 'payload.py')
                elif kind == 'pyc':
                    (device / 'common.pyc').write_bytes(b'forged')
                else:
                    (device / '__pycache__').mkdir()
                with self.subTest(kind=kind), patch.object(common, 'ROOT', root):
                    self.assertEqual(check(root / 'specs/ui-server-parity', root=root)['verdict'], 'FAIL')
                    with self.assertRaises(ApparatusUnsafe):
                        apparatus_hashes()

    def test_entrypoints_reject_injection_before_json_or_local_import_without_B(self):
        # All payloads live in disposable copies, never in the real apparatus.
        entrypoints = ['check_frozen.py', 'run.py', 'calibrate.py', 'check_ruler.py',
                       'compare.py', 'check_coverage.py', 'benchmark_clock.py',
                       'summarize_ruler.py', 'controls/run_all.py']
        for kind in ('symlink-directory', 'symlink-file', 'pyc', 'cache-directory'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                device = root / 'scripts/ui-server-parity'
                shutil.copytree(ROOT / 'scripts/ui-server-parity', device)
                (root / 'scripts/exp008').mkdir()
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
                else:
                    (device / '__pycache__').mkdir()
                env = {key: value for key, value in os.environ.items()
                       if key not in ('PYTHONDONTWRITEBYTECODE', 'PYTHONPATH')}
                if kind == 'pyc':
                    # Control: this is executable bytecode, and -B does not prevent loading it.
                    probe = 'import sys; sys.path.insert(0, sys.argv[1]); import common; assert common.MAX_BRACKET_NS == 10**12'
                    result = subprocess.run([sys.executable, '-B', '-c', probe, str(device)],
                                            env=env, cwd=root, text=True, capture_output=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(marker.exists())
                    marker.unlink()
                for entry in entrypoints:
                    with self.subTest(kind=kind, entry=entry):
                        result = subprocess.run([sys.executable, str(device / entry), '--help'],
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
            shutil.copytree(ROOT / 'scripts/ui-server-parity', device)
            (root / 'scripts/exp008').mkdir()
            env = {key: value for key, value in os.environ.items()
                   if key not in ('PYTHONDONTWRITEBYTECODE', 'PYTHONPATH')}
            result = subprocess.run([sys.executable, str(device / 'check_frozen.py'), '--self-test'],
                                    env=env, cwd=root, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['verdict'], 'PASS')
            self.assertFalse(list(device.rglob('__pycache__')))
