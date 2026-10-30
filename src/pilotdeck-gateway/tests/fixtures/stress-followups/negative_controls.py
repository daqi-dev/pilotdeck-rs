#!/usr/bin/env python3
"""#1109: bounded rerun children must not re-verify the optional baseline.

Every row is one deterministic mutation of the #1109 rewrite (or of the
optional-baseline guard fix-r1 hardened): applying it must make the bound
named libtest fail at its own stable first-line assertion, and restoring
the bytes must make it pass again. Verdicts come exclusively from the
audited M6 contract, ``_negkit/contract.py`` (#1109 fix-r2, review
direction): a row is KILLED only by a normally exited 101 with exactly one
named FAILED test, one same-named panic header and the row's assertion as
the panic's first payload line. This runner owns no classifier of its own.

The three rows that used to need concurrent stress batches (children
re-running the 650/728-source git probe table, the retracted flat 30 s
total bound) are deterministic now: the ``PDRS_VERIFY_DELAY_MS`` /
``PDRS_SEQUENCE_STEP_DELAY_MS`` test hooks inject a wall-clock delay per
git probe / replay step, so the retracted 30 s bounds fail with no ambient
load at all. The hooks are never set in normal runs. The ``--*-stress``
counts, ``STRESS_FLOOR`` and the majority criterion are gone with them;
every row runs exactly once (#1109 fix-r2, review direction: a line that
only fails under load is not a negative control).

Every subprocess, cargo builds included, runs through
``negative_process.run_owned``: each invocation owns a fresh session, a
timeout kills the whole group, and collection after that kill stays
bounded — including the exited-leader case. The runner keeps no Popen
batch of its own (#1109 fix-r2, review P1-1).

Expected runtime: ~4 min for the seven fast rows, ~3 min each for the three
delay rows. Run after committing the candidate (``mutating_sources`` proves
every byte against ``--baseline``; a pre-commit full run can pass the SHA
printed by ``git stash create``, which moves no ref). Never bypass the HEAD
guard.
"""
import argparse
import difflib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / '_negkit'))
from negative_process import exclusive_write, mutating_sources, run_owned, wait_for_slot
import contract

TRANSCRIPT_BINARY = 'transcript_resolve'
TRANSCRIPT_TEST = 'relative_base_normal_cwd'
WORKER_BINARY = 'background_tasks'
WORKER_TEST = 'exit_statuses_with_inherited_ignored_and_blocked_signals'
# Per-source / per-step delays that make the retracted 30 s bounds fail
# deterministically: 650 sources x 50 ms >= 33 s clears the 30 s child and
# silent bounds with margin, while the progress-aware 300 s total bound and
# the receipt-pacing 30 s silent bound still hold on the fixed side. The
# step stretch may only start after the golden replay's own await*
# synchronization (exit-statuses steps 0-10 carry a recorded 0.3 s
# real-time window; stretching them would change the replayed outcomes, not
# just their duration), so it covers steps 11-15: 5 x 6500 ms >= 32 s.
VERIFY_DELAY_MS = '50'
STEP_DELAY_MS = '6500'
STEP_DELAY_FROM = '11'
# One direct test-binary run may verify a 650-source oracle under a delay
# and then wait out a bound; builds get their own generous budget.
RUN_TIMEOUT = 600
BUILD_TIMEOUT = 900

# ---- mutation anchors ------------------------------------------------------

TRANSCRIPT_REMOVE = (
    '                // #1109: the parent verified the baseline once (above); the\n'
    '                // child must not re-verify it under the 30 s bound below.\n'
    '                .env_remove("PDRS_BASELINE")\n')
WORKER_REMOVE = (
    '            // #1109: the calling parent verified the committed oracle once;\n'
    '            // the worker must not re-run the git probe table under the\n'
    '            // bounds below (the #978 scan killed it mid-verification at\n'
    '            // load 90-106). Contract: a worker launched here never needs\n'
    '            // PDRS_BASELINE, so a parent whose worker replays a committed\n'
    '            // oracle must verify that oracle itself before launching (as\n'
    '            // background_tasks and transcript_resolve do).\n'
    '            .env_remove("PDRS_BASELINE")\n')
RECEIPT_PRINT = (
    "        // #1109: progress receipt. Each step is real replay work; printing\n"
    "        // it lets the signal-injected worker's parent distinguish a slow\n"
    '        // but progressing run from a hung one instead of bounding the real\n'
    '        // work by wall clock alone.\n'
    '        println!("BACKGROUND_SEQUENCE_STEP {name} {index}");\n')
PARENT_VERIFY = (
    '    // #1109: verify the committed oracle here, once for this process,\n'
    '    // before launching the worker (which runs without PDRS_BASELINE).\n'
    '    let _ = oracle();\n')
STATIC_CHECK_ARM = (
    '        None => check(&oracle, None),')
SWALLOWED_ARM = (
    '            Ok(Err(message)) => panic!("{message}"),')
COMMIT_LINE = (
    '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"')
BAD_COMMIT_LINE = (
    '"sourceCommit": "0000000000000000000000000000000000000001"')
HASHES_OPEN = (
    '"sourceSha256": {')
HASHES_EMPTIED = (
    '"sourceSha256": {}, "unusedSha256": {')
HASHED_SOURCE = (
    '"src/adapters/channel/protocol/ChannelCommandRegistry.ts": '
    '"e93b583e0a5323bd35ca93c9568e484f876daa367057a2d423aabdc825867c55"')
TAMPERED_HASH = (
    '"src/adapters/channel/protocol/ChannelCommandRegistry.ts": '
    '"f93b583e0a5323bd35ca93c9568e484f876daa367057a2d423aabdc825867c55"')


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
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[5]
    transcript_source = root / 'src/pilotdeck-gateway/tests/transcript_resolve.rs'
    baseline_source = root / 'src/pilotdeck-gateway/tests/common/baseline.rs'
    signals_source = root / 'src/pilotdeck-gateway/tests/common/child_signals.rs'
    tasks_source = root / 'src/pilotdeck-gateway/tests/background_tasks.rs'
    tasks_fixture = root / 'src/pilotdeck-gateway/tests/fixtures/background-tasks/oracle.json'
    mutations = [
        # The regression itself: children of the bounded parent re-run the
        # git probe table; the injected per-source delay makes that table
        # deterministically outgrow the parent's 30 s child bound.
        dict(name='transcript-child-reverification',
             changes=[(transcript_source, TRANSCRIPT_REMOVE, '')],
             crate=TRANSCRIPT_BINARY, test=TRANSCRIPT_TEST,
             assertion='#1109 matrix child timed out (30 s bound)',
             env={'PDRS_VERIFY_DELAY_MS': VERIFY_DELAY_MS}, with_baseline=True),
        # The verification phase must be bounded and must fail loudly.
        # Duration::from_secs(0) debug-formats as 0ns, not 0s (fix-r3,
        # review P1-1: the 0s binding could never match the panic payload).
        dict(name='transcript-verify-bound',
             changes=[(baseline_source,
                       'const VERIFY_BOUND: Duration = Duration::from_secs(300);',
                       'const VERIFY_BOUND: Duration = Duration::from_secs(0);')],
             crate=TRANSCRIPT_BINARY, test=TRANSCRIPT_TEST,
             assertion='#1109 baseline verification exceeded the 0ns bound without finishing',
             env={}, with_baseline=True),
        # Same regression on the signal-injected worker: without the env
        # removal the worker re-verifies, and the delayed probe table keeps
        # it silent past the parent's 30 s silent bound.
        dict(name='worker-reverification',
             changes=[(signals_source, WORKER_REMOVE, '')],
             crate=WORKER_BINARY, test=WORKER_TEST,
             assertion=('#1109 signal-injected worker inherited_exit_statuses_worker '
                        'silent for 30s within the 300s total bound'),
             env={'PDRS_VERIFY_DELAY_MS': VERIFY_DELAY_MS}, with_baseline=True),
        # Duration::from_secs(0) debug-formats as 0ns, not 0s.
        dict(name='worker-silent-window',
             changes=[(signals_source,
                       'let silent_bound = Duration::from_secs(30);',
                       'let silent_bound = Duration::from_secs(0);')],
             crate=WORKER_BINARY, test=WORKER_TEST,
             assertion=('#1109 signal-injected worker inherited_exit_statuses_worker '
                        'silent for 0ns within the 300s total bound'),
             env={}, with_baseline=True),
        dict(name='worker-total-bound',
             changes=[(signals_source,
                       'let total_bound = Duration::from_secs(300);',
                       'let total_bound = Duration::from_secs(0);')],
             crate=WORKER_BINARY, test=WORKER_TEST,
             assertion=('#1109 signal-injected worker inherited_exit_statuses_worker '
                        'exceeded the 0ns total bound without finishing'),
             env={}, with_baseline=True),
        # Retracting the progress-aware total bound back to the pre-#1109
        # flat 30 s must reproduce the #978 kill: the injected per-step
        # delay makes the progressing replay deterministically outgrow it.
        dict(name='worker-flat-30s',
             changes=[(signals_source,
                       'let total_bound = Duration::from_secs(300);',
                       'let total_bound = Duration::from_secs(30);')],
             crate=WORKER_BINARY, test=WORKER_TEST,
             assertion=('#1109 signal-injected worker inherited_exit_statuses_worker '
                        'exceeded the 30s total bound without finishing'),
             env={'PDRS_SEQUENCE_STEP_DELAY_MS': STEP_DELAY_MS,
                  'PDRS_SEQUENCE_STEP_DELAY_FROM': STEP_DELAY_FROM}, with_baseline=True),
        # The receipts must flow: severing them degrades the bounds back to
        # wall clock alone, and the parent must reject that.
        dict(name='progress-receipt-severed',
             changes=[(tasks_source, RECEIPT_PRINT, '')],
             crate=WORKER_BINARY, test=WORKER_TEST,
             assertion='#1109 worker emitted no sequence progress receipts',
             env={}, with_baseline=True),
        # fix-r1 (review P0): without PDRS_BASELINE the static provenance
        # checks must still fire. A bad commit must fail the default run.
        # The retraction severs the new unconditional static check and must
        # re-open the hole (the same corruption then passes).
        dict(name='parent-static-commit',
             changes=[(tasks_fixture, COMMIT_LINE, BAD_COMMIT_LINE)],
             crate=WORKER_BINARY, test=WORKER_TEST,
             assertion=('#1109 oracle baseline commit mismatch: recorded '
                        '0000000000000000000000000000000000000001 expected '
                        'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'),
             env={}, with_baseline=False,
             retractions=[dict(name='static-commit-check-severed',
                               changes=[(baseline_source, STATIC_CHECK_ARM,
                                         '        None => {}')])]),
        # fix-r1 (review P0): an emptied hash table must fail the default run.
        dict(name='parent-static-empty-hashes',
             changes=[(tasks_fixture, HASHES_OPEN, HASHES_EMPTIED)],
             crate=WORKER_BINARY, test=WORKER_TEST,
             assertion='#1109 oracle source hashes must not be empty',
             env={}, with_baseline=False,
             retractions=[dict(name='static-empty-hashes-check-severed',
                               changes=[(baseline_source, STATIC_CHECK_ARM,
                                         '        None => {}')])]),
        # fix-r1 (review P1): a well-formed but wrong recorded hash is
        # invisible to the static checks; the parent's bounded git
        # verification must catch it. Both retractions must re-open the
        # hole: deleting the parent's verification, and ignoring the
        # reported verification verdict, each make the same corruption pass.
        dict(name='parent-hash-mismatch',
             changes=[(tasks_fixture, HASHED_SOURCE, TAMPERED_HASH)],
             crate=WORKER_BINARY, test=WORKER_TEST,
             assertion=('#1109 oracle hash for '
                        'src/adapters/channel/protocol/ChannelCommandRegistry.ts '
                        'does not match cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'),
             env={}, with_baseline=True,
             retractions=[dict(name='parent-verification-severed',
                               changes=[(tasks_source, PARENT_VERIFY, '')]),
                          dict(name='verification-panic-swallowed',
                               changes=[(baseline_source, SWALLOWED_ARM,
                                         '            Ok(Err(_message)) => {}')])]),
    ]
    if args.mutations and not set(args.mutations) <= {row['name'] for row in mutations}:
        parser.error(f"unknown mutation name; known: {sorted(row['name'] for row in mutations)}")
    rows = mutations
    # contract.environment validates the rest (absolute CARGO_TARGET_DIR /
    # PDRS_BASELINE); CARGO_HOME gets cargo's own default so an untagged
    # shell still runs.
    environ = dict(os.environ)
    for name in ('CARGO_TARGET_DIR', 'PDRS_BASELINE'):
        if not environ.get(name):
            parser.error(f'{name} must be supplied by the caller')
    if not environ.get('CARGO_HOME'):
        default_home = Path.home() / '.cargo'
        if not default_home.is_dir():
            parser.error('CARGO_HOME must be supplied (or ~/.cargo must exist)')
        environ['CARGO_HOME'] = str(default_home)
    baseline_path = Path(environ['PDRS_BASELINE']).resolve()
    by_name = {row['name']: row for row in rows}
    all_files = sorted({path for row in rows for path, _old, _new in row['changes']}
                       | {path for row in rows for other in row.get('retractions', [])
                          for path, _old, _new in other['changes']})

    def save(name, text):
        exclusive_write(args.output / name, text)

    def apply_edits(edits, texts):
        """Apply every (path, old, new) exactly once; refuse a moved anchor."""
        for path, old, new in edits:
            if texts[path].count(old) != 1:
                raise RuntimeError(f'anchor count {texts[path].count(old)} != 1 '
                                   f'for {old!r} in {path}')
            texts[path] = texts[path].replace(old, new, 1)
            path.write_text(texts[path])

    def save_diff(prefix, pristine, texts, files):
        """One patch per touched file: the baseline bytes against the text
        that actually ran. apply_edits already moved texts, so re-applying
        the anchors here recorded a second, never-executed mutation — most
        patches came out empty, and a replacement that embeds its own anchor
        recorded a phantom edit (#1109 fix-r3, review P1-3)."""
        for path in sorted(files):
            save(f'{prefix}--{path.name}.patch', ''.join(difflib.unified_diff(
                pristine[path].decode().splitlines(True),
                texts[path].splitlines(True),
                fromfile=str(path), tofile=str(path))))

    def build(env, crate, label):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=10)
        save(f'{label}.slots.log', slots.stdout + slots.stderr)
        # No -q: a quiet cargo hides the compile failure that makes a row
        # INVALID instead of KILLED (rule 4: compile-check every mutant).
        result = run_owned([
            'cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway',
            '--test', crate, '--no-run',
        ], cwd=root, env=env, timeout=BUILD_TIMEOUT)
        save(f'{label}.log', result.stdout + result.stderr)
        save(f'{label}.exit', f'{result.returncode}\n')
        if result.returncode != 0:
            raise RuntimeError(f'{label}: cargo build failed (exit {result.returncode})')

    def run_case(env, row, label):
        """One direct test-binary run through the shared owned-process guard.

        No --nocapture and no --show-output: a passing test's captured
        stdout (which legitimately reprints a child worker's libtest
        report) must stay discarded, or the strict contract would count a
        second `running 1 test` header and refuse the verdict.
        """
        binary = locate_binary(Path(env['CARGO_TARGET_DIR']).resolve(), row['crate'])
        home = args.output / f'{label}.home'
        home.mkdir()
        run_env = dict(env)
        run_env.pop('PDRS_VERIFY_DELAY_MS', None)
        run_env.pop('PDRS_SEQUENCE_STEP_DELAY_MS', None)
        run_env.pop('PDRS_SEQUENCE_STEP_DELAY_FROM', None)
        run_env.update(row['env'])
        run_env.update(HOME=str(home), PILOT_HOME=str(home / '.pilotdeck'))
        if row['with_baseline']:
            run_env['PDRS_BASELINE'] = str(baseline_path)
        else:
            run_env.pop('PDRS_BASELINE', None)
        result = run_owned([str(binary), '--exact', row['test']],
                           cwd=root / 'src/pilotdeck-gateway',
                           env=run_env, timeout=RUN_TIMEOUT)
        save(f'{label}.log', result.stdout + result.stderr)
        save(f'{label}.exit', f'{result.returncode}\n')
        print(f"[{row['name']}] {label}: exit {result.returncode}", flush=True)
        return result

    def expect(result, case, wanted, what):
        status, reason, _hits = contract.classify(result, case)
        print(f'[{case.mutation}] {what}: {status}'
              + (f': {reason}' if reason else ''), flush=True)
        if status != wanted:
            raise RuntimeError(f'{what}: expected {wanted}, got {status}: {reason}')

    def guard():
        # The outer safety net holds every file any row touches; each row
        # re-enters mutating_sources for its own files (the tree is pristine
        # between rows, so the nested pristine proof always holds).
        return mutating_sources(root, all_files, baseline=args.baseline)

    def execute(case, env):
        row = by_name[case.mutation]
        files = sorted({path for path, _old, _new in row['changes']}
                       | {path for other in row.get('retractions', [])
                          for path, _old, _new in other['changes']})
        with mutating_sources(root, files, baseline=args.baseline) as snapshots:
            pristine = dict(snapshots)
            texts = {path: blob.decode() for path, blob in pristine.items()}
            # Every anchor, negative and retraction, must hit exactly once
            # in the pristine tree; a stale anchor is a script defect.
            stale = [f'{label}:{path.name}'
                    for label, edits in
                    [('negative', row['changes']),
                     *((f"retraction-{other['name']}", other['changes'])
                       for other in row.get('retractions', []))]
                    for path, old, _new in edits
                    if texts[path].count(old) != 1]
            if stale:
                raise RuntimeError(f'anchor count != 1: {stale}')
            build(env, row['crate'], f'{case.mutation}-positive-build')
            positive = run_case(env, row, f'{case.mutation}-positive')
            expect(positive, case, 'SURVIVED', 'positive control')
            apply_edits(row['changes'], texts)
            save_diff(f'{case.mutation}-negative', pristine, texts,
                      {path for path, _old, _new in row['changes']})
            build(env, row['crate'], f'{case.mutation}-negative-build')
            negative = run_case(env, row, f'{case.mutation}-negative')
            # Retraction attribution: with the named rewrite severed on top
            # of the same mutation the test must pass again; otherwise the
            # kill above is not bound to that rewrite.
            for other in row.get('retractions', []):
                for path, blob in pristine.items():
                    path.write_bytes(blob)
                texts = {path: blob.decode() for path, blob in pristine.items()}
                apply_edits(row['changes'] + other['changes'], texts)
                save_diff(f"{case.mutation}-{other['name']}", pristine, texts,
                          {path for path, _old, _new
                           in row['changes'] + other['changes']})
                build(env, row['crate'], f"{case.mutation}-{other['name']}-build")
                retracted = run_case(env, row, f"{case.mutation}-{other['name']}")
                expect(retracted, case, 'SURVIVED', f"retraction {other['name']}")
        # mutating_sources's finally restored and byte-checked the sources;
        # this rerun proves the restored tree passes again.
        build(env, row['crate'], f'{case.mutation}-restored-build')
        restored = run_case(env, row, f'{case.mutation}-restored')
        expect(restored, case, 'SURVIVED', 'restored control')
        return negative

    case_table = [contract.Case(row['name'], row['test'], row['assertion']) for row in rows]
    result = contract.run(
        case_table, args.output, guard=guard, execute=execute,
        oracle=['src/pilotdeck-gateway/tests/fixtures/background-tasks/oracle.json',
                'src/pilotdeck-gateway/tests/fixtures/transcript-resolve/oracle.json'],
        selected=list(args.mutations) if args.mutations else None,
        environ=environ)
    print(json.dumps(result.document, indent=2))
    if result.error:
        print(f'error: {result.error}')
    return result.code


if __name__ == '__main__':
    sys.exit(main())
