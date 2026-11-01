#!/usr/bin/env python3
"""Issue #35 controls: real G03 calls over offline mutated TS fixtures."""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it, exit before loading any apparatus module.
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

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-parity-pyc-')
sys.dont_write_bytecode = True

from copy import deepcopy
from datetime import datetime, timezone
from unittest.mock import patch
import importlib.util
import json
from pathlib import Path
import re

_HERE = Path(__file__).resolve().parent


def _load(name, path):
    """Load an apparatus module by explicit path; never resolve via sys.path."""
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


G = _load('g03_compare', _HERE.parent / 'g03_compare.py')
S = _load('score', _HERE.parent / 'score.py')


def run_controls(source):
    catalog = G.scenario_catalog()
    source = G.artifact_dir(source)
    originals = {sid: [json.loads(line) for line in G.jsonl_lines((source / f'{sid}.pilotdeck-current-sidecar.jsonl').read_text())]
                 for sid in catalog}
    cases = {}
    # _tmp/ is gitignored, so a fresh checkout has no such directory yet;
    # TemporaryDirectory(dir=...) refuses to create the parent itself.
    (G.WORKTREE / '_tmp').mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='pdrs-multimodal-', dir=G.WORKTREE / '_tmp') as tmp:
        root = Path(tmp)
        output = '/offline/multimodal-control/artifacts'

        def write(directory, sid, mode='A', side='ts', *, utc_date=None):
            records = deepcopy(originals[sid])
            # Keep structural missing now lines intact, including child requests.
            proofs = [r for r in records if r.get('kind') == 'harness.proof' and r.get('state') == 'sidecar_identity']
            if not proofs:
                proofs = [dict(kind='harness.proof', state='sidecar_identity', scenarioId=sid, q='control')]
                records.append(proofs[0])
            for proof in proofs:
                proof.update(side=side, sha256='control-binary', expectedSha256='control-binary', spawnObserved=True,
                             captureNonce=directory.name)
            if mode != 'A':
                request = next(r for r in records if r['kind'] == 'model.request')
                request['modelView']['systemPrompt'] += f'\nCONTROL_MODE_{mode}'
            if utc_date is not None:
                for record in records:
                    if record.get('kind') == 'model.request':
                        view = record.get('modelView', {})
                        if isinstance(view.get('systemPrompt'), str):
                            view['systemPrompt'] = re.sub(
                                r'(^\s*now:\s*)\d{4}-\d{2}-\d{2}',
                                lambda match: match[1] + utc_date, view['systemPrompt'], flags=re.MULTILINE)
            for i, record in enumerate(records):
                record['sequence'] = i
            (directory / f'{sid}.pilotdeck-current-sidecar.jsonl').write_text(
                ''.join(json.dumps(r) + '\n' for r in records))

        def capture(name, side, *, mode='A', full=True):
            directory = root / name
            directory.mkdir()
            (directory / 'run.json').write_text(json.dumps({
                'side': side, 'valid_round': True, 'identity_ok': True,
                'expected_sha256': 'control-binary', 'capture': name, 'argv': ['--output', output],
                'started_wall': datetime(2026, 10, 14, 12, tzinfo=timezone.utc).timestamp(),
                'inputs_sha256': {'trace.py': 'control-trace', 'run.py': 'control-run',
                                  'scenarios.json': 'control-scenarios', 'adapters/pilotdeck_gateway_impl.mjs': 'control-adapter'},
                'driver_sha256': {'round.py': 'control-round', 'bind.mjs': 'control-bind'},
                'node_sha256': 'control-node'}))
            (directory / 'summary.json').write_text(json.dumps({'blocked': [], 'output': output}))
            for sid in catalog if full else S.MULTIMODAL:
                write(directory, sid, mode if sid in S.MULTIMODAL else 'A', side)
            return directory

        # These are synthetic independent fixtures, not calibration evidence.
        pool = [capture(f'pool-{i}', 'ts', mode='A' if i < 10 else 'B', full=False) for i in range(20)]
        ts = [capture(f'ts-{i}', 'ts') for i in range(5)]
        rust = [capture(f'rust-{i}', 'rust', mode='B' if i == 0 else 'A') for i in range(5)]

        def verdict(result, sid='cancel_during_tool'):
            return result.get('per_scenario', {}).get(sid, {}).get('verdict')

        positive = S.score(ts, rust, pool)
        cases['multimodal_set_positive'] = {
            'ok': positive['valid_score'] and positive['score']['numerator'] == 53
                  and positive['per_scenario']['cancel_during_tool']['runs'][0]['paired']['verdict'] == 'FAIL',
            'verdict': verdict(positive), 'score': positive['score']}

        # (a) One Rust run misses both confirmed TS modes; the other four pass.
        write(rust[0], 'cancel_during_tool', 'C', 'rust')
        unknown = S.score(ts, rust, pool)
        detail = unknown.get('per_scenario', {}).get('cancel_during_tool', {})
        cases['rust_matches_neither_class'] = {
            'ok': unknown['valid_score'] and verdict(unknown) == 'FAIL'
                  and [r['run'] for r in detail['runs'] if r['verdict'] == 'FAIL'] == [1],
            'verdict': verdict(unknown), 'reason': detail.get('reason')}

        # (b) Nineteen independent TS captures cannot establish a pool.
        small = S.score(ts, rust, pool[:19])
        cases['pool_under_twenty'] = {'ok': small['verdict'] == 'REFUSED' and small['score'] is None,
                                     'verdict': small['verdict'], 'reason': small.get('reason')}

        # (c) A recurrent Rust-only class cannot enlarge the TS result set.
        for directory in rust:
            write(directory, 'cancel_during_tool', 'C', 'rust')
        rust_only = S.score(ts, rust, pool)
        cases['rust_only_class'] = {
            'ok': rust_only['valid_score'] and verdict(rust_only) == 'FAIL'
                  and all(not r['matched_ts_runs'] for r in rust_only['per_scenario']['cancel_during_tool']['runs'])
                  and [c['count'] for c in rust_only['TS_calibration']['cancel_during_tool']['classes']] == [10, 10],
            'verdict': verdict(rust_only)}

        # (d) A normal scenario still fails i-to-i, even with a match at another i.
        for directory in rust:
            write(directory, 'cancel_during_tool', 'A', 'rust')
        write(ts[1], 'pure_text', 'B', 'ts')
        write(rust[0], 'pure_text', 'B', 'rust')
        normal = S.score(ts, rust, pool)
        other_pair = G.compare_directories(ts[1], rust[0])['per_scenario']['pure_text']
        cases['normal_stays_paired'] = {
            'ok': normal['valid_score'] and verdict(normal, 'pure_text') == 'FAIL'
                  and normal['per_scenario']['pure_text']['policy'] == 'paired_i_to_i'
                  and other_pair['verdict'] == 'PASS',
            'verdict': verdict(normal, 'pure_text'), 'off_diagonal': other_pair['verdict']}

        write(ts[1], 'pure_text', 'A', 'ts')
        original_date = S.date_context(originals['pure_text'])['date']
        different_date = '2000-01-01' if original_date != '2000-01-01' else '2000-01-02'

        def ledger_day(directory, day):
            path = directory / 'run.json'
            ledger = json.loads(path.read_text())
            ledger['started_wall'] = datetime.fromisoformat(day).replace(tzinfo=timezone.utc).timestamp()
            path.write_text(json.dumps(ledger))

        def record_case(name, result, ok, **evidence):
            cases[name] = {'ok': bool(ok), 'verdict': result['verdict'],
                           'valid_score': result['valid_score'], 'score': result['score'],
                           'reason': result.get('reason'), **evidence}

        # Nonempty, unequal date sets are inconsistent context even on one ledger day.
        for sid in ('pure_text', 'cancel_during_tool'):
            write(rust[0], sid, 'A', 'rust', utc_date=different_date)
            result = S.score(ts, rust, pool)
            run = result['per_scenario'][sid]['runs'][0]
            record_case(f'{sid}_prompt_date_sets_differ_refused', result,
                        not result['valid_score'] and result['score'] is None
                        and result['refused_scenarios'] == [sid]
                        and run['verdict'] == 'REFUSED' and run['triggered_rules'] == ['date_context']
                        and 'observed prompt date sets differ' in run['reason']
                        and run['paired']['verdict'] == 'FAIL'
                        and (sid not in S.MULTIMODAL or run['comparisons'] == []), run=run)
            write(rust[0], sid, 'A', 'rust')

        # UTC midnight invalidates all 53 scenarios, even in paired-only mode.
        ledger_day(rust[4], '2026-10-15')
        for paired in (False, True):
            result = S.score(ts, rust, pool if not paired else None, paired_only=paired)
            record_case(f'cross_utc_midnight_entire_round_{"paired" if paired else "multimodal"}', result,
                        result['verdict'] == 'REFUSED' and result['score'] is None
                        and len(result['refused_scenarios']) == 53
                        and all(r['triggered_rules'] == ['date_context']
                                for scenario in result['per_scenario'].values() for r in scenario['runs'])
                        and result['per_scenario']['cancel_during_tool']['runs'][0].get('comparisons', []) == [],
                        refused_scenarios=result['refused_scenarios'])
        ledger_day(rust[4], '2026-10-14')

        # Internally same-day regression but pool day differs. Keep true paired FAIL.
        for directory in ts + rust:
            ledger_day(directory, '2026-10-15')
        write(rust[0], 'cancel_during_tool', 'B', 'rust')
        result = S.score(ts, rust, pool)
        run = result['per_scenario']['cancel_during_tool']['runs'][0]
        record_case('pool_date_mismatch_refused_preserves_paired', result,
                    result['verdict'] == 'REFUSED' and result['score'] is None
                    and result['refused_scenarios'] == ['cancel_during_tool']
                    and run['triggered_rules'] == ['date_context'] and run['comparisons'] == []
                    and run['paired']['verdict'] == 'FAIL', run=run)
        for directory in ts + rust:
            ledger_day(directory, '2026-10-14')
        write(rust[0], 'cancel_during_tool', 'A', 'rust')
        ledger_day(pool[19], '2026-10-15')
        result = S.score(ts, rust, pool)
        record_case('pool_cross_day_refuses_calibration', result,
                    result['verdict'] == 'REFUSED' and result['score'] is None
                    and 'TS reference pool spans UTC days' in result['reason']
                    and 'TS_calibration' not in result)
        ledger_day(pool[19], '2026-10-14')

        # Equal partial date context must score normally, without filling in now.
        structural_sid = next(sid for sid, records in originals.items()
                              if S.date_context(records)['observed_dates']
                              and S.date_context(records)['requests_without_date'])
        for paired in (False, True):
            result = S.score(ts, rust, pool if not paired else None, paired_only=paired)
            run = result['per_scenario'][structural_sid]['runs'][0]
            record_case(f'both_partial_dates_{"paired" if paired else "multimodal"}_positive', result,
                        result['valid_score'] and not result['refused_scenarios']
                        and run['verdict'] == 'PASS' and run['paired']['verdict'] == 'PASS'
                        and run['date_context']['ts']['observed_dates'] == run['date_context']['rust']['observed_dates']
                        and bool(run['date_context']['ts']['requests_without_date'])
                        and run['date_context']['ts']['requests_without_date'] == run['date_context']['rust']['requests_without_date'],
                        scenario=structural_sid, run=run)

        # One-sided null and partial missing dates are G03 failures, in either direction.
        partial_sid = next(sid for sid, records in originals.items()
                           if sum(r.get('kind') == 'model.request' for r in records) > 1
                           and S.date_context(records)['date']
                           and not S.date_context(records)['requests_without_date'])
        for side, directory in (('ts', ts[0]), ('rust', rust[0])):
            for partial, sid in ((False, 'pure_text'), (True, partial_sid)):
                path = directory / f'{sid}.pilotdeck-current-sidecar.jsonl'
                original = path.read_text()
                rows = [json.loads(line) for line in G.jsonl_lines(original)]
                requests = [r for r in rows if r.get('kind') == 'model.request']
                for request in requests[:1] if partial else requests:
                    view = request['modelView']
                    view['systemPrompt'] = re.sub(r'^\s*now:.*$', '', view['systemPrompt'], flags=re.MULTILINE)
                path.write_text(''.join(json.dumps(r) + '\n' for r in rows))
                result = S.score(ts, rust, pool)
                run = result['per_scenario'][sid]['runs'][0]
                context = run['date_context'][side]
                record_case(f'{side}_{"partial" if partial else "null"}_date_g03_fail', result,
                            result['valid_score'] and not result['refused_scenarios']
                            and run['verdict'] == 'FAIL' and run['paired']['verdict'] == 'FAIL'
                            and 'G03_channels' in run['triggered_rules']
                            and any('modelView' in p for p in run['difference_paths'])
                            and run['date_context']['ts']['capture_utc_day'] == run['date_context']['rust']['capture_utc_day']
                            and bool(context['requests_without_date'])
                            and (bool(context['observed_dates']) if partial else context['date'] is None), run=run)
                path.write_text(original)

        # Same raw trace in another directory, with a changed ledger, is one capture.
        copied = capture('copied-pool', 'ts', full=False)
        trace = 'cancel_during_tool.pilotdeck-current-sidecar.jsonl'
        (copied / trace).write_bytes((pool[0] / trace).read_bytes())
        result = S.score(ts, rust, pool[:19] + [copied])
        record_case('different_directory_same_trace_refused', result,
                    result['verdict'] == 'REFUSED' and 'copied TS capture' in result['reason']
                    and G.sha256(copied / 'run.json') != G.sha256(pool[0] / 'run.json')
                    and G.sha256(copied / trace) == G.sha256(pool[0] / trace))

        result = S.score(ts, rust, pool[:19] + [ts[0]])
        record_case('pool_scoring_directory_overlap_refused', result,
                    result['verdict'] == 'REFUSED' and 'overlaps scoring TS' in result['reason'])
        original = (ts[0] / trace).read_bytes()
        (ts[0] / trace).write_bytes((pool[0] / trace).read_bytes())
        result = S.score(ts, rust, pool)
        record_case('pool_scoring_trace_overlap_refused', result,
                    result['verdict'] == 'REFUSED' and 'overlaps scoring TS' in result['reason'])
        (ts[0] / trace).write_bytes(original)

        # Each provenance field is independently checked across pool and scoring.
        for directory, scope in ((pool[19], 'pool'), (ts[4], 'scoring')):
            for key in S.SOURCE_KEYS:
                path = directory / 'run.json'
                original = path.read_text()
                ledger = json.loads(original)
                if isinstance(ledger[key], dict):
                    ledger[key][next(iter(ledger[key]))] += '-different'
                else:
                    ledger[key] += '-different'
                trace_backups = {}
                if key == 'expected_sha256':
                    for trace_path in directory.glob('*.pilotdeck-current-sidecar.jsonl'):
                        trace_backups[trace_path] = trace_path.read_text()
                        rows = [json.loads(line) for line in G.jsonl_lines(trace_backups[trace_path])]
                        for row in rows:
                            if row.get('kind') == 'harness.proof' and row.get('state') == 'sidecar_identity':
                                row.update(sha256=ledger[key], expectedSha256=ledger[key])
                        trace_path.write_text(''.join(json.dumps(r) + '\n' for r in rows))
                path.write_text(json.dumps(ledger))
                result = S.score(ts, rust, pool)
                record_case(f'{scope}_{key}_different_source_refused', result,
                            result['verdict'] == 'REFUSED' and result['score'] is None
                            and 'TS pool/scoring source sha256 differs' in result['reason']
                            and key in result['reason'])
                path.write_text(original)
                for trace_path, data in trace_backups.items():
                    trace_path.write_text(data)

        # Fault-inject one comparison to exercise a partial class match. All other
        # comparisons use real G03; this injected result is a real A-vs-B FAIL.
        compare = G.compare_records
        failed_comparison = compare('cancel_during_tool', G.load_aligned(pool[10] / trace),
                                    G.load_aligned(rust[0] / trace), catalog['cancel_during_tool'])
        def partial_comparison(sid, left, right, scenario=None):
            left_proof = next((r for r in left if r.get('state') == 'sidecar_identity'), {})
            right_proof = next((r for r in right if r.get('state') == 'sidecar_identity'), {})
            if sid == 'cancel_during_tool' and left_proof.get('captureNonce') == 'pool-9' and right_proof.get('side') == 'rust':
                return deepcopy(failed_comparison)
            return compare(sid, left, right, scenario)
        with patch.object(G, 'compare_records', side_effect=partial_comparison):
            result = S.score(ts, rust, pool)
        run = result['per_scenario']['cancel_during_tool']['runs'][0]
        record_case('partial_class_match_fails', result,
                    result['valid_score'] and run['verdict'] == 'FAIL'
                    and run['matched_ts_runs'] == list(range(1, 10)) and run['matched_classes'] == []
                    and run['class_matches'][0]['matched_members'] == list(range(1, 10))
                    and run['class_matches'][0]['unmatched_members'] == [10],
                    comparison_fault_injection=True, run=run)

        pinned = root / 'reference-registration.json'
        pinned.write_text(json.dumps({'TS_calibration': positive['TS_calibration']}))
        result = S.score(ts, rust, pool, reference_registration=pinned)
        record_case('pinned_registration_positive', result,
                    result['valid_score'] and result['verdict'] == 'PASS')
        changed = deepcopy(positive['TS_calibration'])
        changed['cancel_during_tool']['classes'][0]['count'] = 9
        pinned.write_text(json.dumps({'TS_calibration': changed}))
        result = S.score(ts, rust, pool, reference_registration=pinned)
        record_case('pinned_classification_mismatch_refused', result,
                    result['verdict'] == 'REFUSED' and result['score'] is None
                    and 'differ from pinned reference registration' in result['reason'])

        # Conservative admission and provenance checks are part of the rule.
        for directory in pool[11:]:
            write(directory, 'cancel_during_tool', 'A')
        singleton = S.score(ts, rust, pool)
        cases['singleton_class_refused'] = {
            'ok': singleton['verdict'] == 'REFUSED' and 'singleton' in singleton.get('reason', ''),
            'verdict': singleton['verdict'], 'reason': singleton.get('reason')}
        contaminated = S.score(ts, rust, pool[:19] + [rust[0]])
        cases['rust_cannot_enter_ts_pool'] = {
            'ok': contaminated['verdict'] == 'REFUSED' and 'valid ts identity' in contaminated.get('reason', ''),
            'verdict': contaminated['verdict'], 'reason': contaminated.get('reason')}
        repeated = S.score(ts, rust, pool[:19] + [pool[0]])
        cases['repeated_capture_refused'] = {'ok': repeated['verdict'] == 'REFUSED', 'verdict': repeated['verdict']}
        exclusions = root / 'accepted-diffs.md'
        exclusions.write_text(
            '| 1 | `pure_text` | control | 原版不稳定 | TS evidence | 待批 |\n'
            '| 2 | `deadline` | control | 原版缺陷 | evidence | 已批：人 · control |\n')
        cases['pending_and_other_categories_do_not_exclude'] = {
            'ok': S.approved_unstable(exclusions, catalog) == set()}
        exclusions.write_text('| 1 | `pure_text` | control | 原版不稳定 | TS evidence | 已批：人 · control |\n')
        cases['approved_unstable_denominator'] = {
            'ok': S.approved_unstable(exclusions, catalog) == {'pure_text'}}
    return cases
