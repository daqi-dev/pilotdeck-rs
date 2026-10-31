#!/usr/bin/env python3
"""Linux exec/egress evidence, deliberately outside the frozen scoring apparatus."""
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
TRACE = 'execve,execveat,clone,clone3,fork,vfork,exit,exit_group,connect,sendto,sendmsg,sendmmsg,io_uring_setup'
LIMIT_SECONDS = 60
STRING_LIMIT = 1048576
TRACE_LIMIT = 64 * 1024 * 1024


class Incomplete(ValueError):
    pass


def need(condition, message):
    if not condition:
        raise Incomplete('#1140 ' + message)


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


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


def text_string(value):
    """-xx emits every string byte as hex; no ambiguous C/octal decoding."""
    need(re.fullmatch(r'"(?:\\x[0-9a-f]{2})*"', value) is not None, 'truncated/non-hex string')
    try:
        return bytes.fromhex(value[1:-1].replace('\\x', '')).decode('utf-8', errors='strict')
    except UnicodeDecodeError as error:
        raise Incomplete('#1169 non-UTF-8 trace string') from error


def read_trace(path):
    """Bound allocation on the opened file, including growth after fstat."""
    with Path(path).open('rb') as stream:
        need(os.fstat(stream.fileno()).st_size <= TRACE_LIMIT, 'trace exceeds 64 MiB')
        raw = stream.read(TRACE_LIMIT + 1)
    need(len(raw) <= TRACE_LIMIT, 'trace exceeds 64 MiB')
    try:
        return raw.decode('utf-8', errors='strict')
    except UnicodeDecodeError as error:
        raise Incomplete('#1169 non-UTF-8 trace file') from error


def records(raw):
    pending = {}
    rows = []
    for number, line in enumerate(raw.splitlines(), 1):
        match = re.fullmatch(r'(\d+)\s+(.*)', line)
        need(match is not None, f'unparsed trace line {number}: {line[:120]}')
        pid, body = int(match[1]), match[2]
        if body.endswith('<unfinished ...>'):
            need(pid not in pending, 'duplicate unfinished syscall')
            pending[pid] = (len(rows), body[:-len('<unfinished ...>')])
            rows.append(None)
            continue
        resumed = re.match(r'<\.\.\. (\w+) resumed>(.*)', body)
        if resumed:
            need(pid in pending, 'resumed syscall without entry')
            index, prefix = pending.pop(pid)
            need(prefix.startswith(resumed[1] + '('), 'resumed syscall name changed')
            # Place the completed syscall at its entry, before child execs.
            rows[index] = (pid, prefix + resumed[2])
        else:
            rows.append((pid, body))
    need(not pending, 'truncated unfinished syscall')
    need(bool(rows), 'empty trace')
    return rows


def parse_trace(raw, contract):
    rows = records(raw)
    parents, owners, ended = {}, {}, set()
    processes, events, calls, attempts = [], [], [], []
    by_pid = {}
    root = None
    for tid, body in rows:
        if body.startswith('--- '):
            continue  # Signal delivery is followed by an exit record or more syscalls.
        exited = re.fullmatch(r'\+\+\+ exited with (\d+) \+\+\+', body)
        killed = re.fullmatch(r'\+\+\+ killed by (SIG\w+)(?: \(core dumped\))? \+\+\+', body)
        if exited or killed:
            need(tid in owners and tid not in ended, 'unknown/duplicate task exit')
            ended.add(tid)
            if owners[tid] == tid:
                need(tid in by_pid, 'process exited without an exec witness')
                code = int(exited[1]) if exited else -int(getattr(signal, killed[1]))
                by_pid[tid]['exitCode'] = code
                events.append({'event': 'exit', 'pid': tid, 'exitCode': code})
            continue
        call = re.fullmatch(r'(\w+)\((.*)\)\s+=\s+(.+)', body)
        need(call is not None, 'unparsed syscall: ' + body[:120])
        name, args, result = call.groups()
        # records() pairs by PID and verifies the syscall name before parsing.
        # strace inserts a space before <unfinished ...>; when resumed starts
        # with ')' that separator remains at the end of the argument list.
        args = args.rstrip()
        if name in ('execve', 'execveat'):
            attempts.append({'tid': tid, 'syscall': name, 'arguments': args, 'result': result})
            if result.startswith('-1 '):
                continue
            need(name == 'execve' and result == '0', 'unsupported successful execveat/exec return')
            match = re.fullmatch(r'("(?:\\x[0-9a-f]{2})*"), \[(.*?)\], .+', args)
            need(match is not None, 'truncated/unsupported exec arguments')
            path = text_string(match[1])
            argv = [text_string(s) for s in match[2].split(', ')] if match[2] else []
            allowed = [(role, binary) for role, binary in contract['binaries'].items()
                       if path == binary['path'] and argv == binary['argv']]
            need(len(allowed) == 1, 'undeclared executable/argv')
            role, binary = allowed[0]
            need(tid not in by_pid, 'repeat exec for a PID cannot satisfy frozen audit')
            if root is None:
                need(role == 'host' and not owners, 'first exec is not the host')
                root = tid
                parents[tid] = contract['launcherPid']
                owners[tid] = tid
            need(tid in owners and owners[tid] == tid, 'exec lacks fork lineage or is nonleader thread')
            process = {'pid': tid, 'ppid': parents[tid], 'role': role, 'argv': argv,
                       'executable': path, 'sha256': binary['sha256']}
            processes.append(process)
            by_pid[tid] = process
            events.append({'event': 'exec', **process})
            continue
        need(tid in owners and tid not in ended, 'syscall from untracked/exited task')
        if name in ('clone', 'clone3', 'fork', 'vfork'):
            if result.startswith('-1 '):
                continue
            need(re.fullmatch(r'[1-9]\d*', result) is not None, 'unknown clone result')
            child = int(result)
            need(child not in owners, 'PID reuse is unsupported')
            flags = set(re.findall(r'CLONE_[A-Z_]+', args))
            need(not flags.intersection({'CLONE_UNTRACED', 'CLONE_PARENT'}),
                 'clone flags can escape/alter tracked lineage')
            owners[child] = owners[tid] if 'CLONE_THREAD' in args else child
            parents[child] = owners[tid]
        elif name == 'connect':
            ipv4 = re.search(r'sa_family=AF_INET, sin_port=htons\((\d+)\), sin_addr=inet_addr\(("[^\"]*")\)', args)
            ipv6 = re.search(r'sa_family=AF_INET6, sin6_port=htons\((\d+)\).*inet_pton\(AF_INET6, ("[^\"]*")', args)
            match = ipv4 or ipv6
            need(match is not None, 'unsupported connect address (including local helpers)')
            port, address = int(match[1]), text_string(match[2])
            target = f'[{address}]:{port}' if ipv6 else f'{address}:{port}'
            calls.append({'ownerPid': owners[tid], 'tid': tid, 'target': target,
                          'address': address, 'port': port, 'syscall': name, 'result': result})
        elif name == 'sendmmsg':
            need(False, 'sendmmsg batches are unsupported, including connected sockets')
        elif name in ('sendto', 'sendmsg'):
            # Connected socket writes add no new destination. Addressed datagrams
            # and ancillary descriptor passing cannot be silently certified.
            connected = ((name == 'sendto' and re.search(r', NULL, 0$', args))
                         or (name == 'sendmsg' and 'msg_name=NULL' in args
                             and 'msg_controllen=0' in args))
            need(connected, 'addressed/unsupported datagram or ancillary send')
        elif name == 'io_uring_setup':
            need(result.startswith('-1 '), 'io_uring network/exec effects are unsupported')
        else:
            need(name in ('exit', 'exit_group'), 'unexpected syscall ' + name)
    need(root is not None and set(owners) == ended, 'trace lacks terminal records for every task')
    need(all(owner in by_pid for owner in owners.values()), 'forked process has no exec record')
    need(all(sha(binary['path']) == binary['sha256'] for binary in contract['binaries'].values()),
         'executable bytes changed since prelaunch')
    return {'topology': 'native', 'processes': processes,
            'execWitness': {'complete': True, 'source': 'exec-monitor', 'events': events,
                            'attempts': attempts, 'taskOwners': {str(k): v for k, v in owners.items()}},
            'networkWitness': {'complete': True, 'source': 'strace-connect', 'calls': calls},
            'cleanup': {'allStopped': True}}


def checked_evidence(raw, contract, evidence):
    """Compare derived ledgers to raw trace before calling the frozen consumer.

    Never promote a caller's incomplete/source flags; the audit must reject them.
    The raw trace and prelaunch contract are the independent retained originals.
    """
    replay = parse_trace(raw, contract)
    checked = copy.deepcopy(evidence)
    exec_equal = (evidence['processes'] == replay['processes']
                  and evidence['execWitness'] == replay['execWitness'])
    network_equal = evidence['networkWitness'] == replay['networkWitness']
    checked['execWitness']['complete'] = evidence['execWitness']['complete'] is True and exec_equal
    checked['networkWitness']['complete'] = evidence['networkWitness']['complete'] is True and network_equal
    checked['cleanup']['allStopped'] = evidence['cleanup'] == replay['cleanup']
    return checked


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


def audit(raw, contract, evidence):
    module = frozen_auditor()
    checked = checked_evidence(raw, contract, evidence)
    module.audit_native(checked, contract)
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


def capture(contract, output, environment, *, timeout=LIMIT_SECONDS, exercise=None, validate=None):
    need(sys.platform == 'linux', 'Linux strace required; macOS needs future eslogger + root support')
    strace = shutil.which('strace')
    need(strace is not None, 'strace is not installed')
    need(0 < timeout <= LIMIT_SECONDS, 'capture deadline must be 1..60 seconds')
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
    deferred, pending = False, []
    def cancel(signum, _frame):
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
            deferred = False
            if pending:
                cancel(pending[0], None)
            launch['launcherPid'] = tracer.pid
            save(output / 'launch.json', launch)
            deadline = time.monotonic() + timeout
            if exercise:
                exercise(tracer, output, deadline)
            result = tracer.wait(timeout=max(0.001, deadline - time.monotonic()))
        raw = read_trace(output / 'trace.log')
        evidence = parse_trace(raw, launch)
        need(result == 0, 'tracer/host failed; inspect stderr and trace')
        if validate is not None:
            validate(launch, evidence)
        save(output / 'evidence.json', evidence)
        save(output / 'receipt.json', {'status': 'WITNESS_CAPTURED', 'scoring': 'NOT_RUN',
                                      'traceSha256': sha(output / 'trace.log'),
                                      'launchSha256': sha(output / 'launch.json'),
                                      'prelaunchSha256': sha(output / 'prelaunch.json'),
                                      'evidenceSha256': sha(output / 'evidence.json'), 'tracerExitCode': result})
        return launch, evidence
    except BaseException as error:
        deferred = True  # Repeated cancellation cannot interrupt owned cleanup.
        if tracer is not None and tracer.poll() is None:
            # Popen owns the unreaped tracer; EXITKILL stops every traced task,
            # including a descendant that changed process group/session.
            tracer.kill()
            tracer.wait(timeout=5)
        save(output / 'failure.json', {'status': 'INCOMPLETE', 'error': str(error)})
        raise
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def load_capture(folder):
    folder = Path(folder)
    need(not (folder / 'failure.json').exists(), 'capture carries a failure record')
    receipt = json.loads((folder / 'receipt.json').read_text())
    need(receipt['status'] == 'WITNESS_CAPTURED' and receipt['tracerExitCode'] == 0,
         'capture was not completed successfully')
    raw = read_trace(folder / 'trace.log')
    for name, key in [('trace.log', 'traceSha256'), ('launch.json', 'launchSha256'),
                      ('prelaunch.json', 'prelaunchSha256'), ('evidence.json', 'evidenceSha256')]:
        need(sha(folder / name) == receipt[key], 'capture artifact changed: ' + name)
    contract = json.loads((folder / 'launch.json').read_text())
    prelaunch = json.loads((folder / 'prelaunch.json').read_text())
    need({k: v for k, v in contract.items() if k != 'launcherPid'} == prelaunch,
         'launch contract changed after pre-registration')
    return raw, contract, json.loads((folder / 'evidence.json').read_text())
