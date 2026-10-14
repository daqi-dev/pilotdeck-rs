#!/usr/bin/env python3
"""Issue #35 controls: real G03 calls over offline mutated TS fixtures."""
from copy import deepcopy
import json
from pathlib import Path
import re
import tempfile

import g03_compare as G
import score as S


def run_controls(source):
    catalog = G.scenario_catalog()
    source = G.artifact_dir(source)
    originals = {sid: [json.loads(line) for line in (source / f'{sid}.pilotdeck-current-sidecar.jsonl').read_text().splitlines()]
                 for sid in catalog}
    cases = {}
    with tempfile.TemporaryDirectory(prefix='pdrs-multimodal-', dir=G.WORKTREE / '_tmp') as tmp:
        root = Path(tmp)
        output = '/offline/multimodal-control/artifacts'

        def write(directory, sid, mode='A', side='ts', *, utc_date=None):
            records = deepcopy(originals[sid])
            proofs = [r for r in records if r.get('kind') == 'harness.proof' and r.get('state') == 'sidecar_identity']
            if not proofs:
                proofs = [dict(kind='harness.proof', state='sidecar_identity', scenarioId=sid, q='control')]
                records.append(proofs[0])
            for proof in proofs:
                proof.update(side=side, sha256='control-binary', expectedSha256='control-binary', spawnObserved=True)
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
                'expected_sha256': 'control-binary', 'capture': name, 'argv': ['--output', output]}))
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

        # Date context is an admission condition, not a G03 failure or mode.
        write(ts[1], 'pure_text', 'A', 'ts')
        original_date = S.date_context(originals['pure_text'])['date']
        different_date = '2000-01-01' if original_date != '2000-01-01' else '2000-01-02'
        write(rust[0], 'pure_text', 'A', 'rust', utc_date=different_date)
        normal_date = S.score(ts, rust, pool)
        detail = normal_date.get('per_scenario', {}).get('pure_text', {})
        cases['normal_date_mismatch_refused'] = {
            'ok': not normal_date['valid_score'] and normal_date['verdict'] == 'REFUSED'
                  and normal_date['score'] is None and verdict(normal_date, 'pure_text') == 'REFUSED'
                  and detail['runs'][0]['triggered_rules'] == ['date_context']
                  and 'paired run 1 TS/Rust: UTC date context differs' in detail.get('reason', ''),
            'verdict': verdict(normal_date, 'pure_text'), 'valid_score': normal_date['valid_score'],
            'score': normal_date['score'], 'reason': detail.get('reason')}
        write(rust[0], 'pure_text', 'A', 'rust')

        write(rust[0], 'cancel_during_tool', 'A', 'rust', utc_date=different_date)
        multimodal_date = S.score(ts, rust, pool)
        detail = multimodal_date.get('per_scenario', {}).get('cancel_during_tool', {})
        cases['multimodal_date_mismatch_refused'] = {
            'ok': not multimodal_date['valid_score'] and multimodal_date['verdict'] == 'REFUSED'
                  and multimodal_date['score'] is None and verdict(multimodal_date) == 'REFUSED'
                  and detail['runs'][0]['comparisons'] == []
                  and detail['runs'][0]['triggered_rules'] == ['date_context']
                  and 'Rust run 1 / TS reference 1: UTC date context differs' in detail.get('reason', ''),
            'verdict': verdict(multimodal_date), 'valid_score': multimodal_date['valid_score'],
            'score': multimodal_date['score'], 'reason': detail.get('reason')}
        write(rust[0], 'cancel_during_tool', 'A', 'rust')

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
