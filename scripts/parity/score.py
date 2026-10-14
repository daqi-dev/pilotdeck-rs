#!/usr/bin/env python3
"""Offline k=5 scoring; registered TS modes use a TS-only reference pool."""
from __future__ import annotations

import argparse
from datetime import date
from itertools import combinations
import json
from pathlib import Path
import re
import sys

import g03_compare as G

K = 5
MIN_POOL = 20
# Issue #35: only this multimodal registration is authorized.
MULTIMODAL = ('cancel_during_tool',)
DEFAULT_POOL = G.BASELINE.parents[1] / '_tmp/20261014-flake35'


def date_context(records):
    """Read every request's literal now line; never normalize the prompt."""
    requests = [r for r in records if r.get('kind') == 'model.request']
    dates, missing = set(), []
    for request in requests:
        prompt = request.get('modelView', {}).get('systemPrompt')
        found = re.findall(r'^\s*now:\s*(\d{4}-\d{2}-\d{2})(?=\s|T|$)', prompt, re.MULTILINE) if isinstance(prompt, str) else []
        for value in found:
            date.fromisoformat(value)
        dates.update(found)
        if not found:
            missing.append(request.get('sequence'))
    observed = sorted(dates)
    return {'date': observed[0] if len(observed) == 1 else None,
            'observed_dates': observed, 'requests_without_date': missing,
            'note': ('no model.request; date context is null' if not requests else
                     'no now: YYYY-MM-DD in systemPrompt; date context is null' if not dates else
                     'multiple UTC dates in systemPrompt; capture crossed UTC midnight' if len(dates) > 1 else
                     'some systemPrompt values have no now date' if missing else None)}


def date_mismatch(left, right, label):
    if len(left['observed_dates']) > 1 or len(right['observed_dates']) > 1:
        return f'{label}: multiple UTC dates in a trace; round must be rerun'
    if left['date'] != right['date']:
        return f"{label}: UTC date context differs ({left['date']!r} vs {right['date']!r}); recapture on the same UTC day"
    return None


def refused(reason):
    return {'verdict': 'REFUSED', 'triggered_rules': ['date_context'], 'reason': reason}


def approved_unstable(path, catalog):
    """Only explicitly approved 原版不稳定 rows may reduce the denominator."""
    excluded = set()
    for line in path.read_text().splitlines():
        columns = [column.strip() for column in line.split('|')]
        if len(columns) != 8 or columns[4] != '原版不稳定':
            continue
        if not columns[6].startswith('已批'):
            continue
        ids = re.findall(r'`([^`]+)`', columns[2])
        if not ids or any(sid not in catalog for sid in ids):
            raise ValueError('approved original-unstable row has unknown scenario identity')
        excluded.update(ids)
    if 53 - len(excluded) < 45:
        raise ValueError('approved stable set is below the frozen lower bound 45')
    if excluded.intersection(MULTIMODAL):
        raise ValueError('scenario cannot be both excluded and registered multimodal')
    return excluded


def compact(result):
    return {key: result[key] for key in ('verdict', 'triggered_rules')} | {
        'difference_paths': [d['path'] for d in result.get('channel', {}).get('differences', [])],
        'health': result.get('health', {}),
        'error': result.get('error'),
    }


class Inputs:
    def __init__(self):
        self.hashes = {}
        self.dates = {}

    def track(self, path):
        self.hashes[str(path.resolve())] = G.sha256(path)

    def capture(self, directory, side, scenarios):
        directory = Path(directory).resolve()
        artifacts = G.artifact_dir(directory)
        root = artifacts.parent if artifacts.name == 'artifacts' else directory
        ledger_path = root / 'run.json'
        self.track(ledger_path)
        ledger = json.loads(ledger_path.read_text())
        if (ledger.get('side') != side or ledger.get('valid_round') is not True
                or ledger.get('identity_ok') is not True):
            raise ValueError(f'{root}: requires valid {side} identity ledger')
        summary_path = artifacts / 'summary.json'
        self.track(summary_path)
        summary = json.loads(summary_path.read_text())
        if summary.get('blocked'):
            raise ValueError(f'{root}: BLOCKED invalidates the round')
        # Compare the actual output path recorded by the harness; archives may
        # have different names, but capture-time paths must be identical.
        output = summary.get('output')
        argv = ledger.get('argv', [])
        if not isinstance(output, str) or not output or '--output' not in argv:
            raise ValueError(f'{root}: missing capture-time output path')
        if argv[argv.index('--output') + 1] != output:
            raise ValueError(f'{root}: output path disagrees with ledger')
        self.dates[str(root)] = {}
        for sid in scenarios:
            path = artifacts / f'{sid}.pilotdeck-current-sidecar.jsonl'
            self.track(path)
            self.dates[str(root)][sid] = date_context(
                [json.loads(line) for line in path.read_text().splitlines() if line.strip()])
        # Calling the frozen directory comparator retains its identity checks,
        # E rules and oracle. A targeted TS pool need not contain all 53 traces.
        checked = G.compare_directories(root, root)
        for sid in scenarios:
            result = checked['per_scenario'][sid]
            if 'trace_identity' in result['triggered_rules'] or 'trace_input' in result['triggered_rules']:
                raise ValueError(f'{root}/{sid}: invalid trace identity/input')
        return root, output, checked


def calibrate_pool(directories, inputs):
    """Complete TS-only pair matrix, before reading any Rust capture."""
    directories = [Path(d).resolve() for d in directories]
    if len(directories) < MIN_POOL or len(set(directories)) != len(directories):
        raise ValueError('TS reference pool requires >=20 distinct capture directories')
    captures, records, fingerprints = [], {}, set()
    for directory in directories:
        root, output, checked = inputs.capture(directory, 'ts', MULTIMODAL)
        fingerprint = tuple(inputs.hashes[str(p.resolve())] for p in (
            root / 'run.json', G.artifact_dir(root) / f'{MULTIMODAL[0]}.pilotdeck-current-sidecar.jsonl'))
        if fingerprint in fingerprints:
            raise ValueError('copied TS capture cannot count as another reference run')
        fingerprints.add(fingerprint)
        captures.append((root, output))
        for sid in MULTIMODAL:
            if checked['per_scenario'][sid]['verdict'] != 'PASS':
                raise ValueError(f'{root}/{sid}: TS reference fails G03 self-comparison')
            records[(sid, len(captures) - 1)] = G.load_aligned(
                G.artifact_dir(root) / f'{sid}.pilotdeck-current-sidecar.jsonl')
    if len({output for _, output in captures}) != 1:
        raise ValueError('TS reference pool has different capture-time output paths')
    pool_dates = {d for root, _ in captures for context in inputs.dates[str(root)].values()
                  for d in context['observed_dates']}
    if len(pool_dates) > 1:
        raise ValueError(f'TS reference pool spans UTC days {sorted(pool_dates)}; recapture on one UTC day')
    catalog = G.scenario_catalog()
    registration = {}
    for sid in MULTIMODAL:
        matrix = {}
        for i, j in combinations(range(len(captures)), 2):
            matrix[(i, j)] = compact(G.compare_records(sid, records[(sid, i)], records[(sid, j)], catalog[sid]))
        groups = []
        for i in range(len(captures)):
            matches = [group for group in groups if matrix[(group[0], i)]['verdict'] == 'PASS']
            if len(matches) > 1:
                raise ValueError(f'{sid}: G03 relation is not an equivalence relation')
            if matches:
                matches[0].append(i)
            else:
                groups.append([i])
        # Do not use connected components to accept a non-transitive bridge.
        membership = {i: n for n, group in enumerate(groups) for i in group}
        if any((r['verdict'] == 'PASS') != (membership[i] == membership[j])
               for (i, j), r in matrix.items()):
            raise ValueError(f'{sid}: G03 pair matrix is not an equivalence relation')
        if len(groups) < 2 or any(len(group) < 2 for group in groups):
            details = [{'count': len(group), 'members': [str(captures[i][0]) for i in group]} for group in groups]
            raise ValueError(f'{sid}: requires >=2 TS classes and >=2 runs per class; singleton is unconfirmed noise; {details}')
        registration[sid] = {
            'classes': [{'class': n, 'count': len(group), 'representative': str(captures[group[0]][0]),
                         'members': [str(captures[i][0]) for i in group]} for n, group in enumerate(groups)],
            'pairs': {f'{i+1}-{j+1}': r for (i, j), r in matrix.items()},
        }
    return captures, records, registration


def score(ts_directories, rust_directories, reference_pool=None, *, paired_only=False):
    inputs = Inputs()
    try:
        catalog = G.scenario_catalog()
        exclusions_path = G.WORKTREE / 'specs/parity/accepted-diffs.md'
        inputs.track(exclusions_path)
        excluded = approved_unstable(exclusions_path, catalog)
        if len(catalog) != 53 or len(ts_directories) != K or len(rust_directories) != K:
            raise ValueError('requires all 53 scenarios and exactly k=5 TS / Rust runs')
        if any(len({Path(d).resolve() for d in dirs}) != K for dirs in (ts_directories, rust_directories)):
            raise ValueError('each scoring side requires five distinct captures')
        pool, pool_records, registration = [], {}, {}
        if not paired_only:
            directories = reference_pool if reference_pool is not None else [DEFAULT_POOL / f'ts-r{i}.d' for i in range(1, 21)]
            pool, pool_records, registration = calibrate_pool(directories, inputs)
        # Policy and classes above are fixed without consulting Rust.
        ts = [inputs.capture(d, 'ts', catalog) for d in ts_directories]
        rust = [inputs.capture(d, 'rust', catalog) for d in rust_directories]
        if len({output for _, output, _ in ts + rust} | {output for _, output in pool}) != 1:
            raise ValueError('capture-time output paths differ; offline path alignment is forbidden for scoring')
        round_refusal = None
        for side, captures in (('TS', ts), ('Rust', rust)):
            dates = {d for root, _, _ in captures for context in inputs.dates[str(root)].values()
                     for d in context['observed_dates']}
            if len(dates) > 1:
                round_refusal = f'{side} regression spans UTC days {sorted(dates)}; entire round is invalid and must be rerun'
                break
        scenarios = {}
        for sid in catalog:
            runs = []
            for i, capture in enumerate(rust):
                ts_date = inputs.dates[str(ts[i][0])][sid]
                rust_date = inputs.dates[str(capture[0])][sid]
                pair_reason = date_mismatch(ts_date, rust_date, f'paired run {i + 1} TS/Rust')
                records = G.load_aligned(G.artifact_dir(capture[0]) / f'{sid}.pilotdeck-current-sidecar.jsonl')
                pair = refused(pair_reason or round_refusal) if pair_reason or round_refusal else compact(
                    G.compare_records(sid, G.load_aligned(G.artifact_dir(ts[i][0]) / f'{sid}.pilotdeck-current-sidecar.jsonl'),
                                      records, catalog[sid]))
                if sid in registration:
                    pool_contexts = [inputs.dates[str(root)][sid] for root, _ in pool]
                    reason = next((reason for j, context in enumerate(pool_contexts)
                                   if (reason := date_mismatch(context, rust_date, f'Rust run {i + 1} / TS reference {j + 1}'))), None)
                    if reason or round_refusal:
                        runs.append({'run': i + 1, **refused(reason or round_refusal),
                                     'date_context': {'rust': rust_date, 'TS_reference_pool': pool_contexts},
                                     'matched_ts_runs': [], 'comparisons': [], 'paired': pair})
                        continue
                    comparisons = [compact(G.compare_records(sid, pool_records[(sid, j)], records, catalog[sid]))
                                   for j in range(len(pool))]
                    matches = [j + 1 for j, result in enumerate(comparisons) if result['verdict'] == 'PASS']
                    runs.append({'run': i + 1, 'verdict': 'PASS' if matches else 'FAIL',
                                 'date_context': {'rust': rust_date, 'TS_reference_pool': pool_contexts},
                                 'matched_ts_runs': matches, 'comparisons': comparisons, 'paired': pair})
                else:
                    runs.append({'run': i + 1, **pair, 'date_context': {'ts': ts_date, 'rust': rust_date}})
            refused_runs = [r['run'] for r in runs if r['verdict'] == 'REFUSED']
            failed = [r['run'] for r in runs if r['verdict'] == 'FAIL']
            multimodal = sid in registration
            scenarios[sid] = {'verdict': 'REFUSED' if refused_runs else 'FAIL' if failed else 'PASS',
                'counted': sid not in excluded,
                'policy': 'TS_reference_set' if multimodal else 'paired_i_to_i',
                'reason': ('; '.join(r['reason'] for r in runs if r['verdict'] == 'REFUSED') if refused_runs else
                           (f'Rust runs {failed} match no TS reference' if multimodal else f'paired runs {failed} fail G03')
                           if failed else ('every Rust run matches at least one TS reference' if multimodal else 'all five paired comparisons PASS')),
                'runs': runs}
        denominator = 53 - len(excluded)
        passed = sum(r['verdict'] == 'PASS' for sid, r in scenarios.items() if sid not in excluded)
        refused_scenarios = [sid for sid, result in scenarios.items() if result['verdict'] == 'REFUSED']
        result = {'valid_score': not refused_scenarios, 'verdict': 'REFUSED' if refused_scenarios else 'PASS' if passed == denominator else 'FAIL', 'k': K,
                  'score': None if refused_scenarios else {'numerator': passed, 'denominator': denominator, 'fraction': f'{passed}/{denominator}'},
                  'refused_scenarios': refused_scenarios,
                  'reason': f'date context precondition failed for {refused_scenarios}' if refused_scenarios else None,
                  'excluded_original_unstable': sorted(excluded),
                  'registration_status': '待批（方案 B 人已批准；登记待协调者评审冻结）',
                  'TS_reference_pool': [str(root) for root, _ in pool], 'TS_calibration': registration,
                  'per_scenario': scenarios}
    except (OSError, ValueError, TypeError, KeyError, AttributeError, IndexError) as error:
        result = {'valid_score': False, 'verdict': 'REFUSED', 'score': None, 'reason': str(error)}
    result['input_sha256'] = inputs.hashes
    result['date_context'] = inputs.dates
    result['code_sha256'] = {str(p): G.sha256(p) for p in (Path(__file__), Path(G.__file__))}
    result['original_sha256'] = {name: G.sha256(G.TOOLS / name) for name in ('trace.py', 'scenarios.json')}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ts', nargs='+', required=True, type=Path, metavar='DIR')
    parser.add_argument('--rust', nargs='+', required=True, type=Path, metavar='DIR')
    parser.add_argument('--reference-pool', nargs='+', type=Path, metavar='TS_DIR')
    parser.add_argument('--paired-only', action='store_true', help='replay the old paired policy, without reference calibration')
    parser.add_argument('--json', type=Path)
    args = parser.parse_args()
    if args.paired_only and args.reference_pool is not None:
        parser.error('--paired-only cannot be combined with --reference-pool')
    result = score(args.ts, args.rust, args.reference_pool, paired_only=args.paired_only)
    data = json.dumps(result, ensure_ascii=False, indent=2) + '\n'
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(data)
        print(json.dumps({key: result[key] for key in ('valid_score', 'verdict', 'score')}, ensure_ascii=False))
    else:
        print(data, end='')
    return 2 if not result['valid_score'] else (0 if result['verdict'] == 'PASS' else 1)


if __name__ == '__main__':
    sys.exit(main())
