# Isolated bootstrap: verify the source loader before executing local code.
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:
    print('{"verdict":"FAIL","errors":["os must be preloaded from CPython 3.11+ frozen modules"]}')
    raise SystemExit(1)
import os as _boot_os
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
# #2513 fix-r13 (DS sibling#1): the admission read below must answer from
# this checkout only — bare ``git -C`` inherits a steered GIT_DIR/GIT_WORK_TREE
# (measured) and would admit a decoy repository's HEAD. Same hardened form as
# check_frozen.py, the git() helper and the cli-parity/gateway _head_source twins.
_boot_git_env = {'PATH': '/usr/bin:/bin', 'GIT_CONFIG_NOSYSTEM': '1',
                 'GIT_CONFIG_GLOBAL': _boot_os.devnull}


def _boot_git(*args):
    return _boot_subprocess.check_output(
        ['/usr/bin/git', '-c', 'safe.directory=*', '-C', _boot_root, *args],
        env=_boot_git_env, stderr=_boot_subprocess.PIPE, timeout=30)


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
        _boot_trusted = _boot_git('show', 'HEAD:scripts/ui-server-parity/apparatus.py')
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import ast
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
from apparatus import ApparatusUnsafe, EXP008_FILES, scan, git_admission, manifest_pins


def copy_apparatus(root):
    shutil.copytree(ROOT / 'scripts/ui-server-parity', root / 'scripts/ui-server-parity')
    shutil.copytree(ROOT / 'scripts/exp008', root / 'scripts/exp008')
    shutil.copytree(ROOT / 'specs/ui-server-parity', root / 'specs/ui-server-parity')
    shutil.copyfile(ROOT / 'rust-toolchain.toml', root / 'rust-toolchain.toml')
    subprocess.run(['git', 'init', '-q', str(root)], check=True)
    subprocess.run(['git', '-C', str(root), 'add', 'scripts', 'specs', 'rust-toolchain.toml'], check=True)


def admission_anchor_index(statements):
    """Index of the top-level exec(compile( anchor, or None (#2513 fix-r14).

    The anchor is the structural fact that a file is a bootstrap entry: the
    admission region is everything before it, and both the machine check and
    the decoy inventory below enumerate files by it instead of by a comment or
    a count a new bootstrap could drift past.
    """
    for index, statement in enumerate(statements):
        value = getattr(statement, 'value', None)
        if (isinstance(statement, ast.Expr) and isinstance(value, ast.Call)
                and isinstance(value.func, ast.Name) and value.func.id == 'exec'
                and value.args and isinstance(value.args[0], ast.Call)
                and isinstance(value.args[0].func, ast.Name)
                and value.args[0].func.id == 'compile'):
            return index
    return None


def admission_region(path, marker):
    """Parsed admission region: the module statements before the anchor."""
    tree = ast.parse(path.read_text())
    index = admission_anchor_index(tree.body)
    if index is None:
        raise AssertionError(marker + f': {path.name} has no exec(compile( admission anchor')
    return tree.body[:index]


def admission_violations(region, git_shape, env_shape):
    """Inverted, deny-by-default scan of one admission region (#2513 fix-r14).

    The fix-r13 judgment enumerated subprocess API names; ``run``/``call``/
    ``getoutput`` and aliased imports escaped it, and a matched launch's
    environment was never checked — measured false PASS: a bootstrap that
    launches /usr/bin/git with the inherited environment stayed green (DS r13
    review, b-tier). The judgment is now inverted to deny-by-default:

    * exactly one ``_boot_git`` and one ``_boot_git_env``, both byte-verbatim
      against check_frozen.py (the fix-r13 pin, unchanged);
    * outside the helper the region may not touch the subprocess module at
      all — any alias, any API name, any indirect load or ``__import__`` is
      red. An ``except`` clause's exception type may *name* the module (the
      ``_boot_subprocess.CalledProcessError`` form), but its type expression
      must not evaluate a call;
    * inside the helper every launched call must scrub the environment
      through the ``_boot_git_env`` name (no ``**`` splat);
    * every ``_boot_trusted`` read stays a ``_boot_git('show', ...)`` of the
      apparatus source, at least once (the fix-r13 admission-read rules).
    """
    violations = []
    helpers = [node for node in region
               if isinstance(node, ast.FunctionDef) and node.name == '_boot_git']
    envs = [node for node in region
            if isinstance(node, ast.Assign) and len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name)
            and node.targets[0].id == '_boot_git_env']
    if len(helpers) != 1:
        violations.append(f'_boot_git must be defined exactly once, found {len(helpers)}')
        return violations
    if len(envs) != 1:
        violations.append(f'_boot_git_env must be defined exactly once, found {len(envs)}')
        return violations
    if ast.dump(helpers[0]) != git_shape:
        violations.append('_boot_git must equal check_frozen.py verbatim')
    if ast.dump(envs[0]) != env_shape:
        violations.append('_boot_git_env must equal check_frozen.py verbatim')
    # Every name the region binds to the subprocess module or its members.
    module_aliases, member_aliases = set(), set()
    for node in region:
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name == 'subprocess':
                    module_aliases.add(alias.asname or 'subprocess')
        elif isinstance(node, ast.ImportFrom) and node.module == 'subprocess':
            for alias in node.names:
                member_aliases.add(alias.asname or alias.name)
    # The one carve-out: an except clause may name the module in its
    # exception type, as long as the type expression evaluates no call.
    exempt = set()
    for statement in region:
        for node in ast.walk(statement):
            if isinstance(node, ast.ExceptHandler) and node.type is not None:
                if any(isinstance(item, ast.Call) for item in ast.walk(node.type)):
                    violations.append('an except clause evaluates a call in its type: '
                                      + ast.unparse(node.type))
                else:
                    for item in ast.walk(node.type):
                        exempt.add(id(item))
    for statement in region:
        if statement is helpers[0]:
            continue
        for node in ast.walk(statement):
            if (isinstance(node, ast.Name)
                    and node.id in module_aliases | member_aliases
                    and id(node) not in exempt):
                violations.append('the admission region touches subprocess ('
                                 + node.id + ') outside _boot_git: '
                                 + ast.unparse(statement)[:160])
            elif isinstance(node, ast.Attribute):
                root = node.value
                while isinstance(root, ast.Attribute):
                    root = root.value
                if (isinstance(root, ast.Name) and root.id in module_aliases
                        and id(node) not in exempt and id(root) not in exempt):
                    violations.append('the admission region touches subprocess ('
                                      + root.id + ') outside _boot_git: '
                                      + ast.unparse(statement)[:160])
            elif (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                    and node.func.id == '__import__'):
                violations.append('the admission region calls __import__: '
                                  + ast.unparse(statement)[:160])
    for node in ast.walk(helpers[0]):
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) \
                and node.func.id == '__import__':
            violations.append('_boot_git calls __import__: ' + ast.unparse(node)[:160])
        elif isinstance(node, ast.Call):
            func, launched = node.func, False
            if isinstance(func, ast.Attribute):
                root = func.value
                while isinstance(root, ast.Attribute):
                    root = root.value
                launched = isinstance(root, ast.Name) and root.id in module_aliases
            elif isinstance(func, ast.Name):
                launched = func.id in member_aliases
            if launched:
                scrub = [keyword for keyword in node.keywords if keyword.arg == 'env']
                if (any(keyword.arg is None for keyword in node.keywords)
                        or len(scrub) != 1 or not isinstance(scrub[0].value, ast.Name)
                        or scrub[0].value.id != '_boot_git_env'):
                    violations.append('a launch inside _boot_git must scrub the environment '
                                     'through _boot_git_env: ' + ast.unparse(node)[:160])
    admissions = 0
    for statement in region:
        for node in ast.walk(statement):
            if (isinstance(node, ast.Assign) and len(node.targets) == 1
                    and isinstance(node.targets[0], ast.Name)
                    and node.targets[0].id == '_boot_trusted'):
                admissions += 1
                rendered = ast.unparse(node)
                if not (isinstance(node.value, ast.Call)
                        and isinstance(node.value.func, ast.Name)
                        and node.value.func.id == '_boot_git'
                        and "'show'" in rendered
                        and 'scripts/ui-server-parity/apparatus.py' in rendered):
                    violations.append('the admission read must be a _boot_git show of the '
                                      'apparatus source: ' + rendered)
    if admissions < 1:
        violations.append('the admission region has no admission read')
    return violations



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

    def test_bootstrap_git_admission_is_hardened_machine_check(self):
        """#2513 fix-r13 (DS sibling#1), judgment inverted in fix-r14: fail-closed AST scan.

        Every script the manifest registers is parsed (fail-closed on parse
        errors); a file whose module statements contain a top-level
        ``exec(compile(`` anchor is a bootstrap, and its admission region may
        reach subprocess only through the check_frozen.py ``_boot_git``
        helper — ``/usr/bin/git`` under a scrubbed environment — and must
        admit ``_boot_trusted`` through it. The fix-r13 API-name enumeration
        only ever checked three names, so ``run``/``call``/``getoutput`` and
        aliased imports escaped and the launched calls' environment was never
        inspected; the fix-r14 judgment denies by default instead (see
        admission_violations): any subprocess touch outside the helper, under
        any alias or via ``__import__``, is red. The scan set is the anchor
        itself, not a comment or a count a new bootstrap could drift past.
        """
        marker = '#2513 fix-r13: bootstrap admission must be the hardened _boot_git form'
        registered = manifest_pins(ROOT)
        self.assertGreaterEqual(len(registered), 20,
                                marker + ': manifest registration must not be empty')

        # Structural scan set: registered .py files with the admission anchor.
        anchored = []
        for path in sorted(registered):
            if path.suffix != '.py' or not path.is_file():
                continue
            try:
                tree = ast.parse(path.read_text())
            except SyntaxError as error:
                self.fail(marker + f': {path.name} does not parse: {error}')
            if admission_anchor_index(tree.body) is not None:
                anchored.append(path)

        canonical = admission_region(ROOT / 'scripts/ui-server-parity/check_frozen.py', marker)
        canonical_git = [node for node in canonical
                         if isinstance(node, ast.FunctionDef) and node.name == '_boot_git']
        canonical_env = [node for node in canonical
                         if isinstance(node, ast.Assign) and len(node.targets) == 1
                         and isinstance(node.targets[0], ast.Name)
                         and node.targets[0].id == '_boot_git_env']
        self.assertEqual(len(canonical_git), 1, marker + ': canonical _boot_git missing')
        self.assertEqual(len(canonical_env), 1, marker + ': canonical _boot_git_env missing')
        git_shape, env_shape = ast.dump(canonical_git[0]), ast.dump(canonical_env[0])

        scanned = 0
        for path in anchored:
            violations = admission_violations(admission_region(path, marker),
                                              git_shape, env_shape)
            self.assertEqual(violations, [],
                             marker + f': {path.name}: ' + '; '.join(violations))
            scanned += 1
        self.assertEqual(scanned, len(anchored),
                        marker + ': every anchored script must be scanned')
        # Both closures around this scan are structural: an anchored file
        # without the marker comment, or a marked file outside the
        # registration, escapes it.
        apparatus = ROOT / 'scripts/ui-server-parity'
        marked = {path.resolve() for path in apparatus.rglob('*.py')
                  if '# Isolated bootstrap' in path.read_text()}
        anchored_resolved = {path.resolve() for path in anchored}
        self.assertFalse(anchored_resolved - marked,
                         marker + ': every anchored script must carry the bootstrap marker')
        self.assertFalse(marked - set(registered),
                         marker + ': bootstrap-marked files must stay manifest-registered')

    def test_bootstrap_admission_rejects_inherited_env_and_alias_launches(self):
        """#2513 fix-r14 (DS r13 review, b-tier): deny-by-default over the subprocess surface.

        The fix-r13 machine check enumerated subprocess API names; the
        reviewer's regression — a bootstrap launching /usr/bin/git with the
        inherited environment through ``run`` — stayed green (measured false
        PASS, this round's red-admission-machinecheck-falsepass.py). Each
        variant below is the pristine check_frozen.py admission region with
        exactly one statement added or changed; the inverted scanner must
        reject every one of them, and the pristine region must stay green.
        """
        marker = '#2513 fix-r14: the admission region may not touch subprocess outside _boot_git'
        source = (ROOT / 'scripts/ui-server-parity/check_frozen.py').read_text()
        region_text = source[:source.index('exec(compile(')]
        import_line = 'import subprocess as _boot_subprocess\n'
        statement_anchor = '_boot_manifest = _boot_os.path.join('
        helper_launch = 'env=_boot_git_env, stderr=_boot_subprocess.PIPE, timeout=30)'
        self.assertEqual(region_text.count(import_line), 1, marker + ': import anchor moved')
        self.assertEqual(region_text.count(statement_anchor), 1, marker + ': insert anchor moved')
        self.assertEqual(region_text.count(helper_launch), 1, marker + ': helper anchor moved')

        def region_of(text):
            tree = ast.parse(text)
            return tree.body[:admission_anchor_index(tree.body)]

        def insert_statement(statement, base=None):
            text = region_text if base is None else base
            return text.replace(statement_anchor, statement + statement_anchor, 1)

        def insert_import(extra):
            return region_text.replace(import_line, import_line + extra, 1)

        inherited_env_run = (
            "_boot_probe = _boot_subprocess.run(\n"
            "    ['/usr/bin/git', '-c', 'safe.directory=*', '-C', _boot_root, 'show',\n"
            "     'HEAD:scripts/ui-server-parity/apparatus.py'],\n"
            "    env=dict(_boot_os.environ), capture_output=True, timeout=30)\n")
        aliased_run = (
            "_boot_probe = _boot_run(\n"
            "    ['/usr/bin/git', '-c', 'safe.directory=*', '-C', _boot_root, 'show',\n"
            "     'HEAD:scripts/ui-server-parity/apparatus.py'],\n"
            "    env=dict(_boot_os.environ), timeout=30)\n")
        variants = [
            ('/usr/bin/git launched with the inherited environment (DS concurrency#1 regression)',
             lambda: insert_statement(inherited_env_run),
             'outside _boot_git'),
            ('subprocess launched through a from-import alias',
             lambda: insert_statement(aliased_run,
                                     insert_import('from subprocess import run as _boot_run\n')),
             'outside _boot_git'),
            ('the subprocess module rebound to a bare name',
             lambda: insert_statement('_boot_launcher = _boot_subprocess.run\n'),
             'outside _boot_git'),
            ('the subprocess module fetched through getattr',
             lambda: insert_statement("_boot_launcher = getattr(_boot_subprocess, 'run')\n"),
             'outside _boot_git'),
            ('an enumerated API name launched without any environment scrubbing',
             lambda: insert_statement(
                 "_boot_probe = _boot_subprocess.check_output(\n"
                 "    ['git', '-C', _boot_root, 'rev-parse', 'HEAD'])\n"),
             'outside _boot_git'),
            ('subprocess reached through __import__',
             lambda: insert_statement("_boot_probe = __import__('subprocess')\n"),
             '__import__'),
            ('the helper itself launching with the inherited environment',
             lambda: region_text.replace(
                 helper_launch,
                 'env=dict(_boot_os.environ), stderr=_boot_subprocess.PIPE, timeout=30)', 1),
             'scrub the environment'),
        ]
        canonical = admission_region(ROOT / 'scripts/ui-server-parity/check_frozen.py', marker)
        canonical_git = [node for node in canonical
                         if isinstance(node, ast.FunctionDef) and node.name == '_boot_git']
        canonical_env = [node for node in canonical
                         if isinstance(node, ast.Assign) and len(node.targets) == 1
                         and isinstance(node.targets[0], ast.Name)
                         and node.targets[0].id == '_boot_git_env']
        git_shape, env_shape = ast.dump(canonical_git[0]), ast.dump(canonical_env[0])
        self.assertEqual(admission_violations(canonical, git_shape, env_shape), [],
                         marker + ': the pristine region must stay green')
        for name, build, expected in variants:
            with self.subTest(variant=name):
                violations = admission_violations(region_of(build()), git_shape, env_shape)
                joined = '; '.join(violations)
                self.assertIn(expected, joined,
                              marker + f': {name} must be rejected, got: {joined!r}')

    def test_git_dir_decoy_cannot_false_pass_head_admission(self):
        """#2513 fix-r13 (DS sibling#1): a steered GIT_DIR must not admit a decoy HEAD.

        Without a manifest every entry admits its apparatus bytes by reading
        HEAD of this checkout. Bare ``git -C`` inherits a steered GIT_DIR and
        would admit a decoy repository's bytes instead (measured); the
        hardened admission scrubs the environment. The refusal arm pins
        mutated disk bytes in a decoy repository's HEAD and requires every
        entry to refuse before executing them; the paired control replays the
        same steering over pristine bytes and proves the refusal comes from
        the decoy mismatch, not from the steering itself.
        """
        marker = '#2513 fix-r13: steered GIT_DIR must not admit a decoy HEAD'
        with tempfile.TemporaryDirectory(prefix='b2-decoy-') as tmp:
            root = Path(tmp).resolve()
            copy_apparatus(root)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=Fixture',
                            '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture', '--no-verify'], check=True)
            # No manifest: every entry below takes the no-manifest HEAD branch.
            (root / 'specs/ui-server-parity/FROZEN.md').unlink()
            device = root / 'scripts/ui-server-parity'
            # Run the staged copies, not the checkout's entries: only the mirror
            # has its manifest removed and its apparatus bytes mutated. The
            # inventory is structural — every staged file with a top-level
            # exec(compile( admission anchor — so a newly added bootstrap is
            # exercised in both arms without anyone extending a list (#2513
            # fix-r14, DS r13 review of the comment-string filter).
            entries = []
            for path in sorted(device.rglob('*.py')):
                if not path.is_file():
                    continue
                try:
                    tree = ast.parse(path.read_text())
                except SyntaxError:
                    continue
                if admission_anchor_index(tree.body) is not None:
                    entries.append(path)
            self.assertTrue(entries, marker + ': guard inventory must not be empty')
            pristine = (device / 'apparatus.py').read_bytes()
            decoy_marker = root / 'DECOY_APPARATUS_EXECUTED'
            mutated = pristine + (
                '\nopen(' + repr(str(decoy_marker)) + ', "w").write("decoy")\n').encode()
            decoy = Path(tmp) / 'decoy'
            (decoy / 'scripts/ui-server-parity').mkdir(parents=True)
            (decoy / 'scripts/ui-server-parity/apparatus.py').write_bytes(mutated)
            subprocess.run(['git', 'init', '-q', str(decoy)], check=True)
            subprocess.run(['git', '-C', str(decoy), 'add', '.'], check=True)
            subprocess.run(['git', '-C', str(decoy), '-c', 'user.name=Fixture',
                            '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture', '--no-verify'], check=True)
            steered = dict(os.environ, GIT_DIR=str(decoy / '.git'))

            def run_entry(entry, flag):
                return subprocess.run(
                    [sys.executable, '-I', str(entry), flag],
                    cwd=root, env=steered, capture_output=True, text=True, timeout=120)

            for path in entries:
                relative = path.relative_to(device)
                with self.subTest(entry=str(relative), arm='refusal'):
                    (device / 'apparatus.py').write_bytes(mutated)
                    decoy_marker.unlink(missing_ok=True)
                    result = run_entry(path, '--help')
                    output = result.stdout + result.stderr
                    self.assertEqual(
                        result.returncode, 1,
                        marker + f': {relative} admitted a steered decoy HEAD (rc={result.returncode}): '
                        + output[-300:])
                    self.assertIn('apparatus bootstrap hash admission failed', output,
                                  marker + f': {relative} must fail at the admission, not later: '
                                  + output[-300:])
                    self.assertIn('apparatus bootstrap differs from HEAD without manifest', output,
                                  marker + f': {relative} must name the decoy mismatch: ' + output[-300:])
                    self.assertFalse(
                        decoy_marker.exists(),
                        marker + f': {relative} executed decoy-admitted apparatus bytes')
            # Paired control: pristine bytes under the same steering must still
            # pass admission, so the refusal above is the decoy mismatch's, not
            # the steering environment's. The fix-r13 arm enumerated four
            # hand-picked entries, leaving the other eighteen no-manifest HEAD
            # admission branches unguarded (#2513 fix-r14, DS r13 review);
            # every entry is now exercised. Post-admission behaviour is each
            # entry's own; only the admission outcome is asserted here.
            (device / 'apparatus.py').write_bytes(pristine)
            decoy_marker.unlink(missing_ok=True)
            for path in entries:
                relative = path.relative_to(device)
                with self.subTest(entry=str(relative), arm='control'):
                    result = run_entry(
                        path, '-h' if path.name.startswith('test_') else '--help')
                    output = result.stdout + result.stderr
                    self.assertNotIn('apparatus bootstrap', output,
                                     marker + f': {relative} refused pristine bytes under steering: '
                                     + output[-300:])
                    self.assertFalse(decoy_marker.exists(),
                                     marker + f': {relative} executed unexpected apparatus bytes')

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

    def test_entry_guards_require_preloaded_os(self):
        """fix-r4 GLM P1: import os may only run after its preload is proved.

        fix-r5 Codex P1-1: the forged os.py must be provably reachable. The
        probe unloads ``os`` and puts the real forge directory on sys.path
        under ``-X frozen_modules=off``; the paired control removes exactly the
        preflight from the same bytes and requires the marker, so an
        unreachable fixture can no longer pass vacuously. Keep this probe and
        control pair in sync with ``scripts/cli-parity/test_cli_parity.py``
        (#701) and ``.../parity-lock-release/test_lock_release.py`` (#1023-10).
        """
        marker = ('#1023-11: an interpreter without a preloaded frozen os must be '
                  'rejected before importing os')
        preload = 'os must be preloaded from CPython 3.11+ frozen modules'
        forger = ('import sys\n'
                  'with open(sys.argv[2], "w") as stream:\n'
                  '    stream.write("forged os executed")\n'
                  'raise RuntimeError("forged os module executed")\n')
        # sys.argv: [1] entry, [2] marker, [3] forge. ``os`` is unloaded only
        # after the source was read and compiled, then the real forge directory
        # is put on sys.path: an entry that imports os before its preflight
        # executes forge/os.py.
        probe = ('import sys\n'
                 'entry, marker, forge = sys.argv[1], sys.argv[2], sys.argv[3]\n'
                 'source = open(entry, "rb").read()\n'
                 'code = compile(source, entry, "exec")\n'
                 'del sys.modules["os"]\n'
                 'sys.path.insert(0, forge)\n'
                 'exec(code, {"__file__": entry, "__name__": "__main__"})\n')
        version_probe = ('import sys\n'
                         'entry, marker, forge = sys.argv[1], sys.argv[2], sys.argv[3]\n'
                         'source = open(entry, "rb").read()\n'
                         'code = compile(source, entry, "exec")\n'
                         'sys.version_info = (3, 10, 0, "final", 0)\n'
                         'del sys.modules["os"]\n'
                         'sys.path.insert(0, forge)\n'
                         'exec(code, {"__file__": entry, "__name__": "__main__"})\n')

        def without_preflight(source):
            """The same entry bytes with the os-preload preflight removed.

            If a mutant already disabled or removed the block, the source is
            returned unchanged: its next ``import os`` must still reach forge.
            """
            tree = ast.parse(source)
            node = next((statement for statement in tree.body
                         if isinstance(statement, ast.If)
                         and 'version_info' in ast.unparse(statement.test)), None)
            if node is None:
                return source
            lines = source.splitlines(keepends=True)
            del lines[node.lineno - 1:node.end_lineno]
            return ''.join(lines)

        apparatus = ROOT / 'scripts/ui-server-parity'
        entries = [path for path in sorted(apparatus.rglob('*.py'))
                   if '# Isolated bootstrap' in path.read_text()]
        self.assertGreaterEqual(len(entries), 20, '#1023-11: guard inventory must not be empty')
        with tempfile.TemporaryDirectory(prefix='b2-osguard-') as temp:
            holder = Path(temp)
            for path in entries:
                relative = path.relative_to(apparatus)
                staged = holder / relative
                staged.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, staged)
                control = holder / 'control' / relative
                control.parent.mkdir(parents=True, exist_ok=True)
                control.write_text(without_preflight(staged.read_text()))
                label_path = '__'.join(relative.with_suffix('').parts)
                forge = holder / ('forge-' + label_path)
                forge.mkdir()
                (forge / 'os.py').write_text(forger)
                marker_path = holder / (label_path + '.marker')
                cases = (
                    ('frozen-modules-off', probe),
                    ('version-below-3-11', version_probe),
                )
                for label, case_probe in cases:
                    base = [sys.executable, '-I', '-X', 'frozen_modules=off',
                            '-c', case_probe]
                    with self.subTest(entry=str(relative), case=label):
                        marker_path.unlink(missing_ok=True)
                        result = subprocess.run(
                            base + [str(staged), str(marker_path), str(forge)],
                            capture_output=True, text=True, timeout=60)
                        output = result.stdout + result.stderr
                        self.assertEqual(
                            result.returncode, 1,
                            marker + f' [{relative}/{label}] rc={result.returncode}: '
                            + output[-300:])
                        self.assertIn(
                            preload, output,
                            marker + f' [{relative}/{label}] the preflight must reject '
                            'before importing os: ' + output[-300:])
                        self.assertFalse(
                            marker_path.exists(),
                            marker + f' [{relative}/{label}] the forged os.py executed')
                        # Paired control: removing exactly the preflight from the
                        # same bytes must execute the same forged module; without
                        # this arm the marker check above could stay vacuously true.
                        marker_path.unlink(missing_ok=True)
                        control_result = subprocess.run(
                            base + [str(control), str(marker_path), str(forge)],
                            capture_output=True, text=True, timeout=60)
                        control_output = control_result.stdout + control_result.stderr
                        self.assertTrue(
                            marker_path.exists(),
                            marker + f' [{relative}/{label}] control: removing the '
                            'preflight must execute the forged os.py: '
                            + control_output[-300:])
                        self.assertIn(
                            'forged os module executed', control_output,
                            marker + f' [{relative}/{label}] control: the forged module '
                            'must be the failure: ' + control_output[-300:])

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
