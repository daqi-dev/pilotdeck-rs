#!/usr/bin/env python3
"""Bounded offline regressions for lock lifecycle, corrupt evidence and observer."""
import json
import multiprocessing
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import threading
import tempfile
import time
import unittest

# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# Load apparatus modules by explicit path, never sys.path: a same-named package
# or extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing common.py is what this block defends.  -B only
# stops cache writes, so redirect the bytecode prefix too (fix-r5).
import importlib.util, sys, tempfile
from pathlib import Path

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before loading anything and print the command.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {Path(__file__).resolve()}')

sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

APPARATUS_PROBES = common.APPARATUS_PROBES
DATA_FILES = common.DATA_FILES
DEGENERATE_PROBES = common.DEGENERATE_PROBES
MAX_AGE = common.MAX_AGE
ProbeSkipped = common.ProbeSkipped
apparatus_whitelist = common.apparatus_whitelist
baseline_dependency_hashes = common.baseline_dependency_hashes
cargo_config_problems = common.cargo_config_problems
guard = common.guard
materialize_probe = common.materialize_probe
node_modules_entry = common.node_modules_entry
owner = common.owner
parity_lock = common.parity_lock
probe_expected = common.probe_expected
remove_probe = common.remove_probe
save = common.save
unknown_apparatus_files = common.unknown_apparatus_files
compare_module = _load('compare', common.SCRIPT / 'compare.py')
Normalizer = compare_module.Normalizer
compare = compare_module.compare
wire_frame = compare_module.wire_frame
observer_module = _load('observer', common.SCRIPT / 'observer.py')
Decoder = observer_module.Decoder

BOOTSTRAP_START = '# >>> apparatus bootstrap'
BOOTSTRAP_END = '# <<< apparatus bootstrap'
# Derived from the registered-entry table, never hand-maintained: a new A2
# module (run_all, build_proxy, ...) is covered by the sys.path-import checks
# without a second edit.
APPARATUS_MODULES = tuple(sorted({
    Path(name).stem for name in common.A2_SCRIPTS if name.endswith('.py')
}))
# ``import common`` and ``import os, common`` are both bare-name imports that
# would resolve through sys.path; match the whole import list, not just the
# first name after ``import``.
APPARATUS_IMPORT = (r'(?m)^\s*(?:from\s+(?:' + '|'.join(APPARATUS_MODULES) + r')\b'
                    r'|import\s+[^\n#]*\b(?:' + '|'.join(APPARATUS_MODULES) + r')\b)')


def contender(lock, events):
    os.setsid()
    with parity_lock(timeout=3, lock=Path(lock)):
        with open(events, 'a') as file:
            file.write(f'{os.getpid()} enter\n')
        time.sleep(.04)
        with open(events, 'a') as file:
            file.write(f'{os.getpid()} exit\n')


class Safety(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-gateway-ruler-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.lock = self.root / 'lock'

    def test_live_owner_times_out_without_removal(self):
        self.lock.mkdir()
        previous = {'pid': os.getpid(), 'timestamp': time.time(), 'token': 'live'}
        save(self.lock / 'owner.json', previous)
        with self.assertRaises(TimeoutError):
            with parity_lock(.01, lock=self.lock):
                self.fail('must not acquire live owner')
        self.assertEqual(owner(self.lock), previous)

    def test_dead_pid_takeover_and_release(self):
        child = subprocess.Popen([sys.executable, '-c', 'pass'], start_new_session=True)
        child.wait(timeout=3)
        self.lock.mkdir()
        save(self.lock / 'owner.json', {'pid': child.pid, 'timestamp': time.time(), 'token': 'dead'})
        takeovers = []
        with parity_lock(takeovers=takeovers, lock=self.lock):
            self.assertEqual(owner(self.lock)['pid'], os.getpid())
        self.assertFalse(self.lock.exists())
        self.assertEqual(takeovers[0]['reason'], 'dead_pid')

    def test_old_release_cannot_remove_replacement_owner(self):
        replacement = {'pid': os.getpid(), 'timestamp': time.time(), 'token': 'replacement'}
        with parity_lock(lock=self.lock):
            save(self.lock / 'owner.json', replacement)
        self.assertEqual(owner(self.lock), replacement)

    def test_stale_unpublished_lock_takeover(self):
        self.lock.mkdir()
        stamp = time.time() - MAX_AGE - 1
        os.utime(self.lock, (stamp, stamp))
        takeovers = []
        with parity_lock(lock=self.lock, takeovers=takeovers):
            pass
        self.assertEqual(takeovers[0]['reason'], 'expired')

    def test_lease_timestamp_starts_after_wait(self):
        self.lock.mkdir()
        save(self.lock / 'owner.json', {'pid': os.getpid(), 'timestamp': time.time(), 'token': 'earlier-owner'})
        released = []

        def release():
            time.sleep(.2)
            with guard(self.lock):
                (self.lock / 'owner.json').unlink()
                self.lock.rmdir()
                released.append(time.time())

        thread = threading.Thread(target=release)
        thread.start()
        try:
            with parity_lock(timeout=3, lock=self.lock):
                self.assertGreaterEqual(owner(self.lock)['timestamp'], released[0])
        finally:
            thread.join(3)

    def test_concurrent_waiters_are_serialized(self):
        events = self.root / 'events'
        children = [multiprocessing.Process(target=contender, args=(str(self.lock), str(events))) for _ in range(3)]
        try:
            for child in children:
                child.start()
            for child in children:
                child.join(5)
                self.assertEqual(child.exitcode, 0)
        finally:
            for child in children:
                if child.is_alive():
                    child.terminate()
                    child.join(3)
        lines = events.read_text().splitlines()
        self.assertEqual(len(lines), 6)
        for start, end in zip(lines[::2], lines[1::2]):
            self.assertEqual(start.split()[0], end.split()[0])
            self.assertTrue(start.endswith('enter') and end.endswith('exit'))
        self.assertFalse(self.lock.exists())

    def test_two_corrupt_rounds_are_fail_not_exception(self):
        save(self.root / 'run.json', [])
        result = compare(self.root, self.root)
        self.assertEqual(result['status'], 'FAIL')
        self.assertIn('G0.round', {row['rule'] for row in result['failures']})

    def test_observer_decodes_split_masked_wire_without_rewriting(self):
        records = []
        decoder = Decoder(records.append, 'send')
        payload = b'{"type":"request","method":"supported_agents","id":"test"}'
        mask = b'1234'
        wire = bytes([0xc1, 128 | len(payload)]) + mask + bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        data = b'GET /ws HTTP/1.1\r\nUpgrade: websocket\r\n\r\n' + wire
        for byte in data:
            decoder.feed(bytes([byte]))
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]['payloadHex'], payload.hex())
        self.assertEqual(records[0]['frame']['method'], 'supported_agents')

    def test_business_times_and_generated_id_aliases_survive(self):
        a, b = Normalizer(), Normalizer()
        self.assertNotEqual(a.value({'durationMs': 1, 'runAt': '2099-01-01'}), b.value({'durationMs': 2, 'runAt': '2099-01-02'}))
        x = '00000000-0000-4000-8000-000000000001'
        y = '00000000-0000-4000-8000-000000000002'
        self.assertNotEqual(Normalizer().value([x, x]), Normalizer().value([x, y]))

    def test_uuid_definitions_pair_by_entity_not_list_position(self):
        x, y, z, w = [f'00000000-0000-4000-8000-{i:012d}' for i in range(1, 5)]
        a, b = Normalizer(), Normalizer()
        left = [{'id': x, 'name': 'alpha'}, {'id': y, 'name': 'beta'}]
        right = [{'id': w, 'name': 'beta'}, {'id': z, 'name': 'alpha'}]
        a.index(left)
        b.index(right)
        self.assertEqual(a.value([x, y]), b.value([z, w]))
        self.assertNotEqual(a.value([x, y]), b.value([w, z]))
        self.assertNotEqual(Normalizer().value([x, y]), Normalizer().value([y, x]))
        with self.assertRaises(ValueError):
            a.index({'id': z, 'name': 'alpha'})

    def test_binding_generation_cannot_change_on_same_entity(self):
        n = Normalizer()
        n.value({'connectionId': 'fixed-connection', 'generation': 1})
        with self.assertRaises(ValueError):
            n.value({'connectionId': 'fixed-connection', 'generation': 2})
        with self.assertRaises(ValueError):
            n.value({'connectionId': 'another-connection', 'generation': 1})

    def test_clock_container_boundaries(self):
        shapes = [({'name': 'business'}, ('createdAt', 'lastActivity')),
                  ({'requestId': 'business'}, ('createdAt',)),
                  ({'name': 'business', 'lastActivity': 1}, ('createdAt',)),
                  ({'projectKey': 'business'}, ('lastActivity',)),
                  ({'taskId': 'business'}, ('createdAt', 'updatedAt')),
                  ({'runId': 'business', 'ownerPid': 1}, ('updatedAt',)),
                  ({'runId': 'business'}, ('acceptedAt',)),
                  ({'firstPrompt': 'business'}, ('updatedAt',))]
        for parent, fields in shapes:
            for field in fields:
                with self.subTest(parent=parent, field=field):
                    self.assertNotEqual(Normalizer().value(dict(parent, **{field: 1})),
                                        Normalizer().value(dict(parent, **{field: 2})))
        containers = [({'projectKey': 'p', 'fullPath': 'p', 'name': 'n', 'lastActivity': 1}, 'createdAt'),
                      ({'projectKey': 'p', 'path': 'p', 'name': 'n'}, 'lastActivity'),
                      ({'ownerId': 's', 'kind': 'question', 'toolCallId': 't', 'requestId': 'r'}, 'createdAt'),
                      ({'runId': 'r', 'ownerPid': 1, 'acceptedAt': 1}, 'updatedAt')]
        for parent, field in containers:
            self.assertEqual(Normalizer().value(dict(parent, **{field: 1})),
                             Normalizer().value(dict(parent, **{field: 2})))

    def test_receive_header_prefix_and_metadata_are_exact(self):
        for code, extension in [(2, b''), (126, b'\x00\x02'), (127, (2).to_bytes(8, 'big'))]:
            for first in (0x81, 0x01, 0xc1):
                for mask in (b'', b'1234'):
                    payload = b'{}'
                    if mask:
                        payload = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
                    header = bytes([first, code | (128 if mask else 0)]) + extension + mask
                    row = {'direction': 'receive', 'wireHex': (header + payload).hex(), 'frame': {},
                           'wire': {'opcode': 1, 'fin': bool(first & 128), 'rsv': first & 0x70,
                                    'masked': bool(mask), 'lengthCode': code, 'payloadLength': 2}}
                    self.assertEqual(wire_frame(row), header.hex())
                    row['wire']['masked'] = not bool(mask)
                    with self.assertRaises(ValueError):
                        wire_frame(row)
        for data in ('', '81', '817e00', '817f0000', '8182'):
            with self.assertRaises(ValueError):
                wire_frame({'wireHex': data})


class ApparatusInputs(unittest.TestCase):
    """#52 / #85: unregistered files, installed deps and cargo configs are inputs."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='pdrs-apparatus-inputs-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    # Shared probes (common.APPARATUS_PROBES): the kind says how to materialize
    # the shape, and every one must be rejected.  The same table and the same
    # materialize_probe()/remove_probe() implementation drive
    # controls/prerequisites.py, so the two machine controls cannot drift apart.
    def test_every_shared_probe_is_rejected(self):
        outside = Path(tempfile.mkdtemp(prefix='pdrs-apparatus-outside-', dir=self.root.parent))
        self.addCleanup(shutil.rmtree, outside, ignore_errors=True)
        (outside / '__init__.py').write_text('shadow package')
        (outside / 'evil.py').write_text('shadow module')
        skipped = {}
        for kind, name in APPARATUS_PROBES:
            # Each shape is its own subTest.  A host-degenerate skip records the
            # entry and continues: raising SkipTest here would abort the whole
            # method and silently stop the remaining probes (fix-r6 P2).
            with self.subTest(kind=kind, name=name):
                try:
                    path = materialize_probe(self.root, kind, name, outside)
                except ProbeSkipped as error:
                    skipped[(kind, name)] = str(error)
                    print(f'[SKIPPED] {kind}:{name}: {error}', file=sys.stderr)
                    continue
                # The scan reports the first __pycache__ directory itself, not
                # each file below it (it never descends into one).
                expected = probe_expected(name)
                try:
                    if kind == 'unreadable-dir':
                        # onerror must surface the failure, never skip the tree.
                        with self.assertRaises(OSError):
                            unknown_apparatus_files(self.root)
                    else:
                        self.assertEqual(unknown_apparatus_files(self.root), [expected])
                finally:
                    remove_probe(kind, path)
        self.assertLessEqual(set(skipped), set(DEGENERATE_PROBES),
                             f'undeclared probe skip: {skipped}')
        print(f'probes: {len(APPARATUS_PROBES)} shapes, {len(skipped)} declared '
              f'degenerate skips, {len(APPARATUS_PROBES) - len(skipped)} exercised',
              file=sys.stderr)
        self.assertEqual(unknown_apparatus_files(self.root), [])

    def test_unknown_probe_kind_is_an_error(self):
        # A new table kind must fail loudly in both consumers instead of being
        # silently materialized as a plain file (the old else-branch drift).
        outside = Path(tempfile.mkdtemp(prefix='pdrs-probe-kind-', dir=self.root.parent))
        self.addCleanup(shutil.rmtree, outside, ignore_errors=True)
        with self.assertRaises(ValueError):
            materialize_probe(self.root, 'fifo', 'pipe', outside)
        with self.assertRaises(ValueError):
            remove_probe('fifo', self.root / 'pipe')
        self.assertFalse((self.root / 'pipe').exists())
        self.assertEqual(unknown_apparatus_files(self.root), [])

    def test_a2_entries_share_one_verbatim_path_bootstrap(self):
        # Every A2 entry that touches common must carry the same inline block.
        # It cannot move into common.py (importing common is what it defends),
        # so the machine check is what stops a future entry from reverting to
        # ``from common import`` and being shadowed before the scan (fix-r6 P1).
        folder = Path(__file__).resolve().parent
        blocks = {}
        for name in common.A2_SCRIPTS:
            if name == 'common.py' or not name.endswith('.py'):
                continue
            text = (folder / name).read_text(encoding='utf-8')
            if 'common' not in text and '_load(' not in text:
                continue
            start = text.find(BOOTSTRAP_START)
            self.assertNotEqual(start, -1, f'{name}: missing apparatus bootstrap')
            end = text.find(BOOTSTRAP_END, start)
            self.assertNotEqual(end, -1, f'{name}: unterminated apparatus bootstrap')
            blocks[name] = text[start:end + len(BOOTSTRAP_END)]
            prefix = text[:start]
            self.assertNotRegex(prefix, r'sys[.]path[.]insert', name)
            self.assertNotRegex(prefix, APPARATUS_IMPORT, name)
        self.assertEqual(sorted(blocks), sorted([
            'build_proxy.py', 'check_frozen.py', 'check_ruler.py', 'compare.py',
            'run_round.py', 'session_domain.py', 'test_safety.py',
            'controls/run_all.py', 'controls/prerequisites.py',
            'controls/session_domain.py']))
        self.assertEqual(len(set(blocks.values())), 1,
                         'apparatus bootstrap differs between entries')
        block = next(iter(blocks.values()))
        self.assertIn('sys.flags.isolated', block)
        self.assertLess(block.index('sys.flags.isolated'),
                        block.index('sys.pycache_prefix'),
                        'isolated-mode guard must precede module loading')

    def test_a2_entries_never_resolve_apparatus_through_sys_path(self):
        # Covers entries that do not mention ``common`` but still import a
        # sibling module by name; sys.path lookup is the P1 bypass in both cases.
        folder = Path(__file__).resolve().parent
        for name in common.A2_SCRIPTS:
            if not name.endswith('.py') or name == 'common.py':
                continue
            text = (folder / name).read_text(encoding='utf-8')
            self.assertNotRegex(text, r'sys[.]path[.]insert', name)
            self.assertNotRegex(text, APPARATUS_IMPORT, name)

    def test_apparatus_import_pattern_covers_import_lists(self):
        # A hand-maintained tuple can silently miss an A2 module; the
        # derivation and the comma form of ``import a, b`` both need a machine
        # check, otherwise this guard stops covering the omitted module.
        self.assertIn('observer', APPARATUS_MODULES)
        self.assertIn('run_all', APPARATUS_MODULES)
        for text in ('import common\n', 'import os, common\n',
                     'import os, sys, compare as c\n', 'from common import x\n'):
            self.assertRegex(text, APPARATUS_IMPORT)
        for text in ('import os, commonish\n', 'import os, mycommon\n'):
            self.assertNotRegex(text, APPARATUS_IMPORT)

    def test_controls_entry_ignores_controls_common_shadow(self):
        # The controls/ entries must resolve common.py from the apparatus dir,
        # never from their own directory: a controls/common.py is not a second
        # canonical module, and preferring it would be the same P1 bypass one
        # level down (fix-r6 P1).
        #
        # The shadow is written into a throwaway copy of the apparatus, never
        # the real tree: addCleanup does not run on SIGKILL, and a stray
        # controls/common.py would make every later scan report an unregistered
        # file (false FAIL) until someone deletes it by hand (fix-r7 P1).
        folder = Path(__file__).resolve().parent
        git_status = ['git', '-C', str(folder), 'status', '--porcelain', '--', '.']
        before = subprocess.check_output(git_status, timeout=30)
        self.assertFalse((folder / 'controls/common.py').exists(),
                         'real apparatus tree has a controls/common.py residue')
        tree = self.root / 'apparatus'
        # Dereference links while copying: a linked controls/ directory must
        # not turn the shadow write below into a write through to its target.
        shutil.copytree(folder, tree,
                        ignore=shutil.ignore_patterns('node_modules', '__pycache__'))
        shadow = tree / 'controls/common.py'
        marker = self.root / 'controls-common-executed'
        shadow.write_text("from pathlib import Path\n"
                          f"Path({str(marker)!r}).write_text('executed')\n"
                          "raise RuntimeError('controls/common.py must not be loaded')\n",
                          encoding='utf-8')
        saved = sys.modules.get('common')
        try:
            module = _load('probe_controls_prerequisites',
                           tree / 'controls/prerequisites.py')
            self.assertEqual(Path(module.common.__file__).resolve(),
                             (tree / 'common.py').resolve())
            self.assertFalse(marker.exists())
        finally:
            if saved is None:
                sys.modules.pop('common', None)
            else:
                sys.modules['common'] = saved
        after = subprocess.check_output(git_status, timeout=30)
        self.assertEqual(after, before, 'test wrote into the real apparatus tree')

    def test_skipped_probe_is_not_counted_as_passed(self):
        # Three-state gate (fix-r6 P2): a skipped control is listed but never
        # satisfies ``all(passed)``.  Only declared host-degenerate skips are
        # tolerated; an undeclared skip fails, and a skip-only report cannot pass.
        prerequisites = _load('prerequisites',
                              Path(__file__).resolve().parent / 'controls/prerequisites.py')
        declared = 'apparatus-unregistered-case-variant-COMMON.PY'
        results = {
            'ok': {'passed': True, 'skipped': False, 'detail': ''},
            declared: {'passed': None, 'skipped': True, 'detail': 'SKIPPED: APFS folds case'},
        }
        passed, skipped, unexpected = prerequisites.summarize_results(results, {declared})
        self.assertTrue(passed)
        self.assertEqual(skipped, {declared: 'SKIPPED: APFS folds case'})
        self.assertEqual(unexpected, [])
        results['apparatus-unregistered-file-surprise'] = {
            'passed': None, 'skipped': True, 'detail': 'SKIPPED: unexpected'}
        passed, skipped, unexpected = prerequisites.summarize_results(results, {declared})
        self.assertFalse(passed)
        self.assertEqual(unexpected, ['apparatus-unregistered-file-surprise'])
        only_skip = {'x': {'passed': None, 'skipped': True, 'detail': 'SKIPPED'}}
        self.assertFalse(prerequisites.summarize_results(only_skip, {'x'})[0])
        # The pre-fix shape had no explicit state and ``passed: True``; the gate
        # must refuse it rather than silently counting it as an executed pass.
        legacy = {'old': {'passed': True, 'detail': 'SKIPPED (degenerate)'}}
        with self.assertRaises(ValueError):
            prerequisites.summarize_results(legacy, set())

    def test_probe_table_prefers_running_tree_over_old_code_root(self):
        # fix-r6 P2: --code-root may point at a pre-fix tree with an old table.
        # The running table must win, otherwise the new shapes are silently not
        # exercised in exactly the comparison that is meant to expose them.
        prerequisites = _load('prerequisites',
                              Path(__file__).resolve().parent / 'controls/prerequisites.py')
        running_path = Path(__file__).resolve().parent / 'common.py'
        old = type('OldCommon', (), {
            'APPARATUS_PROBES': (('file', 'old-only.py'),),
            'materialize_probe': None,
        })()
        module, table = prerequisites.select_probe_source(old, running_path)
        self.assertEqual(tuple(table), tuple(APPARATUS_PROBES))
        self.assertNotIn(('file', 'old-only.py'), table)
        current, current_table = prerequisites.select_probe_source(common, running_path)
        self.assertIs(current, common)
        self.assertEqual(tuple(current_table), tuple(APPARATUS_PROBES))

    def test_whitelisted_and_data_files_are_accepted_by_path(self):
        for name in apparatus_whitelist():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('registered apparatus input')
        self.assertEqual(unknown_apparatus_files(self.root), [])
        extra = self.root / 'fixtures/extra.json'
        extra.write_text('{}')
        self.assertEqual(unknown_apparatus_files(self.root), ['fixtures/extra.json'])
        self.assertIn('fixtures/gateway-protocol.fixtures.json', DATA_FILES)

    def test_node_modules_symlinks_are_digested_not_scanned(self):
        # The one exempt top-level tree stays pruned; links inside it are
        # covered by the node_modules digest, which records the link target.
        modules = self.root / 'node_modules'
        modules.mkdir()
        outside = Path(tempfile.mkdtemp(prefix='pdrs-node-modules-', dir=self.root.parent))
        self.addCleanup(shutil.rmtree, outside, ignore_errors=True)
        for name in ('first', 'second'):
            (outside / name).mkdir()
            (outside / name / '__init__.py').write_text('dependency')
        link = modules / 'pkg'
        link.symlink_to(outside / 'first', target_is_directory=True)
        self.assertEqual(unknown_apparatus_files(self.root), [])
        first = node_modules_entry(self.root)
        link.unlink()
        link.symlink_to(outside / 'second', target_is_directory=True)
        self.assertNotEqual(node_modules_entry(self.root), first)
        self.assertEqual(unknown_apparatus_files(self.root), [])

    def test_python_cache_is_always_unregistered(self):
        # fix-r5 P0: unchecked-hash (PEP 552) bytecode is loaded without
        # re-reading the source, so no .pyc/.pyo gets a tolerance class even
        # beside a registered source.  An empty __pycache__ is rejected too:
        # the scanner reports the directory itself and never descends into it.
        (self.root / 'common.py').write_text('registered member')
        (self.root / '__pycache__').mkdir()
        self.assertEqual(unknown_apparatus_files(self.root), ['__pycache__'])
        cache = self.root / '__pycache__/common.cpython-314.pyc'
        cache.write_bytes(b'x')
        self.assertEqual(unknown_apparatus_files(self.root), ['__pycache__'])
        self.root.joinpath('__pycache__/evil.py').write_text('not cache bytecode')
        self.assertEqual(unknown_apparatus_files(self.root), ['__pycache__'])
        shutil.rmtree(self.root / '__pycache__')
        (self.root / 'legacy.pyc').write_bytes(b'x')
        (self.root / 'legacy.pyo').write_bytes(b'x')
        self.assertEqual(unknown_apparatus_files(self.root), ['legacy.pyc', 'legacy.pyo'])
        # A cache below a subdirectory is rejected at its first __pycache__
        # segment too; controls/run_all.py is loaded by path and would shadow.
        (self.root / 'controls/__pycache__').mkdir(parents=True)
        (self.root / 'controls/__pycache__/run_all.cpython-314.pyc').write_bytes(b'x')
        self.assertEqual(unknown_apparatus_files(self.root), [
            'controls/__pycache__', 'legacy.pyc', 'legacy.pyo'])

    def test_ds_store_is_tolerated_only_as_a_regular_file(self):
        # Same exemption as scripts/ui-server-parity/apparatus.py: ordinary
        # Finder metadata only.  Links/directories stay rejected (the shared
        # probe table covers those shapes).
        (self.root / '.DS_Store').write_text('finder metadata')
        (self.root / 'fixtures').mkdir()
        (self.root / 'fixtures/.DS_Store').write_text('finder metadata')
        self.assertEqual(unknown_apparatus_files(self.root), [])
        (self.root / 'fixtures/.DS_Store').unlink()
        (self.root / 'fixtures/.DS_Store').mkdir()
        self.assertEqual(unknown_apparatus_files(self.root), ['fixtures/.DS_Store'])
        (self.root / 'fixtures/.DS_Store').rmdir()
        (self.root / 'common.py').write_text('registered member')
        (self.root / 'fixtures/.DS_Store').symlink_to(self.root / 'common.py')
        self.assertEqual(unknown_apparatus_files(self.root), ['fixtures/.DS_Store'])

    def test_registered_path_case_variant_requires_the_same_inode(self):
        (self.root / 'common.py').write_text('registered member')
        variant = self.root / 'COMMON.PY'
        variant.write_text('case variant')
        same = os.path.samefile(self.root / 'common.py', variant)
        # APFS is case-insensitive: the variant is the registered inode.  On a
        # case-sensitive filesystem it is a distinct, unregistered file.
        self.assertEqual(unknown_apparatus_files(self.root), [] if same else ['COMMON.PY'])

    def test_unreadable_directory_aborts_the_digest_too(self):
        if os.geteuid() == 0:
            self.skipTest('root can read mode-000 directories')
        modules = self.root / 'node_modules/locked'
        modules.mkdir(parents=True)
        (modules / 'index.js').write_text('x')
        os.chmod(modules, 0)
        try:
            with self.assertRaises(OSError):
                node_modules_entry(self.root)
        finally:
            os.chmod(modules, 0o700)

    def test_frozen_hash_rows_are_inside_the_whitelist(self):
        manifest = Path(__file__).resolve().parents[2] / 'specs/gateway-parity/FROZEN.md'
        rows = re.findall(r'^[0-9a-f]{64}\s+\.\./\.\./scripts/gateway-parity/(\S+)\s*$',
                          manifest.read_text(encoding='utf-8'), re.M)
        self.assertTrue(rows)
        self.assertEqual(sorted(set(rows) - set(apparatus_whitelist())), [])

    def test_real_apparatus_has_no_unregistered_files(self):
        # The frozen tree itself must pass the whitelist: no existing data file
        # or generated cache may be misclassified.
        self.assertEqual(unknown_apparatus_files(), [])

    def test_installed_dependency_bytes_change_digest(self):
        self.assertIsNone(node_modules_entry(self.root))
        (self.root / 'node_modules/pkg').mkdir(parents=True)
        (self.root / 'node_modules/pkg/index.js').write_text('a')
        first = node_modules_entry(self.root)
        self.assertIsNotNone(first)
        (self.root / 'node_modules/pkg/index.js').write_text('b')
        self.assertNotEqual(node_modules_entry(self.root), first)

    def test_baseline_dependencies_bind_loaded_packages(self):
        modules = self.root / 'baseline/node_modules'
        for name in ('tsx', 'ws'):
            package = modules / name
            package.mkdir(parents=True)
            (package / 'package.json').write_text(json.dumps({'name': name, 'dependencies': {}}))
            (package / 'index.js').write_text(name)
        before = baseline_dependency_hashes(self.root / 'baseline')
        self.assertEqual(sorted(before), ['node_modules/tsx', 'node_modules/ws'])
        (modules / 'tsx/index.js').write_text('tampered loader')
        self.assertNotEqual(baseline_dependency_hashes(self.root / 'baseline'), before)
        shutil.rmtree(modules / 'ws')
        with self.assertRaises(ValueError):
            baseline_dependency_hashes(self.root / 'baseline')

    def test_untracked_and_persistent_cargo_configs_block(self):
        environ = {'CARGO_HOME': str(self.root / 'cargo-home')}
        self.assertEqual(cargo_config_problems(self.root, environ), [])
        (self.root / '.cargo').mkdir()
        (self.root / '.cargo/config.toml').write_text('[build]\n')
        self.assertTrue(any('untracked worktree config' in item
                            for item in cargo_config_problems(self.root, environ)))
        (self.root / '.cargo/config.toml').unlink()
        (self.root / '.cargo').rmdir()
        home = self.root / 'cargo-home'
        home.mkdir()
        (home / 'config.toml').write_text('[registries]\n')
        self.assertTrue(any('persistent CARGO_HOME config' in item
                            for item in cargo_config_problems(self.root, environ)))
        (home / 'config.toml').unlink()
        self.assertEqual(cargo_config_problems(self.root, environ), [])

    def test_tracked_worktree_cargo_config_is_reviewable(self):
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True, timeout=30)
        config = self.root / '.cargo/config.toml'
        config.parent.mkdir()
        config.write_text('[build]\n')
        subprocess.run(['git', '-C', str(self.root), 'add', '.cargo/config.toml'],
                       check=True, timeout=30)
        environ = {'CARGO_HOME': str(self.root / 'cargo-home')}
        self.assertEqual(cargo_config_problems(self.root, environ), [])


if __name__ == '__main__':
    unittest.main()
