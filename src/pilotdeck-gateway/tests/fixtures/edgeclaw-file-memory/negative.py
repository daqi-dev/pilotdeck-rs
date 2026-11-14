#!/usr/bin/env python3
"""Run three real product mutations in the exclusively owned worktree; restore it.

#1087 fix-r5 (sibling #3): the verdict rows stay in memory for the whole
guarded window and one final ``summary.json`` is published by a single
rename only after ``mutating_sources`` restored and byte-verified the
guarded source (the same skeleton as the edgeclaw tables). An abort inside
the window — a cancellation, a restore failure, the source guard — voids
the recorded rows, publishes one INVALID summary and exits 3. A red
positive baseline keeps exit 1 but still publishes its rows. The old
``finally``-inside-the-window summary write left a partial all-killed
summary behind when the run was cancelled mid-suite.
"""
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import (dead_stream_print, detach_dead_streams,
                              mutating_sources, new_run_token, refuse_summary,
                              run_owned, unique_summary_tmp,
                              wait_for_slot, write_source_atomic)


# Module level so anchor_selfcheck can resolve the table's target path from
# the rows below (the IMPLICIT 'source' receiver), bound before the table.
root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'

mutants = [
    ('provider-reentry-deadlocks',
     'if HELD_OPERATIONS.with(|held| held.borrow().contains(&key)) {',
     'if false {',
     'same_root_provider_reentry_finishes_like_synchronous_ts',
     'same-root provider operation must finish within 5s'),
    ('operation-lock-dropped',
     '        self.core.execute(method, input)',
     '        drop(_guards);\n        self.core.execute(method, input)',
     'concurrent_same_origin_keeps_one_record_like_real_ts',
     # Without the guard one worker panics before the final extra-file
     # assertion; both are runtime detections of the same lost lock.
     'same-origin concurrent writes created extra files|called `Result::unwrap()` on an `Err` value: Any'),
    ('edit-meta-passes-dream-field',
     'copy(&mut next, input, &["projectName", "description", "status"]);',
     'next = input.clone();',
     'rust_reads_and_writes_the_real_ts_transcripts',
     'edit-meta-whitelist operation 0 editProjectMeta'),
]





def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    # #1087 fix-r6 (P1a) / fix-r8 (S1-b): every evidence name carries the
    # shared per-run token (pid.serial.time_ns) — the OUT_DIR above is a
    # plain reused mkdir, so two overlapping runs of this table must not
    # truncate each other's fixed-name artifacts, and the time_ns component
    # survives pid recycling (a recycled pid must not replay another run's
    # token).
    run_token = new_run_token()
    # #1087 fix-r8 (S2-a): the verdict summary is per-run, too — this run
    # publishes its own file and never overwrites another run's green verdict
    # recorded in the same reused OUT_DIR.
    summary_name = f'summary.{run_token}.json'
    source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
    env = dict(os.environ)
    env.setdefault('CARGO_TARGET_DIR', str(root / '_build/target'))
    command = ['cargo', 'test', '--offline', '--locked', '--manifest-path', str(root / 'src/Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_file_memory']
    results = []
    # The guard's snapshot bytes, kept for the final restored comparison
    # after the window closes.
    original = None

    def write_summary():
        """Publish the one final summary atomically (#1087 fix-r5, sibling #3).

        Called exactly once per run, after ``mutating_sources`` restored and
        byte-verified the guarded source — or from the abort handler, after
        the same guard unwound. The old ``finally``-inside-the-window write
        published a summary even when the run had been cancelled mid-suite,
        leaving a partial all-killed file behind.
        """
        # The snapshot never happened (the guard refused a dirty baseline):
        # report that instead of crashing the void path on `None.encode()`.
        restored = None if original is None else source.read_text() == original
        payload = dict(
            results=results,
            restored=restored,
            beforeSha256=(None if original is None
                          else hashlib.sha256(original.encode()).hexdigest()),
            afterSha256=hashlib.sha256(source.read_bytes()).hexdigest())
        # #1087 fix-r5 (concurrency #3): this runner's OUT_DIR is created with
        # plain mkdir, not an exclusive claim, so two overlapping runs of the
        # same table must not share one fixed staging name.
        # #1087 fix-r8 (S2-a): the publish target is this run's per-run
        # summary name and the payload names the run — the twin
        # test_negative globs the per-run shape.
        tmp = unique_summary_tmp(output, summary_name)
        payload['runToken'] = run_token
        tmp.write_text(json.dumps(payload, indent=2) + '\n')
        tmp.replace(output / summary_name)

    def _refuse_summary(reason):
        """Publish this run's refusal in its own per-run file (#1087 fix-r6, P1c).

        The evidence directory is created with ``mkdir(exist_ok=True)`` so a
        host re-run can land in the same path; an exit-2 "never started"
        refusal is recorded in this run's own ``summary.<token>.json`` —
        one rename via the shared writer, carrying no verdict rows (the
        same rule llm-envelope / llm-recall keep with their
        ``_refuse_summary``). A previous run's verdict stays in its own
        file, untouched: it is never this run's evidence. A fresh directory
        simply receives its first file (fix-r9 per-run markers).
        """
        # #1087 fix-r6 (P1c) / fix-r8 (S1-c + S2-a): the refused marker is
        # this run's own per-run summary file — one rename via the shared
        # writer, never another run's verdict, and a failing write reports
        # through the guarded print instead of a traceback.
        refuse_summary(output, reason, run_token, summary_name=summary_name)

    def _void_pending(error):
        """Void the recorded rows and publish one INVALID summary (sibling #3).

        A cancellation or crash inside the guarded window is not a verdict:
        the rows recorded so far are voided, the remaining mutants are
        recorded void, and the single summary is published after the guard
        unwound. The process then exits 3 — deliberately different from 2
        ("never started", the stale-anchor refusal below) so a reader can
        tell a cancelled run from one that never ran.
        """
        reason = f'runner: {type(error).__name__}: {error}'
        for row in results:
            row['killed'] = False
            row['voided'] = reason
        for name, _before, _after, test, marker in mutants[len(results):]:
            results.append(dict(name=name, test=test, exit=None,
                                marker=marker, killed=False, voided=reason))
        write_summary()
        # #1087 fix-r8 (S1-a): the shared guarded print — the summary is
        # already published, "aborted = 3" is the only contract left
        # to keep; a dead stderr must not replace the exit code.
        dead_stream_print(f'voided {len(results)} row(s): {reason}', stream='stderr')
        if isinstance(error, (OSError, ValueError)):
            # #1087 fix-r5 (concurrency #2): a stream-failure abort detaches
            # the dead fds first, so the interpreter-shutdown flush cannot
            # rewrite the exit code below to 120 ("Exception ignored in:").
            detach_dead_streams()
        if isinstance(error, SystemExit):
            # Normalize: 128+signum and any other raw code must not decide
            # how a coordinator reads this run.
            raise SystemExit(3) from error
        if not isinstance(error, Exception):
            raise
        raise SystemExit(3) from error

    # The flock covers reading the pristine bytes, every mutation and the
    # restored run, so no concurrent in-place runner can capture a mutant.
    try:
        with mutating_sources(root, [source]) as snapshots:
            original = snapshots[source].decode()
            # A stale anchor is a harness defect: refuse before any compile
            # (exit 2, nothing measured, nothing published).
            for name, before, _, _, _ in mutants:
                if original.count(before) != 1:
                    # #1087 fix-r8 (S1-a): the shared guarded print — a dead
                    # stderr must not turn this exit-2 refusal into an
                    # in-window abort (exit 3 with a voided INVALID summary),
                    # an exec-time ``2>&-`` reroute, or a 120.
                    dead_stream_print(f'STALE ANCHOR {name}: {original.count(before)} matches',
                                      stream='stderr')
                    raise SystemExit(2)

            def run(name, test, gate=None):
                # #1087 fix-r6 (P1a): the log name carries the run token and
                # the verdict below reads the transcript this run's own
                # ``run_owned`` returned — a concurrent run on the same
                # reused directory can no longer truncate this run's log
                # into a false KILLED / SURVIVED.
                with (output / f'{name}.{run_token}.log').open('w') as log:
                    gate = gate or wait_for_slot(root / 'scripts/cargo-slots.sh')
                    head = gate.stdout + gate.stderr
                    log.write(head)
                    log.flush()
                    result = run_owned(command + ([test, '--', '--exact'] if test else []),
                                       cwd=root, env=env, timeout=600)
                    log.write(result.stdout + result.stderr)
                    return result.returncode, head + result.stdout + result.stderr

            if run('positive', '')[0] != 0:
                # The positive baseline is red: nothing below is a verdict.
                # The guard unwinds and verifies while this exception
                # travels to the handler, which still publishes.
                print('positive tests did not pass before mutating', flush=True)
                raise SystemExit(1)

            for name, before, after, test, marker in mutants:
                mutated = original.replace(before, after, 1)
                # Reserve the Cargo slot before exposing the mutation.
                gate = wait_for_slot(root / 'scripts/cargo-slots.sh')
                # #1087 fix-r5 (sibling #3): one rename, never a truncating
                # in-place write; the shared publish helper (#1903) keeps a
                # SIGKILL mid-write from leaving a source the next preflight
                # would refuse as "differs from HEAD". No `mode=`: the
                # helper's own fallback stat covers the shape (boundary #5).
                write_source_atomic(root, source, mutated.encode())
                try:
                    code, raw = run(name, test, gate)
                    killed = (code == 101 and 'test result: FAILED' in raw
                              and 'could not compile' not in raw
                              # #813: every a|b candidate must be in this test's
                              # own panic block, not any other test's output.
                              and named_test_killed(raw, test, marker.split('|')))
                    results.append(dict(name=name, test=test, exit=code,
                                        marker=marker, killed=killed))
                    print(f'{name}: killed={killed}, exit={code}', flush=True)
                finally:
                    # Same #1087 fix-r5 window: the in-loop restore is also
                    # one rename; the guard's own exit verification covers
                    # the bytes.
                    write_source_atomic(root, source, original.encode())
            if run('restored', '')[0] != 0:
                raise RuntimeError('restored product did not pass its tests')
    except SystemExit as error:
        if error.code == 1:
            # The deliberate positive-baseline failure: the guard already
            # restored and byte-verified the tree while unwinding the
            # with-block; publish the (empty) rows and keep the exit code.
            write_summary()
            raise
        if error.code == 2:
            # The deliberate stale-anchor refusal: nothing was measured, so
            # nothing is published — and a reused directory's previous green
            # summary is invalidated before the refusal propagates, so it
            # cannot stay readable as this run's evidence (#1087 fix-r6, P1c).
            _refuse_summary('stale anchors: negative.py is out of sync with its sources')
            raise
        _void_pending(error)
    except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
        _void_pending(error)
    # Reaching here means mutating_sources restored the guarded file and
    # verified it against the baseline bytes. Only now may a summary exist.
    write_summary()

    killed = [row for row in results if row['killed']]
    print(f'summary: {len(killed)}/{len(results)} killed', flush=True)
    raise SystemExit(0 if results and len(killed) == len(results) else 1)


if __name__ == '__main__':
    main()
