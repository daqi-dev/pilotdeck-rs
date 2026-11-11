"""Recompute #1356 cold-cut and real drain-failure evidence on the same build."""
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
import shutil

atexit.register(shutil.rmtree, sys.pycache_prefix, ignore_errors=True)
from datetime import datetime
import hashlib
import json
import re


PHASES = ('ts-seed', 'rust-write', 'ts-readback', 'rust-again', 'rust-final-read')
ENGINES = ('ts', 'rust', 'ts', 'rust', 'rust')
CRON_ROLLBACK_DIFF = ('A8.6-COLD-TS-CRON-MIGRATION-001.md', 'd0cb15cd2e9b82721837c0555eb1194191371935291e285ab05fefdd3a27bf93')
CRON_TS_SOURCES = {
    'src/cron/runtime/CronManager.ts': 'c7c4fe74b4a916f5cc159ad5f1119482055d3a3d9355e773a409c71a8c93089b',
    'src/cron/runtime/CronProjectStorageProvider.ts': '380e121ec3c393848e375be23faf2ab0d9d6a08550535c31bec34c6a4e6dade0',
    'src/cron/storage/CronStoreMigration.ts': 'a6c8923632f392e69a0b7fa32cbbcabe134feccb0d50ee576ac8e2108f7757d2',
}


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def exact_cron_startup_delta(d, changes, before, after, original, readback, project, bind):
    """Narrowly classify the two known TS startup effects using original bytes."""
    marker = 'home/cron/store-migration-v1.json'
    d.require(marker in changes and marker not in before and len(changes) <= 2,
              'COLD_DURABILITY', 'expected a new Cron v1 marker and at most one task serialization change')
    data = after[marker].get('jsonValue')
    stamp = data.get('completedAt') if isinstance(data, dict) else None
    d.require(isinstance(data, dict) and list(data) == ['version', 'completedAt']
              and type(data['version']) is int and data['version'] == 1 and isinstance(stamp, str)
              and re.fullmatch(r'\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z', stamp),
              'COLD_DURABILITY', 'invalid TS Cron startup migration marker')
    datetime.fromisoformat(stamp)
    d.require(bind(readback / marker).read_bytes() == (json.dumps(data, indent=2) + '\n').encode(),
              'COLD_DURABILITY', 'migration marker differs from canonical TS serialization')
    paths = [key for key in changes if key != marker]
    for path in paths:
        parts = Path(path).parts
        d.require(len(parts) == 5 and parts[:3] == ('home', 'cron', 'projects') and parts[-1] == 'tasks.json'
                  and path in before,
                  'COLD_DURABILITY', 'only the existing project Cron task store may be normalized')
        cwd = Path(path).with_name('.cwd')
        d.require(bind(original / cwd).read_bytes() == bind(readback / cwd).read_bytes()
                  == (str(project) + '\n').encode(),
                  'COLD_DURABILITY', 'task store does not belong to the same staged project')
        old = bind(original / path).read_bytes()
        new = bind(readback / path).read_bytes()
        d.require(bool(old) and not old.endswith(b'\n') and new == old + b'\n',
                  'COLD_DURABILITY', 'task store changed by more than a single trailing LF')
    return {'marker': marker, 'trailingLfOnly': paths, 'allPriorDataRetained': True}


def verify_cold(d, native, folder, baseline):
    require = d.require
    evidence = {}

    def bind(path):
        require(path.is_file() and not path.is_symlink() and path.resolve() == path,
                'COLD_EVIDENCE', f'missing or aliased cold-cut original: {path}')
        evidence[str(path)] = sha(path)
        return path

    def read(relative):
        return d.read(bind(folder / relative))

    run = read('measure/run.json')
    build = read('measure/build-receipt.json')
    preflight = read('inside/network-isolation.json')
    cleanup = read('container-cleanup.json')
    require(run['head'] == build['head'] == native.source['head'] and
            build['sourceManifest'] == {'path': str(native.source_manifest), 'sha256': native.source_sha} and
            build['binaryManifest'] == {'path': str(native.manifest), 'sha256': native.manifest_sha},
            'COLD_SOURCE', 'cold-cut output belongs to a different source/binary build')
    paths = {row['path']: row['sha256'] for row in d.read(native.manifest)}
    require(build['binaries'] == paths, 'COLD_SOURCE', 'cold-cut binary ledger differs from the fixed build')
    libraries = d.common.verify_native_runtime_libraries(native.source, native.manifest)
    require(build.get('runtimeLibraries') == native.source.get('runtimeLibraries'), 'COLD_SOURCE',
            'cold-cut runtime library manifest differs from the fixed build')
    probe = d.ROOT / 'src/pilotdeck-gateway/tests/fixtures/native-cold-switch/probe.py'
    driver = probe.with_name('driver.mts')
    shared = probe.parent.parent / 'native-business-loop/probe.py'
    sources = {str(p): sha(p) for p in (probe, driver, shared, native.manifest, native.source_manifest)}
    require(run['sourceFiles'] == sources, 'COLD_SOURCE', 'cold-cut fixture source changed')
    tests = preflight['negativeControls']
    require(run['networkNamespace'] == preflight['networkNamespace'] and preflight['interfaces'] == [[1, 'lo']]
            and preflight['loopbackConnect'] == 'PASS' and len(tests) == 4
            and {(row['family'], row['kind']) for row in tests} == {(2, 1), (2, 2), (10, 1), (10, 2)}
            and all(row['blocked'] is True for row in tests) and cleanup['ownedContainerStillRunning'] == '',
            'COLD_ISOLATION', 'cold-cut loopback controls or owned-container cleanup are unproven')
    require(run['exitCodes'] == {'positive': 0, 'real-drain-failure': 2},
            'COLD_EXECUTION', 'normal and real drain-failure exits are not the registered pair')
    build_receipt_sha = evidence[str(folder / 'measure/build-receipt.json')]
    d._retirement_baseline_revision(baseline)
    ts_dist = sha(baseline / 'dist/src/cli/pilotdeck.js')
    ts_gateway_source = sha(baseline / 'src/gateway/server/GatewayServer.ts')
    for label, flags in [('positive', (True, False, False)), ('real-drain-failure', (False, True, True))]:
        result = read(f'measure/{label}/result.json')
        provenance = read(f'measure/{label}/provenance.json')
        require((result['pass'], result['handoffBlocked'], result['realDrainFailureObserved']) == flags
                and all(type(result[key]) is bool for key in ('pass', 'handoffBlocked', 'realDrainFailureObserved'))
                and result['cleanupVerified'] is True and result['remainingOwnedIdentities'] == []
                and result['binariesUnchanged'] is True and result['noSnapshotRestorePerformed'] is True
                and result['sourceReceiptSha256'] == build_receipt_sha
                and provenance['sourceReceipt']['sha256'] == build_receipt_sha
                and provenance['driverSha256'] == sources[str(driver)]
                and provenance['tsDistSha256'] == ts_dist and provenance['tsGatewaySourceSha256'] == ts_gateway_source
                and [provenance['nodeEnvironment']['versions'].get(key) for key in ('node', 'icu', 'uv')]
                == ['22.22.0', '78.2', '1.52.1'],
                'COLD_EXECUTION', f'{label} results do not establish their owned execution')
        for name in ('gateway', 'sidecar'):
            expected = paths['pilotdeck-gateway' if name == 'gateway' else 'pilotdeck-agent-loop-sidecar']
            snapshot = folder / 'measure' / label / 'binaries' / name
            require(provenance['binaries'][name]['sha256'] == expected and (bind(snapshot) and evidence[str(snapshot)] == expected),
                    'COLD_SOURCE', f'{label} {name} snapshot differs from fixed binary')
        snapshots = provenance.get('runtimeLibraries', [])
        require(len(snapshots) == len(libraries), 'COLD_SOURCE', f'{label} runtime snapshots incomplete')
        for copied, library in zip(snapshots, libraries):
            expected_path = folder / 'measure' / label / 'binaries' / library['path']
            require(copied == {**library, 'source': str(native.manifest.parent / library['path']),
                               'snapshot': str(expected_path)} and bind(expected_path).stat().st_size == library['bytes']
                    and evidence[str(expected_path)] == library['sha256'],
                    'COLD_SOURCE', f'{label} runtime snapshot differs: {library["path"]}')

    def snapshot(relative, path_prefix):
        document = read(relative)
        require(isinstance(document, dict) and document, 'COLD_DURABILITY', 'empty durable snapshot')
        for key, row in document.items():
            segments = Path(key).parts
            require(len(segments) >= 2 and segments[0] in ('home', 'project') and '..' not in segments,
                    'COLD_DURABILITY', f'unsafe durable key {key!r}')
            path = bind(folder / path_prefix / key)
            require(path.stat().st_size == row['bytes'] and evidence[str(path)] == row['sha256']
                    and ('jsonValue' not in row or d.read(path) == row['jsonValue']),
                    'COLD_DURABILITY', f'durable bytes differ from snapshot: {key}')
        return document

    snapshots = {}
    for phase, engine in zip(PHASES, ENGINES):
        prefix = f'measure/positive/{phase}'
        start, drain, observed = (read(prefix + '/' + name) for name in ('start.json', 'drain.json', 'result.json'))
        require(start['phase'] == phase and start['engine'] == engine and start['previousDrainVerified'] is True
                and str(start['writerPid']) in start['birthTable'] and drain['exit'] == 0
                and drain['portsClosed'] is True and drain['ownedIdentitiesGone'] is True
                and drain['nextWriterAllowed'] is True and observed['pass'] is True,
                'COLD_HANDOFF', f'{phase} has no valid stop-before-next-owner evidence')
        snapshots[phase] = snapshot(prefix + '/durable-snapshot.json', prefix + '/durable-files')
        require(snapshots[phase] == drain['snapshot'], 'COLD_DURABILITY', 'drain snapshot differs from archived bytes')
    require(snapshots['rust-again'] == snapshots['rust-final-read'], 'COLD_DURABILITY',
            'restart changed durable Rust data')
    before, after = snapshots['rust-write'], snapshots['ts-readback']
    require(set(before) <= set(after), 'COLD_DURABILITY', 'rollback deleted new Rust data')
    mutations = sorted(k for k in after if before.get(k) != after[k])
    # Retain the observed TS startup migration without inventing approval for
    # a write made during readback. The original fixture tests full JSON value
    # preservation and supplies the retained bytes; this report stays BLOCKED.
    readback = read('measure/positive/ts-readback/readback-effects.json')
    require(readback['allPriorValuesPreserved'] is True and readback['noPathsDeleted'] is True
            and sorted(change['path'] for change in readback['changes']) == mutations,
            'COLD_DURABILITY', 'TS readback effect ledger does not match archived bytes')
    registered = False
    startup = None
    if mutations:
        require(all(sha(baseline / name) == digest for name, digest in CRON_TS_SOURCES.items()),
                'COLD_SOURCE', 'fixed TS Cron source differs from the recorded policy')
        original = folder / 'measure/positive/rust-write/durable-files'
        consumed = folder / 'measure/positive/ts-readback/durable-files'
        startup = exact_cron_startup_delta(d, mutations, before, after, original, consumed,
                                           folder / 'measure/positive/project', bind)
        name, digest = CRON_ROLLBACK_DIFF
        registered_diff = d.ROOT / 'specs/gateway-parity/diffs' / name
        registered = (registered_diff.is_file() and not registered_diff.is_symlink()
                      and sha(registered_diff) == digest and d.frozen_check()['exitCode'] == 0)
    fault_prefix = 'measure/real-drain-failure'
    blocked = read(fault_prefix + '/blocked-next-writer.json')
    upgrade = read(fault_prefix + '/rust-write/survivor-upgrade.json')
    stderr = bind(folder / fault_prefix / 'rust-write/.stderr.log')
    close = bytes.fromhex(blocked['survivorTailHex'])
    require(upgrade['response'].startswith('HTTP/1.1 101') and close == b'\x88\x12\x03\xe9server switching'
            and type(blocked['runtimeExit']) is int and blocked['runtimeExit'] != 0
            and blocked['nextWriterStarted'] is False and blocked['lastDrainCertificate'] is False
            and blocked['runtimeStderrSha256'] == evidence[str(stderr)] and 'drain deadline' in stderr.read_text()
            and not any((folder / fault_prefix / phase).exists() for phase in PHASES[2:]),
            'COLD_REAL_DRAIN', 'real WebSocket drain failure did not stop the next writer')
    snapshot(fault_prefix + '/rust-write/before-failed-drain.json', fault_prefix)
    require(all(sha(Path(path)) == digest for path, digest in evidence.items()), 'COLD_EVIDENCE',
            'cold-cut originals changed during verification')
    status, _ = d.common.aggregate_status('BLOCKED' if mutations and not registered else 'PASS')
    return {'status': status, 'rule': ('COLD_TS_STARTUP_WRITE_UNAPPROVED' if not registered else
                                      'COLD_TS_CRON_MIGRATION_REGISTERED') if mutations else None,
            'exactStartupEffects': startup, 'registeredDifference': registered,
            'phases': list(PHASES), 'realDrainControl': 'PASS', 'durableReadbackChanges': mutations,
            'inputFilesSha256': evidence, 'sourceHead': run['head']}
