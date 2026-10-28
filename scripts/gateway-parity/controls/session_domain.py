#!/usr/bin/env python3
"""E1 mutation fixtures; PASS means apparatus self-test, never native execution."""
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
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code


def _load(name, path):
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        if not isinstance(getattr(existing, '_apparatus_source', None), bytes):
            raise RuntimeError(f'#166 unverified apparatus module cache: {path}')
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        source = path.read_bytes()
        exec(compile(source, str(path), 'exec'), module.__dict__)
    except BaseException:
        sys.modules.pop(name, None)
        raise
    module._apparatus_source = source
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module



def _verify_cli_imports():
    # This trust root uses only stdlib + Git, never a helper from disk common:
    # that module's verifier can itself be the active negative-control mutant.
    import subprocess
    from types import ModuleType
    root = _apparatus.parent.parent
    head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', '--verify', 'HEAD'],
                                   timeout=30).decode().strip()
    source = subprocess.check_output(['git', '-C', str(root), 'show',
                                      f'{head}:scripts/gateway-parity/common.py'], timeout=30)
    trusted = ModuleType('pristine_cli_verifier')
    trusted.__file__ = str(_apparatus / 'common.py')
    exec(compile(source, trusted.__file__, 'exec'), trusted.__dict__)
    trusted.verify_loaded_apparatus(head)


_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap

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

CASES = {
    'ts-gateway': 'TS_HOST',
    'hidden-helper': 'HIDDEN_HELPER',
    'missing-group': 'COVERAGE',
    'fake-unavailable': 'CAPABILITY',
    'dropped-event': 'FRAME_PUBLICATIONS',
    'duplicated-event': 'FRAME_PUBLICATIONS',
    'reordered-event': 'FRAME_PUBLICATIONS',
    'wrong-binding': 'BINDING',
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
                'binding': copy.deepcopy(frame['interactionBinding']), 'observerRows': [row], 'binaries': {}}
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


def check_case(name, output, source=None):
    if name in CALIBRATION_CASES:
        calibration_case(name, output, source)
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
    elif name == 'fake-unavailable':
        evidence['capabilities']['model'] = 'unavailable'
    elif name == 'dropped-event':
        observation['frames'].pop()
    elif name == 'duplicated-event':
        observation['frames'].append(copy.deepcopy(observation['frames'][-1]))
    elif name == 'reordered-event':
        observation['frames'][0], observation['frames'][1] = observation['frames'][1], observation['frames'][0]
    elif name == 'wrong-binding':
        evidence['binding']['generation'] += 1
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
    not_run = [] if source else list(CALIBRATION_CASES)
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
    try:
        _verify_cli_imports()
    except (Exception, KeyboardInterrupt) as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        raise SystemExit(2)
    raise SystemExit(main())
