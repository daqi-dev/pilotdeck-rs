#!/usr/bin/env python3
"""Mutate real successful evidence; require FAIL from the corresponding rule."""
import argparse
import base64
import json
from pathlib import Path
import shutil
import sys
import tempfile

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

DEFAULT_OUT = common.DEFAULT_OUT
save = common.save
compare_module = _load('compare', common.SCRIPT / 'compare.py')
compare = compare_module.compare
prerequisites_module = _load('prerequisites', common.SCRIPT / 'controls/prerequisites.py')
ROOT = prerequisites_module.ROOT
run_prerequisites = prerequisites_module.run_prerequisites


def rows(folder, name='auth'):
    return [json.loads(line) for line in (folder / f'scenarios/{name}/frames.jsonl').read_text().splitlines()]


def write_rows(folder, value, name='auth', footer=False):
    path = folder / f'scenarios/{name}/frames.jsonl'
    path.write_text(''.join(json.dumps(row, ensure_ascii=False) + '\n' for row in value))
    if footer:
        for seq, row in enumerate(value, 1):
            row['seq'] = seq
        path.write_text(''.join(json.dumps(row, ensure_ascii=False) + '\n' for row in value))
        complete = json.loads((path.parent / 'complete.json').read_text())
        complete['records'] = len(value)
        save(path.parent / 'complete.json', complete)


def run_controls(source, output, proxy_source=None):
    if compare(source, source)['status'] != 'PASS':
        return {'passed': False, 'error': 'controls require a valid real source round'}
    output.mkdir(parents=True, exist_ok=False)
    results = {}

    def case(name, rule, mutate, both=False, prepare=None, reference_source=None):
        target = output / name
        reference = reference_source or source
        shutil.copytree(reference, target)
        if prepare:
            reference = output / (name + '-reference')
            shutil.copytree(reference_source or source, reference)
            prepare(reference)
            prepare(target)
            if compare(reference, reference)['status'] != 'PASS':
                raise ValueError(f'{name}: invalid prepared reference')
        mutate(target)
        result = compare(target if both else reference, target)
        observed = [row['rule'] for row in result['failures']]
        passed = result['status'] == 'FAIL' and bool(observed) and observed[0] == rule
        results[name] = {'passed': passed, 'expectedRule': rule, 'observedRules': observed, 'primaryRule': observed[0] if observed else None, 'bothSides': both, 'comparison': str(target / 'comparison.json')}
        save(target / 'comparison.json', result)

    def frame_difference(folder):
        data = rows(folder)
        row = next(row for row in data if isinstance(row.get('frame'), dict) and row['frame'].get('type') == 'hello_ok')
        row['frame']['serverVersion'] = 'INJECTED_FRAME_DIFFERENCE'
        row['raw'] = json.dumps(row['frame'])
        write_rows(folder, data)

    def delete_event(folder):
        data = rows(folder, 'turn')
        index = next(i for i, row in enumerate(data) if isinstance(row.get('frame'), dict) and row['frame'].get('type') == 'event' and not row['frame'].get('final'))
        del data[index]
        # Even with a forged healthy recorder footer, protocol seq detects loss.
        write_rows(folder, data, 'turn', footer=True)

    def disk_difference(folder):
        artifact = next((folder / 'project').rglob('fixture.txt'))
        artifact.write_text('INJECTED_DURABLE_DIFFERENCE\n')

    def missing_scenario(folder):
        shutil.rmtree(folder / 'scenarios/readonly')

    def truncate(folder):
        path = folder / 'scenarios/turn/frames.jsonl'
        path.write_bytes(path.read_bytes()[:-25])

    def same_bad(folder):
        data = rows(folder, 'turn')
        for row in data:
            if isinstance(row.get('frame'), dict):
                row['frame'] = json.loads(json.dumps(row['frame']).replace('GP_MOCK_OK', 'WRONG_MODEL_OUTPUT'))
                if 'raw' in row:
                    row['raw'] = json.dumps(row['frame'])
        write_rows(folder, data, 'turn')

    def http_difference(folder):
        path = folder / 'scenarios/http/http.json'
        data = json.loads(path.read_text())
        data[0]['headers']['x-injected'] = 'difference'
        save(path, data)

    def wrong_path(folder):
        path = folder / 'run.json'
        data = json.loads(path.read_text())
        data['runtime']['pilotHome'] += '-different'
        save(path, data)

    def reorder_notification(folder):
        data = rows(folder, 'permission')
        notifications = [i for i, row in enumerate(data) if isinstance(row.get('frame'), dict) and row['frame'].get('type') == 'notification']
        a, b = notifications
        data[a], data[b] = data[b], data[a]
        write_rows(folder, data, 'permission', footer=True)

    def business_duration(folder):
        data = rows(folder)
        row = next(row for row in data if isinstance(row.get('frame'), dict) and row['frame'].get('type') == 'hello_ok')
        row['frame']['serverInfo']['durationMs'] = 123
        row['raw'] = json.dumps(row['frame'])
        write_rows(folder, data)

    def broken_reference(folder):
        data = rows(folder, 'readonly')
        row = next(row for row in data if isinstance(row.get('frame'), dict) and row['frame'].get('type') == 'response')
        row['frame']['id'] = 'unknown-request-id'
        row['raw'] = json.dumps(row['frame'])
        write_rows(folder, data, 'readonly')

    def corrupt_fingerprint(folder):
        path = next((folder / 'home').rglob('*gateway-parity-permission.jsonl'))
        data = [json.loads(line) for line in path.read_text().splitlines()]
        event = next(row['event'] for row in data if row.get('type') == 'model_stream_event' and 'requestFingerprint' in row['event'])
        event['requestFingerprint'] = '0' * 64
        path.write_text(''.join(json.dumps(row) + '\n' for row in data))

    def both_bad_disk(folder):
        (folder / 'project/written.txt').write_text('WRONG_HTTP_DURABLE_WRITE\n')

    def both_bad_http(folder):
        path = folder / 'scenarios/http/http.json'
        data = json.loads(path.read_text())
        data[0]['body'] = '{"ok":false}'
        data[0]['chunks'] = [{'base64': base64.b64encode(data[0]['body'].encode()).decode(), 'elapsedMs': 0}]
        save(path, data)

    def wire_change(folder, change, close=False):
        data = rows(folder, 'quirks')
        row = next(row for row in data if row.get('direction') == 'receive' and
                   row.get('wire', {}).get('opcode') == (8 if close else 1))
        wire = bytes.fromhex(row['wireHex'])
        code = wire[1] & 127
        offset = 2 + (2 if code == 126 else 8 if code == 127 else 0)
        if change == 'mask-key':
            old_mask = wire[offset:offset + 4]
            encoded = wire[offset + 4:]
            assert wire[1] & 128
            decoded = bytes(byte ^ old_mask[i % 4] for i, byte in enumerate(encoded))
            mask = b'abcd'
            row['wireHex'] = (wire[:offset] + mask + bytes(byte ^ mask[i % 4] for i, byte in enumerate(decoded))).hex()
            write_rows(folder, data, 'quirks')
            return
        assert not wire[1] & 128
        payload = wire[offset:]
        if change in (126, 127):
            row['wireHex'] = (bytes([wire[0], change]) + len(payload).to_bytes(2 if change == 126 else 8, 'big') + payload).hex()
            row['wire']['lengthCode'] = change
        elif change in ('fin', 'rsv'):
            row['wireHex'] = (bytes([wire[0] ^ (128 if change == 'fin' else 64)]) + wire[1:]).hex()
            row['wire']['fin' if change == 'fin' else 'rsv'] = False if change == 'fin' else 64
        elif change == 'mask':
            mask = b'1234'
            row['wireHex'] = (bytes([wire[0], wire[1] | 128]) + wire[2:offset] + mask +
                              bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))).hex()
            row['wire']['masked'] = True
        elif change == 'metadata':
            row['wire']['fin'] = not row['wire']['fin']
        write_rows(folder, data, 'quirks')

    def hello_fixture(folder, value):
        data = rows(folder)
        row = next(row for row in data if isinstance(row.get('frame'), dict) and row['frame'].get('type') == 'hello_ok')
        row['frame']['serverInfo']['r2Fixture'] = value
        row['raw'] = json.dumps(row['frame'])
        write_rows(folder, data)

    x = '00000000-0000-4000-8000-000000000001'
    y = '00000000-0000-4000-8000-000000000002'
    case('uuid-list-reorder', 'G4.frames', lambda folder: hello_fixture(folder, [y, x]),
         prepare=lambda folder: hello_fixture(folder, [x, y]))
    case('uuid-entity-list-reorder', 'G4.frames',
         lambda folder: hello_fixture(folder, [{'id': y, 'name': 'second'}, {'id': x, 'name': 'first'}]),
         prepare=lambda folder: hello_fixture(folder, [{'id': x, 'name': 'first'}, {'id': y, 'name': 'second'}]))
    for label, shape, field in [
        ('name-createdAt', {'name': 'business', 'createdAt': 1}, 'createdAt'),
        ('partial-project-createdAt', {'name': 'business', 'lastActivity': 1, 'createdAt': 1}, 'createdAt'),
        ('requestId-createdAt', {'requestId': 'business', 'createdAt': 1}, 'createdAt'),
        ('name-lastActivity', {'name': 'business', 'lastActivity': 1}, 'lastActivity'),
        ('projectKey-lastActivity', {'projectKey': 'business', 'lastActivity': 1}, 'lastActivity'),
        ('partial-registry-updatedAt', {'runId': 'business', 'ownerPid': 1, 'updatedAt': 1}, 'updatedAt'),
    ]:
        case('business-clock-' + label, 'G4.frames',
             lambda folder, value=shape, key=field: hello_fixture(folder, dict(value, **{key: 2})),
             prepare=lambda folder, value=shape: hello_fixture(folder, value))
    for marker in ('sessionId', 'runId', 'toolCallId'):
        for field in ('startedAt', 'completedAt'):
            shape = {marker: 'business', field: 1}
            case(f'business-clock-{marker}-{field}', 'G4.frames',
                 lambda folder, shape=shape, field=field: hello_fixture(folder, dict(shape, **{field: 2})),
                 prepare=lambda folder, shape=shape: hello_fixture(folder, shape))
    for label, change, close in [('126-to-127', 127, False), ('short-to-126', 126, True),
                                 ('short-to-127', 127, True), ('fin', 'fin', False),
                                 ('rsv', 'rsv', False), ('mask', 'mask', False)]:
        case('reencoded-frame-' + label, 'G4.frames',
             lambda folder, change=change, close=close: wire_change(folder, change, close))
    case('reencoded-frame-mask-key', 'G4.frames', lambda folder: wire_change(folder, 'mask-key'),
         prepare=lambda folder: wire_change(folder, 'mask'))
    case('both-bad-wire-metadata', 'G2.recording', lambda folder: wire_change(folder, 'metadata'), both=True)
    case('both-bad-disk', 'G3.oracle', both_bad_disk, both=True)
    case('both-bad-http', 'G3.oracle', both_bad_http, both=True)

    case('frame-difference', 'G4.frames', frame_difference)
    case('deleted-event', 'G2.event-sequence', delete_event)
    case('changed-disk', 'G6.disk', disk_difference)
    case('missing-scenario', 'G1.coverage', missing_scenario)
    case('truncated-recording', 'G2.recording', truncate)
    case('http-difference', 'G5.http', http_difference)
    case('different-runtime-path', 'G0.paths', wrong_path)
    case('notification-reorder', 'G4.frames', reorder_notification)
    case('business-duration', 'G4.frames', business_duration)
    case('broken-id-reference', 'G2.link', broken_reference)
    case('corrupt-fingerprint', 'G2.recording', corrupt_fingerprint, both=True)
    case('both-sides-bad', 'G3.oracle', same_bad, both=True)
    if proxy_source is not None:
        if compare(proxy_source, proxy_source)['status'] != 'PASS':
            raise ValueError('observer controls require a valid real proxy source')
        case('missing-upstream', 'G2.observer', lambda folder: (folder / 'upstream.jsonl').unlink(),
             reference_source=proxy_source)
        case('empty-upstream', 'G2.observer', lambda folder: (folder / 'upstream.jsonl').write_text(''),
             reference_source=proxy_source)
        case('truncated-upstream', 'G2.observer', lambda folder: (folder / 'upstream.jsonl').write_text('{broken'),
             reference_source=proxy_source)
    prerequisites = run_prerequisites(output / 'prerequisites')
    summary = {'passed': all(item['passed'] for item in results.values()) and prerequisites['passed'],
               'cases': results, 'prerequisites': prerequisites, 'source': str(source)}
    save(output / 'results.json', summary)
    return summary


def main():
    common.verify_loaded_apparatus()
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--source', type=Path, default=DEFAULT_OUT / 'ts1')
    ap.add_argument('--output', type=Path, default=DEFAULT_OUT / 'controls')
    ap.add_argument('--proxy-source', type=Path, help='real successful proxy round for upstream observer controls')
    ap.add_argument('--offline', action='store_true', help='only A4 prerequisites; no source round or services needed')
    ap.add_argument('--code-root', type=Path, default=ROOT, help='saved pre-change tree for offline counterproof')
    args = ap.parse_args()
    result = (run_prerequisites(args.output, args.code_root) if args.offline else
              run_controls(args.source, args.output, args.proxy_source))
    print(json.dumps(result, ensure_ascii=False))
    return 0 if result['passed'] else 1


if __name__ == '__main__':
    try:
        _verify_cli_imports()
    except (Exception, KeyboardInterrupt) as error:
        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))
        raise SystemExit(2)
    raise SystemExit(main())
