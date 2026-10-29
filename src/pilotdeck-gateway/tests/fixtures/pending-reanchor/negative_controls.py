#!/usr/bin/env python3
"""#1048 Python meta-controls. Commit first; never bypass the HEAD source guard."""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
checker = fixtures / 'anchor_selfcheck.py'
registry = fixtures / 'pending_reanchor.py'
suite = fixtures / 'negative_suite.py'
adaptive = fixtures / '_shared/negative_control.py'
tests = fixtures / 'pending-reanchor/test_controls.py'
mutations = [
    ('downgrade-no-op', checker,
     "                    if entry is not None:\n                        print(f'WARN {detail}: {entry.reason()}')",
     "                    if False and entry is not None:\n                        print(f'WARN {detail}: {entry.reason()}')",
     'test_scanner_four_cells', '#1048 downgrade'),
    ('unregistered-downgraded', checker, '                        errors.append(detail)',
     "                        print(f'WARN {detail}')", 'test_scanner_four_cells', '#1048 downgrade'),
    ('scanner-count-no-op', checker, '                    pending_names.add((relative, anchor.name))',
     '                    pass', 'test_scanner_four_cells', '#1048 count'),
    ('partial-anchor-treated-healthy', checker,
     'stale.get(anchor.name, True) and actual == anchor.count', 'actual == anchor.count',
     'test_multiple_anchors_count_once_and_warn_only_when_all_match', '#1048 all anchors must match'),
    ('stale-warning-no-op', checker, '                if unchanged:', '                if False and unchanged:',
     'test_scanner_four_cells', '#1048 warning'),
    ('unknown-registration-ignored', checker, '        if entry not in matched:', '        if False and entry not in matched:',
     'test_unknown_mutation_and_switches_are_errors', '#1048 unknown mutation'),
    ('switch-exempted', checker, '                    if not anchor.text_mutation:',
     '                    if False and not anchor.text_mutation:',
     'test_unknown_mutation_and_switches_are_errors', '#1048 compiled switch'),
    ('unwired-host-exempted', checker, '                    elif not pending.supports_host(relative):',
     '                    elif False and not pending.supports_host(relative):',
     'test_unwired_host_cannot_downgrade', '#1048 unwired host'),
    ('unreviewed-host-enrolled', registry, '    return relative in TEXT_HOSTS', '    return True',
     'test_unwired_host_cannot_downgrade', '#1048 unwired host'),
    ('fixture-boundary-removed', registry,
     "return self.fixture == fixture and self.mutation in ('*', mutation)",
     "return self.mutation in ('*', mutation)", 'test_wildcard_and_fixture_boundary', '#1048 fixture boundary'),
    ('wildcard-disabled', registry, "self.mutation in ('*', mutation)", 'self.mutation == mutation',
     'test_wildcard_and_fixture_boundary', '#1048 wildcard'),
    ('overlap-accepted', registry, '            if any(old.fixture == fixture and (old.mutation == mutation',
     '            if False and any(old.fixture == fixture and (old.mutation == mutation',
     'test_bad_registry_fails_closed', '#1048 malformed registration'),
    ('cases-head-guard-no-op', suite,
     'with mutating_sources(root, paths, baseline=baseline) as originals:',
     "with __import__('contextlib').nullcontext({}) as originals:",
     'test_skip_cannot_bypass_head_guard', '#1048 HEAD guard retained'),
    ('matrix-head-guard-no-op', suite, 'with mutating_sources(root, guarded) as originals:',
     "with __import__('contextlib').nullcontext({}) as originals:",
     'test_matrix_skips_without_compilation_and_compile_only_fails', '#1048 matrix HEAD guard retained'),
    ('adaptive-head-guard-no-op', adaptive, 'with mutating_sources(REPO, paths) as snapshots:',
     "with __import__('contextlib').nullcontext({}) as snapshots:",
     'test_adaptive_skips_without_build_or_mutation', '#1048 adaptive HEAD guard retained'),
    ('host-match-no-op', registry,
     "    return next((entry for entry in entries if entry.mutation in ('*', name)), None)",
     '    return None', 'test_host_four_cells', '#1048 skip status'),
    ('skip-count-omitted', registry, "        result['pending-reanchor'] = skipped",
     "        result['pending-reanchor'] = 0", 'test_mixed_suite_continues_and_fails', '#1048 mixed counts'),
    ('cases-skip-killed', suite, '                    record(case, pending.SKIPPED, entry.reason())',
     "                    record(case, 'KILLED', entry.reason())", 'test_host_four_cells', '#1048 skip status'),
    ('cases-skip-green', suite, "return int(not results or any(row['status'] != 'KILLED' for row in results))",
     "return int(not results or any(row['status'] not in ('KILLED', pending.SKIPPED) for row in results))",
     'test_mixed_suite_continues_and_fails', '#1048 mixed exit'),
    ('matrix-skip-killed', suite, '                    item.update(status=pending.SKIPPED, reason=entry.reason())',
     "                    item.update(status='KILLED', reason=entry.reason())",
     'test_matrix_skips_without_compilation_and_compile_only_fails', '#1048 matrix skip exit'),
    ('matrix-skip-green', suite, "return int(any(row['status'] != success for row in results))",
     "return int(any(row['status'] not in (success, pending.SKIPPED) for row in results))",
     'test_matrix_skips_without_compilation_and_compile_only_fails', '#1048 matrix skip exit'),
    ('adaptive-skip-killed', adaptive, '                        item.update(status=pending.SKIPPED, reason=entry.reason())',
     "                        item.update(status='KILLED', reason=entry.reason())",
     'test_adaptive_skips_without_build_or_mutation', '#1048 adaptive skip exit'),
    ('adaptive-skip-green', adaptive, "return int(not records or any(row['status'] != 'KILLED' for row in records))",
     "return int(not records or any(row['status'] not in ('KILLED', pending.SKIPPED) for row in records))",
     'test_adaptive_skips_without_build_or_mutation', '#1048 adaptive skip exit'),
]


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT_DIR [mutation ...]')
    requested = sys.argv[2:]
    selected = [row for row in mutations if not requested or row[0] in requested]
    if not selected or (requested and len(selected) != len(requested)):
        raise SystemExit('unknown/duplicate mutation')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    records = []

    def run(label, test):
        result = run_owned([sys.executable, '-B', str(tests), 'Controls.' + test],
                           cwd=root, env=dict(os.environ, PYTHONDONTWRITEBYTECODE='1'), timeout=30)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    # Python-only mutations: compile() proves syntax; no cargo/slot is consumed.
    # wait_for_slot is imported to keep the shared runner contract explicit.
    for name, path, before, after, test, marker in selected:
        row = dict(name=name, status='INVALID', reason='not run', compiled=False, restoredPass=False)
        try:
            with mutating_sources(root, [checker, registry, suite, adaptive, tests,
                                        Path(__file__).resolve()]) as originals:
                original = originals[path].decode()
                if original.count(before) != 1:
                    raise ValueError(f'#1048 stale anchor: {name}, count={original.count(before)}')
                positive = run(name + '-positive', test)
                if positive.returncode != 0:
                    raise RuntimeError('#1048 bound positive unittest failed')
                try:
                    changed = original.replace(before, after, 1)
                    compile(changed, str(path), 'exec')
                    row['compiled'] = True
                    path.write_text(changed)
                    result = run(name + '-negative', test)
                    log = result.stdout + result.stderr
                    failure = log.partition(f'FAIL: {test} (')[2].split('\n====')[0]
                    if result.returncode == 1 and 'AssertionError:' in failure and marker in failure:
                        row.update(status='KILLED', reason='bound #1048 assertion failed')
                    elif result.returncode == 0:
                        row.update(status='SURVIVED', reason='bound unittest still passed')
                    else:
                        row.update(reason='unrelated failure or missing bound assertion')
                finally:
                    path.write_bytes(originals[path])
                restored = run(name + '-restored', test)
                row['restoredPass'] = restored.returncode == 0
                if not row['restoredPass']:
                    row.update(status='INVALID', reason='restored unittest failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        records.append(row)
        print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    counts = {status: sum(row['status'] == status for row in records)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': records, 'counts': counts}, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    return int(not records or any(row['status'] != 'KILLED' for row in records))


if __name__ == '__main__':
    sys.exit(main())
