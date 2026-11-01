"""Frozen evidence replay, attribution and expectation-derived request needs."""
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
import copy
import hashlib
import json
import re
import signal
from collections import Counter

TRACE_LIMIT = 64 * 1024 * 1024

# One receipt schema for producers, frozen consumers and simulated controls.
NATIVE_RECEIPT_KEYS = {
    'capture/evidence.json': 'evidenceSha256',
    'capture/launch.json': 'launchSha256',
    'capture/prelaunch.json': 'prelaunchSha256',
    'capture/trace.log': 'traceSha256',
    'session-domain/observer.json': 'observerSha256',
    'session-domain/cases.json': 'casesSha256',
    'session-domain/provider.json': 'providerSha256',
    'session-domain/driver.json': 'driverSha256',
    'session-domain/sources.json': 'sourcesSha256',
}


def receipt_artifacts(scope):
    """Paths relative to the capture/session-domain directory, from one schema."""
    prefix = scope + '/'
    return {name.removeprefix(prefix): key for name, key in NATIVE_RECEIPT_KEYS.items()
            if name.startswith(prefix)}


def jsonl_lines(text):
    """Only LF delimits JSONL; preserve internal blanks for caller validation."""
    if not text:
        return []
    lines = text.split('\n')
    return lines[:-1] if lines[-1] == '' else lines

class Incomplete(ValueError):
    pass


def need(condition, message):
    if not condition:
        raise Incomplete('#1140 ' + message)


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


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
    processes, events, calls, attempts, transcripts = [], [], [], [], []
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
                need(tid in owners and tid not in ended, 'failed exec from untracked/exited task')
                continue
            need(tid not in ended, 'successful exec from exited task')
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
        elif name == 'openat':
            match = re.fullmatch(r'(AT_FDCWD|\d+), ("(?:\\x[0-9a-f]{2})*"), (.+)', args)
            need(match is not None, 'openat is not a parsable dirfd/path/result triple')
            path = text_string(match[2])
            marker = transcript_key(path)
            if marker is None or result.startswith('-1 ') or owners[tid] != root:
                continue
            need(path.startswith('/'), 'a session transcript path must be absolute to be attributed')
            need(re.fullmatch(r'\d+', result) is not None, 'transcript open result is not a file descriptor')
            transcripts.append({'pid': owners[tid], 'afterExecs': len(processes), 'path': path,
                                'flags': match[3], 'result': result, 'dirfd': match[1]})
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
    verify_binaries(contract)
    return {'topology': 'native', 'processes': processes,
            'execWitness': {'complete': True, 'source': 'exec-monitor', 'events': events,
                            'attempts': attempts, 'taskOwners': {str(k): v for k, v in owners.items()}},
            'networkWitness': {'complete': True, 'source': 'strace-connect', 'calls': calls},
            'transcriptWitness': {'complete': True, 'source': 'strace-openat', 'opens': transcripts},
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
    transcript_equal = evidence['transcriptWitness'] == replay['transcriptWitness']
    checked['execWitness']['complete'] = evidence['execWitness']['complete'] is True and exec_equal
    checked['networkWitness']['complete'] = evidence['networkWitness']['complete'] is True and network_equal
    checked['transcriptWitness']['complete'] = (evidence['transcriptWitness']['complete'] is True
                                                and transcript_equal)
    checked['cleanup']['allStopped'] = evidence['cleanup'] == replay['cleanup']
    return checked


def load_capture(folder):
    folder = Path(folder)
    need(not (folder / 'failure.json').exists(), 'capture carries a failure record')
    receipt = json.loads((folder / 'receipt.json').read_text())
    need(receipt['status'] == 'WITNESS_CAPTURED' and receipt['tracerExitCode'] == 0,
         'capture was not completed successfully')
    raw = read_trace(folder / 'trace.log')
    for name, key in receipt_artifacts('capture').items():
        need(sha(folder / name) == receipt[key], 'capture artifact changed: ' + name)
    contract = json.loads((folder / 'launch.json').read_text())
    prelaunch = json.loads((folder / 'prelaunch.json').read_text())
    need({k: v for k, v in contract.items() if k != 'launcherPid'} == prelaunch,
         'launch contract changed after pre-registration')
    return raw, contract, json.loads((folder / 'evidence.json').read_text())


class BinaryUnavailable(Incomplete):
    """Offline replay requires the original absolute executable paths."""


def verify_binaries(contract):
    for binary in contract['binaries'].values():
        try:
            actual = sha(binary['path'])
        except OSError as error:
            raise BinaryUnavailable('#1140 BINARY_UNAVAILABLE: restore original absolute path ' + binary['path']) from error
        need(actual == binary['sha256'], 'executable bytes changed since prelaunch')


def session_key(case):
    return case.replace('/', '.')


def transcript_key(path):
    match = re.fullmatch(r'(.+)/chats/([^/]+)\.jsonl', path) if isinstance(path, str) else None
    return match[2] if match else None


def canonical_path(path):
    need(isinstance(path, str) and path.startswith('/') and os.path.normpath(path) == path,
         'evidence path must be absolute and normalized')
    return path


def ws_profile(direction, wire):
    need(direction in ('send', 'receive'), 'WS profile direction')
    need(wire.get('opcode') == 1 and wire.get('fin') is True and wire.get('rsv') == 0,
         'WS profile requires complete text frame without RSV')
    need(wire.get('masked') is (direction == 'send'), 'WS profile mask/direction')


def session_profile(contract, cases):
    """Independent frozen profile; a submitted contract cannot widen it."""
    need(contract.get('cases') == cases, 'session profile full manifest')
    need(Counter(contract.get('processRoles', [])) == Counter(['host'] + ['sidecar'] * len(cases))
         and set(contract.get('binaries', {})) == {'host', 'sidecar'}, 'session profile roles')
    need(contract.get('connections') == 1, 'session profile connections')
    argv = contract['binaries']['host']['argv']
    need(argv.count('--listen') == argv.count('--home') == argv.count('--project') == 1,
         'session profile host arguments')
    listen = argv[argv.index('--listen') + 1]
    need(re.fullmatch(r'127\.0\.0\.1:2899[9]|127\.0\.0\.1:2900[0-8]', listen) is not None,
         'session profile gateway endpoint')
    endpoint = '127.0.0.1:' + str(int(listen.rsplit(':', 1)[1]) + 1)
    need(contract.get('externalEndpoints') == [endpoint], 'session profile provider endpoint')
    need(contract.get('capabilities') == {'provider': 'loopback-mock', 'tools': []}, 'session profile capabilities')
    home = canonical_path(argv[argv.index('--home') + 1])
    need(argv[argv.index('--project') + 1] == canonical_path(contract['project']), 'session profile project')
    return home, endpoint


def session_windows(evidence, cases, paths):
    processes = evidence['processes']
    need(Counter(p['role'] for p in processes) == Counter(['host'] + ['sidecar'] * len(cases)),
         'session capture must record exactly one sidecar per manifest session')
    witness = evidence.get('transcriptWitness')
    need(isinstance(witness, dict) and witness.get('complete') is True
         and witness.get('source') == 'strace-openat' and isinstance(witness.get('opens'), list),
         'session capture must carry the complete transcript-open witness')
    host = next(p['pid'] for p in processes if p['role'] == 'host')
    need(len(paths) == len(cases) and len(set(paths)) == len(paths), 'session transcript path coverage')
    first = {}
    for entry in witness['opens']:
        path = entry.get('path') if isinstance(entry, dict) else None
        key = transcript_key(path)
        need(key is not None and entry.get('pid') == host
             and type(entry.get('afterExecs')) is int and 0 <= entry['afterExecs'] <= len(processes),
             'transcript opens must be host-owned session transcripts')
        canonical_path(path)
        need(path in paths, 'transcript marker is not the actual case transcript')
        need(isinstance(entry.get('flags'), str) and bool(entry['flags'])
             and re.fullmatch(r'\d+', entry.get('result', '')) is not None,
             'transcript marker must be a successful open')
        first.setdefault(key, entry['afterExecs'])
    need(list(first) == [session_key(case) for case in cases],
         'session capture must open every manifest session transcript in manifest order')
    bounds = [0] + list(first.values()) + [len(processes)]
    windows = [sum(p['role'] == 'sidecar' for p in processes[low:high]) for low, high in zip(bounds, bounds[1:])]
    need(windows == [0] + [1] * len(cases),
         'session capture must attribute exactly one sidecar exec to every session')


def disk_observation(source, identity, home):
    """Project-owned raw files, never a glob by transcript basename."""
    project, session, run = (identity[k] for k in ('projectKey', 'sessionKey', 'runId'))
    path = canonical_path(source['transcriptPath'])
    marker = canonical_path(source['projectMarkerPath'])
    parent = Path(marker).parent
    need(parent.parent == Path(home) / 'projects' and Path(marker).name == '.cwd'
         and source['projectMarkerText'].strip() == project,
         'transcript project marker disagrees with request project')
    need(path == str(parent / 'chats' / (session + '.jsonl'))
         and identity['transcriptPath'] == path, 'actual transcript path disagrees with request session')
    try:
        entries = [json.loads(line) for line in jsonl_lines(source['transcriptText']) if line.strip()]
    except ValueError as error:
        raise Incomplete('#1140 transcript JSONL malformed') from error
    need(all(isinstance(entry, dict) for entry in entries), 'transcript entries malformed')
    need(source['registryPath'] == str(Path(home) / 'runs.json'), 'registry original path')
    try:
        runs = json.loads(source['registryText']) if source['registryText'] is not None else {}
    except ValueError as error:
        raise Incomplete('#1140 registry JSON malformed') from error
    need(isinstance(runs, dict), 'registry original malformed')
    key = f'{project}\0{session}\0{run}'
    records = [record for record in runs.get('records', [])
               if (record.get('projectKey'), record.get('sessionKey'), record.get('runId')) == (project, session, run)]
    need(len(records) <= 1, 'duplicate full run key')
    events = runs.get('events', {}).get(key, [])
    record = records[0] if records else {}
    return {'durableStatus': {'entries': [entry for entry in entries if entry.get('type') == 'agent_status_message']},
            'registry': {'applicable': bool(events or record), 'events': events, 'record': record}}


def required_provider_cases(document):
    """Pure #1284 A derivation from #1177's registered output frames.

    The projection drops the model_request_started prefix. turn_completed
    (including model_error) is the retained post-model output; assistant and
    tool frames also contain model output. Host status/error frames alone do
    not establish a model result. Never inspect case labels or Rust captures.
    """
    output_types = {'turn_completed', 'assistant_text_delta', 'assistant_thinking_delta',
                    'assistant_block', 'model_stream_event', 'tool_calls_detected',
                    'tool_call_started', 'tool_call_completed'}
    return {row['case'] for row in document['cases']
            if any(frame.partition(':')[0] in output_types for frame in row['rust']['frames'])}


def provider_cases(requests, cases):
    """Authenticate observed model request ownership, not product correctness."""
    covered = []
    for request in requests:
        body = json.loads(request['raw'])
        if body.get('stream') is not True:
            continue  # Title requests cannot supply a model case witness.
        messages = body.get('messages')
        need(body.get('model') == 'mock' and isinstance(messages, list), 'provider model request shape')
        texts = []
        for message in messages:
            if message.get('role') == 'user':
                content = message.get('content')
                if isinstance(content, str):
                    texts.append(content)
                elif isinstance(content, list):
                    texts.extend(part['text'] for part in content if isinstance(part, dict)
                                 and part.get('type') == 'text' and isinstance(part.get('text'), str))
        case = request.get('case')
        markers = set(re.findall(r'\[native-witness:([^\]]+)\]', '\n'.join(texts)))
        need(markers == {case}, 'provider request must have one case owner')
        need(case in cases and f'[native-witness:{case}] controlled wait' in texts
             and request.get('path') == '/v1/chat/completions', 'provider request case provenance')
        covered.append(case)
    return covered


def session_chain(contract, rows, observer, provider, driver, sources, cases):
    home, endpoint = session_profile(contract, cases)
    need([row.get('case') for row in rows] == cases, 'case identity full manifest order')
    need(provider.get('endpoint') == endpoint, 'provider endpoint disagrees with session profile')
    requests = provider.get('requests')
    need(isinstance(requests, list), 'provider requests malformed')
    provider_cases(requests, cases)
    need(len(sources) == len(rows) and len(driver.get('identities', [])) == len(rows), 'case originals coverage')
    submitted, received, finals = {}, {}, set()
    hello_seen = False
    for row in observer['rows']:
        ws_profile(row['direction'], row['wire'])
        frame = json.loads(row['frame']) if row['direction'] == 'send' else row['frame']
        if frame.get('type') in ('hello', 'hello_ok'):
            if frame.get('type') == 'hello_ok':
                hello_seen = True
            continue
        request_id = frame.get('id')
        if row['direction'] == 'send':
            need(hello_seen and frame.get('type') == 'request' and frame.get('method') == 'submit_turn'
                 and isinstance(request_id, str) and bool(request_id) and request_id not in submitted,
                 'observer submit identity/sequence')
            need(set(submitted) == finals, 'observer previous case has no final')
            submitted[request_id] = frame['params']
            received[request_id] = []
        else:
            need(request_id in submitted and request_id not in finals, 'observer response belongs to active request')
            need(frame.get('type') == 'event' and type(frame.get('seq')) is int
                 and frame['seq'] == len(received[request_id]) and type(frame.get('final')) is bool
                 and isinstance(frame.get('event'), dict), 'observer event envelope/sequence')
            if isinstance(frame.get('event'), dict):
                received[request_id].append(frame['event'])
            if frame.get('final') is True:
                finals.add(request_id)
    need(len(submitted) == len(cases) and set(submitted) == finals, 'observer submit/final coverage')
    need(list(submitted) == [row['identity']['requestId'] for row in rows], 'case request order')
    seen_runs = set()
    for case, row, anchor, source in zip(cases, rows, driver['identities'], sources):
        identity = row['identity']
        need(all(isinstance(identity.get(k), str) and bool(identity[k])
                 for k in ('requestId', 'projectKey', 'sessionKey', 'runId', 'transcriptPath')), 'case identity shape')
        need(identity == anchor, 'driver and case identity disagree')
        params = submitted[identity['requestId']]
        need(identity['projectKey'] == contract['project'] and identity['sessionKey'] == session_key(case)
             and all(identity[k] == params.get(k) for k in ('projectKey', 'sessionKey', 'runId'))
             and params.get('message') == f'[native-witness:{case}] controlled wait', 'case request identity chain')
        need(identity['runId'] not in seen_runs, 'case run identity reused')
        seen_runs.add(identity['runId'])
        original = disk_observation(source, identity, home)
        original['frames'] = received[identity['requestId']]
        observation = row.get('observation')
        # Absence is kept for the existing NOT_RUN scorer, never promoted.
        if observation is not None:
            registry = observation.get('registry', {})
            need(registry.get('applicable') is not False or not (registry.get('events') or registry.get('record')),
                 'inapplicable registry carries records')
            need(observation == original, 'case observation disagrees with retained originals')
    return [row['identity']['transcriptPath'] for row in rows]


def main():
    raise SystemExit('native_evidence is a library; use session_domain.py --suite deadline')


if __name__ == '__main__':
    run_entry(main, 'traceback')
