#!/usr/bin/env python3
"""Mutate real successful evidence; require FAIL from the corresponding rule."""
import argparse
import json
from pathlib import Path
import shutil
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import DEFAULT_OUT, save
from compare import compare


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


def run_controls(source, output):
    if compare(source, source)['status'] != 'PASS':
        return {'passed': False, 'error': 'controls require a valid real source round'}
    output.mkdir(parents=True, exist_ok=False)
    results = {}

    def case(name, rule, mutate, both=False):
        target = output / name
        shutil.copytree(source, target)
        mutate(target)
        result = compare(target if both else source, target)
        observed = [row['rule'] for row in result['failures']]
        passed = result['status'] == 'FAIL' and rule in observed
        results[name] = {'passed': passed, 'expectedRule': rule, 'observedRules': observed, 'comparison': str(target / 'comparison.json')}
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
    summary = {'passed': all(item['passed'] for item in results.values()), 'cases': results, 'source': str(source)}
    save(output / 'results.json', summary)
    return summary


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--source', type=Path, default=DEFAULT_OUT / 'ts1')
    ap.add_argument('--output', type=Path, default=DEFAULT_OUT / 'controls')
    args = ap.parse_args()
    result = run_controls(args.source, args.output)
    print(json.dumps(result, ensure_ascii=False))
    raise SystemExit(0 if result['passed'] else 1)
