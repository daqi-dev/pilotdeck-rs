#!/usr/bin/env python3
"""Offline k=5 TS sidecar calibration; 10 pairs and two alternative sets."""
import argparse
from itertools import combinations
import json
from pathlib import Path
import sys

import g03_compare as G

DEFAULT_SOURCE = G.WORKTREE / '_tmp/20261014-ts5'


def calibrate(source):
    catalog = G.scenario_catalog()
    if 'DONE' not in (source / 'progress.txt').read_text().split():
        raise ValueError('TS five-round capture not DONE; do not calibrate partial data')
    inputs = {str(source / 'progress.txt'): G.sha256(source / 'progress.txt')}
    cache, health, summaries = {}, {}, {}
    valid = len(catalog) == 53
    for i in range(1, 6):
        directory = G.artifact_dir(source / f'r{i}')
        summary_path = directory / 'summary.json'
        summary = json.loads(summary_path.read_text())
        inputs[str(summary_path)] = G.sha256(summary_path)
        summaries[f'r{i}'] = {k: summary.get(k) for k in ('blocked', 'oracleFailures', 'provenance')}
        if summary.get('scenarios') != 53 or summary.get('blocked') or summary.get('oracleFailures'):
            valid = False
        cache[i], health[f'r{i}'] = {}, {}
        for sid, scenario in catalog.items():
            path = directory / f'{sid}.pilotdeck-current-sidecar.jsonl'
            records = G.load_aligned(path)
            inputs[str(path)] = G.sha256(path)
            cache[i][sid] = records
            errors = G.trace_health(sid, records, scenario)
            if errors:
                health[f'r{i}'][sid] = errors
                valid = False
    pairs, passes = {}, {}
    for i, j in combinations(range(1, 6), 2):
        results = {sid: G.compare_records(sid, cache[i][sid], cache[j][sid], scenario) for sid, scenario in catalog.items()}
        key = f'r{i}-r{j}'
        passes[(i, j)] = {sid for sid, r in results.items() if r['verdict'] == 'PASS'}
        pairs[key] = {'passed': len(passes[(i, j)]), 'per_scenario': {
            sid: {'verdict': r['verdict'], 'triggered_rules': r['triggered_rules'],
                  'per_stream_diffs': r['channel']['per_stream_diffs'],
                  'difference_paths': [d['path'] for d in r['channel']['differences']],
                  'health': r['health']} for sid, r in results.items()}}
    def intersection(which):
        return sorted(set.intersection(*(passes[p] for p in which)))
    all_pairs = list(combinations(range(1, 6), 2))
    s = intersection(all_pairs)
    reference = intersection([(1, i) for i in range(2, 6)])
    adjacent = intersection([(i, i+1) for i in range(1, 5)])
    return {'k': 5, 'policy': 'all_10_pairs', 'source': str(source), 'valid_calibration': valid,
            'S': s, 'S_size': len(s), 'excluded': sorted(set(catalog) - set(s)),
            'alternatives': {'r1_reference': {'S': reference, 'S_size': len(reference)},
                             'adjacent': {'S': adjacent, 'S_size': len(adjacent)}},
            'pairs': pairs, 'round_health': health, 'round_summaries': summaries,
            'input_sha256': inputs,
            'code_sha256': {str(p): G.sha256(p) for p in Path(__file__).parent.rglob('*') if p.is_file() and p.suffix in ('.py', '.mjs')},
            'original_sha256': {f: G.sha256(G.TOOLS / f) for f in ('trace.py', 'scenarios.json', 'run.py', 'adapters/pilotdeck_gateway_impl.mjs')}}


def markdown(result):
    lines = ['# TS sidecar 五遍自比', '', f"原料：`{result['source']}`；k = 5；有效校准：{result['valid_calibration']}。", '',
             '| 口径 | 配对数 | S 大小 |', '|---|---:|---:|',
             f"| 默认：两两全一致 | 10 | {result['S_size']} |",
             f"| r1 为基准 | 4 | {result['alternatives']['r1_reference']['S_size']} |",
             f"| 相邻 | 4 | {result['alternatives']['adjacent']['S_size']} |", '',
             '| 场景 | 10 对 | r1 基准 | 相邻 | 排除状态 |', '|---|---|---|---|---|']
    for sid in sorted(set(result['S']) | set(result['excluded'])):
        values = [sid in result['S'], sid in result['alternatives']['r1_reference']['S'], sid in result['alternatives']['adjacent']['S']]
        lines.append('| ' + sid + ' | ' + ' | '.join('一致' if v else '不一致' for v in values) + (' | — |' if values[0] else ' | 待批 |'))
    lines += ['', '| 配对 | 一致场景数 / 53 |', '|---|---:|']
    for name, pair in result['pairs'].items():
        lines.append(f"| {name} | {pair['passed']} |")
    return '\n'.join(lines) + '\n'


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--source', type=Path, default=DEFAULT_SOURCE)
    ap.add_argument('--json', type=Path, default=G.WORKTREE / '_tmp/self-compare.json')
    ap.add_argument('--markdown', type=Path, default=G.WORKTREE / '_tmp/self-compare.md')
    args = ap.parse_args()
    try:
        result = calibrate(args.source.resolve())
    except (OSError, ValueError, TypeError, KeyError) as error:
        print(json.dumps({'valid_calibration': False, 'error': str(error)}, ensure_ascii=False))
        return 1
    for path in (args.json, args.markdown):
        path.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    args.markdown.write_text(markdown(result))
    print(json.dumps({k: result[k] for k in ('valid_calibration', 'k', 'S_size', 'excluded', 'alternatives')}, ensure_ascii=False))
    return 0 if result['valid_calibration'] and result['S_size'] >= 45 else 1


if __name__ == '__main__':
    sys.exit(main())
