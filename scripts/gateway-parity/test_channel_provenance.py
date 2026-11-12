"""Channel v2 preserved-file boundaries and truthful prerequisite classification."""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is builtin; os is imported only after the frozen-module preflight below.
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import importlib.util
from pathlib import Path
import tempfile

# -B only stops cache writes, so redirect the bytecode prefix too (fix-r5).
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code

# Load the one guard by explicit path, never sys.path: a same-named package or
# extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing the guard is what this block defends.
_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
_guard_path = _apparatus / '_guard.py'
_guard = sys.modules.get('_guard')
if not (getattr(_guard, '__file__', None)
        and Path(_guard.__file__).resolve() == _guard_path
        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):
    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)
    _guard = importlib.util.module_from_spec(_spec)
    sys.modules['_guard'] = _guard
    _guard_source = _guard_path.read_bytes()
    try:
        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)
    except BaseException:
        sys.modules.pop('_guard', None)
        raise
    _guard._apparatus_source = _guard_source
if Path(_guard.__file__).resolve() != _guard_path:
    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')

_load = _guard.load
_has_git_checkout = _guard._has_git_checkout
_verify_cli_imports = _guard.verify_cli_imports
run_entry = _guard.run_entry
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

import atexit
import copy
import errno
import hashlib
import json
import shutil
import subprocess
import types
import unittest
from unittest.mock import Mock, patch

atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
entries = _load('retirement_entries', _apparatus / 'retirement_entries.py')


class EvidenceError(ValueError):
    def __init__(self, rule, detail):
        self.rule = rule
        super().__init__(rule + ': ' + detail)


def require(condition, rule, detail):
    if not condition:
        raise EvidenceError(rule, detail)


def reference(path):
    return {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


class ChannelProvenance(unittest.TestCase):
    """Synthetic unit inputs only; no fixture here claims production capture."""

    def setUp(self):
        self.root = _apparatus.parents[1]
        (self.root / '_tmp').mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix='channel-provenance-', dir=self.root / '_tmp')
        self.addCleanup(temporary.cleanup)
        self.out = Path(temporary.name).resolve()
        self.fixture = self.root / 'src/pilotdeck-gateway/tests/fixtures/native-channel-production'
        self.roles = {role: reference(self.fixture / filename) for role, filename in {
            'producer': 'eight.mjs', 'observer': 'owned-observer.mjs',
            'finalizer': 'finalize-process.py', 'preserver': 'preserve-layout.py'}.items()}
        self.other_source = reference(self.fixture / 'nine-http.mjs')
        source_refs = [*self.roles.values(), self.other_source]
        self.native = types.SimpleNamespace(source={'files': [
            {'path': str(Path(item['path']).relative_to(self.root)), 'sha256': item['sha256']} for item in source_refs]})
        self.original = self.out / 'cur'
        self.preserved = self.out / 'native' / 'preserved'
        for name in ('home', 'project'):
            (self.preserved / name).mkdir(parents=True)
        self.business = self.preserved / 'home' / 'history.jsonl'
        self.business.write_bytes(b'{"unitFixture":true}\n')
        self.layout = {'root': str(self.original), 'mockPort': 28690, 'controlPort': 28610}
        layout_ref = self.write_json('layout.json', self.layout)
        self.runtime = {'node': 'v22.22.0', 'osHome': str(self.original / 'os-home'),
                        'pilotHome': str(self.original / 'home'), 'home': str(self.original / 'home'),
                        'project': str(self.original / 'project'), 'defaultProject': str(self.original / 'home'),
                        'cwd': str(self.original / 'project'), 'tmp': str(self.original / 'tmp'),
                        'mockPort': 28690, 'controlPort': 28610}
        preservation = {'layout': layout_ref, 'originalRoot': str(self.original),
                        'preservedRoot': str(self.preserved), 'actualClosedPorts': [28690, 28610],
                        'preserver': self.roles['preserver']}
        receipt = {'producer': self.roles['producer'], 'finalizer': self.roles['finalizer'],
                   'preserver': self.roles['preserver'],
                   'observer': self.write_json('observer-source.json', dict(self.roles['observer'], ownerPid=123, records=1)),
                   'preservation': self.write_json('preservation.json', preservation),
                   'originalBusinessRoots': {key: self.runtime[key] for key in ('home', 'project')},
                   'process': {'cwd': self.runtime['cwd'], 'readyListeners': [{'port': 28610}]}}
        self.raw = {'layout': dict(self.layout, **layout_ref), 'runtime': self.runtime,
                    'fixtureSha256': self.roles['producer']['sha256']}
        file_ref = dict(reference(self.business), originalPath=str(self.original / 'home/history.jsonl'))
        self.evidence = {'receipt': receipt, 'afterReapFiles': [file_ref],
                         'missing': [], 'originals': [], 'transportRecords': []}
        self.d = types.SimpleNamespace(ROOT=self.root, SCRIPT=_apparatus, EvidenceError=EvidenceError,
                                       RETIREMENT_BASELINE_REVISION='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',
                                       require=require, read=lambda path: json.loads(Path(path).read_text()))

    def write_json(self, name, value):
        path = self.out / name
        path.write_text(json.dumps(value) + '\n')
        return reference(path)

    def test_git_source_scan_failure_has_a_source_rule(self):
        self.d.subprocess = subprocess
        with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_SOURCE.*cannot read fixed TS source tree'):
            entries.verify_channel_ts_sources(self.d, self.out / 'missing-checkout')

    def test_source_walk_and_file_read_errors_have_a_source_rule(self):
        baseline = self.out / 'source-baseline'
        names = ('src/unit.ts', 'packages/sdk/src/unit.ts',
                 'package.json', 'pnpm-lock.yaml', 'tsconfig.json')
        tree = []
        for name in names:
            path = baseline / name
            path.parent.mkdir(parents=True, exist_ok=True)
            content = b'unit fixed source\n'
            path.write_bytes(content)
            identity = hashlib.sha1(b'blob ' + str(len(content)).encode() + b'\0' + content).hexdigest()
            tree.append(b'100644 blob ' + identity.encode() + b'\t' + name.encode() + b'\0')
        self.d.subprocess = types.SimpleNamespace(check_output=Mock(return_value=b''.join(tree)),
                                                 SubprocessError=subprocess.SubprocessError)
        entries.verify_channel_ts_sources(self.d, baseline)
        scandir, read_bytes = os.scandir, Path.read_bytes

        def deny_source_directory(path):
            if Path(path) == baseline / 'src':
                raise PermissionError(errno.EACCES, 'unit unreadable source directory', str(path))
            return scandir(path)

        def deny_source_file(path):
            if path == baseline / 'src/unit.ts':
                raise PermissionError(errno.EACCES, 'unit unreadable source file', str(path))
            return read_bytes(path)

        # Real os.walk must dispatch its onerror callback; do not stub the walk.
        with patch.object(entries.os, 'scandir', side_effect=deny_source_directory):
            with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_SOURCE.*unit unreadable source directory'):
                entries.verify_channel_ts_sources(self.d, baseline)
        with patch.object(Path, 'read_bytes', deny_source_file):
            with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_SOURCE.*unit unreadable source file'):
                entries.verify_channel_ts_sources(self.d, baseline)

    def test_git_failures_do_not_abort_other_channel_results(self):
        self.write_json('manifest.json', [
            {'path': 'pilotdeck-gateway', 'sha256': 'unit-native'},
            {'path': 'pilotdeck-agent-loop-sidecar', 'sha256': 'unit-sidecar'},
        ])
        native = types.SimpleNamespace(manifest=self.out / 'manifest.json', source_sha='unit-source',
                                       source={'files': []})
        raw_ref = self.write_json('native-evidence.json', {
            'sourceManifestSha256': 'unit-source', 'gatewaySha256': 'unit-native',
            'sidecarSha256': 'unit-sidecar', 'fixedTS': self.d.RETIREMENT_BASELINE_REVISION,
        })
        rows = [{'id': 'gateway.channel:' + channel, 'profile': 'native-eight-v2',
                 'rawEvidence': raw_ref, 'processReceipt': {'path': 'unit-native-provenance'}}
                for channel in ('telegram', 'discord')]
        self.write_json('channels.json', {'schemaVersion': 1, 'entries': rows})
        transport = self.out / 'transport.jsonl'
        transport.write_text('')
        evidence = dict(self.evidence, receipt={'transportTrace': reference(transport)})
        applicability = {face: {'applicable': False, 'kinds': []}
                         for face in ('attachment', 'channelInteraction')}
        baseline = self.out / 'unused-baseline'
        semantic = {'equal': True, 'differentFields': [], 'formalAcceptance': False}

        def compare(d, _native, actual_baseline, row, _raw, _evidence):
            if row['id'] == rows[0]['id']:
                entries.verify_channel_ts_sources(d, actual_baseline)
            return {'semanticComparison': semantic, 'missing': [], 'originals': []}

        failures = [(subprocess.CalledProcessError(128, ['git', 'ls-tree']), 'exit status'),
                    (subprocess.TimeoutExpired(['git', 'ls-tree'], 10), 'timed out'),
                    (PermissionError(errno.EACCES, 'unreadable git'), 'unreadable git')]
        for error, detail in failures:
            with self.subTest(error=type(error).__name__):
                self.d.subprocess = types.SimpleNamespace(
                    check_output=Mock(side_effect=error), SubprocessError=subprocess.SubprocessError)
                with patch.object(entries, 'verify_owned_process', return_value=evidence), \
                        patch.object(entries, 'channel_applicability', return_value=applicability), \
                        patch.object(entries, 'channel_v2_comparison', side_effect=compare):
                    cells = entries.channel_results(self.d, native, self.out, baseline=baseline)
                self.assertEqual(len(cells), 32)
                self.assertEqual(cells[rows[0]['id']]['status'], 'FAIL')
                self.assertEqual(cells[rows[0]['id']]['rule'], 'ENTRY_CHANNEL_SOURCE')
                self.assertIn(detail, cells[rows[0]['id']]['detail'])
                self.assertEqual(cells[rows[1]['id']]['status'], 'BLOCKED')
                self.assertEqual(cells[rows[1]['id']]['semanticComparison'], semantic)

    def _assert_ts_process_failure_is_per_channel(self, error_type):
        manifest = self.write_json('manifest.json', [
            {'path': 'pilotdeck-gateway', 'sha256': 'unit-native'},
            {'path': 'pilotdeck-agent-loop-sidecar', 'sha256': 'unit-sidecar'},
        ])
        source = {'files': []}
        source_ref = self.write_json('source.json', source)
        native = types.SimpleNamespace(manifest=Path(manifest['path']), manifest_sha=manifest['sha256'],
                                       source_manifest=Path(source_ref['path']), source_sha=source_ref['sha256'],
                                       source=source)
        raw_ref = self.write_json('native-evidence.json', {
            'sourceManifestSha256': native.source_sha, 'gatewaySha256': 'unit-native',
            'sidecarSha256': 'unit-sidecar', 'fixedTS': self.d.RETIREMENT_BASELINE_REVISION,
        })
        receipt_ref = self.write_json('ts-receipt.json', {
            'schemaVersion': 1, 'channel': 'telegram', 'rawEvidence': raw_ref,
            'sourceManifest': source_ref, 'candidateManifest': manifest, 'runtimeLibraries': None,
        })
        rows = [{'id': 'gateway.channel:' + channel, 'profile': 'native-eight-v2',
                 'rawEvidence': raw_ref, 'processReceipt': receipt_ref}
                for channel in ('telegram', 'discord')]
        self.write_json('channels.json', {'schemaVersion': 1, 'entries': rows})
        transport = self.out / 'transport.jsonl'
        transport.write_text('')
        evidence = dict(self.evidence, receipt={'transportTrace': reference(transport)})
        applicability = {face: {'applicable': False, 'kinds': []}
                         for face in ('attachment', 'channelInteraction')}
        semantic = {'equal': True, 'differentFields': [], 'formalAcceptance': False}
        node = self.out / 'unit-node'
        node.write_bytes(b'unit fixture; check_output is mocked, never executed')
        node_ref = reference(node)
        baseline = self.out / 'unit-baseline'
        probes = [([str(node), '-p', 'JSON.stringify(process.versions)'], 5,
                   b'{"node":"22.22.0","icu":"78.2","uv":"1.52.1"}'),
                  (['git', '-C', str(baseline), 'rev-parse', 'HEAD'], 10,
                   self.d.RETIREMENT_BASELINE_REVISION.encode()),
                  (['git', '-C', str(baseline), 'status', '--porcelain', '--untracked-files=no'], 10, b'')]
        real_verify = entries.verify_owned_process

        def verify(d, actual_native, receipt, raw, channel):
            if channel == 'telegram':
                # Exercise the actual TS helper and its identity guards; only
                # the unrelated native lane and subprocess outputs are synthetic.
                return real_verify(d, actual_native, receipt, raw, channel,
                                   executable_role='ts-node', node_ref=node_ref, baseline=baseline)
            return evidence

        for index, (command, timeout, _) in enumerate(probes):
            with self.subTest(probe=command):
                if error_type is subprocess.CalledProcessError:
                    error = error_type(128, command)
                elif error_type is subprocess.TimeoutExpired:
                    error = error_type(command, timeout)
                else:
                    error = error_type(errno.EACCES, 'unit unreadable process probe', command[0])
                check_output = Mock(side_effect=[probe[2] for probe in probes[:index]] + [error])
                self.d.subprocess = types.SimpleNamespace(check_output=check_output,
                                                         SubprocessError=subprocess.SubprocessError)
                with patch.object(entries, 'verify_owned_process', side_effect=verify), \
                        patch.object(entries, 'channel_applicability', return_value=applicability), \
                        patch.object(entries, 'channel_v2_comparison', return_value={
                            'semanticComparison': semantic, 'missing': [], 'originals': []}):
                    cells = entries.channel_results(self.d, native, self.out, baseline=baseline)
                self.assertEqual(check_output.call_count, index + 1)
                check_output.assert_called_with(command, timeout=timeout)
                self.assertEqual(len(cells), 32)
                self.assertEqual(cells[rows[0]['id']]['status'], 'FAIL')
                self.assertEqual(cells[rows[0]['id']]['rule'], 'ENTRY_CHANNEL_PROCESS')
                self.assertIn('cannot read fixed TS process source or runtime', cells[rows[0]['id']]['detail'])
                self.assertIn(str(error), cells[rows[0]['id']]['detail'])
                self.assertEqual(cells[rows[1]['id']]['status'], 'BLOCKED')
                self.assertEqual(cells[rows[1]['id']]['semanticComparison'], semantic)

    def test_ts_process_exit_failures_do_not_abort_other_channel_results(self):
        self._assert_ts_process_failure_is_per_channel(subprocess.CalledProcessError)

    def test_ts_process_timeouts_do_not_abort_other_channel_results(self):
        self._assert_ts_process_failure_is_per_channel(subprocess.TimeoutExpired)

    def test_ts_process_permission_errors_do_not_abort_other_channel_results(self):
        self._assert_ts_process_failure_is_per_channel(PermissionError)

    def test_owned_process_inspection_requires_running_positive_integer_pid(self):
        # Synthetic unit originals only: no process, container or TS is launched.
        executable = self.out / 'pilotdeck-gateway'
        executable.write_bytes(b'unit candidate bytes; never executed')
        executable_ref = reference(executable)
        manifest = self.write_json('process-manifest.json', [
            {'path': executable.name, 'sha256': executable_ref['sha256']}])
        source = copy.deepcopy(self.native.source)
        source_ref = self.write_json('process-source.json', source)
        native = types.SimpleNamespace(manifest=Path(manifest['path']), manifest_sha=manifest['sha256'],
                                       source_manifest=Path(source_ref['path']), source_sha=source_ref['sha256'],
                                       source=source)
        raw_ref = self.write_json('process-raw.json', {'unitFixture': True})
        process = {'pid': 123, 'argv': [str(executable), '--native'], 'executable': executable_ref,
                   'birth': {'bootId': 'unit-boot', 'startTimeTicks': '1'}, 'cwd': str(self.out),
                   'exit': {'code': 0, 'signal': None}, 'reaped': True, 'portsClosed': True,
                   'remainingOwnedIdentities': [], 'networkNamespace': 'net:[unit]', 'pidNamespace': 'pid:[unit]',
                   'readyListeners': [{'host': '127.0.0.1', 'port': 28610, 'ownerPid': 123}]}
        observation = {**process, 'errors': [], 'closeObservedAt': 1, 'identityStillPresent': False,
                       'portsStillListening': [], 'observations': [{'readyListeners': process['readyListeners']}]}
        preflight = {'networkNamespace': process['networkNamespace'], 'pidNamespace': process['pidNamespace'],
                     'hostname': 'unit-host', 'interfaces': [[1, 'lo']], 'loopbackConnect': 'PASS',
                     'negativeControls': [{'family': family, 'kind': kind, 'blocked': True}
                                          for family in (2, 10) for kind in (1, 2)]}
        inspection = {'Id': 'unit-container', 'Name': '/unit-container', 'Config': {'Hostname': 'unit-host'},
                      'HostConfig': {'NetworkMode': 'none'}, 'State': {'Running': True, 'Pid': 123}}
        receipt = {'schemaVersion': 1, 'channel': 'telegram', 'rawEvidence': raw_ref,
                   'sourceManifest': source_ref, 'candidateManifest': manifest, 'runtimeLibraries': None,
                   'producer': self.roles['producer'], 'finalizer': self.roles['finalizer'], 'bindingErrors': [],
                   'observer': self.write_json('process-observer.json', self.roles['observer']),
                   'observationTrace': self.write_json('process-observations.json', [observation]),
                   'process': process, 'networkPreflight': self.write_json('process-preflight.json', preflight),
                   'containerCleanup': self.write_json('process-cleanup.json',
                       {'name': 'unit-container', 'containerId': 'unit-container', 'ownedContainerStillRunning': ''}),
                   'containerInspection': self.write_json('process-inspection.json', [inspection]),
                   'afterReapFiles': [dict(reference(self.business), bytes=self.business.stat().st_size)]}
        receipt_ref = self.write_json('process-receipt.json', receipt)
        verified = entries.verify_owned_process(self.d, native, receipt_ref, raw_ref, 'telegram')
        self.assertEqual(verified['receipt'], receipt)
        self.assertNotIn('source-bound-owned-container-cleanup', verified['missing'])
        self.assertIn('production-inbound-and-outbound-response-originals', verified['missing'])
        missing = object()
        cases = [('missing', missing), ('null', None), ('list', []), ('empty', {}),
                 ('running-bool-required', {'Running': 1, 'Pid': 123}),
                 ('not-running', {'Running': False, 'Pid': 123}),
                 ('pid-bool', {'Running': True, 'Pid': True}),
                 ('pid-float', {'Running': True, 'Pid': 123.0}),
                 ('pid-string', {'Running': True, 'Pid': '123'}),
                 ('pid-zero', {'Running': True, 'Pid': 0}),
                 ('pid-negative', {'Running': True, 'Pid': -1})]
        for label, state in cases:
            with self.subTest(state=label):
                changed = copy.deepcopy(inspection)
                if state is missing:
                    changed.pop('State')
                else:
                    changed['State'] = state
                receipt['containerInspection'] = self.write_json('process-inspection.json', [changed])
                receipt_ref = self.write_json('process-receipt.json', receipt)
                with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_ISOLATION'):
                    entries.verify_owned_process(self.d, native, receipt_ref, raw_ref, 'telegram')

    def test_v2_roles_accept_the_actual_registered_sources(self):
        entries.channel_v2_roles(self.d, self.native, self.raw, self.evidence)

    def test_different_tracked_producer_cannot_stand_in_for_eight(self):
        evidence = copy.deepcopy(self.evidence)
        evidence['receipt']['producer'] = self.other_source
        with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_ROLE.*producer'):
            entries.channel_v2_roles(self.d, self.native, self.raw, evidence)

    def test_different_tracked_observer_cannot_authenticate_transport(self):
        evidence = copy.deepcopy(self.evidence)
        evidence['receipt']['observer'] = self.write_json('other-observer.json', dict(self.other_source, ownerPid=123, records=1))
        with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_ROLE.*observer'):
            entries.channel_v2_roles(self.d, self.native, self.raw, evidence)

    def test_different_tracked_finalizer_cannot_bind_process_receipt(self):
        evidence = copy.deepcopy(self.evidence)
        evidence['receipt']['finalizer'] = self.other_source
        with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_ROLE.*finalizer'):
            entries.channel_v2_roles(self.d, self.native, self.raw, evidence)

    def test_different_tracked_preserver_cannot_certify_original_inodes(self):
        evidence = copy.deepcopy(self.evidence)
        preserved = self.d.read(Path(evidence['receipt']['preservation']['path']))
        preserved['preserver'] = self.other_source
        evidence['receipt']['preservation'] = self.write_json('other-preservation.json', preserved)
        evidence['receipt']['preserver'] = self.other_source
        with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_ROLE.*preserver'):
            entries.channel_v2_roles(self.d, self.native, self.raw, evidence)

    def test_capture_and_receipt_must_agree_on_role_hashes(self):
        raw = dict(self.raw, fixtureSha256=self.other_source['sha256'])
        with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_ROLE'):
            entries.channel_v2_roles(self.d, self.native, raw, self.evidence)
        evidence = copy.deepcopy(self.evidence)
        evidence['receipt']['preserver'] = self.other_source
        with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_ROLE'):
            entries.channel_v2_roles(self.d, self.native, self.raw, evidence)

    def test_preservation_keeps_original_runtime_names(self):
        root, contents = entries.channel_v2_preserved(self.d, self.raw, self.evidence)
        self.assertEqual(root, self.preserved)
        self.assertEqual(contents, {str(self.original / 'home/history.jsonl'): self.business.read_bytes()})
        self.assertNotIn(str(self.business), contents)

    def test_other_layout_cannot_be_spliced_into_capture(self):
        other = dict(self.layout, controlPort=28611)
        raw = copy.deepcopy(self.raw)
        raw['layout'] = dict(other, **self.write_json('other-layout.json', other))
        with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_LAYOUT'):
            entries.channel_v2_preserved(self.d, raw, self.evidence)

    def test_unreceipted_business_file_is_rejected(self):
        (self.preserved / 'project' / 'unexpected.txt').write_bytes(b'not in the stop receipt')
        with self.assertRaisesRegex(EvidenceError, 'ENTRY_CHANNEL_DURABLE'):
            entries.channel_v2_preserved(self.d, self.raw, self.evidence)

    def test_unreadable_subdirectory_cannot_be_silently_omitted(self):
        hidden = self.preserved / 'home' / 'unreadable'
        hidden.mkdir()
        scandir = os.scandir

        def deny_one_directory(path):
            if Path(path) == hidden:
                raise PermissionError(errno.EACCES, 'unit fixture unreadable business directory', str(hidden))
            return scandir(path)

        # Patch the filesystem boundary, not os.walk or its callback. This
        # exercises os.walk's real error path even when the test runs as root.
        with patch.object(entries.os, 'scandir', side_effect=deny_one_directory):
            with self.assertRaises(PermissionError):
                entries.channel_v2_preserved(self.d, self.raw, self.evidence)

    def test_prerequisite_block_retains_reason_without_ts_coverage(self):
        adapter = _load('retirement_channel_v2', _apparatus / 'retirement_channel_v2.py')
        source_paths = ['scripts/gateway-parity/retirement_channel_v2.py',
                        'src/pilotdeck-gateway/tests/fixtures/native-channel-production/eight.mjs',
                        'src/pilotdeck-gateway/tests/fixtures/native-channel-production/reference-round-context.mjs',
                        'src/pilotdeck-gateway/tests/fixtures/native-channel-production/owned-observer.mjs',
                        'src/pilotdeck-gateway/tests/fixtures/native-channel-production/finalize-process.py',
                        'src/pilotdeck-gateway/tests/fixtures/native-channel-production/preserve-layout.py']
        source_rows = [{'path': path, 'sha256': reference(self.root / path)['sha256']} for path in source_paths]
        bins = [{'path': 'pilotdeck-gateway', 'sha256': 'unit-native'},
                {'path': 'pilotdeck-agent-loop-sidecar', 'sha256': 'unit-sidecar'}]
        self.write_json('manifest.json', bins)
        native = types.SimpleNamespace(manifest=self.out / 'manifest.json', source_sha='unit-source',
                                       source={'files': source_rows})
        executable = {'path': str(self.out / 'pilotdeck-gateway'), 'sha256': 'unit-native'}
        self.raw.update(schemaVersion=2, profile='native-eight-v2', engine='native', channel='telegram',
                        sourceManifestSha256='unit-source', gatewaySha256='unit-native',
                        sidecarSha256='unit-sidecar', fixedTS=self.d.RETIREMENT_BASELINE_REVISION,
                        fixtureSha256=source_rows[1]['sha256'], contextHelperSha256=source_rows[2]['sha256'],
                        actualExecutable=executable)
        process = self.evidence['receipt']['process']
        process['executable'] = executable
        process['argv'] = [executable['path'], '--native', '--home', self.runtime['home'],
                           '--project', self.runtime['home'], '--listen', '127.0.0.1:28610',
                           '--token-file', str(self.original / 'home/token'), '--sidecar',
                           str(self.out / 'pilotdeck-agent-loop-sidecar'), '--drain-ms', '2000']
        transport = self.out / 'transport.jsonl'
        transport.write_text('')
        self.evidence['receipt']['transportTrace'] = reference(transport)
        blockers = [{'kind': 'missing-real-dependency', 'dependency': 'grammy'}]
        blocked = {'schemaVersion': 2, 'profile': 'native-eight-v2', 'engine': 'ts', 'channel': 'telegram',
                   'fixedTS': self.d.RETIREMENT_BASELINE_REVISION, 'status': 'BLOCKED',
                   'formalAcceptance': False, 'prerequisiteFailures': blockers, 'steps': [],
                   'failure': 'Production prerequisites unavailable; no gateway or replacement SDK was executed.'}
        raw_ref = self.write_json('native-evidence.json', self.raw)
        ts_ref = self.write_json('ts-blocked.json', blocked)
        native_receipt = {'path': 'unit-only-native-provenance-stub'}
        row = {'id': 'gateway.channel:telegram', 'profile': 'native-eight-v2',
               'rawEvidence': raw_ref, 'processReceipt': native_receipt,
               'reference': {'rawEvidence': ts_ref}}
        self.write_json('channels.json', {'schemaVersion': 1, 'entries': [row]})
        applicability = {face: {'applicable': False, 'kinds': []}
                         for face in ('attachment', 'channelInteraction')}
        observe = Mock(return_value={'unitNativeObservation': True})
        compare = Mock(side_effect=AssertionError('a prerequisite failure must not enter TS comparison'))
        # Only the already-tested native observation/provenance and source
        # applicability are stubbed; artifact hashes, native source headers,
        # preserved files, reference parsing, and channel status wiring run.
        with patch.object(entries, 'verify_owned_process', return_value=self.evidence) as provenance, \
                patch.object(entries, 'channel_applicability', return_value=applicability), \
                patch.object(adapter, 'observe', observe), patch.object(adapter, 'compare_pair', compare):
            cells = entries.channel_results(self.d, native, self.out, baseline=self.out / 'unused-baseline')
        cell = cells[row['id']]
        self.assertEqual(cell['status'], 'BLOCKED', cell)
        self.assertEqual(cell['referenceBlockers'], blockers)
        self.assertIn('fixed-ts-v2-prerequisites', cell['missing'])
        self.assertIn('fixed-ts-business-contract-comparison', cell['missing'])
        self.assertIn('v2-http-same-socket-wire-binding', cell['missing'])
        self.assertIn('v2-websocket-frame-and-header-binding', cell['missing'])
        self.assertNotIn('comparison', cell)
        self.assertNotIn('fixed-ts-v2-business-contract-comparison', cell['covered'])
        self.assertIn('v2-current-turn-interaction-reload-and-durable-originals', cell['covered'])
        self.assertEqual(cell['observations'], {'unitNativeObservation': True})
        self.assertIn(ts_ref, cell['originals'])
        provenance.assert_called_once_with(self.d, native, native_receipt, raw_ref, 'telegram')
        observe.assert_called_once()
        compare.assert_not_called()

    def _wire_proof_cell(self, comparison, carried):
        """Exercise aggregation only; the comparator/process proof is a stub."""
        self.write_json('manifest.json', [
            {'path': 'pilotdeck-gateway', 'sha256': 'unit-native'},
            {'path': 'pilotdeck-agent-loop-sidecar', 'sha256': 'unit-sidecar'}])
        native = types.SimpleNamespace(manifest=self.out / 'manifest.json', source_sha='unit-source',
                                       source={'files': []})
        raw_ref = self.write_json('native-evidence.json', {
            'sourceManifestSha256': 'unit-source', 'gatewaySha256': 'unit-native',
            'sidecarSha256': 'unit-sidecar', 'fixedTS': self.d.RETIREMENT_BASELINE_REVISION})
        row = {'id': 'gateway.channel:telegram', 'profile': 'native-eight-v2',
               'rawEvidence': raw_ref, 'processReceipt': {'path': 'unit-native-provenance'}}
        self.write_json('channels.json', {'schemaVersion': 1, 'entries': [row]})
        transport = self.out / 'transport.jsonl'
        transport.write_text('')
        evidence = dict(self.evidence, receipt={'transportTrace': reference(transport)})
        applicability = {'attachment': {'applicable': True, 'kinds': ['image']},
                         'channelInteraction': {'applicable': False, 'kinds': []}}
        paired = {'semanticComparison': comparison, 'wireBindings': carried,
                  'missing': ['unit-other-production-gap'], 'originals': []}
        with patch.object(entries, 'verify_owned_process', return_value=evidence), \
                patch.object(entries, 'channel_applicability', return_value=applicability), \
                patch.object(entries, 'channel_v2_comparison', return_value=paired):
            return entries.channel_results(self.d, native, self.out,
                                           baseline=self.out / 'unused-baseline')[row['id']]

    def test_complete_equal_wire_proof_clears_only_two_gaps(self):
        bindings = [{'engine': engine, 'httpSameSocket': True, 'websocketFrames': True}
                    for engine in ('ts', 'native')]
        comparison = {'equal': True, 'differentFields': [], 'formalAcceptance': False,
                      'wireBindings': bindings}
        cell = self._wire_proof_cell(comparison, copy.deepcopy(bindings))
        self.assertEqual(cell['wireBindings'], bindings)
        for gap in entries.CHANNEL_V2_WIRE_GAPS:
            self.assertNotIn(gap, cell['missing'])
            self.assertIn(gap, cell['covered'])
        self.assertEqual(cell['status'], 'BLOCKED')
        self.assertIn('attachment:image:production-scenario', cell['missing'])
        self.assertIn('unit-other-production-gap', cell['missing'])
        self.assertIn('fixed-ts-business-contract-comparison', cell['missing'])
        self.assertNotIn('comparison', cell)
        self.assertIs(cell['semanticComparison']['formalAcceptance'], False)

    def test_partial_duplicate_or_nonboolean_wire_proofs_keep_gaps(self):
        good = [{'engine': engine, 'httpSameSocket': True, 'websocketFrames': True}
                for engine in ('ts', 'native')]
        cases = [None, [], good[:1], [good[0], good[0]],
                 [good[0], {**good[1], 'engine': 'other'}],
                 [good[0], {**good[1], 'httpSameSocket': 1}],
                 [good[0], {**good[1], 'websocketFrames': 'true'}],
                 [good[0], {**good[1], 'websocketFrames': False}],
                 [good[0], {'engine': 'native', 'httpSameSocket': True}],
                 [good[0], {**good[1], 'extra': True}]]
        for bindings in cases:
            with self.subTest(bindings=bindings):
                comparison = {'equal': True, 'differentFields': [], 'formalAcceptance': False,
                              'wireBindings': bindings}
                cell = self._wire_proof_cell(comparison, bindings)
                self.assertEqual(cell['status'], 'BLOCKED', cell)
                self.assertNotIn('wireBindings', cell)
                for gap in entries.CHANNEL_V2_WIRE_GAPS:
                    self.assertIn(gap, cell['missing'])
                    self.assertNotIn(gap, cell['covered'])

    def test_wire_proof_requires_equal_comparison_and_carried_proof(self):
        bindings = [{'engine': engine, 'httpSameSocket': True, 'websocketFrames': True}
                    for engine in ('ts', 'native')]
        base = {'equal': True, 'differentFields': [], 'formalAcceptance': False,
                'wireBindings': bindings}
        cases = [({**base, 'equal': False}, bindings), ({**base, 'equal': 1}, bindings),
                 ({**base, 'differentFields': ['wireConnections']}, bindings),
                 ({**base, 'formalAcceptance': True}, bindings),
                 (base, None), (base, [bindings[0], {**bindings[1], 'httpSameSocket': 1}])]
        cases.extend(({key: value for key, value in base.items() if key != missing}, bindings)
                     for missing in ('equal', 'differentFields', 'formalAcceptance'))
        for comparison, carried in cases:
            with self.subTest(comparison=comparison, carried=carried):
                cell = self._wire_proof_cell(comparison, carried)
                self.assertEqual(cell['status'], 'BLOCKED', cell)
                self.assertNotIn('wireBindings', cell)
                for gap in entries.CHANNEL_V2_WIRE_GAPS:
                    self.assertIn(gap, cell['missing'])
                    self.assertNotIn(gap, cell['covered'])
        for comparison in (None, [], 'invalid'):
            with self.subTest(nonmapping=comparison):
                self.assertIsNone(entries.channel_v2_wire_bindings(comparison))
                cell = self._wire_proof_cell(comparison, bindings)
                # The outer cell retains its existing fail-closed TypeError path.
                self.assertEqual(cell['status'], 'FAIL', cell)
                self.assertEqual(cell['rule'], 'ENTRY_CHANNEL_EVIDENCE', cell)
                self.assertIn('not a mapping', cell['detail'])
                self.assertNotIn('wireBindings', cell)
                for gap in entries.CHANNEL_V2_WIRE_GAPS:
                    self.assertIn(gap, cell['missing'])
                    self.assertNotIn(gap, cell['covered'])

    def test_semantic_equality_and_retained_tcp_chunks_do_not_certify_wire(self):
        self.write_json('manifest.json', [
            {'path': 'pilotdeck-gateway', 'sha256': 'unit-native'},
            {'path': 'pilotdeck-agent-loop-sidecar', 'sha256': 'unit-sidecar'},
        ])
        native = types.SimpleNamespace(manifest=self.out / 'manifest.json', source_sha='unit-source',
                                       source={'files': []})
        raw_ref = self.write_json('native-evidence.json', {
            'sourceManifestSha256': 'unit-source', 'gatewaySha256': 'unit-native',
            'sidecarSha256': 'unit-sidecar', 'fixedTS': self.d.RETIREMENT_BASELINE_REVISION,
        })
        row = {'id': 'gateway.channel:telegram', 'profile': 'native-eight-v2',
               'rawEvidence': raw_ref, 'processReceipt': {'path': 'unit-native-provenance'}}
        self.write_json('channels.json', {'schemaVersion': 1, 'entries': [row]})
        applicability = {face: {'applicable': False, 'kinds': []}
                         for face in ('attachment', 'channelInteraction')}
        semantic = {'equal': True, 'differentFields': [], 'formalAcceptance': False}
        observation = {'unitNativeObservation': True}
        tcp = [{'kind': kind, 'sequence': sequence,
                'socket': {'id': 9, 'localPort': 28690, 'remotePort': 42000},
                'payload': {'bytes': 1, 'rawBytesHex': '78', 'truncated': False}}
               for sequence, kind in enumerate(('fixture-tcp-data', 'fixture-tcp-write'))]
        for records in ([], tcp):
            with self.subTest(retained_tcp=bool(records)):
                transport = self.out / 'transport.jsonl'
                transport.write_text(''.join(json.dumps(record) + '\n' for record in records))
                evidence = dict(self.evidence, receipt={'transportTrace': reference(transport)})
                # This tests the consumer boundary: even a successful semantic
                # result cannot supply the missing independent wire proof.
                paired = {'observations': observation, 'semanticComparison': semantic,
                          'missing': [], 'originals': []}
                with patch.object(entries, 'verify_owned_process', return_value=evidence), \
                        patch.object(entries, 'channel_applicability', return_value=applicability), \
                        patch.object(entries, 'channel_v2_comparison', return_value=paired):
                    cell = entries.channel_results(self.d, native, self.out,
                                                   baseline=self.out / 'unused-baseline')[row['id']]
                self.assertEqual(cell['status'], 'BLOCKED', cell)
                self.assertEqual(cell['semanticComparison'], semantic)
                self.assertIn('fixed-ts-v2-semantic-business-comparison', cell['covered'])
                self.assertNotIn('comparison', cell)
                self.assertNotIn('fixed-ts-v2-business-contract-comparison', cell['covered'])
                self.assertIn('fixed-ts-business-contract-comparison', cell['missing'])
                self.assertIn('v2-http-same-socket-wire-binding', cell['missing'])
                self.assertIn('v2-websocket-frame-and-header-binding', cell['missing'])


if __name__ == '__main__':
    unittest.main()
