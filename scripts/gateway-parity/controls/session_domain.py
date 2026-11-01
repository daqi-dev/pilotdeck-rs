#!/usr/bin/env python3
"""E1 mutation fixtures; PASS means apparatus self-test, never native execution."""
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
import argparse
import copy
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
from unittest.mock import patch


SCRIPT = common.SCRIPT
digest = common.digest
save = common.save
session_domain_module = _load('session_domain', common.SCRIPT / 'session_domain.py')
CELLS = session_domain_module.CELLS
EvidenceError = session_domain_module.EvidenceError
audit_native = session_domain_module.audit_native
calibration_coverage = session_domain_module.calibration_coverage
observation_check = session_domain_module.observation_check
read = session_domain_module.read
require = session_domain_module.require
validate_calibration = session_domain_module.validate_calibration
probe_process_group = session_domain_module.probe_process_group
wait_exit_unreaped = session_domain_module.wait_exit_unreaped
cleanup_status = session_domain_module.cleanup_status
HeldProcessGroup = session_domain_module.HeldProcessGroup
live_group_members = session_domain_module.live_group_members
CLEANUP_VERIFIED = session_domain_module.CLEANUP_VERIFIED
CLEANUP_GROUP_NOT_EMPTY = session_domain_module.CLEANUP_GROUP_NOT_EMPTY
CLEANUP_BLOCKED = session_domain_module.CLEANUP_BLOCKED
CLEANUP_NOT_VERIFIED = session_domain_module.CLEANUP_NOT_VERIFIED
CLEANUP_UNVERIFIED_STATUS = session_domain_module.CLEANUP_UNVERIFIED_STATUS

# Batch aggregation lives in exactly one place: common.aggregate_status().
# Human decision A (2026-10-25, decision-164): "装置报告的顶层状态不得比它的
# 任一子核验更乐观".  Every top-level status/exit computation below calls it.

CALIBRATION_CASES = {
    'calibration-repetition': 'COVERAGE',
    'calibration-phase': 'COVERAGE',
    'calibration-variant': 'COVERAGE',
    'calibration-empty-observer': 'EMPTY_OBSERVER',
    'calibration-capability': 'CAPABILITY',
    'calibration-independent-oracle': 'INDEPENDENT_ORACLE',
    'calibration-identity': 'IDENTITY',
}

# A5.4 expectation-registration controls (#1177).  Each mutates only the
# session-expectations fixture (or the registered scope table) while the
# measured source stays healthy, so the first failing rule names exactly the
# structural check that must catch a corrupted registration: a missing/extra
# case, one mutated surface on either side, a class label that disagrees with
# the preregistered fault configuration, and a rust difference outside the
# A19-HOST entry's listed surfaces (including a shrunk scope table).
# fix-r1 (#1177 review): per-class canonical-output controls.  Every D4-B
# rust-derivation class now has at least one rust-side mutation inside its
# allowed scope, so only the canonical-output comparison can reject it:
# registry events/state/result on the weakest class (D3), the U/E(T) frame
# order (D4), no agent_status:turn_timeout in a status-failed registry (D2),
# both no-registry durable shapes, and the inapplicable-registry shape.
# #1190: the counts are never pinned here or in prose; the meta control
# derives them from this table and machine-checks its bindings against it.
EXPECTATION_CASES = {
    'expectations-missing-case': 'EXPECTATION_COVERAGE',
    'expectations-extra-case': 'EXPECTATION_COVERAGE',
    'expectations-bad-structure': 'EXPECTATION_STRUCTURE',
    'expectations-class-swap': 'EXPECTATION_CLASS',
    'expectations-ts-drift': 'EXPECTATION_TS_DRIFT',
    'expectations-rust-out-of-scope': 'EXPECTATION_SCOPE',
    'expectations-rust-surface': 'EXPECTATION_RUST_DERIVATION',
    'expectations-scope-shrunk': 'EXPECTATION_SCOPE',
    'expectations-rust-registry-state': 'EXPECTATION_RUST_DERIVATION',
    'expectations-rust-registry-result': 'EXPECTATION_RUST_DERIVATION',
    'expectations-rust-registry-events': 'EXPECTATION_RUST_DERIVATION',
    'expectations-rust-events-timeout-status': 'EXPECTATION_RUST_DERIVATION',
    'expectations-rust-frame-swap': 'EXPECTATION_RUST_DERIVATION',
    'expectations-rust-no-registry-durable': 'EXPECTATION_RUST_DERIVATION',
    'expectations-rust-no-registry-status-durable': 'EXPECTATION_RUST_DERIVATION',
    'expectations-rust-no-registry-registry': 'EXPECTATION_STRUCTURE',
    'expectations-rust-registry-inapplicable': 'EXPECTATION_RUST_DERIVATION',
}

# Direct Node-path label controls, paired with the Python calibration-* variants.
# Each mutates exactly one label while the id (the expected source) stays fixed,
# so checkObservationLabels must reject with COVERAGE.
LABEL_CASES = {
    'observation-repetition-zero': ('repetition', 0),
    'observation-repetition-wrong-round': ('repetition', 2),
    'observation-repetition-out-of-range': ('repetition', 6),
    'observation-repetition-boolean': ('repetition', True),
    'observation-repetition-string': ('repetition', '1'),
    'observation-repetition-null': ('repetition', None),
    'observation-phase-label': ('phase', 'tool'),
    'observation-variant-label': ('variant', {'name': 'unknown-first'}),
    'observation-variant-null': ('variant', None),
    'observation-variant-array': ('variant', ['unknown-first']),
    # Five segments: only the four-segment shape check rejects, so removing just
    # that clause must accept (the remaining labels still match the first four).
    'observation-id-shape': ('id', '/extra'),
    # Non-canonical id repetition segment ('01' parses to the same number as the
    # observation label, so only the canonical-integer regex can reject it).
    'observation-id-repetition-noncanonical': ('id-repetition', '01'),
}

# float 1.0 is structurally undetectable on the Node side: JSON.parse turns it
# into the number 1, so neither Number.isInteger nor === can tell it apart. Do
# NOT pretend this is a negative control. Pin the documented acceptance instead
# (a canary: if Node ever rejects it, docs must be updated); the rejecting
# oracle for this variant is Python validate_calibration via calibration-repetition.
LABEL_KNOWN_ACCEPT = {
    'observation-repetition-float-known-accept': ('repetition', 1.0),
}

# #1140 native deadline-scoring controls.  Each mutates exactly one surface of
# a synthetic witness-capture directory (receipt hashes re-pinned, so the
# scoring logic is under test, not the tamper detector) while the rest of the
# capture stays healthy: a missing sidecar descendant, egress the strace
# witness lost while the driver's provider ledger still shows the request, a
# TS gateway in the process tree, one mutated surface of one case's
# observation, a case row the capture did not execute, a contract that
# declares only part of the manifest, an emptied observer, and a binding
# pre-registered before launch, plus the fix-r2 addition: a hello binding
# damaged after capture, and the fix-r3 addition: a hello binding
# rewritten to another well-formed value, plus the cap-fix additions: a
# sidecar the recorder never recorded, a session with two sidecars, and one
# sidecar whose recorded argv is not the pre-registered sidecar identity
# (the real assembly spawns exactly one sidecar per session, so the role
# multiset is host x1 plus sidecar x sessions), plus the fix-r4 addition: a
# balanced misallocation whose role multiset is unchanged -- one session with
# two sidecars and another with none -- so only the per-session window
# attribution can reject it.  The healthy predecessor must
# score 72/72 native PASS while the whole batch stays NOT_RUN: cells without
# evidence can never make the deadline suite PASS.
NATIVE_DEADLINE_CASES = {
    'native-missing-descendant': 'PROCESS_WITNESS',
    'native-missing-egress': 'NETWORK_WITNESS',
    'native-ts-host': 'TS_HOST',
    'native-surface-mismatch': 'EXPECTATION_MISMATCH',
    'native-missing-case': 'COVERAGE',
    'native-empty-observer': 'EMPTY_OBSERVER',
    'native-preregistered-binding': 'BINDING',
    'native-shrunk-manifest': 'COVERAGE',
    'native-forged-hello-binding': 'BINDING',
    'native-forged-binding-value': 'BINDING',
    'native-missing-sidecar': 'NATIVE_PROCESS',
    'native-extra-sidecar': 'NATIVE_PROCESS',
    'native-sidecar-wrong-argv': 'NATIVE_PROCESS',
    'native-sidecar-misallocated': 'NATIVE_PROCESS',
}

CASES = {
    'ts-gateway': 'TS_HOST',
    'hidden-helper': 'HIDDEN_HELPER',
    'missing-group': 'COVERAGE',
    'dropped-event': 'FRAME_PUBLICATIONS',
    'duplicated-event': 'FRAME_PUBLICATIONS',
    'reordered-event': 'FRAME_PUBLICATIONS',
    'forged-binding-source': 'BINDING',
    'empty-observer': 'EMPTY_OBSERVER',
    'both-sides-bad': 'FRAME_PUBLICATIONS',
    'exec-identity-mismatch': 'PROCESS_WITNESS',
    'exit-before-exec': 'PROCESS_WITNESS',
    'short-lived-helper': 'PROCESS_WITNESS',
    'both-sides-bad-binding': 'BINDING',
    'calibration-missing-group': 'COVERAGE',
    'calibration-missing-variant': 'COVERAGE',
    'calibration-unavailable': 'CAPABILITY',
    **CALIBRATION_CASES,
    **EXPECTATION_CASES,
    **NATIVE_DEADLINE_CASES,
    **{name: 'COVERAGE' for name in LABEL_CASES},
    **{name: None for name in LABEL_KNOWN_ACCEPT},
}


def native_fixture(output):
    """Deliberately synthetic identities. These are not Rust host run evidence."""
    binary = Path(sys.executable).resolve()
    frame = {'type': 'hello_ok', 'protocolVersion': '1.2',
             'interactionBinding': {'connectionId': 'fixture-connection', 'generation': 1}}
    payload = json.dumps(frame).encode()
    prefix = bytes([0x81, 126]) + len(payload).to_bytes(2, 'big') if len(payload) >= 126 else bytes([0x81, len(payload)])
    row = {'connection': 1, 'direction': 'receive', 'frame': frame,
           'wireHex': (prefix + payload).hex(),
           'wire': {'opcode': 1, 'fin': True, 'rsv': 0, 'masked': False,
                    'lengthCode': 126 if len(payload) >= 126 else len(payload), 'payloadLength': len(payload)}}
    contract = {'scope': 'SYNTHETIC_CONTROL_ONLY', 'host': 'rust', 'launcherPid': 100,
                'processRoles': ['host', 'sidecar'], 'cases': list(CELLS),
                'externalEndpoints': ['mock-model'], 'connections': 1,
                'capabilities': {'model': 'available', 'tools': 'available'},
                'binding': copy.deepcopy(frame['interactionBinding']), 'observerRows': [row],
                'bindingSource': 'observer-hello', 'binaries': {}}
    processes = []
    for pid, role in ((101, 'host'), (102, 'sidecar')):
        identity = {'path': str(binary), 'sha256': digest(binary),
                    'argv': [str(binary), f'synthetic-{role}-fixture']}
        contract['binaries'][role] = identity
        processes.append({'pid': pid, 'ppid': 100 if role == 'host' else 101, 'role': role,
                          'kind': 'fixture', 'argv': identity['argv'], 'executable': str(binary),
                          'sha256': identity['sha256']})
    surfaces = {}
    for name in ('output', 'disk', 'http'):
        path = output / (name + '.json')
        save(path, {'scope': 'SYNTHETIC_CONTROL_ONLY', 'surface': name})
        surfaces[name] = {'status': 'RECORDED', 'artifacts': [{'path': str(path), 'sha256': digest(path)}]}
    evidence = {'scope': 'SYNTHETIC_CONTROL_ONLY', 'topology': 'native', 'processes': processes,
                'execWitness': {'complete': True, 'source': 'exec-monitor',
                                'events': [{'event': 'exec', **copy.deepcopy(p)} for p in processes]
                                          + [{'event': 'exit', 'pid': pid} for pid in (102, 101)]},
                'networkWitness': {'complete': True, 'calls': [{'ownerPid': 101, 'target': 'mock-model'}]},
                'expectedCases': list(CELLS), 'executedCases': list(CELLS),
                'capabilities': copy.deepcopy(contract['capabilities']), 'surfaces': surfaces,
                'cleanup': {'allStopped': True}, 'binding': copy.deepcopy(contract['binding']),
                'observer': {'connections': 1, 'rows': copy.deepcopy(contract['observerRows'])}}
    return evidence, contract


def observation_fixture(source):
    if source:
        return read(source / 'raw/no-registry-fast.json')['observations'][0]
    # Small healthy rule fixture for offline self-tests; never counted as TS data.
    frames = [{'type': 'turn_started'}, {'type': 'error', 'code': 'turn_timeout'}]
    return {'id': 'synthetic-control-only/healthy/model/1',
            'variant': {'name': 'healthy'}, 'phase': 'model', 'repetition': 1, 'frames': frames,
            'timeline': [{'kind': 'queue.enqueue', 'closed': False, 'event': copy.deepcopy(f)} for f in frames],
            'durableStatus': {'entries': [], 'rawText': ''},
            'registry': {'applicable': False, 'reason': 'runRegistry was not configured'}}


def calibration_case(name, output, source):
    """Mutate fresh reads only; validate every variant against a healthy real run."""
    require(source is not None, 'CONTROL_PRECONDITION', 'calibration control requires --source')
    source = source.resolve()
    baseline = Path(read(source / 'launch.json')['environment']['A52_BASELINE']).resolve()
    try:
        validate_calibration(source, output / 'healthy-calibration', baseline)
    except EvidenceError as error:
        raise EvidenceError('CONTROL_PRECONDITION', f'healthy calibration failed: {error.rule}: {error}') from error

    cell = 'registry-fast' if name == 'calibration-capability' else 'no-registry-fast'
    target = source / 'raw' / (cell + '.json')
    original = read(target)
    if name == 'calibration-identity':
        target = source / 'raw/manifest.json'
        original = read(target)
        variants = [('baseline-commit', ('baselineCommit',), '0' * 40),
                    ('fixture-hash', ('fixtureFiles', 0, 'sha256'), '0' * 64),
                    ('imported-source-hash', ('importedSources', 0, 'sha256'), '0' * 64)]
    else:
        observation = original['observations'][0]
        prefix = ('observations', 0)
        if name == 'calibration-repetition':
            variants = [(label, prefix + ('repetition',), value) for label, value in
                        (('zero', 0), ('wrong-round', 2), ('out-of-range', 6),
                         ('boolean', True), ('float', 1.0), ('string', '1'), ('null', None))]
        elif name == 'calibration-phase':
            variants = [('wrong-phase', prefix + ('phase',), 'tool')]
        elif name == 'calibration-variant':
            variants = [('wrong-variant', prefix + ('variant', 'name'), 'unknown-first')]
        elif name == 'calibration-empty-observer':
            variants = [(field, prefix + (field,), []) for field in ('frames', 'timeline')]
        elif name == 'calibration-capability':
            variants = [('registry-applicable', prefix + ('registry', 'applicable'),
                         not observation['registry']['applicable'])]
        else:
            variants = [('missing-loop-final', prefix + ('timeline',),
                         [row for row in observation['timeline'] if row['kind'] != 'loop.final'])]

    results = []
    for variant, keys, value in variants:
        mutated = copy.deepcopy(original)
        parent = mutated
        for key in keys[:-1]:
            parent = parent[key]
        parent[keys[-1]] = value

        def mutated_read(path):
            return copy.deepcopy(mutated) if Path(path).resolve() == target else read(path)

        failure = None
        with patch('session_domain.read', side_effect=mutated_read):
            try:
                validate_calibration(source, output / variant, baseline)
            except EvidenceError as error:
                failure = error
        results.append({'variant': variant, 'firstRule': failure.rule if failure else None,
                        'detail': str(failure) if failure else 'mutation accepted'})
        save(output / 'calibration-mutations.json', results)
        require(failure is not None and failure.rule == CASES[name], 'CONTROL_EXPECTATION',
                f'{variant}: expected {CASES[name]}, got {results[-1]}')
    raise failure


def expectation_case(name, output, source):
    """Mutate fresh reads of the expectations fixture only; the measured source
    stays healthy, so the first failing rule names the structural check that
    must reject a corrupted A19-HOST registration (#1177)."""
    require(source is not None, 'CONTROL_PRECONDITION', 'expectation control requires --source')
    source = source.resolve()
    baseline = Path(read(source / 'launch.json')['environment']['A52_BASELINE']).resolve()
    try:
        validate_calibration(source, output / 'healthy-expectations', baseline)
    except EvidenceError as error:
        raise EvidenceError('CONTROL_PRECONDITION', f'healthy calibration failed: {error.rule}: {error}') from error

    if name == 'expectations-scope-shrunk':
        # Shrink the registered scope table: the canonical durable difference of
        # the timeout-first no-registry cases is now outside the listed surfaces
        # and the scope check itself must reject it.
        shrunk = {cls: tuple(surface for surface in surfaces if surface != 'durableStatus')
                  for cls, surfaces in session_domain_module.A19_ALLOWED_SURFACES.items()}
        failure = None
        with patch.object(session_domain_module, 'A19_ALLOWED_SURFACES', shrunk):
            try:
                validate_calibration(source, output / 'scope-shrunk', baseline)
            except EvidenceError as error:
                failure = error
        save(output / 'expectation-mutation.json',
             {'case': name, 'firstRule': failure.rule if failure else None,
              'detail': str(failure) if failure else 'mutation accepted'})
        require(failure is not None and failure.rule == CASES[name], 'CONTROL_EXPECTATION',
                'scope table shrunk: expected ' + CASES[name]
                + (', got ' + failure.rule if failure else ', mutation accepted'))
        raise failure

    fixture = SCRIPT / 'fixtures/session-expectations.json'
    if name == 'expectations-rust-registry-inapplicable':
        # #1190: bind the canonical inapplicable-registry require. The same
        # corrupted shape is rejected earlier by the structure applicability
        # clause and the registry.applicable scope surface (defense in depth;
        # expectations-rust-no-registry-registry binds the structure layer),
        # so this control relaxes exactly those two earlier rejections and the
        # canonical output comparison itself must reject a rust registry that
        # claims applicability without a run registry.  fix-r1 (#1190 review
        # P3): the relaxation names its target -- only the mutated row's
        # structure applicability clause, only the target class's scope
        # surface -- and the failure must be the target require's own
        # message, so no other row or class is let through and the meta
        # control's KILLED is attributable to this require.
        target = 'no-registry-fast/fast/model'
        target_class = 'no-registry-timeout-final'
        target_detail = target + ': rust registry must be inapplicable ' \
                                'without a run registry'
        document = copy.deepcopy(read(fixture))
        row = next(r for r in document['cases'] if r['case'] == target)
        row['rust']['registry'] = {'applicable': True,
                                  'events': ['1:agent_status:turn_timeout'],
                                  'state': 'running', 'result': None}

        def mutated_read(path):
            return copy.deepcopy(document) if Path(path).resolve() == fixture else read(path)

        original_structure = session_domain_module._expectation_structure

        def relaxed_structure(check_row):
            problem = original_structure(check_row)
            if (isinstance(check_row, dict) and check_row.get('case') == target
                    and problem == 'registry applicability'):
                return None
            return problem

        failure = None
        with patch('session_domain.read', side_effect=mutated_read), \
                patch.object(session_domain_module, '_expectation_structure',
                             side_effect=relaxed_structure), \
                patch.object(session_domain_module, 'A19_ALLOWED_SURFACES',
                             {label: tuple(surfaces)
                              + (('registry.applicable',) if label == target_class else ())
                              for label, surfaces
                              in session_domain_module.A19_ALLOWED_SURFACES.items()}):
            try:
                validate_calibration(source, output / name, baseline)
            except EvidenceError as error:
                failure = error
        save(output / 'expectation-mutation.json',
             {'case': name, 'targetCase': target,
              'firstRule': failure.rule if failure else None,
              'detail': str(failure) if failure else 'mutation accepted'})
        require(failure is not None and failure.rule == CASES[name]
                and str(failure) == target_detail, 'CONTROL_EXPECTATION',
                name + ': expected ' + CASES[name] + ' with the target require detail '
                + (repr(target_detail) + ', got ' + repr(str(failure)) if failure
                   else ', mutation accepted'))
        raise failure
    document = copy.deepcopy(read(fixture))
    row = next(r for r in document['cases'] if r['case'] == 'registry-fast/fast/model')
    if name == 'expectations-missing-case':
        document['cases'].remove(row)
    elif name == 'expectations-extra-case':
        extra = copy.deepcopy(row)
        extra['case'] = 'registry-fast/fast/extra'
        document['cases'].append(extra)
    elif name == 'expectations-bad-structure':
        row['rust']['registry'] = {'applicable': 'yes'}
    elif name == 'expectations-class-swap':
        row['a19Class'] = 'registry-timeout-status-failed'
    elif name == 'expectations-ts-drift':
        row = next(r for r in document['cases'] if r['case'] == 'no-registry-fast/fast/model')
        row['ts']['frames'] = row['ts']['frames'][:1]
    elif name == 'expectations-rust-out-of-scope':
        # ts-equal case: no surface may differ, so any rust edit is out of scope.
        row = next(r for r in document['cases'] if r['case'] == 'no-registry-fast/unknown-first/model')
        row['rust']['durableStatus'] = ['turn_timeout']
    elif name == 'expectations-rust-surface':
        # An allowed surface: only the canonical-output check can catch the edit.
        row['rust']['frames'] = row['rust']['frames'][:1]
    elif name == 'expectations-rust-registry-state':
        # D3 weakest derivation: the registry state is the disk fact (running).
        row = next(r for r in document['cases']
                   if r['case'] == 'registry-failure/timeout-append-fails-status-fast/model')
        row['rust']['registry']['state'] = 'failed'
    elif name == 'expectations-rust-registry-result':
        # D3: a result that was never appended must stay absent.
        row = next(r for r in document['cases']
                   if r['case'] == 'registry-failure/timeout-append-fails-status-fast/model')
        row['rust']['registry']['result'] = session_domain_module.A19_TIMEOUT_ERROR
    elif name == 'expectations-rust-registry-events':
        # registry-timeout-final: the anchored registry event sequence itself.
        row = next(r for r in document['cases'] if r['case'] == 'registry-fast/fast/model')
        events = row['rust']['registry']['events']
        seq, _, label = events[-1].partition(':')
        events[-1] = f'{int(seq) + 1}:{label}'
    elif name == 'expectations-rust-events-timeout-status':
        # D2: a durable write failure must not register a timeout status event.
        row = next(r for r in document['cases']
                   if r['case'] == 'registry-failure/timeout-status-fails-append-fast/model')
        events = row['rust']['registry']['events']
        first = int(events[0].partition(':')[0])
        events.insert(0, f'{first}:{session_domain_module.A19_TIMEOUT_STATUS}')
    elif name == 'expectations-rust-frame-swap':
        # D4: the frame order is the enqueue order; U comes before E(T).
        row = next(r for r in document['cases']
                   if r['case'] == 'registry-failure/timeout-status-event-append-fails/model')
        row['rust']['frames'][0], row['rust']['frames'][1] = \
            row['rust']['frames'][1], row['rust']['frames'][0]
    elif name == 'expectations-rust-no-registry-durable':
        # no-registry-timeout-final: the durable status difference itself.
        row = next(r for r in document['cases'] if r['case'] == 'no-registry-fast/fast/model')
        row['rust']['durableStatus'] = []
    elif name == 'expectations-rust-no-registry-status-durable':
        # no-registry-timeout-status-failed: durable stays empty.
        row = next(r for r in document['cases']
                   if r['case'] == 'no-registry-failure/timeout-status-fails/model')
        row['rust']['durableStatus'] = ['turn_timeout']
    elif name == 'expectations-rust-no-registry-registry':
        # Without a run registry the rust expectation must stay inapplicable.
        # The ts side keeps the measured inapplicable registry, so the
        # applicability-equality structure check rejects this first; the
        # canonical-output require on the same shape is defense in depth.
        row = next(r for r in document['cases'] if r['case'] == 'no-registry-fast/fast/model')
        row['rust']['registry'] = {'applicable': True,
                                   'events': ['1:agent_status:turn_timeout'],
                                   'state': 'running', 'result': None}

    def mutated_read(path):
        return copy.deepcopy(document) if Path(path).resolve() == fixture else read(path)

    failure = None
    with patch('session_domain.read', side_effect=mutated_read):
        try:
            validate_calibration(source, output / name, baseline)
        except EvidenceError as error:
            failure = error
    save(output / 'expectation-mutation.json',
         {'case': name, 'firstRule': failure.rule if failure else None,
          'detail': str(failure) if failure else 'mutation accepted'})
    require(failure is not None and failure.rule == CASES[name], 'CONTROL_EXPECTATION',
            name + ': expected ' + CASES[name]
            + (', got ' + failure.rule if failure else ', mutation accepted'))
    raise failure


def observation_from_expectation(expectation):
    """Inverse projection: an observation whose three surfaces project onto *expectation*.

    Registered labels are ``type:detail`` strings; rebuild raw events that
    ``project_observation``/``_session_label`` map back onto exactly those
    labels, so a fixture built from the rust expectation scores PASS unless a
    control mutates a surface.
    """
    def raw(label):
        kind, _, detail = label.partition(':')
        return {'type': kind, 'event': detail}
    registry = expectation['registry']
    if registry.get('applicable'):
        durable_registry = {'applicable': True,
                            'events': [{'seq': int(item.partition(':')[0]),
                                        'event': raw(item.partition(':')[2])}
                                       for item in registry['events']],
                            'record': {'state': registry['state'],
                                       'result': None if registry['result'] is None
                                       else raw(registry['result'])}}
    else:
        durable_registry = {'applicable': False}
    return {'frames': [raw(label) for label in expectation['frames']],
            'durableStatus': {'entries': [{'event': entry} for entry in expectation['durableStatus']]},
            'registry': durable_registry}


# Receipt keys re-pinned when a control rewrites one capture artifact: the
# scoring logic is under test, not the tamper detector.
NATIVE_RECEIPT_KEYS = session_domain_module.native_evidence.NATIVE_RECEIPT_KEYS


def control_wire(frame, direction='receive'):
    payload = json.dumps(frame).encode()
    size = len(payload)
    code = size if size < 126 else 126 if size <= 65535 else 127
    extra = b'' if code < 126 else size.to_bytes(2 if code == 126 else 8, 'big')
    masked = direction == 'send'
    mask = b'\x01\x02\x03\x04' if masked else b''
    data = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload)) if masked else payload
    wire = bytes([129, code | (128 if masked else 0)]) + extra + mask + data
    return {'connection': 1, 'direction': direction, 'frame': payload.decode() if masked else frame,
            'wireHex': wire.hex(), 'wire': {'opcode': 1, 'fin': True, 'rsv': 0,
                                           'masked': masked, 'lengthCode': code, 'payloadLength': size}}


def native_capture_fixture(output):
    """Complete simulated originals; explicitly ineligible for formal scoring."""
    n = session_domain_module.native_evidence
    folder = (output / 'capture-fixture').resolve()
    home, project = folder / 'home', str(folder / 'project')
    capture, sessions = folder / 'capture', folder / 'session-domain'
    capture.mkdir(parents=True)
    sessions.mkdir()
    binary = str(Path(sys.executable).resolve())
    cases = read(SCRIPT / 'fixtures/session-calibration.json')['cases']
    argv = [binary, '--native', '--listen', '127.0.0.1:28999', '--home', str(home), '--project', project]
    contract = {'scope': 'SYNTHETIC_CONTROL_ONLY', 'host': 'rust', 'project': project,
                'processRoles': ['host'] + ['sidecar'] * len(cases),
                'externalEndpoints': ['127.0.0.1:29000'], 'connections': 1,
                'capabilities': {'provider': 'loopback-mock', 'tools': []}, 'cases': cases,
                'binaries': {'host': {'path': binary, 'sha256': digest(Path(binary)), 'argv': argv},
                             'sidecar': {'path': binary, 'sha256': digest(Path(binary)), 'argv': [binary]}}}
    def hx(value):
        return '"' + ''.join(f'\\x{byte:02x}' for byte in value.encode()) + '"'
    def execute(pid, args):
        return f'{pid} execve({hx(args[0])}, [{", ".join(hx(arg) for arg in args)}], 0x1 /* 0 vars */) = 0\n'
    raw = execute(101, argv)
    binding = {'connectionId': 'fixture-connection', 'generation': 1}
    observer = [control_wire({'type': 'hello_ok', 'protocolVersion': '1.2', 'interactionBinding': binding})]
    rows, requests, sources, identities = [], [], [], []
    expectations = {row['case']: row['rust'] for row in read(SCRIPT / 'fixtures/session-expectations.json')['cases']}
    for index, case in enumerate(cases, 1):
        pid, key = 101 + index, n.session_key(case)
        path = str(home / 'projects' / 'fixture' / 'chats' / (key + '.jsonl'))
        identity = {'requestId': f'case-{index:02d}', 'runId': f'run-{index:02d}',
                    'projectKey': project, 'sessionKey': key, 'transcriptPath': path}
        message = f'[native-witness:{case}] controlled wait'
        params = {k: identity[k] for k in ('projectKey', 'sessionKey', 'runId')}
        params['message'] = message
        observer.append(control_wire({'type': 'request', 'id': identity['requestId'], 'method': 'submit_turn', 'params': params}, 'send'))
        observation = observation_from_expectation(expectations[case])
        observation['frames'].append({'type': 'turn_started', 'event': 'completed'})
        for seq, event in enumerate(observation['frames']):
            observer.append(control_wire({'type': 'event', 'id': identity['requestId'], 'seq': seq,
                                          'final': seq == len(observation['frames']) - 1, 'event': event}))
        entries = [dict(entry, type='agent_status_message') for entry in observation['durableStatus']['entries']]
        observation['durableStatus']['entries'] = entries
        registry = observation['registry']
        if registry['applicable']:
            registry['record'].update({k: identity[k] for k in ('projectKey', 'sessionKey', 'runId')})
            runs = {'events': {project + '\0' + key + '\0' + identity['runId']: registry['events']}, 'records': [registry['record']]}
        else:
            registry.update(events=[], record={})
            runs = {'events': {}, 'records': []}
        source = {'transcriptPath': path, 'transcriptText': ''.join(json.dumps(entry) + '\n' for entry in entries),
                  'projectMarkerPath': str(home / 'projects/fixture/.cwd'), 'projectMarkerText': project,
                  'registryPath': str(home / 'runs.json'), 'registryText': json.dumps(runs)}
        rows.append({'case': case, 'identity': identity, 'observation': observation})
        sources.append(source)
        identities.append(identity)
        requests.append({'case': case, 'path': '/v1/chat/completions',
                         'raw': json.dumps({'model': 'mock', 'stream': True, 'messages': [{'role': 'user', 'content': message}]})})
        raw += (f'101 openat(AT_FDCWD, {hx(path)}, O_WRONLY|O_CREAT|O_APPEND, 0644) = 5\n'
                + f'101 clone(flags=SIGCHLD) = {pid}\n' + execute(pid, [binary])
                + f'{pid} connect(3, {{sa_family=AF_INET, sin_port=htons(29000), sin_addr=inet_addr({hx("127.0.0.1")})}}, 16) = 0\n'
                + f'{pid} +++ exited with 0 +++\n')
    raw += '101 +++ exited with 0 +++\n'
    launch = dict(contract, launcherPid=100)
    evidence = dict(n.parse_trace(raw, launch), scope='SYNTHETIC_CONTROL_ONLY')
    save(capture / 'prelaunch.json', contract)
    save(capture / 'launch.json', launch)
    (capture / 'trace.log').write_text(raw)
    save(capture / 'evidence.json', evidence)
    save(sessions / 'observer.json', {'connections': 1, 'rows': observer})
    save(sessions / 'cases.json', {'cases': rows})
    save(sessions / 'provider.json', {'endpoint': '127.0.0.1:29000', 'requests': requests})
    save(sessions / 'driver.json', {'binding': binding, 'identities': identities})
    save(sessions / 'sources.json', sources)
    save(capture / 'receipt.json', {'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN', 'tracerExitCode': 0,
                                   **{key: digest(folder / name) for name, key in NATIVE_RECEIPT_KEYS.items()}})
    return folder


def control_deadline_report(folder):
    """Labelled simulation route; never invoked by the formal deadline entry."""
    session = session_domain_module
    capture = session._load_native_artifacts(folder)
    return session._deadline_report(session._native_deadline_result(capture))


def native_deadline_case(name, output):
    """#1140 native scoring controls: mutate exactly one capture surface.

    The healthy predecessor must score 72/72 native PASS while the whole batch
    stays NOT_RUN (three cells lack witness evidence and cannot be PASS); the
    #892 runtime observation -- the published batch status equals what the one
    shared aggregation returns for the observed cell statuses -- is pinned in
    test_safety, an aggregation-audit-registered test-only point.
    """
    folder = native_capture_fixture(output)
    session = session_domain_module
    healthy_capture = session._load_native_artifacts(folder)
    healthy = control_deadline_report(folder)
    native = next(cell for cell in healthy['cells'] if (cell['host'], cell['sidecar']) == ('rust', 'rust'))
    require([cell['status'] for cell in healthy['cells']] == ['NOT_RUN', 'NOT_RUN', 'NOT_RUN', 'PASS']
            and native['status'] == 'PASS' and healthy['status'] == 'NOT_RUN'
            and len(native['cases']) == len(native['expectedCases']) == 72
            and all(row['status'] == 'PASS' for row in native['cases']),
            'CONTROL_PRECONDITION',
            'healthy capture must score 72/72 native PASS with a NOT_RUN batch')

    def rewrite(relative, value):
        save(folder / relative, value)
        receipt = read(folder / 'capture/receipt.json')
        receipt[NATIVE_RECEIPT_KEYS[relative]] = digest(folder / relative)
        save(folder / 'capture/receipt.json', receipt)

    if name == 'native-missing-descendant':
        # The descendant stays in the process tree; its exec/exit records are
        # lost, so only the complete exec witness can reject (ps sampling
        # cannot see short-lived children either).
        evidence = read(folder / 'capture/evidence.json')
        evidence['execWitness']['events'] = [e for e in evidence['execWitness']['events'] if e.get('pid') != 102]
        rewrite('capture/evidence.json', evidence)
    elif name == 'native-missing-egress':
        evidence = read(folder / 'capture/evidence.json')
        evidence['networkWitness']['calls'] = []
        rewrite('capture/evidence.json', evidence)
        trace = folder / 'capture/trace.log'
        trace.write_text(''.join(line for line in trace.read_text().splitlines(True) if ' connect(' not in line))
        receipt = read(folder / 'capture/receipt.json')
        receipt['traceSha256'] = digest(trace)
        save(folder / 'capture/receipt.json', receipt)
    elif name == 'native-ts-host':
        evidence = read(folder / 'capture/evidence.json')
        evidence['processes'][0]['argv'] = ['node', '/baseline/gateway-cli.mjs']
        rewrite('capture/evidence.json', evidence)
    elif name == 'native-surface-mismatch':
        document = read(folder / 'session-domain/cases.json')
        document['cases'][0]['observation']['frames'][0] = {'type': 'agent_status', 'event': 'gateway_submit_failed'}
        rewrite('session-domain/cases.json', document)
    elif name == 'native-missing-case':
        document = read(folder / 'session-domain/cases.json')
        document['cases'].pop()
        rewrite('session-domain/cases.json', document)
    elif name == 'native-empty-observer':
        observer = read(folder / 'session-domain/observer.json')
        observer['rows'] = []
        observer['connections'] = 0
        rewrite('session-domain/observer.json', observer)
    elif name == 'native-preregistered-binding':
        # A binding known before launch: prelaunch and launch both carry it,
        # so only the provenance rule can reject it.
        binding = {'connectionId': 'pre-registered', 'generation': 1}
        prelaunch = read(folder / 'capture/prelaunch.json')
        launch = read(folder / 'capture/launch.json')
        prelaunch['binding'] = copy.deepcopy(binding)
        launch['binding'] = copy.deepcopy(binding)
        rewrite('capture/prelaunch.json', prelaunch)
        rewrite('capture/launch.json', launch)
    elif name == 'native-shrunk-manifest':
        # The capture contract declares only 71 of the 72 manifest cases: the
        # loader's provenance require fires before any scoring can shrink the
        # denominator.
        prelaunch = read(folder / 'capture/prelaunch.json')
        launch = read(folder / 'capture/launch.json')
        prelaunch['cases'].pop()
        launch['cases'].pop()
        rewrite('capture/prelaunch.json', prelaunch)
        rewrite('capture/launch.json', launch)
    elif name == 'native-forged-hello-binding':
        # The observer's hello binding is damaged after capture (wire bytes
        # recomputed, receipt re-pinned).  The fix-r2 deleted evidence/contract
        # equality can no longer catch it; the observer-side gates must:
        # _observer_binding's hello shape check on the rows themselves.
        observer = read(folder / 'session-domain/observer.json')
        row = observer['rows'][0]
        row['frame'].pop('interactionBinding')
        payload = json.dumps(row['frame']).encode()
        prefix = bytes([0x81, 126]) + len(payload).to_bytes(2, 'big') if len(payload) >= 126 else bytes([0x81, len(payload)])
        row['wireHex'] = (prefix + payload).hex()
        row['wire']['lengthCode'] = 126 if len(payload) >= 126 else len(payload)
        row['wire']['payloadLength'] = len(payload)
        rewrite('session-domain/observer.json', observer)
    elif name == 'native-forged-binding-value':
        # #1140 fix-r3: the hello binding is rewritten to another well-formed
        # value (wire bytes recomputed, receipt re-pinned).  Every observer-side
        # comparison is self-consistent with the rewritten rows, so only the
        # unchanged driver copy can reject this observer-only edit as BINDING.
        # #1206: rewriting the driver and receipt too is outside this control.
        observer = read(folder / 'session-domain/observer.json')
        row = observer['rows'][0]
        row['frame']['interactionBinding'] = {'connectionId': 'forged-connection', 'generation': 2}
        payload = json.dumps(row['frame']).encode()
        prefix = bytes([0x81, 126]) + len(payload).to_bytes(2, 'big') if len(payload) >= 126 else bytes([0x81, len(payload)])
        row['wireHex'] = (prefix + payload).hex()
        row['wire']['lengthCode'] = 126 if len(payload) >= 126 else len(payload)
        row['wire']['payloadLength'] = len(payload)
        rewrite('session-domain/observer.json', observer)
    elif name == 'native-missing-sidecar':
        # #1140 cap-fix 漏记: one session's sidecar exec was never recorded
        # (process and events both absent), so the role multiset loses one
        # sidecar and only the count comparison against the declared contract
        # can reject it.
        evidence = read(folder / 'capture/evidence.json')
        pid = min(p['pid'] for p in evidence['processes'] if p['role'] == 'sidecar')
        evidence['processes'] = [p for p in evidence['processes'] if p['pid'] != pid]
        evidence['execWitness']['events'] = [e for e in evidence['execWitness']['events']
                                             if e.get('pid') != pid]
        rewrite('capture/evidence.json', evidence)
    elif name == 'native-extra-sidecar':
        # #1140 cap-fix 多出: one session spawned two sidecars, so the role
        # multiset carries an extra sidecar the pre-launch contract never
        # declared (fresh pid, parented by the host, fully witnessed).
        evidence = read(folder / 'capture/evidence.json')
        host = next(p for p in evidence['processes'] if p['role'] == 'host')
        template = next(p for p in evidence['processes'] if p['role'] == 'sidecar')
        pid = max(p['pid'] for p in evidence['processes']) + 1
        process = dict(copy.deepcopy(template), pid=pid, ppid=host['pid'])
        evidence['processes'] = evidence['processes'] + [process]
        events = [e for e in evidence['execWitness']['events'] if e['event'] != 'exit'
                  or e['pid'] != host['pid']]
        evidence['execWitness']['events'] = (events + [{'event': 'exec', **copy.deepcopy(process)},
                                                       {'event': 'exit', 'pid': pid},
                                                       {'event': 'exit', 'pid': host['pid']}])
        rewrite('capture/evidence.json', evidence)
    elif name == 'native-sidecar-wrong-argv':
        # #1140 cap-fix 错配-身份: one recorded sidecar's argv is not the
        # pre-registered sidecar identity, so the per-process identity check
        # rejects what the role counts alone would accept.
        evidence = read(folder / 'capture/evidence.json')
        pid = min(p['pid'] for p in evidence['processes'] if p['role'] == 'sidecar')
        forged = [str(Path(sys.executable).resolve()), 'synthetic-sidecar-forged']
        for process in evidence['processes']:
            if process['pid'] == pid:
                process['argv'] = list(forged)
        for event in evidence['execWitness']['events']:
            if event.get('pid') == pid and event['event'] == 'exec':
                event['argv'] = list(forged)
        rewrite('capture/evidence.json', evidence)
    elif name == 'native-sidecar-misallocated':
        # #1140 fix-r4 P1-1 平衡错配: one session spawned two sidecars and
        # another session none -- the role multiset is unchanged (host x1
        # plus sidecar x72) and every argv/ppid/identity check passes, so only
        # the per-session window attribution over the host's transcript opens
        # can reject the capture.  Shifting one marker's afterExecs by one
        # empties its window and doubles its predecessor's, the exact shape
        # a real balanced misallocation leaves in the trace order.
        evidence = read(folder / 'capture/evidence.json')
        opens = evidence['transcriptWitness']['opens']
        opens[len(opens) // 2]['afterExecs'] += 1
        rewrite('capture/evidence.json', evidence)

    def mutated_report():
        # Stage-local audit/comparison controls deliberately start with an
        # authenticated labelled fixture. Formal raw-to-ledger regression
        # tests live in test_chain.py, so replay cannot mask these branches.
        if name in ('native-surface-mismatch', 'native-missing-sidecar', 'native-extra-sidecar',
                    'native-sidecar-wrong-argv', 'native-sidecar-misallocated', 'native-ts-host'):
            capture = copy.deepcopy(healthy_capture)
            capture['evidence'].update(read(folder / 'capture/evidence.json'))
            capture['rows'] = read(folder / 'session-domain/cases.json')['cases']
            for surface in capture['evidence']['surfaces'].values():
                for artifact in surface['artifacts']:
                    artifact['sha256'] = digest(Path(artifact['path']))
            return session._deadline_report(session._native_deadline_result(capture))
        return control_deadline_report(folder)

    if name == 'native-surface-mismatch':
        # A scoring mismatch is an honest FAIL result, not an audit rejection:
        # exactly one case must fail on exactly the mutated surface.
        report = mutated_report()
        cell = next(c for c in report['cells'] if (c['host'], c['sidecar']) == ('rust', 'rust'))
        verdict = next((row for row in cell['cases'] if row['status'] != 'PASS'), None)
        require(report['status'] == 'FAIL' and cell['status'] == 'FAIL' and verdict is not None
                and verdict['rule'] == 'EXPECTATION_MISMATCH' and verdict['surfaces'] == ['frames']
                and sum(1 for row in cell['cases'] if row['status'] == 'PASS') == 71,
                'CONTROL_EXPECTATION',
                'a single mutated surface must fail exactly one case on that surface')
        raise EvidenceError('EXPECTATION_MISMATCH',
                            f"{verdict['case']}: frames differ from the registered rust expectation")
    try:
        mutated_report()
    except EvidenceError as error:
        require(error.rule == CASES[name], 'CONTROL_EXPECTATION',
                f'expected {CASES[name]}, got {getattr(error, "rule", None)}: {error}')
        raise
    require(False, 'CONTROL_EXPECTATION', 'mutation accepted')


def check_case(name, output, source=None):
    if name in CALIBRATION_CASES:
        calibration_case(name, output, source)
        return
    if name in EXPECTATION_CASES:
        expectation_case(name, output, source)
        return
    if name in NATIVE_DEADLINE_CASES:
        native_deadline_case(name, output)
        return
    evidence, contract = native_fixture(output)
    observation = observation_fixture(source)
    # Every mutation has the same known-good predecessor; exceptions are not a pass.
    audit_native(evidence, contract)
    observation_check({'observations': [observation]}, output, 'healthy-observation')
    if name.startswith('calibration-'):
        contract_cases = read(SCRIPT / 'fixtures/session-calibration.json')['cases']
        registration = {'cells': [{'name': cell, 'registry': cell.startswith('registry-'),
                         'variants': [{'name': variant} for variant in
                         dict.fromkeys(case.split('/')[1] for case in contract_cases if case.startswith(cell + '/'))]}
                         for cell in CELLS], 'phases': ['model', 'tool'], 'repeats': 5}
        manifest = {'expectedCases': contract_cases[:], 'executedCases': contract_cases[:]}
        calibration_coverage(registration, manifest)
        removed = registration['cells'][4]['variants']
        if name == 'calibration-unavailable':
            registration['cells'][4]['registry'] = False
            removed = []
        if name == 'calibration-missing-variant':
            removed = removed[:1]
        for variant in removed[:]:
            registration['cells'][4]['variants'].remove(variant)
            for field in ('expectedCases', 'executedCases'):
                manifest[field] = [c for c in manifest[field] if not c.startswith('registry-slow/' + variant['name'] + '/')]
        save(output / 'calibration-mutation.json', {'registration': registration, 'manifest': manifest})
        calibration_coverage(registration, manifest)
    if name == 'ts-gateway':
        evidence['processes'][0]['argv'] = ['node', '/baseline/gateway-cli.mjs']
    elif name == 'hidden-helper':
        evidence['networkWitness']['calls'].append({'ownerPid': 101, 'target': 'internal-ts-helper'})
    elif name == 'missing-group':
        evidence['executedCases'].pop()
    elif name == 'dropped-event':
        observation['frames'].pop()
    elif name == 'duplicated-event':
        observation['frames'].append(copy.deepcopy(observation['frames'][-1]))
    elif name == 'reordered-event':
        observation['frames'][0], observation['frames'][1] = observation['frames'][1], observation['frames'][0]
    elif name == 'forged-binding-source':
        # A binding source claimed at pre-registration time: only the #1140
        # provenance rule can reject it, the binding itself is healthy.
        contract['bindingSource'] = 'pre-registered'
    elif name == 'empty-observer':
        evidence['observer']['rows'] = []
        evidence['observer']['connections'] = 0
    elif name in LABEL_CASES or name in LABEL_KNOWN_ACCEPT:
        field, value = (LABEL_CASES[name] if name in LABEL_CASES
                        else LABEL_KNOWN_ACCEPT[name])
        if field == 'id':
            observation['id'] += value
        elif field == 'id-repetition':
            parts = observation['id'].split('/')
            parts[3] = value
            observation['id'] = '/'.join(parts)
        else:
            observation[field] = value
    elif name == 'both-sides-bad':
        observation['frames'].pop()
    elif name == 'exec-identity-mismatch':
        evidence['execWitness']['events'][0]['argv'] = ['node', 'pilotdeck.js', 'server']
    elif name == 'exit-before-exec':
        evidence['execWitness']['events'].reverse()
    elif name == 'short-lived-helper':
        evidence['execWitness']['events'].insert(1, {'event': 'exec', 'pid': 103})
    elif name == 'both-sides-bad-binding':
        for rows in (evidence['observer']['rows'], contract['observerRows']):
            row = rows[0]
            row['frame']['interactionBinding']['generation'] = 0
            payload = json.dumps(row['frame']).encode()
            prefix = bytes([0x81, 126]) + len(payload).to_bytes(2, 'big') if len(payload) >= 126 else bytes([0x81, len(payload)])
            row['wireHex'] = (prefix + payload).hex()
            row['wire']['lengthCode'] = 126 if len(payload) >= 126 else len(payload)
            row['wire']['payloadLength'] = len(payload)
    save(output / 'contract.json', contract)
    save(output / 'evidence.json', evidence)
    audit_native(evidence, contract)
    payload = ({'pair': [observation, copy.deepcopy(observation)]} if name == 'both-sides-bad'
               else {'observations': [observation]})
    observation_check(payload, output, 'mutated-observation')


def _pid_state(pid):
    """Three-state pid observation; ``blocked`` never means gone."""
    return HeldProcessGroup.observe_pid(pid)


def _grandchild_gone(pid):
    """Only a proven ``missing`` pid may be called gone.

    ``blocked`` (EPERM/EACCES) means this process cannot signal it, not that it
    exited; collapsing it to gone would falsely pass the leftover check.
    """
    return _pid_state(pid) == 'missing'


def cleanup_controls(output):
    """Exercise the ps-free group probe and its top-level status mapping.

    These are positive behavior controls (the apparatus must report the right
    finding), not product mutations.  The post-commit source negative controls
    are under tests/fixtures/session-domain-cleanup/.
    """
    results = []

    def check(name, ok, detail, **extra):
        results.append({'name': name, 'passed': bool(ok), 'detail': detail, **extra})

    def skip(name, reason):
        # A skipped platform-specific case is recorded as passed=None and is not
        # counted as a pass; only Darwin gives the exact group census this
        # apparatus proves.
        results.append({'name': name, 'passed': None, 'skipped': reason, 'detail': reason})

    exact_census = sys.platform == 'darwin'

    def group_empty(pgid):
        # Signal-0 observation through the same ownership entry; it never
        # delivers a signal and is only used after the probe reaped the leader.
        return HeldProcessGroup.observe(pgid) == 'empty'

    def wait_group_empty(pgid, timeout=5):
        """Observe only.  The probe reaped the leader last, so after it returns
        there is no owned pid left to signal; a residual group is a finding."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if group_empty(pgid):
                return True
            time.sleep(.05)
        return group_empty(pgid)

    if exact_census:
        # 1. Leader exits as an unreaped zombie; empty group -> VERIFIED.
        child = subprocess.Popen([sys.executable, '-I', '-c', 'pass'], start_new_session=True)
        probe = probe_process_group(child, wait=10)
        if child.returncode is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        check('cleanup-empty-group', probe['status'] == CLEANUP_VERIFIED
              and probe.get('leaderUnreaped') is True and probe.get('leaderReaped') is True
              and cleanup_status({'processTreeCleanup': probe['status']}) == 'PASS',
              f"status={probe['status']} top={cleanup_status({'processTreeCleanup': probe['status']})} "
              f"leaderUnreaped={probe.get('leaderUnreaped')} leaderReaped={probe.get('leaderReaped')}",
              probe=probe)

    else:
        skip('cleanup-empty-group', 'non-darwin probe is BLOCKED (no exact group census)')

    if exact_census:
        # 2. The launcher exits while a sleep grandchild stays in its group.  The
        # probe waits with WNOWAIT (leader stays a zombie, pgid cannot be reused),
        # reports GROUP_NOT_EMPTY, SIGTERMs that group, and reaps last.  The top
        # level must not claim PASS.  The grandchild's stdio is /dev/null so a
        # parent capture pipe cannot stay open if cleanup fails.
        marker = output / 'leaky-grandchild.pid'
        code = ("import subprocess,sys;"
                "p=subprocess.Popen(['sleep','20'],stdout=subprocess.DEVNULL,"
                "stderr=subprocess.DEVNULL);"
                "open(sys.argv[1],'w').write(str(p.pid))")
        child = subprocess.Popen([sys.executable, '-I', '-c', code, str(marker)],
                                 start_new_session=True)
        probe = probe_process_group(child, wait=10)
        top = cleanup_status({'processTreeCleanup': probe['status']})
        grandchild = int(marker.read_text()) if marker.is_file() else None
        grandchild_state = _pid_state(grandchild) if grandchild is not None else None
        grandchild_gone = _grandchild_gone(grandchild) if grandchild is not None else False
        group_empty_after = wait_group_empty(child.pid)
        check('cleanup-leftover-grandchild',
              probe['status'] == CLEANUP_GROUP_NOT_EMPTY and top == CLEANUP_UNVERIFIED_STATUS
              and grandchild_gone and group_empty_after,
              f"status={probe['status']} top={top} grandchildState={grandchild_state} "
              f"grandchildGone={grandchild_gone} groupEmpty={group_empty_after}", probe=probe)

    else:
        skip('cleanup-leftover-grandchild', 'non-darwin probe is BLOCKED (no exact group census)')

    # 3. An inaccessible probe is BLOCKED and never a PASS.  Both killpg and
    # the leader kill(pid, 0) are denied, so the probe cannot mistake a sandbox
    # denial for "only the zombie leader remains".
    child = subprocess.Popen([sys.executable, '-I', '-c', 'pass'], start_new_session=True)
    state = wait_exit_unreaped(child.pid, 10)
    blocked = {'status': None}
    error = None
    if state == 'EXITED':
        try:
            with patch.object(os, 'killpg', side_effect=PermissionError('denied')), \
                    patch.object(os, 'kill', side_effect=PermissionError('denied')):
                blocked = probe_process_group(child)
        except Exception as exc:  # a removed BLOCKED branch must be visible
            error = f'{type(exc).__name__}: {exc}'
    else:
        error = f'leader state {state}'
    if child.returncode is None:
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
    blocked_top = cleanup_status({'processTreeCleanup': blocked.get('status')})
    check('cleanup-probe-blocked',
          blocked.get('status') == CLEANUP_BLOCKED and blocked_top == CLEANUP_UNVERIFIED_STATUS
          and error is None,
          f"status={blocked.get('status')} top={blocked_top} error={error}", probe=blocked)

    if exact_census:
        # 4. EPERM alone is not "only the zombie leader remains": deny just the
        # group probe while a real live grandchild is in the group.  The independent
        # live-member census must find it, so the probe reports GROUP_NOT_EMPTY and
        # the top level is non-PASS, never VERIFIED.
        folder = output / 'eperm-live-member'
        folder.mkdir(parents=True, exist_ok=False)
        marker = folder / 'grandchild.pid'
        code = ("import subprocess,sys;"
                "p=subprocess.Popen(['sleep','12'],stdout=subprocess.DEVNULL,"
                "stderr=subprocess.DEVNULL);"
                "open(sys.argv[1],'w').write(str(p.pid))")
        child = subprocess.Popen([sys.executable, '-I', '-c', code, str(marker)],
                                 start_new_session=True)
        state = wait_exit_unreaped(child.pid, 10)
        eperm = {'status': None}
        error = None
        if state == 'EXITED':
            try:
                with patch.object(os, 'killpg', side_effect=PermissionError('denied')):
                    eperm = probe_process_group(child)
            except Exception as exc:
                error = f'{type(exc).__name__}: {exc}'
        else:
            error = f'leader state {state}'
        if child.returncode is None:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        eperm_empty = wait_group_empty(child.pid, timeout=15)
        eperm_top = cleanup_status({'processTreeCleanup': eperm.get('status')})
        check('cleanup-eperm-live-member',
              eperm.get('status') == CLEANUP_GROUP_NOT_EMPTY
              and eperm_top == CLEANUP_UNVERIFIED_STATUS and eperm_empty and error is None,
              f"status={eperm.get('status')} top={eperm_top} groupEmpty={eperm_empty} "
              f"error={error}", probe=eperm)

    else:
        skip('cleanup-eperm-live-member', 'non-darwin probe is BLOCKED (no exact group census)')

    # 5. An already-reaped leader gives NOT_VERIFIED and provably no signal.
    child = subprocess.Popen([sys.executable, '-I', '-c', 'pass'], start_new_session=True)
    state = wait_exit_unreaped(child.pid, 10)
    signals = []

    def deny(*args, **_kwargs):
        signals.append(args)
        raise AssertionError('no signal may be sent after the leader was reaped')

    lost = {'status': None}
    error = None
    if state == 'EXITED':
        try:
            with patch.object(session_domain_module, 'wait_exit_unreaped',
                              return_value='REAPED'), \
                    patch.object(os, 'killpg', side_effect=deny):
                lost = probe_process_group(child)
        except Exception as exc:
            error = f'{type(exc).__name__}: {exc}'
    else:
        error = f'leader state {state}'
    if child.returncode is None:
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
    check('cleanup-identity-lost',
          lost.get('status') == CLEANUP_NOT_VERIFIED and not signals and error is None,
          f"status={lost.get('status')} signalCalls={len(signals)} error={error}", probe=lost)

    # 6. A member appears between two complete group snapshots: the first
    # snapshot sees only the zombie leader, the second sees the descendant.
    # Accepting either snapshot alone would report empty; the stable pair must
    # report the live member, and the top level must never be PASS.
    child = subprocess.Popen([sys.executable, '-I', '-c', 'pass'], start_new_session=True)
    state = wait_exit_unreaped(child.pid, 10)
    leader = child.pid
    race = {'status': None}
    delivered = []
    error = None
    if state == 'EXITED':
        snapshot_pids = iter([[leader], [leader, 424244], [leader, 424244],
                              [leader, 424244], [leader], [leader]])

        def next_snapshot(*_args):
            return set(next(snapshot_pids, [leader]))

        def record_group(*args, **_kwargs):
            if len(args) > 1 and args[1] != 0:
                delivered.append(args)
            raise PermissionError('sandbox denies group signals')

        try:
            with patch.object(session_domain_module, 'sys',
                              SimpleNamespace(platform='darwin')), \
                    patch.object(session_domain_module, '_group_snapshot',
                                 side_effect=next_snapshot), \
                    patch.object(os, 'killpg', side_effect=record_group):
                race = probe_process_group(child)
        except Exception as exc:  # a crash must not count as the expected finding
            error = f'{type(exc).__name__}: {exc}'
    else:
        error = f'leader state {state}'
    if child.returncode is None:
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
    race_top = cleanup_status({'processTreeCleanup': race.get('status')})
    check('cleanup-census-snapshot-race',
          race.get('status') == CLEANUP_GROUP_NOT_EMPTY
          and race_top == CLEANUP_UNVERIFIED_STATUS and delivered and error is None,
          f"status={race.get('status')} top={race_top} "
          f"deliveredSignalCalls={len(delivered)} error={error}", probe=race)

    # 7. A stable pair that includes the descendant must report it, not the
    # leader-only first snapshot.
    snapshots = iter([[900], [900, 424244], [900, 424244], [900, 424244], [900, 424244]])

    def next_members(*_args):
        return set(next(snapshots, [900, 424244]))

    with patch.object(session_domain_module, '_group_snapshot', side_effect=next_members):
        members = live_group_members(900)
    check('cleanup-census-descendant', members == {900, 424244},
          f'members={None if members is None else sorted(members)}')

    # 8. Two complete snapshots that disagree mean membership changed while the
    # census was being taken; no set may be reported as stable.
    alternating = iter([[900, 424244], [900, 424245]] * 4)

    def next_unstable(*_args):
        return set(next(alternating, [900, 424245]))

    with patch.object(session_domain_module, '_group_snapshot', side_effect=next_unstable):
        unstable = live_group_members(900)
    check('cleanup-census-unstable', unstable is None,
          f'members={None if unstable is None else sorted(unstable)}')

    # 9. libproc reports a full buffer: the group was truncated, not complete.
    class FullBufferLibrary:
        def __init__(self):
            self.calls = []

        def proc_listpids(self, kind, typeinfo, buffer, size):
            self.calls.append((kind, typeinfo, size))
            if buffer is None:
                return 2832
            buffer[0] = 424242
            return size

    library = FullBufferLibrary()
    truncated = session_domain_module._darwin_group_pids(900, library=library)
    check('cleanup-census-truncated', truncated is None and library.calls[:1] == [(2, 900, 0)],
          f'returned={None if truncated is None else sorted(truncated)} calls={library.calls}')

    # 10. Enumeration failure is not an empty group.
    child = subprocess.Popen([sys.executable, '-I', '-c', 'pass'], start_new_session=True)
    state = wait_exit_unreaped(child.pid, 10)
    unavailable = {'status': None}
    error = None
    if state == 'EXITED':
        try:
            with patch.object(session_domain_module, 'sys',
                              SimpleNamespace(platform='darwin')), \
                    patch.object(session_domain_module, '_group_snapshot', return_value=None), \
                    patch.object(os, 'killpg', side_effect=PermissionError('denied')):
                unavailable = probe_process_group(child)
        except Exception as exc:
            error = f'{type(exc).__name__}: {exc}'
    else:
        error = f'leader state {state}'
    if child.returncode is None:
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
    unavailable_top = cleanup_status({'processTreeCleanup': unavailable.get('status')})
    check('cleanup-census-enumeration-failure',
          unavailable.get('status') == CLEANUP_BLOCKED
          and unavailable_top == CLEANUP_UNVERIFIED_STATUS and error is None,
          f"status={unavailable.get('status')} top={unavailable_top} error={error}",
          probe=unavailable)

    # 11. The single signal entry refuses a reaped leader for both group and
    # direct-pid sends; a denied gate is never a lucky kill.
    child = subprocess.Popen([sys.executable, '-I', '-c', 'pass'])
    child.wait(timeout=10)
    held = HeldProcessGroup(child)
    calls = []

    def deny(*args, **_kwargs):
        calls.append(args)
        raise PermissionError('denied')

    try:
        with patch.object(os, 'killpg', side_effect=deny), \
                patch.object(os, 'kill', side_effect=deny):
            group_outcome = held.send(signal.SIGTERM)
            leader_outcome = held.kill_leader(signal.SIGKILL)
        entry_error = None
    except Exception as exc:
        group_outcome = leader_outcome = (None, None)
        entry_error = f'{type(exc).__name__}: {exc}'
    check('cleanup-signal-entry-reaped',
          entry_error is None and group_outcome[0] == 'refused'
          and leader_outcome[0] == 'refused' and not calls,
          f'group={group_outcome} leader={leader_outcome} calls={len(calls)} error={entry_error}')

    # 12. Positive pair: the same entry really delivers while the leader is a
    # live, unreaped direct child.
    child = subprocess.Popen(['sleep', '30'], start_new_session=True)
    held = HeldProcessGroup(child)
    entry_error = None
    outcome = (None, None)
    try:
        outcome = held.send(signal.SIGTERM)
        child.wait(timeout=10)
    except Exception as exc:
        entry_error = f'{type(exc).__name__}: {exc}'
    finally:
        if child.returncode is None:
            try:
                child.kill()
                child.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                pass
    check('cleanup-signal-entry-held',
          entry_error is None and outcome[0] == 'sent' and child.returncode is not None,
          f'outcome={outcome} returncode={child.returncode} error={entry_error}')

    # 13. kill(pid, 0)=EPERM is "blocked", not "gone": a denied probe must not
    # satisfy the leftover-grandchild assertion.  The real observe_pid path is
    # exercised by denying os.kill, so a call-site or source collapse to a
    # two-state bool turns this red.
    with patch.object(os, 'kill', side_effect=PermissionError('denied')):
        blocked_state = _pid_state(os.getpid())
        blocked_gone = _grandchild_gone(os.getpid())
    check('cleanup-pid-blocked',
          blocked_state == 'blocked' and not blocked_gone,
          f'pidState={blocked_state} grandchildGone={blocked_gone}',
          pidState=blocked_state, grandchildGone=blocked_gone)

    # 14. Linux hidepid-style filtering: /proc shows only the leader while a
    # live same-group descendant is hidden.  The non-darwin gate must report
    # BLOCKED before reading that visible set or sending any signal; reading it
    # as a complete group would return empty/VERIFIED/PASS.
    child = subprocess.Popen([sys.executable, '-I', '-c', 'pass'], start_new_session=True)
    state = wait_exit_unreaped(child.pid, 10)
    leader = child.pid
    visible = iter([[leader]] * 6)
    census_calls = []

    def visible_only(*args):
        census_calls.append(args)
        return set(next(visible, [leader]))

    filtered = {'status': None}
    error = None
    if state == 'EXITED':
        try:
            with patch.object(session_domain_module, 'sys',
                              SimpleNamespace(platform='linux')), \
                    patch.object(session_domain_module, '_group_snapshot',
                                 side_effect=visible_only), \
                    patch.object(os, 'killpg', side_effect=PermissionError('EPERM')):
                filtered = probe_process_group(child)
        except Exception as exc:
            error = f'{type(exc).__name__}: {exc}'
    else:
        error = f'leader state {state}'
    if child.returncode is None:
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
    filtered_top = cleanup_status({'processTreeCleanup': filtered.get('status')})
    check('cleanup-linux-filtered-proc',
          filtered.get('status') == CLEANUP_BLOCKED
          and filtered_top == CLEANUP_UNVERIFIED_STATUS
          and not census_calls and error is None,
          f"status={filtered.get('status')} top={filtered_top} "
          f"censusCalls={len(census_calls)} error={error}", probe=filtered)

    executed = [row for row in results if row.get('passed') is not None]
    skipped = [row['name'] for row in results if row.get('passed') is None]
    case_status = ('FAIL' if not executed or any(not row['passed'] for row in executed)
                   else 'PASS')
    status, _ = common.aggregate_status(case_status,
                                        'PASS_WITH_SKIPS' if skipped else None)
    report = {'status': status,
              'scope': 'cleanup probe behavior controls; not native evidence',
              'skipped': skipped, 'cases': results}
    save(output / 'cleanup-controls.json', report)
    return report

def run_all(output, source=None):
    output.mkdir(parents=True, exist_ok=False)
    results = []
    not_run = [] if source else list(CALIBRATION_CASES) + list(EXPECTATION_CASES)
    for name in ('healthy', *(case for case in CASES if case not in not_run)):
        command = [sys.executable, '-I', str(SCRIPT / 'controls/session_domain.py'),
                   '--case', name, '--output', str(output / name)]
        if source:
            command += ['--source', str(source)]
        proc = subprocess.run(command, capture_output=True, text=True, timeout=60)
        (output / (name + '.stdout')).write_text(proc.stdout)
        (output / (name + '.stderr')).write_text(proc.stderr)
        try:
            verdict = json.loads(proc.stdout)
        except ValueError:
            verdict = {}
        expected = CASES.get(name)
        accept = name == 'healthy' or name in LABEL_KNOWN_ACCEPT
        passed = (proc.returncode == (0 if accept else 1)
                  and verdict.get('rule') == expected
                  and verdict.get('status') == ('PASS' if accept else 'FAIL'))
        results.append({'case': name, 'command': command, 'exitCode': proc.returncode,
                        'expectedRule': expected, 'firstRule': verdict.get('rule'), 'passed': passed})
    cleanup = cleanup_controls(output)
    status, _ = common.aggregate_status(
        'PASS' if all(r['passed'] for r in results) else 'FAIL',
        'PASS_WITH_SKIPS' if not_run else None,
        cleanup['status'])
    report = {'status': status,
              'scope': 'apparatus mutation tests only', 'cases': results,
              'notRun': [{'case': case, 'reason': 'requires real calibration --source'} for case in not_run]}
    report['cleanup'] = cleanup
    report['cleanupSkipped'] = cleanup.get('skipped', [])
    save(output / 'results.json', report)
    return report


def main():
    common.verify_loaded_apparatus()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--case', choices=('all', 'healthy', *CASES), default='all')
    parser.add_argument('--source', type=Path,
                        help='fresh A5.2a output; omitted: synthetic controls only, real calibration controls NOT_RUN')
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    if args.case == 'all':
        report = run_all(args.output.resolve(), args.source)
        print(json.dumps(report, ensure_ascii=False))
        return common.aggregate_status(report['status'])[1]
    args.output.mkdir(parents=True, exist_ok=False)
    try:
        check_case(args.case, args.output.resolve(), args.source)
    except (ValueError, OSError, KeyError, TypeError, StopIteration, subprocess.SubprocessError) as error:
        result = {'status': 'FAIL', 'rule': getattr(error, 'rule', 'CONTROL_EXECUTION'), 'detail': str(error)}
    else:
        result = {'status': 'PASS', 'rule': None}
    result.update(case=args.case, scope='apparatus fixture; not native run evidence')
    save(args.output / 'verdict.json', result)
    print(json.dumps(result, ensure_ascii=False))
    return common.aggregate_status(result['status'])[1]


if __name__ == '__main__':
    run_entry(main, 'traceback')
