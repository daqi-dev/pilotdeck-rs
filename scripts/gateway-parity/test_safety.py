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

# python3 -I removes the script directory from sys.path; the apparatus directory
# is the only trusted import root.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from common import (APPARATUS_PROBES, DATA_FILES, MAX_AGE, apparatus_whitelist,
                    baseline_dependency_hashes, cargo_config_problems, guard,
                    node_modules_entry, owner, parity_lock, save, unknown_apparatus_files)
from compare import Normalizer, compare, wire_frame
from observer import Decoder


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
    # the shape, and every one must be rejected.  The same table drives
    # controls/prerequisites.py, so the two machine controls cannot drift apart.
    def _materialize(self, kind, name, outside):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        if kind == 'dir-link':
            path.symlink_to(outside, target_is_directory=True)
        elif kind == 'file-link':
            path.symlink_to(outside / 'evil.py')
        elif kind == 'dir':
            path.mkdir()
        elif kind == 'nested-modules':
            path.mkdir()
            (path / 'evil.py').write_text('unregistered nested dependency')
        elif kind == 'nested-modules-empty':
            path.mkdir()
        elif kind == 'unreadable-dir':
            path.mkdir()
            (path / 'evil.py').write_text('unregistered file in an unreadable tree')
            os.chmod(path, 0)
        else:
            path.write_text('unregistered apparatus input')
        return path

    def _remove(self, kind, path):
        if kind in ('nested-modules', 'nested-modules-empty', 'unreadable-dir'):
            if kind == 'unreadable-dir':
                os.chmod(path, 0o700)
            shutil.rmtree(path)
        elif kind == 'dir':
            path.rmdir()
        else:
            path.unlink()

    def test_every_shared_probe_is_rejected(self):
        outside = Path(tempfile.mkdtemp(prefix='pdrs-apparatus-outside-', dir=self.root.parent))
        self.addCleanup(shutil.rmtree, outside, ignore_errors=True)
        (outside / '__init__.py').write_text('shadow package')
        (outside / 'evil.py').write_text('shadow module')
        for kind, name in APPARATUS_PROBES:
            with self.subTest(kind=kind, name=name):
                if kind == 'unreadable-dir' and os.geteuid() == 0:
                    continue  # root can read mode-000 directories
                path = self._materialize(kind, name, outside)
                try:
                    if kind == 'unreadable-dir':
                        # onerror must surface the failure, never skip the tree.
                        with self.assertRaises(OSError):
                            unknown_apparatus_files(self.root)
                    else:
                        self.assertEqual(unknown_apparatus_files(self.root), [name])
                finally:
                    self._remove(kind, path)
        self.assertEqual(unknown_apparatus_files(self.root), [])

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

    def test_bytecode_cache_is_tolerated_only_for_a_registered_source(self):
        (self.root / '__pycache__').mkdir()
        cache = self.root / '__pycache__/common.cpython-314.pyc'
        cache.write_bytes(b'x')
        self.assertEqual(unknown_apparatus_files(self.root), ['__pycache__/common.cpython-314.pyc'])
        (self.root / 'common.py').write_text('registered member')
        self.assertEqual(unknown_apparatus_files(self.root), [])
        orphan = self.root / '__pycache__/orphan.cpython-314.pyc'
        orphan.write_bytes(b'x')
        self.assertEqual(unknown_apparatus_files(self.root), ['__pycache__/orphan.cpython-314.pyc'])
        orphan.unlink()
        (self.root / '__pycache__/evil.py').write_text('not cache bytecode')
        self.assertEqual(unknown_apparatus_files(self.root), ['__pycache__/evil.py'])
        (self.root / '__pycache__/evil.py').unlink()
        cache.unlink()
        (self.root / 'legacy.pyc').write_bytes(b'x')
        self.assertEqual(unknown_apparatus_files(self.root), ['legacy.pyc'])

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
