#!/usr/bin/env python3
"""Prove the sample/full verdict contract by mutating the runner itself.

Run from the repository root after the work is committed (mutating_sources
refuses to mutate a runner that differs from HEAD):

    python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-capture-turn/negative_meta.py \
        _tmp/20261026-issue-786/fix-r1/negative-meta

#786 fix-r1: the sample mode must never be readable as a full pass. Each
mutant below reverts one half of that contract inside ``negative.py`` — the
exit-code split or one summary field — and the bound ``test_negative.py`` unit
test must fail with its anchored message. A compile (import) failure of the
mutated runner, another test failing, or a failure without the anchored
message is INVALID evidence, not a kill. Every mutant is classified before the
summary; a non-KILLED mutant never aborts the loop, and any non-KILLED result
exits 1. No cargo is involved, so no cargo slot is taken.
"""
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned

runner = Path(__file__).resolve().with_name('negative.py')
tests = Path(__file__).resolve().with_name('test_negative.py')
# (name, path, before, after, test, anchor)
mutants = [
    ('sample-exit-zero', runner,
     "    return SAMPLE_EXIT if mode == 'sample' else 0",
     '    return 0',
     'SampleModeVerdict.test_clean_sample_is_not_full_pass',
     '#786 a clean sample run must not exit with the full-pass code'),
    ('sample-mode-erased', runner,
     '        mode=mode,\n        selected=selected,',
     "        mode='full',\n        selected=selected,",
     'SampleModeVerdict.test_summary_marks_sample',
     '#786 the sample summary must record mode=sample'),
    ('sample-selected-erased', runner,
     '        selected=selected,',
     '        selected=[],',
     'SampleModeVerdict.test_summary_marks_sample',
     '#786 the sample summary must name the selected mutants'),
]


def classify(mutant, code, raw):
    """Killed only by the bound unit test failing at its anchored assertion."""
    if code == 0:
        return 'SURVIVED', 'the bound unit test passed on the mutated runner'
    if 'FAILED (failures=' not in raw:
        return 'INVALID', 'the mutated runner did not fail as a unittest assertion'
    if f'FAIL: {mutant[4].rsplit(".", 1)[-1]}' not in raw or f'.{mutant[4]}' not in raw:
        return 'INVALID', 'another test failed, not the bound one'
    if mutant[5] not in raw:
        return 'INVALID', 'missing the bound assertion message'
    return 'KILLED', 'the bound unit test failed at its anchored assertion'


def main():
    root = Path(__file__).resolve().parents[5]
    output = new_output_directory(sys.argv[1])
    original = runner.read_text()
    results = []
    failures = []

    def run(name, test):
        with (output / f'{name}.log').open('w') as log:
            result = run_owned(
                [sys.executable, '-I', str(tests), test] if test
                else [sys.executable, '-I', str(tests)],
                timeout=120,
            )
            log.write(result.stdout + result.stderr)
            return result.returncode, result.stdout + result.stderr

    with mutating_sources(root, [runner]) as snapshots:
        assert snapshots[runner].decode() == original, 'runner bytes moved under us'
        assert run('positive', '')[0] == 0, 'the unit suite must pass before mutating'
        try:
            for name, path, before, after, test, anchor in mutants:
                assert original.count(before) == 1, (
                    name, 'mutation anchor must be unique')
                mutated = original.replace(before, after, 1)
                path.write_text(mutated)
                try:
                    try:
                        code, raw = run(name, test)
                        status, reason = classify(
                            (name, path, before, after, test, anchor), code, raw)
                    except Exception as error:
                        # Account for every mutant even when its runner fails.
                        code, status = None, 'INVALID'
                        reason = f'{type(error).__name__}: {error}'
                    results.append(dict(
                        name=name, test=test, anchor=anchor,
                        exit=code, status=status, reason=reason))
                    print(f'{name}: {status}, exit={code}', flush=True)
                    if status != 'KILLED':
                        failures.append(name)
                finally:
                    assert path.read_text() == mutated, 'another writer changed the mutant'
                    path.write_text(original)
        finally:
            assert runner.read_text() == original, 'the mutated runner was not restored'
        restored_ok, _ = run('restored', '')
    payload = dict(
        results=results,
        failures=failures,
        counts={state: sum(row['status'] == state for row in results)
                for state in ('KILLED', 'SURVIVED', 'INVALID')},
        restoredPass=restored_ok,
        beforeSha256=hashlib.sha256(original.encode()).hexdigest(),
        afterSha256=hashlib.sha256(runner.read_bytes()).hexdigest(),
    )
    pending = output / 'summary.json.tmp'
    pending.write_text(json.dumps(payload, indent=2) + '\n')
    pending.replace(output / 'summary.json')
    print('summary: ' + ', '.join(
        f'{state}={count}' for state, count in payload['counts'].items()), flush=True)
    if failures or not restored_ok:
        print(f'NOT KILLED: {", ".join(failures) or "<none>"}; '
              f'restored_passed={restored_ok}', file=sys.stderr)
        sys.exit(1)


if __name__ == '__main__':
    main()
