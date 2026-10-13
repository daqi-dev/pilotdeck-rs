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
    previous = 0
    for line in path.read_text().splitlines():
        row = json.loads(line)
        assert row['seq'] > previous, 'recorder sequence must be strictly monotonic'
        previous = row['seq']
        if 'raw' in row:
            assert json.loads(row['raw']) == row['frame'], 'recorded raw frame and parsed frame disagree'
        # URL, monotonic clock and redundant raw JSON bytes stay in the evidence;
        # ordered semantic frames are compared after parsing JSON only.
        result.append({'connection': row['connection'], 'direction': row['direction'],
                       'frame': value(row.get('frame', row.get('close')))})
    assert result, 'empty recordings are never a PASS'
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('left', type=Path)
    parser.add_argument('right', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    left, right = normalize(args.left), normalize(args.right)
    differences = [{'index': index, 'left': a, 'right': b}
                   for index, (a, b) in enumerate(zip(left, right)) if a != b]
    if len(left) != len(right):
        differences.append({'lengths': [len(left), len(right)]})
    result = {'status': 'FAIL' if differences else 'PASS', 'frames': [len(left), len(right)], 'differences': differences}
    args.out.write_text(json.dumps(result, indent=2, ensure_ascii=False) + '\n')
    print(json.dumps(result, ensure_ascii=False))
    return 1 if differences else 0


if __name__ == '__main__':
    raise SystemExit(main())
