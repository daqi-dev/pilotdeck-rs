#!/usr/bin/env python3
"""Four ruler checks. Read five real TS direct archives; never manufacture calibration."""
# Admit source-only apparatus before json or any local module can be imported.
import os as _boot_os
import sys as _boot_sys
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
with open(_boot_source, 'rb') as _boot_file:
    exec(compile(_boot_file.read(), _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import argparse
import hashlib
import importlib.util
from itertools import combinations
import json
from pathlib import Path

from common import apparatus_hashes
from compare import compare_sides, read_json, read_jsonl

HERE = Path(__file__).resolve().parent


def check(rounds, enumeration, proxy=None, matrix=None, scenarios=None, replay_of=None, reference_pool=None, paired_only=False,
          replay_pool_of=None):
    spec = importlib.util.spec_from_file_location('b2_controls', HERE / 'controls/run_all.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    controls = module.run_controls()
    rows = controls['controls']
    negative = [r for r in rows if r.get('expected_rule') and not r.get('both_sides')]
    same_bad = [r for r in rows if r.get('both_sides')]
    result = {'k': 5, 'spec_status': 'DRAFT', 'self_compare': None,
              'per_rule_negative': controls['verdict'] == 'PASS' and bool(negative) and all(r['passed'] for r in negative) and not controls['missing_rule_controls'],
              'same_bad_rejected': len(same_bad) >= 2 and all(r['passed'] for r in same_bad),
              'independent_review': None, 'apparatus_sha256': apparatus_hashes(), 'controls': controls, 'pairs': [], 'errors': [],
              'verdict': 'PENDING_CALIBRATION'}
    recorded_apparatus = None
    result['calibration_eligible'] = replay_of is None and not paired_only
    result['comparison_mode'] = 'calibration' if replay_of is None else 'historical-replay'
    if replay_pool_of is not None:
        result.update(calibration_eligible=False, comparison_mode='historical-replay')
        if reference_pool is None or replay_of is not None or paired_only:
            result.update(verdict='REFUSED', errors=['--replay-pool-of requires --reference-pool and excludes --replay-of/--paired-only'])
            return result
    if replay_of is not None:
        try:
            prior = read_json(replay_of)
            recorded_apparatus = prior['apparatus_sha256']
            if (prior.get('k') != 5 or len(prior.get('pairs', [])) != 10
                    or len(set(prior.get('run_ids', []))) != 5
                    or not isinstance(recorded_apparatus, dict) or not recorded_apparatus
                    or any(not isinstance(v, str) or len(v) != 64 for v in recorded_apparatus.values())):
                raise ValueError('replay requires the original real k=5 ruler report')
            result['replay_provenance'] = {
                'path': str(replay_of), 'sha256': hashlib.sha256(Path(replay_of).read_bytes()).hexdigest(),
                'recorded_apparatus_sha256': recorded_apparatus,
                'changed_apparatus': sorted(k for k in recorded_apparatus.keys() | result['apparatus_sha256'].keys()
                                            if recorded_apparatus.get(k) != result['apparatus_sha256'].get(k)),
                'note': 'Historical evidence only; the changed apparatus requires new real k=5 capture.',
            }
        except (OSError, ValueError, KeyError, TypeError) as error:
            result['errors'].append(f'replay provenance: {error}')
            result['verdict'] = 'FAIL'
            return result
    if (reference_pool is not None and paired_only) or (rounds and reference_pool is None and not replay_of and not paired_only):
        result['errors'].append('requires explicit --reference-pool (>=20 TS); --paired-only is diagnostic only and cannot be combined with a pool')
        result['verdict'] = 'REFUSED'
        result['calibration_eligible'] = False
        return result
    if paired_only:
        result['comparison_mode'] = 'paired-diagnostic'
    pool = None
    if reference_pool is not None:
        try:
            if replay_of is not None:
                raise ValueError('reference pool cannot certify historical replay; capture pool + fresh k=5')
            if len(rounds) != 5 or enumeration is None:
                raise ValueError('reference pool requires exactly five fresh --round and --enumeration')
            from multimodal import ReferencePool
            pool = ReferencePool(reference_pool, enumeration, matrix, scenarios, replay_pool_of=replay_pool_of)
            if pool.replay_provenance:
                recorded_apparatus = pool.recorded_apparatus
                result['replay_provenance'] = pool.replay_provenance
            result['TS_calibration'] = pool.registration
            # Freeze TS-only classes before reading scoring TS or proxy metadata.
            for directory in rounds:
                pool.admit(directory, 'direct')
            if proxy:
                pool.admit(proxy, 'proxy')
        except (OSError, ValueError, KeyError, TypeError, AttributeError, OverflowError) as error:
            result['errors'].append(str(error))
            result['verdict'] = 'REFUSED'
            result['calibration_eligible'] = False
            return result
    if rounds:
        if len(rounds) != 5 or enumeration is None:
            result['errors'].append('exactly five --round directories and --enumeration are required')
        else:
            run_ids, pids, paths = [], [], []
            for directory in rounds:
                try:
                    meta = read_json(directory / 'run-meta.json')
                    audit = read_json(directory / 'audit.json')
                    recorder = [p for p in audit['processes'] if p['name'] == 'recorder']
                    if (meta['mode'] != 'direct' or meta.get('synthetic') is not False
                            or meta.get('apparatus_sha256') != (recorded_apparatus if recorded_apparatus is not None else apparatus_hashes())
                            or audit['owned_groups_remaining'] or audit['cleanup_errors']
                            or len(recorder) != 1 or recorder[0]['exitCode'] != 0):
                        raise ValueError('not an independent, successfully stopped real TS direct round')
                    if (not isinstance(meta.get('run_id'), str) or not meta['run_id']
                            or not isinstance(meta.get('fixture_path'), str)
                            or type(recorder[0].get('pid')) is not int or recorder[0]['pid'] <= 0):
                        raise ValueError('invalid run identity/path/PID types')
                    run_ids.append(meta['run_id'])
                    pids.append(recorder[0]['pid'])
                    paths.append(meta['fixture_path'])
                    local_enum = directory.parent / 'enumeration/router-enumeration.json'
                    if read_json(local_enum) != read_json(enumeration):
                        # Enumeration carries ancillary fields; compare only real route denominator.
                        local = read_json(local_enum)
                        expected = read_json(enumeration)
                        if local.get('routes') != expected.get('routes') or local.get('uniqueRouteCount') != expected.get('uniqueRouteCount') or local.get('appCaptured') is not True:
                            raise ValueError('runtime denominator changed between rounds')
                    if not read_jsonl(directory / 'recording.jsonl'):
                        raise ValueError('empty round')
                except (OSError, ValueError, KeyError, TypeError) as error:
                    result['errors'].append(f'{directory}: {error}')
            if len(set(run_ids)) != 5 or len(set(pids)) != 5 or len(set(paths)) != 1:
                result['errors'].append('need five distinct run IDs/PIDs and byte-identical fixture_path')
            if replay_of and (run_ids != prior['run_ids'] or pids != prior.get('recorder_pids')):
                result['errors'].append('replay rounds do not match original ruler run IDs/PIDs in order')
            for a, b in combinations(range(5), 2):
                report = (pool.score_pair(rounds[a], rounds[b]) if pool else
                          compare_sides(rounds[a], rounds[b], enumeration, matrix, scenarios,
                                        recorded_apparatus=recorded_apparatus))
                result['pairs'].append({'pair': [a + 1, b + 1], **report})
            result['self_compare'] = not result['errors'] and all(p['verdict'] == 'PASS' for p in result['pairs'])
            result['run_ids'], result['recorder_pids'] = run_ids, pids
            result['verdict'] = 'PENDING_REVIEW' if result['self_compare'] else 'FAIL'
            if paired_only and result['self_compare']:
                result['verdict'] = 'PAIRED_ONLY'
            if (replay_of or replay_pool_of) and result['self_compare']:
                result['verdict'] = 'REPLAY_ONLY'
    if proxy:
        if not rounds or enumeration is None:
            result['errors'].append('--proxy requires calibration rounds and enumeration')
        else:
            try:
                proxy_meta = read_json(proxy / 'run-meta.json')
                if proxy_meta.get('mode') != 'proxy' or proxy_meta.get('synthetic') is not False:
                    result['errors'].append('--proxy must be a real DUT/proxy archive')
            except (OSError, ValueError, TypeError) as error:
                result['errors'].append(f'proxy metadata: {error}')
            result['proxy_compare'] = (pool.score_pair(rounds[0], proxy) if pool else
                                       compare_sides(rounds[0], proxy, enumeration, matrix, scenarios,
                                                     recorded_apparatus=recorded_apparatus))
            if result['proxy_compare']['verdict'] != 'PASS':
                result['verdict'] = 'FAIL'
    if result['errors'] or not result['per_rule_negative'] or not result['same_bad_rejected']:
        result['verdict'] = 'FAIL'
    if any(p['verdict'] == 'REFUSED' for p in result['pairs']) or result.get('proxy_compare', {}).get('verdict') == 'REFUSED':
        result.update(verdict='REFUSED', calibration_eligible=False)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--round', type=Path, action='append', default=[])
    parser.add_argument('--enumeration', type=Path)
    parser.add_argument('--proxy', type=Path)
    parser.add_argument('--matrix', type=Path)
    parser.add_argument('--scenarios', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--replay-of', type=Path, help='Re-evaluate historical archives bound to this original ruler report; never certifies the current apparatus')
    parser.add_argument('--reference-pool', nargs='+', type=Path, help='Required same-day TS-only reference pool (>=20) for calibration')
    parser.add_argument('--paired-only', action='store_true', help='Strict paired diagnostics; cannot certify calibration')
    parser.add_argument('--replay-pool-of', type=Path, help='Offline pool replay bound to an original ruler report; never current calibration')
    args = parser.parse_args()
    result = check(args.round, args.enumeration, args.proxy, args.matrix, args.scenarios, args.replay_of, args.reference_pool,
                   args.paired_only, args.replay_pool_of)
    raw = json.dumps(result, ensure_ascii=False, indent=2) + '\n'
    if args.out:
        args.out.write_text(raw)
    print(raw, end='')
    return 1 if result['verdict'] == 'FAIL' else 2


if __name__ == '__main__':
    raise SystemExit(main())
