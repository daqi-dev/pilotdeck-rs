#!/usr/bin/env python3
"""Linux capture execution; parsing and authentication come from the frozen apparatus."""
import sys

if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
TRACE = 'execve,execveat,clone,clone3,fork,vfork,exit,exit_group,connect,sendto,sendmsg,sendmmsg,io_uring_setup,openat'
LIMIT_SECONDS = 60
STRING_LIMIT = 1048576


# Frozen source is loaded explicitly, with executed bytes retained for provenance.
_native_path = ROOT / 'scripts/gateway-parity/native_evidence.py'
_native_spec = importlib.util.spec_from_file_location('native_evidence', _native_path)
native = sys.modules.get(_native_spec.name)
if native is None:
    native = importlib.util.module_from_spec(_native_spec)
    native._apparatus_source = _native_path.read_bytes()
    exec(compile(native._apparatus_source, str(_native_path), 'exec'), native.__dict__)
    sys.modules[_native_spec.name] = native
Incomplete, need, sha = native.Incomplete, native.need, native.sha
TRACE_LIMIT = native.TRACE_LIMIT
text_string, records = native.text_string, native.records
parse_trace, checked_evidence = native.parse_trace, native.checked_evidence
read_trace, load_capture = native.read_trace, native.load_capture


def save(path, value):
    # Atomic publication; evidence directories are exclusively created by capture.
    path = Path(path)
    with path.with_suffix(path.suffix + '.tmp').open('x') as stream:
        json.dump(value, stream, ensure_ascii=True, indent=2)
        stream.write('\n')
    path.with_suffix(path.suffix + '.tmp').replace(path)


def identity(path, argv):
    path = Path(path).resolve(strict=True)
    with path.open('rb') as executable:
        magic = executable.read(4)
    need(magic == b'\x7fELF', 'only prehashed ELF executables are supported (no shebang/interpreter ambiguity)')
    need(argv and argv[0] == str(path), 'argv[0] must be the absolute executable')
    return {'path': str(path), 'argv': argv, 'sha256': sha(path)}


def frozen_auditor():
    path = ROOT / 'scripts/gateway-parity/session_domain.py'
    spec = importlib.util.spec_from_file_location('native_witness_frozen_session', path)
    module = importlib.util.module_from_spec(spec)
    # Source loader rather than a potentially stale cache; frozen guard remains active.
    source = path.read_bytes()
    exec(compile(source, str(path), 'exec'), module.__dict__)
    module._apparatus_source = source
    sys.modules[spec.name] = module
    module._verify_cli_imports()
    return module


def observer_binding(evidence):
    """The interaction binding the independent observer recorded at hello.

    Paired with session_domain._observer_binding; test_observer_binding_parity
    checks both implementations over the same accepted/rejected inputs.
    A runtime record cannot be pre-registered; the binding exists only once
    the gateway has answered hello, and every hello must agree.
    """
    rows = (evidence.get('observer') or {}).get('rows')
    need(isinstance(rows, list) and bool(rows), 'observer rows are required to bind the hello record')
    hellos = [row['frame'].get('interactionBinding') for row in rows
              if isinstance(row, dict) and isinstance(row.get('frame'), dict)
              and row['frame'].get('type') == 'hello_ok']
    need(bool(hellos), 'observer recorded no server hello/binding')
    need(all(isinstance(binding, dict) for binding in hellos), 'observer hello binding malformed')
    need(all(json.dumps(binding, sort_keys=True) == json.dumps(hellos[0], sort_keys=True)
            for binding in hellos), 'observer hello records disagree on the binding')
    return hellos[0]


def bind_contract(contract, evidence):
    """Bind the observer-recorded hello binding to a launch contract (#1140).

    Mirrors the frozen loader's rule: a contract that already carries a
    binding, observer rows or a binding source is rejected here instead of
    being audited; the runtime record is the only admissible source.
    """
    for key in ('binding', 'observerRows', 'bindingSource'):
        need(key not in contract, 'a pre-registered ' + key + ' cannot audit; bind the observer hello record')
    return dict(contract, binding=observer_binding(evidence),
                observerRows=copy.deepcopy(evidence['observer']['rows']),
                bindingSource='observer-hello')


def audit(raw, contract, evidence):
    module = frozen_auditor()
    checked = checked_evidence(raw, contract, evidence)
    module.audit_native(checked, bind_contract(contract, checked))
    return checked


def command(strace, output, argv):
    return [strace, '--kill-on-exit', '-f', '-q', '-xx', '-s', str(STRING_LIMIT),
            '-e', 'trace=' + TRACE, '-o', str(output / 'trace.log'), '--', *argv]


def stop_host(tracer, pid):
    """Signal only a pidfd still proven to be this unreaped tracer's child."""
    need(tracer.poll() is None, 'tracer exited before orderly shutdown')
    fd = os.pidfd_open(pid)
    try:
        stat = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        need(int(stat[1]) == tracer.pid, 'host is no longer our tracer child')
        signal.pidfd_send_signal(fd, signal.SIGTERM)
    finally:
        os.close(fd)


def capture(contract, output, environment, *, timeout=LIMIT_SECONDS, exercise=None, validate=None,
            limit=LIMIT_SECONDS, ownership=None, defer_receipt=False):
    need(sys.platform == 'linux', 'Linux strace required; macOS needs future eslogger + root support')
    strace = shutil.which('strace')
    need(strace is not None, 'strace is not installed')
    need(0 < timeout <= limit, f'capture deadline must be 1..{limit} seconds')
    # --kill-on-exit is mandatory, never silently downgrade older strace.
    help_text = subprocess.run([strace, '--help'], capture_output=True, text=True, timeout=5, check=True).stdout
    need('--kill-on-exit' in help_text, 'strace lacks --kill-on-exit')
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    launch = copy.deepcopy(contract)
    for binary in launch['binaries'].values():
        need(sha(binary['path']) == binary['sha256'], 'prelaunch binary changed')
    save(output / 'prelaunch.json', launch)
    tracer = None
    ownership = set() if ownership is None else ownership
    owned = None
    deferred, pending = False, []
    def cancel(signum, _frame):
        if isinstance(sys.exception(), KeyboardInterrupt):
            return
        if deferred:
            pending.append(signum)
        else:
            raise KeyboardInterrupt(f'#1140 capture cancelled by signal {signum}')
    previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
    for sig in previous:
        signal.signal(sig, cancel)
    try:
        with (output / 'stdout.log').open('xb') as stdout, (output / 'stderr.log').open('xb') as stderr:
            # Defer Python signal exceptions until ownership is published.
            # The child inherits an unblocked mask (no unsafe preexec in threads).
            deferred = True
            tracer = subprocess.Popen(command(strace, output, launch['binaries']['host']['argv']),
                                      cwd=launch['project'], env=environment, stdin=subprocess.DEVNULL,
                                      stdout=stdout, stderr=stderr, start_new_session=True)
            owned = f'native-witness-tracer:{tracer.pid}'
            ownership.add(owned)
            deferred = False
            if pending:
                cancel(pending[0], None)
            launch['launcherPid'] = tracer.pid
            save(output / 'launch.json', launch)
            deadline = time.monotonic() + timeout
            if exercise:
                exercise(tracer, output, deadline)
            result = tracer.wait(timeout=max(0.001, deadline - time.monotonic()))
            ownership.discard(owned)
        raw = read_trace(output / 'trace.log')
        evidence = parse_trace(raw, launch)
        need(result == 0, 'tracer/host failed; inspect stderr and trace')
        if validate is not None:
            validate(launch, evidence)
        save(output / 'evidence.json', evidence)
        save(output / 'receipt.json', {'status': 'INCOMPLETE' if defer_receipt else 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN',
                                      **{key: sha(output / name)
                                         for name, key in native.receipt_artifacts('capture').items()},
                                      'tracerExitCode': result})
        return launch, evidence
    except BaseException as error:
        deferred = True  # Repeated cancellation cannot interrupt owned cleanup.
        cleanup_errors = []
        if tracer is not None and owned in ownership:
            # kill and wait are independent: a failed signal is not proof of
            # a live process, and only a successful wait confirms reaping.
            try:
                tracer.kill()
            except BaseException as cleanup_error:
                cleanup_errors.append('kill: ' + repr(cleanup_error))
            try:
                tracer.wait(timeout=5)
                ownership.discard(owned)
            except BaseException as cleanup_error:
                cleanup_errors.append('wait: ' + repr(cleanup_error))
        failure = {'status': 'INCOMPLETE', 'error': repr(error),
                   'cleanupErrors': cleanup_errors, 'cleanupPending': sorted(ownership)}
        try:
            save(output / 'failure.json', failure)
        except BaseException as publish_error:
            error.add_note('#1140 failure publication: ' + repr(publish_error))
        for cleanup_error in cleanup_errors:
            error.add_note('#1140 cleanup: ' + cleanup_error)
        raise
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
