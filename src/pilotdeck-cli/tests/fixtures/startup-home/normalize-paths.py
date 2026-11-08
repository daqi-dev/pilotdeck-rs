#!/usr/bin/env python3
"""Normalize only the private cwd in two fixed Node path captures."""
import argparse
import hashlib
import json
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('--relative', type=Path, required=True)
p.add_argument('--empty', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
rows = []
sources = {}
for label, path in [('relative', a.relative), ('empty', a.empty)]:
    raw = path.read_bytes()
    sources[label] = hashlib.sha256(raw).hexdigest()
    data = json.loads(raw)
    data = data.get('result', data)
    assert data['node'] == 'v22.22.0' and data['icu'] == '78.2'
    assert data['HOME'] == ('relative-home' if label == 'relative' else '')
    for row in data['rows']:
        clean = {key: value.replace(data['cwd'], '<CWD>') if isinstance(value, str) else value
                 for key, value in row.items()}
        rows.append(dict(HOME=data['HOME'], **clean))
with a.output.open('x') as target:
    json.dump(dict(baseline='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',
                   node='v22.22.0', icu='78.2', rawSha256=sources, cases=rows),
              target, ensure_ascii=False, indent=2)
    target.write('\n')
print('normalized', len(rows), 'fixed Node path cases')
