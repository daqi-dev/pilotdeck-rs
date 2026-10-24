#!/usr/bin/env python3
"""A5.3 Module-host matrix; production WS/HTTP/background remain explicit NOT_RUN.

Uses the existing parity lock and A5.1 method denominator. This is not a new
frozen ruler and cannot certify retirement: no complete exec/egress observer is
available here. Only the external model service is scripted; the Rust host,
model module/router/HTTP provider and both sidecars execute their real code.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / 'scripts/gateway-parity'))
from common import parity_lock
from compare import Normalizer

BASELINE = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
MODES = ('Rust-sidecar-ts', 'Rust-sidecar-native')
CASES = ('model-stream', 'provider-error', 'provider-unconfigured', 'token-saver-judge')
REPEATS = 5


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def baseline_identity(baseline):
    commit = subprocess.check_output(['git', '-C', str(baseline), 'rev-parse', 'HEAD'], text=True).strip()
    require(commit == BASELINE, 'BASELINE: wrong fixed commit')
    # Compare actual bytes to Git objects, including assume-unchanged files.
    files = subprocess.check_output(['git', '-C', str(baseline), 'ls-tree', '-rz', 'HEAD', '--', 'src', 'package.json'])
    # Match baseline_source_guard.mjs: equal bytes through a symlink are not
    # equal module identity (Node resolves imports relative to the real path).
    source_root = baseline / 'src'
    require(source_root.is_dir() and not source_root.is_symlink(), 'BASELINE: invalid source root')
    actual_names = {'package.json'}
    for directory, children, names in os.walk(source_root, followlinks=False):
        for name in children:
            require(not (Path(directory) / name).is_symlink(), 'BASELINE: source directory symlink')
        for name in names:
            path = Path(directory) / name
            require(path.is_file() and not path.is_symlink(), f'BASELINE: non-regular source {path}')
            actual_names.add(path.relative_to(baseline).as_posix())
    package = baseline / 'package.json'
    require(package.is_file() and not package.is_symlink(), 'BASELINE: non-regular package.json')
    hashes = {}
    for row in files.split(b'\0'):
        if not row:
            continue
        metadata, name = row.split(b'\t', 1)
        mode, _, oid = metadata.decode().split()
        path = baseline / os.fsdecode(name)
        data = os.fsencode(os.readlink(path)) if mode == '120000' else path.read_bytes()
        actual = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        require(actual == oid, f'BASELINE: modified source {path}')
        hashes[os.fsdecode(name)] = hashlib.sha256(data).hexdigest()
    require(len(hashes) > 0, 'BASELINE: empty source inventory')
    require(set(hashes) == actual_names, 'BASELINE: untracked, ignored or missing source files')
    return hashes


class Provider(HTTPServer):
    def get_request(self):
        connection, address = super().get_request()
        connection.settimeout(5)
        return connection, address

    def __init__(self):
        for port in range(28500, 29000):
            try:
                super().__init__(('127.0.0.1', port), Handler)
                break
            except OSError as error:
                if error.errno not in (48, 98):
                    raise
        else:
            raise RuntimeError('no free owned high port')
        self.rows = []
        self.case = ''
        self.nonce = ''


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('content-length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(400)
            return
        body = self.rfile.read(size)
        request = json.loads(body)
        status = 401 if self.server.case == 'provider-error' else 200
        if status == 401:
            response = json.dumps({'error': {'type': 'authentication_error', 'message': 'a53 fixture denied'}}).encode()
        elif request.get('stream') is False:
            response = json.dumps({'choices': [{'message': {'role': 'assistant', 'content': '<tier>economical</tier>'},
                                                'finish_reason': 'stop'}]}).encode()
        else:
            frames = [
                {'choices': [{'index': 0, 'delta': {'role': 'assistant', 'content': self.server.nonce}, 'finish_reason': None}]},
                {'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'stop'}],
                 'usage': {'prompt_tokens': 10, 'completion_tokens': 3, 'total_tokens': 13}},
            ]
            response = (''.join('data: ' + json.dumps(frame) + '\n\n' for frame in frames) + 'data: [DONE]\n\n').encode()
        self.server.rows.append({'method': 'POST', 'path': self.path, 'request': request,
                                 'requestBytes': body.decode(), 'status': status,
                                 'responseBytes': response.decode()})
        self.send_response(status)
        self.send_header('content-type', 'application/json' if status != 200 or request.get('stream') is False else 'text/event-stream')
        self.send_header('content-length', str(len(response)))
        self.end_headers()
        self.wfile.write(response)


def check_observations(case, rows, http, nonce):
    """Independent expectations, never merely compare the two implementations."""
    terminals = [row['value'] for row in rows if row['kind'] == 'terminal']
    calls = [row['value'] for row in rows if row['kind'] == 'module_call']
    require(len(terminals) == 1, 'TERMINAL: exactly one terminal required')
    require(calls and all(call.get('method') == 'module_call' and call.get('payload', {}).get('operation') for call in calls),
            'EMPTY_OBSERVER: valid actual Module callbacks required')
    terminal = terminals[0]
    events = [row['value'] for row in rows if row['kind'] == 'event']
    require(events and events[-1] == terminal and terminal.get('final') is True, 'EVENTS: final event must equal terminal')
    seq = [event.get('sequence') for event in events]
    # HostClient suppresses the non-authoritative turn_completed projection
    # (host/execution.rs, TS readFinalTerminal:680). Only its final predecessor
    # is absent; every other event and its sequence must be present.
    require(seq == list(range(len(events) - 1)) + [len(events)], 'EVENT_ORDER: missing, duplicate or unordered event')
    expected = ['agent.model_request_started']
    if case in ('model-stream', 'token-saver-judge'):
        expected += ['agent.model_event'] * 5 + ['agent.assistant_message', 'agent.stop_requested', 'agent.execute.completed']
    else:
        expected += ['agent.model_event'] * (2 if case == 'provider-error' else 1)
        expected += ['agent.assistant_message', 'agent.stop_failure', 'agent.agent_status', 'agent.turn_failed', 'agent.execute.failed']
    require([event.get('eventType') for event in events] == expected, 'EVENT_TYPES: missing or extra events')
    require(all(event.get('final') is False for event in events[:-1]), 'TERMINAL: premature final')
    require(all(event.get('runId') == 'a53-run' and event.get('operationId') == 'a53-operation' for event in events), 'IDENTITY: wrong execution')
    operations = [call['payload']['operation'] for call in calls]
    require('prepare' in operations, 'MODULES: no model preparation')
    if case in ('model-stream', 'token-saver-judge'):
        judge = case == 'token-saver-judge'
        require(len(http) == (2 if judge else 1), 'HTTP_COUNT: wrong number of actual provider invocations')
        if judge:
            require(http[0]['request'].get('stream') is False and http[0]['request'].get('model') == 'judge',
                    'JUDGE: native complete request absent')
        require(http[-1]['request'].get('stream') is True, 'HTTP_STREAM: native streaming request absent')
        require(http[-1]['request'].get('model') == ('economical' if judge else 'controlled'), 'HTTP_MODEL: wrong model or judge fallback')
        require(terminal.get('outcome') == 'completed', 'OUTCOME: model completion required')
        result = terminal.get('payload', {}).get('result', {})
        content = result.get('finalMessage', {}).get('content')
        require(isinstance(content, list) and len(content) == 1 and content[0].get('type') == 'text'
                and content[0].get('text') == nonce and result.get('finalMessage', {}).get('role') == 'assistant',
                'CONTENT: assistant output must match provider challenge')
        # The baseline remote iterator closes only on early return, not after
        # normal exhaustion. Require the real pull path in both cases.
        require('stream_next' in operations, 'MODULES: missing stream lifecycle')
    elif case == 'provider-error':
        require(len(http) == 1 and http[0]['status'] == 401, 'HTTP_ERROR: auth failure must reach provider once')
        require(terminal.get('outcome') == 'failed', 'ERROR_AS_SUCCESS: provider failure cannot complete')
        error = terminal.get('error', {}).get('details', {})
        # OpenAI stream.ts:42 retains error.type. complete() instead uses
        # normalizeModelError (auth_error), covered by native_judge.rs.
        require(error.get('code') == 'authentication_error' and error.get('message') == 'a53 fixture denied', 'PROVIDER_ERROR: code/message lost')
    else:
        require(not http, 'UNCONFIGURED: missing provider must not touch HTTP')
        require(terminal.get('outcome') == 'failed', 'UNCONFIGURED_AS_SUCCESS')
        error = terminal.get('error', {})
        require(error.get('message') == 'Provider absent does not exist.', 'UNCONFIGURED_MESSAGE: missing provider message lost')
        require(error.get('details', {}).get('code') == 'provider_not_found', 'UNCONFIGURED_CODE: missing provider code lost')
    return terminal


def process_census():
    rows = subprocess.check_output(['ps', '-axo', 'pid=,ppid=,pgid=,lstart=,command='], text=True, timeout=5)
    census = {}
    for line in rows.splitlines():
        fields = line.split(None, 8)
        pid, parent, group = fields[:3]
        census[int(pid)] = {'pid': int(pid), 'ppid': int(parent), 'pgid': int(group),
                            'start': ' '.join(fields[3:8]), 'command': fields[8] if len(fields) > 8 else ''}
    return census


def collect_descendants(pid, owned):
    census = process_census()
    selected = {pid}
    while True:
        children = {child for child, row in census.items() if row['ppid'] in selected}
        if children <= selected:
            break
        selected |= children
    for child in selected:
        if child in census:
            owned[child] = census[child]


def cleanup_owned(child, owned):
    try:
        census = process_census()
        for pid, row in reversed(list(owned.items())):
            current = census.get(pid)
            # PID reuse is not authorization to signal a different process.
            if current and current['start'] == row['start'] and pid != child.pid:
                try:
                    if row['pgid'] == pid:
                        os.killpg(pid, signal.SIGKILL)
                    else:
                        os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
    finally:
        if child.poll() is None:
            # The probe handles SIGINT by cancelling through HostClient, which
            # closes the sidecar's separate process group even if ps fails.
            try:
                os.killpg(child.pid, signal.SIGINT)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=8)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        child.wait(timeout=10)
    deadline = time.monotonic() + 5
    while True:
        census = process_census()
        remaining = [pid for pid, row in owned.items() if pid in census and census[pid]['start'] == row['start']]
        if not remaining:
            return
        require(time.monotonic() < deadline, f'CLEANUP: owned processes still alive: {remaining}')
        time.sleep(.05)


def semantic_observations(rows):
    events = [row['value'] for row in rows if row['kind'] == 'event']
    normalizer = Normalizer()
    normalizer.index(events)  # Existing gateway entity/clock rules, unmodified.
    normalized = normalizer.value(events)
    for event in normalized:
        for key in ('messageId', 'streamId'):
            event.pop(key, None)  # Opaque Module transport IDs, retained raw.
    return normalized


def check_matrix(report, comparisons):
    """Fixed registered denominator; empty/missing/duplicate rows cannot pass."""
    required = {(mode, case, round_id) for mode in MODES for case in CASES for round_id in range(5)}
    observed = []
    for mode in MODES:
        cells = [cell for cell in report['cells'] if cell['id'] == mode]
        require(len(cells) == 1 and cells[0]['status'] == 'PASS', 'MATRIX_CELL: missing or failed cell')
        for row in cells[0]['rounds']:
            require(row['status'] == 'PASS', 'MATRIX_CASE: failed case')
            observed.append((mode, row['case'], row['round']))
    require(len(observed) == 40 and set(observed) == required, 'MATRIX_COVERAGE: missing or duplicated round')
    require(set(comparisons) == {(round_id, case) for round_id in range(5) for case in CASES},
            'MATRIX_COMPARE: missing dual-configuration comparison')


def check_launch(rows, config):
    launches = [row for row in rows if row['kind'] == 'launch']
    require(len(launches) == 1, 'BINDING: exactly one launch required')
    launch = launches[0]
    binding = launch.get('binding', {})
    require(isinstance(binding, dict) and binding.get('moduleId') == 'pilotdeck-agent-loop'
            and binding.get('moduleInstanceId') and binding.get('connectionGeneration'), 'BINDING: invalid negotiated binding')
    if config['topology'] == 'Rust-sidecar-ts':
        expected_program = config['node']
        expected_argv = ['--import', str(Path(config['baseline']) / 'node_modules/tsx/dist/loader.mjs'),
                         str(Path(config['baseline']) / 'src/cli/pilotdeck-agent-loop-sidecar.ts')]
    else:
        expected_program, expected_argv = config['sidecar'], ['--stdio']
    require(launch.get('program') == expected_program and launch.get('argv') == expected_argv,
            'LAUNCH: configured sidecar differs from executed command')


def run_probe(args, config, directory):
    save(directory / 'config.json', config)
    command = [str(args.probe), str(directory / 'config.json')]
    process_census()  # Refuse before spawning when the observer is unavailable.
    with (directory / 'stdout.jsonl').open('xb') as stdout, (directory / 'stderr.log').open('xb') as stderr:
        child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True,
                                 cwd=config['isolation'], env={'PATH': '/usr/bin:/bin', 'HOME': config['isolation'],
                                 'PILOT_HOME': config['isolation'], 'TMPDIR': config['isolation']})
        owned = {}
        code = None
        try:
            deadline = time.monotonic() + 60
            while child.poll() is None:
                collect_descendants(child.pid, owned)
                require(time.monotonic() < deadline, 'PROBE_TIMEOUT: 60 seconds')
                time.sleep(.05)
            code = child.returncode
        finally:
            cleanup_error = None
            try:
                cleanup_owned(child, owned)
            except Exception as error:
                cleanup_error = str(error)
                raise
            finally:
                save(directory / 'launch.json', {'argv': command, 'pid': child.pid, 'exitCode': code,
                    'probeSha256': digest(args.probe), 'hostReaped': child.poll() is not None,
                    'processSnapshots': list(owned.values()), 'cleanupScope': 'observed descendants only',
                    'cleanupStatus': 'FAIL' if cleanup_error else 'PASS', 'cleanupError': cleanup_error,
                    'execWitness': 'NOT_RUN', 'egressWitness': 'NOT_RUN'})
    require(code == 0, f'PROBE_EXIT: {code}; see stderr.log')
    return [json.loads(line) for line in (directory / 'stdout.jsonl').read_text().splitlines()]


def run(args):
    args.output.mkdir(parents=True, exist_ok=False)
    ledger_path = ROOT / 'specs/gateway-coverage/ledger.json'
    ledger = json.loads(ledger_path.read_text())
    methods = [row['method'] for row in ledger['methods']]
    require(len(methods) == len(set(methods)) == 101, 'A5.1 method denominator changed')
    report = {'status': 'NOT_RUN', 'scope': 'A5.3 Module-host probe; no production entry claim',
              'repeats': REPEATS, 'ledgerSha256': digest(ledger_path), 'cells': [],
              'entryCoverage': [{ 'method': method, 'branches': {
                  branch: {'status': 'NOT_RUN', 'reason': 'production native request adapter not connected'}
                  for branch in ('connection-state', 'method-not-found', 'provider-error', 'provider-unconfigured')
              }} for method in methods],
              'entries': {entry: {'status': 'NOT_RUN', 'reason': 'native production adapter not connected'}
                          for entry in ('ws', 'http', 'background')},
              'retirement': {'status': 'NOT_RUN', 'reason': 'complete exec/egress and storage observers absent'}}
    for sidecar in ('ts', 'native'):
        report['cells'].append({'id': f'TS-sidecar-{sidecar}', 'host': 'ts', 'sidecar': sidecar,
                                'status': 'NOT_RUN', 'reason': 'TS host not executed', 'executedCases': []})
    save(args.output / 'report.json', report)
    for mode in MODES:
        report['cells'].append({'id': mode, 'host': 'rust', 'sidecar': 'ts' if mode.endswith('-ts') else 'native',
                               'status': 'NOT_RUN', 'expectedCases': list(CASES), 'executedCases': [], 'rounds': []})
    provider, thread = None, None
    comparisons = {}
    compared = set()
    try:
        sources = baseline_identity(args.baseline)
        report['sourceCommit'], report['sourceSha256'] = BASELINE, sources
        report['artifacts'] = {str(path): digest(path) for path in (args.probe, args.sidecar, args.node,
            args.baseline / 'src/cli/pilotdeck-agent-loop-sidecar.ts', Path(__file__),
            ROOT / 'scripts/gateway-parity/compare.py', ROOT / 'scripts/gateway-parity/common.py')}
        provider = Provider()
        thread = threading.Thread(target=provider.serve_forever, daemon=True)
        thread.start()
        for mode in MODES:
            cell = next(cell for cell in report['cells'] if cell['id'] == mode)
            cell['status'] = 'RUNNING'
            for round_id in range(REPEATS):
                for case in CASES:
                    directory = args.output / mode / str(round_id) / case
                    directory.mkdir(parents=True)
                    # Same host/provider inputs, and same actual isolation path
                    # for both configurations. Archive before the next launch.
                    isolation = args.output / 'cur'
                    isolation.mkdir()
                    provider.case, provider.nonce = case, f'a53-round-{round_id}-独立证据'
                    provider.rows = []
                    capabilities = {'supportsStreaming': True, 'supportsToolUse': True,
                                    'supportsPromptCache': False, 'supportsSystemPrompt': True,
                                    'supportsParallelToolCalls': True, 'supportsThinking': True,
                                    'maxContextTokens': 128000, 'maxOutputTokens': 32768}
                    router_config = {'enabled': False}
                    if case == 'token-saver-judge':
                        route = lambda model: {'provider': 'fixture', 'model': model}
                        router_config = {'enabled': True, 'stats': {'enabled': False},
                                         'scenarios': {'default': route('controlled')},
                                         'zeroUsageRetry': {'enabled': False},
                                         'tokenSaver': {'enabled': True, 'judge': route('judge'), 'defaultTier': 'default',
                                             'cacheAwareSwitching': {'enabled': False},
                                             'tiers': {'default': {'model': route('controlled')},
                                                       'economical': {'model': route('economical')}}}}
                    config = {'topology': mode, 'node': str(args.node), 'baseline': str(args.baseline),
                              'sidecar': str(args.sidecar), 'isolation': str(isolation),
                              'provider': {'id': 'fixture', 'protocol': 'openai',
                                  'url': f'http://127.0.0.1:{provider.server_port}/v1', 'apiKey': 'fixture-only',
                                  'retry': {'streamMaxRetries': 0, 'requestMaxRetries': 0},
                                  'models': {model: {'id': model, 'capabilities': capabilities,
                                                    'multimodal': {'input': ['text']}}
                                             for model in ('controlled', 'economical', 'judge')}},
                              'snapshot': {'config': {'router': router_config}},
                              'input': {'provider': 'absent' if case == 'provider-unconfigured' else 'fixture',
                                        'model': 'controlled', 'task': {'prompt': 'A5.3 native topology fixture'}, 'tools': []}}
                    try:
                        rows = run_probe(args, config, directory)
                        save(directory / 'provider-http.json', provider.rows)
                        check_launch(rows, config)
                        check_observations(case, rows, provider.rows, provider.nonce)
                        key = (round_id, case)
                        normalized = {'events': semantic_observations(rows), 'http': provider.rows,
                                      'operations': [row['value']['payload']['operation'] for row in rows if row['kind'] == 'module_call']}
                        if mode == MODES[0]:
                            comparisons[key] = normalized
                        else:
                            save(directory / 'comparison.json', {'expected': comparisons[key], 'actual': normalized})
                            require(normalized == comparisons[key], 'DUAL_CONFIG: independent observations differ')
                            compared.add(key)
                        cell['rounds'].append({'round': round_id, 'case': case, 'status': 'PASS'})
                    except Exception as error:
                        save(directory / 'provider-http.json', provider.rows)
                        cell['status'], report['status'] = 'FAIL', 'FAIL'
                        cell['rounds'].append({'round': round_id, 'case': case, 'status': 'FAIL', 'reason': str(error)})
                    finally:
                        isolation.rename(directory / 'isolation')
                        save(args.output / 'report.json', report)
            cell['executedCases'] = list(CASES)
            if cell['status'] != 'FAIL':
                cell['status'] = 'PASS'
        require(baseline_identity(args.baseline) == sources, 'BASELINE: source changed during capture')
        require(all(digest(Path(path)) == value for path, value in report['artifacts'].items()),
                'ARTIFACT: executable or observer changed during capture')
        check_matrix(report, compared)
        report['moduleMatrixStatus'] = 'PASS'
    except BaseException as error:
        report['status'], report['moduleMatrixStatus'] = 'FAIL', 'FAIL'
        report['failure'] = str(error) or type(error).__name__
        raise
    finally:
        try:
            if provider is not None:
                try:
                    if thread is not None and thread.ident is not None:
                        if thread.is_alive():
                            provider.shutdown()
                        thread.join(timeout=5)
                finally:
                    provider.server_close()
        except BaseException as error:
            report['status'], report['moduleMatrixStatus'] = 'FAIL', 'FAIL'
            report['cleanupFailure'] = str(error) or type(error).__name__
            raise
        finally:
            save(args.output / 'report.json', report)
    return 1 if report['status'] == 'FAIL' else 2  # NOT_RUN never means success.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('baseline', 'node', 'sidecar', 'probe', 'output'):
        parser.add_argument('--' + name, type=lambda value: Path(value).resolve(), required=True)
    args = parser.parse_args()
    with parity_lock():
        return run(args)


if __name__ == '__main__':
    sys.exit(main())
