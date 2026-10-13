#!/usr/bin/env python3
"""A1 provisional frame comparator; not the frozen A2 acceptance ruler.

Retain frame order, directions, event seq, every semantic payload. Map UUIDs
consistently and normalize explicit timing/binding noise only. Raw logs survive.
"""
import argparse
import json
from pathlib import Path
import re

UUID = re.compile(r'\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b', re.I)
TIME_KEYS = {'createdAt', 'updatedAt', 'startedAt', 'completedAt', 'timestamp', 'durationMs', 'elapsedMs'}


def normalize(path):
    ids = {}
    generations = {}

    def value(item, key=''):
        if key in TIME_KEYS and isinstance(item, (str, int, float)):
            return '<time>'
        if isinstance(item, dict):
            return {key: (generations.setdefault(val, f'<generation-{len(generations) + 1}>')
                          if key == 'generation' and 'connectionId' in item else value(val, key))
                    for key, val in item.items()}
        if isinstance(item, list):
            return [value(val) for val in item]
        if isinstance(item, str):
            return UUID.sub(lambda match: ids.setdefault(match.group(), f'<uuid-{len(ids) + 1}>'), item)
        return item

    result = []
    errors = []
    previous = 0
    try:
        lines = path.read_text().splitlines()
    except OSError as error:
        return [], [{'error': str(error)}]
    for number, line in enumerate(lines, 1):
        try:
            row = json.loads(line)
            if not isinstance(row, dict):
                raise ValueError('recorder row must be an object')
            if not isinstance(row.get('seq'), int) or row['seq'] <= previous:
                raise ValueError('recorder sequence must be strictly monotonic')
            previous = row['seq']
            if 'raw' in row:
                try:
                    parsed = json.loads(row['raw'])
                except (ValueError, TypeError) as error:
                    raise ValueError(f'recorded raw frame is not JSON: {error}') from error
                if parsed != row['frame']:
                    raise ValueError('recorded raw frame and parsed frame disagree')
            # Raw and all recorder metadata remain in the original evidence.
            result.append({'connection': row['connection'], 'direction': row['direction'],
                           'frame': value(row.get('frame', row.get('close')))})
        except (ValueError, KeyError, TypeError) as error:
            errors.append({'line': number, 'error': str(error)})
    if not result:
        errors.append({'error': 'empty/invalid recordings are never a PASS'})
    return result, errors


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('left', type=Path)
    parser.add_argument('right', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    left, left_errors = normalize(args.left)
    right, right_errors = normalize(args.right)
    differences = [{'side': side, **error} for side, errors in [('left', left_errors), ('right', right_errors)]
                   for error in errors]
    differences += [{'index': index, 'left': a, 'right': b}
                   for index, (a, b) in enumerate(zip(left, right)) if a != b]
    if len(left) != len(right):
        differences.append({'lengths': [len(left), len(right)]})
    result = {'status': 'FAIL' if differences else 'PASS', 'frames': [len(left), len(right)], 'differences': differences}
    args.out.write_text(json.dumps(result, indent=2, ensure_ascii=False) + '\n')
    print(json.dumps(result, ensure_ascii=False))
    return 1 if differences else 0


if __name__ == '__main__':
    raise SystemExit(main())
