"""Run every selected control without treating harness failures as kills."""
from dataclasses import dataclass
import json
import subprocess

from negative_process import mutating_sources, run_owned, wait_for_slot


@dataclass
class Case:
    mutation: str
    path: object
    old: str
    new: str
    test: str
    assertion: object
    scope: object = None


def failed_tests(log, prefix):
    """Failed names from libtest's authoritative ``failures:`` list.

    With ``--nocapture`` (or parallel workers) a test's own output can land
    between the test name and its verdict, so ``... FAILED`` must not be
    required to stay adjacent to the name (#879). The indented list after the
    run is the one spelling libtest guarantees.
    """
    return {line.strip() for line in log.splitlines()
            if line.startswith(f'    {prefix}')}


def case_name(test):
    """Matrix cells name their dedicated assertion after the dashed case id."""
    return test.removesuffix('_cwd').replace('_', '-')


def cell_hits(log, prefix, tests, marker, case_name):
    """Split declared cells into dedicated-marker kills and unrelated panics.

    A declared cell counts as hit only when the ``failures:`` list names it and
    the log carries its dedicated ``<marker> <case>`` message; a declared cell
    failing without that message is unrelated, never a kill.
    """
    failed = failed_tests(log, prefix)
    hit, unrelated = [], []
    for test in tests:
        if f'{prefix}{test}' not in failed:
            continue
        (hit if f'{marker} {case_name(test)}' in log else unrelated).append(test)
    return hit, unrelated


def passed(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0 and f'test {test} ... ok' in log
            and 'test result: ok. 1 passed;' in log)


def classify(result, case):
    log = result.stdout + result.stderr
    if passed(result, case.test):
        return 'SURVIVED', 'bound test still passed'
    anchors = case.assertion if isinstance(case.assertion, list) else [case.assertion]
    if (result.returncode != 0 and 'test result: FAILED' in log
            and f'test {case.test} ... FAILED' in log and 'panicked at' in log
            and any(anchor in log for anchor in anchors)):
        return 'KILLED', 'bound test failed at its assertion'
    return 'INVALID', 'missing bound runtime failure (build error, wrong panic, or missing test)'


def run_cases(root, cases, out, env, mutate, *, timeout=900, baseline='HEAD'):
    """Return an exit code; each case must pass before and after mutation."""
    results = []
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']

    def run(label, test):
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
            (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            # Exact binding still runs one test, including explicit TS/regeneration guards.
            result = run_owned(command + [test, '--', '--exact', '--include-ignored', '--nocapture'],
                               cwd=root, env=env, timeout=timeout)
            (out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for value in (error.stdout, error.stderr):
                    raw += value.decode(errors='replace') if isinstance(value, bytes) else value or ''
            with (out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(case, status, reason):
        results.append({'mutation': case.mutation, 'test': case.test,
                        'assertion': case.assertion, 'status': status, 'reason': reason})
        counts = {state: sum(r['status'] == state for r in results)
                  for state in ('KILLED', 'SURVIVED', 'INVALID')}
        (out / 'summary.json').write_text(json.dumps(
            {'controls': results, 'counts': counts}, indent=2) + '\n')
        print(f'{case.mutation}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, list(dict.fromkeys(c.path for c in cases)),
                              baseline=baseline) as originals:
            for case in cases:
                status, reason, stage = 'INVALID', '', 'anchor'
                try:
                    changed = mutate(originals[case.path].decode(), case.scope,
                                     [(case.old, case.new)])
                    stage = 'positive'
                    if not passed(run(case.mutation + '-positive', case.test), case.test):
                        reason = 'positive control did not pass the bound test'
                    else:
                        stage = 'negative'
                        case.path.write_text(changed)
                        status, reason = classify(run(case.mutation + '-negative', case.test), case)
                except Exception as error:
                    status, reason = 'INVALID', f'{stage}: {type(error).__name__}: {error}'
                finally:
                    # Restore before every successor, also on timeout or bad anchors.
                    # The enclosing guard independently restores and verifies on exit.
                    case.path.write_bytes(originals[case.path])
                    if case.path.read_bytes() != originals[case.path]:
                        raise RuntimeError(f'byte restoration failed: {case.path}')
                try:
                    if not passed(run(case.mutation + '-restored', case.test), case.test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
                record(case, status, reason)
    except Exception as error:
        # A baseline/lock/restoration failure makes further mutation unsafe.
        # Still account for every selected case and never leave an all-green summary.
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for case in cases[len(results):]:
            record(case, 'INVALID', reason)
        if len(results) == len(cases):
            (out / 'summary.json').write_text(json.dumps({
                'controls': results,
                'counts': {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(results)},
            }, indent=2) + '\n')
    counts = {state: sum(r['status'] == state for r in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))
