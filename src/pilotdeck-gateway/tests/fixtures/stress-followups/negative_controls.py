#!/usr/bin/env python3
"""#1109: bounded rerun children must not re-verify the optional baseline.

Every mutation reverts one piece of the #1109 rewrite and must make the
bound parent test fail; restoring the bytes must make it pass again. The
two re-verification rows reproduce the #978 flake itself (children of a
bounded parent re-running the 650/728-source `git show` probe table under
load), so they are judged under concurrent direct-binary stress, on the raw
``test result:`` lines only — no retry, no ``-q``, no artificial CPU load:
the test binaries themselves are the load, the way #978's stress-bin.sh
applied it. The two bound rows and the receipt row are deterministic kills
that only need one run. The #978 regression is itself load-proportional
(observed at load 39-106; an idle host needs the stress batch to supply the
load), so a stress row is KILLED when at least half its runs fail at the
bound message and no run fails anywhere else — a minority kill, or any
failure outside the bound message, keeps the row SURVIVED.

Expected runtime: ~20 min for the four deterministic rows, ~10 min for the
transcript stress row (8x) and ~25 min for the worker stress row (48x). Run after committing the
candidate (``mutating_sources`` proves every byte against ``--baseline``; a
pre-commit full run can pass the SHA printed by ``git stash create``, which
moves no ref). Never bypass the HEAD guard. A run with zero KILLED rows
exits nonzero; every row that is not KILLED (INVALID/SURVIVED) is reported
with its reason before the nonzero exit — the runner never stops at the
first failure.
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

# Direct-binary stress shape (calibrated 2026-10-30, 10 CPUs; ambient load
# 5-83 across the runs): transcript 6x: reverted 6/6 killed, fixed 6/6 passed
# -> ship 8x; worker 24x killed 17/24 on an idle host (24/24 at load 46-83)
# -> ship 48x so the batch itself supplies #978's load, with the majority
# criterion below absorbing the rest of the load proportionality.
TRANSCRIPT_STRESS = 8
WORKER_STRESS = 48

TRANSCRIPT_BINARY = 'transcript_resolve'
WORKER_TEST = 'exit_statuses_with_inherited_ignored_and_blocked_signals'
WORKER_BINARY = 'background_tasks'


def locate_binary(target_dir, crate):
    """Newest executable test binary for `crate`, as stress-bin.sh picks it."""
    deps = target_dir / 'debug' / 'deps'
    candidates = sorted(
        (path.stat().st_mtime, path)
        for path in deps.glob(f'{crate}-*')
        if path.is_file() and path.stat().st_mode & 0o111 and '.' not in path.name
    )
    if not candidates:
        raise RuntimeError(f'no {crate} test binary under {deps}; run cargo test --no-run')
    return candidates[-1][1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='fresh evidence directory')
    parser.add_argument('mutations', nargs='*')
    parser.add_argument('--baseline', default='HEAD',
                        help='tree state the mutated files must match')
    parser.add_argument('--transcript-stress', type=int, default=TRANSCRIPT_STRESS)
    parser.add_argument('--worker-stress', type=int, default=WORKER_STRESS)
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    if not os.environ.get('PDRS_BASELINE'):
        parser.error('PDRS_BASELINE must point at the fixed TS checkout')
    target_dir = Path(os.environ['CARGO_TARGET_DIR']).resolve()
    root = Path(__file__).resolve().parents[5]
    baseline_path = Path(os.environ['PDRS_BASELINE'])
    transcript_source = root / 'src/pilotdeck-gateway/tests/transcript_resolve.rs'
    baseline_source = root / 'src/pilotdeck-gateway/tests/common/baseline.rs'
    signals_source = root / 'src/pilotdeck-gateway/tests/common/child_signals.rs'
    tasks_source = root / 'src/pilotdeck-gateway/tests/background_tasks.rs'
    env = os.environ.copy()
    env['CARGO_PROFILE_DEV_DEBUG'] = 'line-tables-only'
    env['CARGO_PROFILE_TEST_DEBUG'] = 'line-tables-only'
    mutations = [
        # The regression itself: children of the bounded parent re-run the
        # git probe table; under load the 30 s bound kills them mid-verification.
        ('transcript-child-reverification', transcript_source,
         '                // #1109: the parent verified the baseline once (above); the\n'
         '                // child must not re-verify it under the 30 s bound below.\n'
         '                .env_remove("PDRS_BASELINE")\n',
         '',
         (TRANSCRIPT_BINARY, []), args.transcript_stress,
         '#1109 matrix child timed out'),
        # The verification phase must be bounded and must fail loudly.
        ('transcript-verify-bound', baseline_source,
         'const VERIFY_BOUND: Duration = Duration::from_secs(300);',
         'const VERIFY_BOUND: Duration = Duration::from_secs(0);',
         (TRANSCRIPT_BINARY, ['--exact', 'relative_base_normal_cwd']), 1,
         '#1109 baseline verification exceeded'),
        ('worker-reverification', signals_source,
         '            // #1109: the calling parent verified the committed oracle once;\n'
         '            // the worker must not re-run the git probe table under the\n'
         '            // bounds below (the #978 scan killed it mid-verification at\n'
         '            // load 90-106).\n'
         '            .env_remove("PDRS_BASELINE")\n',
         '',
         (WORKER_BINARY, ['--exact', WORKER_TEST]), args.worker_stress,
         'signal-injected worker inherited_exit_statuses_worker silent for 30s'),
        # Duration::from_secs(0) debug-formats as 0ns, not 0s.
        ('worker-silent-window', signals_source,
         'let silent_bound = Duration::from_secs(30);',
         'let silent_bound = Duration::from_secs(0);',
         (WORKER_BINARY, ['--exact', WORKER_TEST]), 1,
         'silent for 0ns within the 300s total bound'),
        ('worker-total-bound', signals_source,
         'let total_bound = Duration::from_secs(300);',
         'let total_bound = Duration::from_secs(0);',
         (WORKER_BINARY, ['--exact', WORKER_TEST]), 1,
         'silent for 30s within the 0ns total bound'),
        # The receipts must flow: severing them degrades the bounds back to
        # wall clock alone, and the parent must reject that.
        ('progress-receipt-severed', tasks_source,
         '        // #1109: progress receipt. Each step is real replay work; printing\n'
         '        // it lets the signal-injected worker\'s parent distinguish a slow\n'
         '        // but progressing run from a hung one instead of bounding the real\n'
         '        // work by wall clock alone.\n'
         '        println!("BACKGROUND_SEQUENCE_STEP {name} {index}");\n',
         '',
         (WORKER_BINARY, ['--exact', WORKER_TEST]), 1,
         '#1109 worker emitted no sequence progress receipts'),
    ]
    if args.mutations:
        selected = set(args.mutations)
        if not selected <= {row[0] for row in mutations}:
            parser.error('unknown mutation name')
        mutations = [row for row in mutations if row[0] in selected]
    args.output.mkdir(parents=True, exist_ok=False)

    def save(name, text):
        with (args.output / name).open('x') as stream:
            stream.write(text)

    def build(crate, label):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=10)
        save(f'{label}.slots.log', slots.stdout + slots.stderr)
        # No -q: a quiet cargo hides the compile failure that makes a row
        # INVALID instead of KILLED (#1109 rule 4: compile-check every mutant).
        result = run_owned([
            'cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway',
            '--test', crate, '--no-run',
        ], cwd=root, env=env, timeout=900)
        log = result.stdout + result.stderr
        save(f'{label}.log', log)
        save(f'{label}.exit', f'{result.returncode}\n')
        return result.returncode == 0

    def stress(crate, extra, count, label):
        """`count` concurrent direct-binary runs; returns [(exit code, log)]."""
        binary = locate_binary(target_dir, crate)
        # Plain children of this runner, no new session: a killed runner takes
        # the whole stress batch with it, and the per-run log files (not
        # pipes) keep a descendant that outlives its run from holding an fd.
        runs = []
        for index in range(count):
            home = args.output / f'{label}-run-{index}.home'
            home.mkdir()
            run_env = dict(os.environ)
            run_env.update(HOME=str(home), PILOT_HOME=str(home / '.pilotdeck'),
                           PDRS_BASELINE=str(baseline_path))
            log = args.output / f'{label}-run-{index}.log'
            with log.open('w') as sink:
                process = subprocess.Popen(
                    [str(binary), *extra], cwd=root / 'src/pilotdeck-gateway', env=run_env,
                    stdout=sink, stderr=subprocess.STDOUT)
            runs.append((process, log))
        return [(process.wait(), log.read_text()) for process, log in runs]

    def judged(crate, extra, count, label, binding):
        runs = stress(crate, extra, count, label)
        passed = sum(code == 0 and 'test result: ok.' in log for code, log in runs)
        killed = sum(code != 0 and 'test result: FAILED' in log and binding in log
                     for code, log in runs)
        return runs, passed, killed

    summary = []
    for name, source, old, new, (crate, extra), count, binding in mutations:
        record = {'name': name, 'status': 'INVALID', 'reason': ''}
        summary.append(record)
        try:
            with mutating_sources(root, [source], baseline=args.baseline) as snapshots:
                content = snapshots[source].decode()
                if content.count(old) != 1:
                    # A stale anchor is a script defect, never a SURVIVED row.
                    record['reason'] = f'anchor count {content.count(old)} != 1'
                    continue
                if not build(crate, f'{name}-positive-build'):
                    record['reason'] = 'positive build failed'
                    continue
                runs, passed, killed = judged(
                    crate, extra, count, f'{name}-positive', binding)
                if passed != len(runs):
                    record['reason'] = f'positive control: {passed}/{len(runs)} runs passed'
                    continue
                changed = content.replace(old, new, 1)
                save(f'{name}.patch', ''.join(difflib.unified_diff(
                    content.splitlines(True), changed.splitlines(True),
                    fromfile=str(source), tofile=str(source))))
                source.write_text(changed)
                if not build(crate, f'{name}-negative-build'):
                    record['reason'] = 'mutant does not compile'
                    continue
                runs, passed, killed = judged(
                    crate, extra, count, f'{name}-negative', binding)
                ambiguous = len(runs) - killed - passed
                record.update(runs=len(runs), positive=passed, negativeKilled=killed,
                              sourceSha256=hashlib.sha256(
                                  snapshots[source]).hexdigest())
                if ambiguous:
                    record['status'] = 'INVALID'
                    record['reason'] = (f'{ambiguous}/{len(runs)} runs failed outside '
                                        'the bound message')
                elif killed == len(runs) or (count > 1 and killed * 2 >= len(runs)):
                    record['status'] = 'KILLED'
                    record['reason'] = (f'{killed}/{len(runs)} runs failed at the bound '
                                        f'message ({passed} passed under load)')
                else:
                    record['status'] = 'SURVIVED'
                    record['reason'] = (f'{killed}/{len(runs)} runs failed at the bound '
                                        f'message, {passed} passed')
            # mutating_sources's finally restored and byte-checked the source;
            # this rerun proves the restored tree passes again.
            with mutating_sources(root, [source], baseline=args.baseline):
                if not build(crate, f'{name}-restored-build'):
                    record['status'] = 'INVALID'
                    record['reason'] = 'restored build failed'
                    continue
                runs, passed, killed = judged(
                    crate, extra, 1, f'{name}-restored', binding)
                if passed != 1:
                    record['status'] = 'INVALID'
                    record['reason'] = f'restored control: {passed}/1 runs passed'
        except BaseException as error:  # noqa: BLE001 - recorded, never silent
            record['status'] = 'INVALID'
            record['reason'] = f'{type(error).__name__}: {error}'
        finally:
            save(f'{name}.json', json.dumps(record, indent=2) + '\n')
            print(f"{name}: {record['status']}: {record['reason']}", flush=True)
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    save('summary.json', json.dumps({'controls': summary, 'counts': counts}, indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    # Never stop at the first failure, never exit 0 while any row is not
    # KILLED, and never exit 0 from a run that killed nothing.
    return int(not summary or counts['KILLED'] == 0
               or any(row['status'] != 'KILLED' for row in summary))


if __name__ == '__main__':
    sys.exit(main())
