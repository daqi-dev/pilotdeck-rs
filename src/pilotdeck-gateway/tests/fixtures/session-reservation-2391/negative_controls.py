#!/usr/bin/env python3
"""#2391: --check; [--compile-only] NEW_OUTPUT [mutation ...]. Commit first.

The transcript-write reservation's `Drop` guard, over the two model methods
that hold it. Each mutation restores one shape of the pre-#2391 leak (a
request dropped at an await point strands its key at `SESSION_BUSY` until
restart) and is bound to the test that must die:

- `guard-drop-leak`: the guard's `Drop` stops releasing — the exact leak the
  issue reported.
- `set-guard-early-release` / `clear-guard-early-release`: the call site
  releases before the reserved body runs, so the reservation no longer spans
  the write and the parked request holds nothing.

Each mutant compiles before its exact test runs. Every case is reported, then
restoration and all statuses determine the exit code; never bypass HEAD.
"""
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                               write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered, verdict_exit)
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
# The two sources the rows mutate; the assertion messages also live in CONFIG
# (the `methods/config.rs` test module), so that file is scanned for pins too.
REPLACEMENT = ROOT / 'src/pilotdeck-gateway/src/runtime/replacement.rs'
CONFIG = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/config.rs'
SOURCE_TEXTS = (REPLACEMENT, CONFIG)
CFG = 'native_process::methods::config::tests::'
mutations = [
    ('guard-drop-leak', CONFIG,
     'self.replacement.release_transcript_write(&self.key);',
     'let _ = (&self.replacement, &self.key);',
     'native_process::methods::config::tests::session_model_set_dropped_mid_write_releases_after_completion',
     'detached write finishes and releases reservation'),
    ('set-guard-early-release', CONFIG,
     '        tokio::spawn(async move {\n            requests\n                .session_model_set_reserved(&params, owner.as_ref(), reservations)\n                .await\n        })',
     '        drop(reservations);\n        tokio::spawn(async move {\n            requests\n                .session_model_set_reserved(&params, owner.as_ref(), Vec::new())\n                .await\n        })',
     'native_process::methods::config::tests::session_model_set_dropped_mid_write_releases_after_completion',
     '#2391 the parked request holds the reservation'),
    ('clear-guard-early-release', CONFIG,
     '        tokio::spawn(async move {\n            requests\n                .session_model_clear_reserved(&params, owner.as_ref(), reservations)\n                .await\n        })',
     '        drop(reservations);\n        tokio::spawn(async move {\n            requests\n                .session_model_clear_reserved(&params, owner.as_ref(), Vec::new())\n                .await\n        })',
     'native_process::methods::config::tests::session_model_clear_dropped_mid_write_releases_after_completion',
     '#2391 the parked clear request holds the reservation'),
]
# Every bound assertion lives verbatim in the scanned sources; each needs its
# own pin or the completeness rule refuses the run (#2065 shape).
LITERAL_ANCHORS = [
    (CONFIG, 'detached write finishes and releases reservation'),
    (CONFIG, '#2391 the parked request holds the reservation'),
    (CONFIG, '#2391 the parked clear request holds the reservation'),
]

# The denominator is this literal, never `len(mutations)` (#2391 fix-r1,
# specs/negative-controls/README.md invariant 1): `registered()` compares the
# two in order and refuses on any drift, so a dropped, renamed, duplicated or
# reordered row is a startup refusal (rc 2) instead of a silently shorter run
# that still reports "N/N killed".
EXPECTED_NAMES = (
    'guard-drop-leak',
    'set-guard-early-release',
    'clear-guard-early-release',
)


def _assertions(case):
    return case.assertion if isinstance(case.assertion, list) else [case.assertion]


def check_literal_anchors(cases, bound_rows=None):
    """`cases` are validated; a pin may bind to any of `bound_rows`."""
    bound_rows = cases if bound_rows is None else bound_rows
    texts = {}
    for path in SOURCE_TEXTS:
        try:
            texts[path] = path.read_text()
        except OSError as error:
            refuse(f'#2391 scanned source unreadable: {path.name}: {error}')
    for case in cases:
        if case.path not in texts:
            refuse('#2391 row mutates a file outside SOURCE_TEXTS: '
                   f'{case.mutation} -> {case.path.name}')
    for path, anchor in LITERAL_ANCHORS:
        if path not in texts:
            refuse(f'#2391 literal anchor declared outside SOURCE_TEXTS: {path.name}')
        if anchor not in texts[path]:
            refuse(f'#2391 literal anchor not in {path.name}: {anchor!r}')
        if not any(anchor == assertion
                   for case in bound_rows for assertion in _assertions(case)):
            refuse(f'#2391 literal anchor unbound to any row: {anchor!r}')
    pinned = set(LITERAL_ANCHORS)
    for case in cases:
        for assertion in _assertions(case):
            unregistered = [path.name for path, text in texts.items()
                            if assertion in text and (path, assertion) not in pinned]
            if unregistered:
                refuse('#2391 unpinned literal anchor '
                       f'(verbatim in {", ".join(unregistered)}): {assertion!r}')


def mutate(case, original):
    count = original.count(case.old)
    if count != 1 or case.old == case.new:
        raise ValueError(f'#2391 {case.mutation}: stale anchor (expected 1, got {count})')
    return original.replace(case.old, case.new, 1)


def main():
    args = sys.argv[1:]
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    every_row = [Case(*row) for row in mutations]
    selected_rows = every_row
    # #2391 fix-r1: every refusal below happens before any mutation, so it is
    # rc 2 ("this run never started", printed by `negative_selection.refuse`)
    # and not rc 1 ("ran, not green") — the four-way exit table in
    # `specs/negative-controls/README.md`.
    try:
        registered(mutations, EXPECTED_NAMES)
    except (ValueError, OSError) as error:
        refuse(f'mutation roster drifted: {error}')
    if args == ['--check']:
        # Guard first (native-config shape): it reports an unreadable scanned
        # source as a clean refusal, while the anchor count probe below would
        # raise a bare OSError on the same tree. A stale anchor is a
        # ValueError, and a source that becomes unreadable between the two
        # reads is an OSError (#297 r7 shape) -- both are refused the same
        # #1433 way so --check never reports "ran, not green" (rc 1) for a
        # run that never started.
        try:
            check_literal_anchors(selected_rows, bound_rows=every_row)
            for case in selected_rows:
                mutate(case, case.path.read_text())
        except (ValueError, OSError) as error:
            refuse(str(error))
        print(f'#2391 {len(selected_rows)} mutation anchors OK; '
              f'{len(LITERAL_ANCHORS)} literal anchors OK')
        return 0
    if not args:
        refuse('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT [mutation ...]')
    selected = args[1:]
    unknown = sorted(set(selected) - {case.mutation for case in selected_rows})
    if unknown:
        refuse(f'#2391 unknown mutation names: {unknown}')
    if len(selected) != len(set(selected)):
        repeated = sorted({name for name in selected if selected.count(name) > 1})
        refuse(f'#2391 repeated mutation names: {repeated}')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    check_literal_anchors(selected_rows, bound_rows=every_row)
    # The target is admitted by the shared rule (absent, empty, blank, dot,
    # root, repo root, inside `src`) *before* the evidence directory exists,
    # so a rejected target cannot leave a directory that reads like a
    # finished run.
    if missing_target():
        refuse('#2391 set CARGO_TARGET_DIR to the caller-owned build tree')
    refuse_unusable_target()
    out = claim_dir(args[0])
    env = dict(os.environ)
    for key in ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT'):
        env.pop(key, None)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, args):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=ROOT, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    def record(row):
        results.append(row)
        print(f"{row['mutation']}: {row['status']}: {row['reason']}", flush=True)

    def invalid(case, reason):
        return dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                    status='INVALID', reason=reason, compiled=False)

    phase = 'enter'
    body_error = None
    try:
        paths = list(dict.fromkeys(case.path for case in selected_rows))
        with mutating_sources(ROOT, [*paths, Path(__file__).resolve()]) as originals:
            phase = 'body'
            try:
                for case in selected_rows:
                    row = invalid(case, '')
                    args = [case.test, '--', '--exact']
                    try:
                        changed = mutate(case, originals[case.path].decode())
                        if not compile_only and not passed(run(case.mutation + '-positive', args), case.test):
                            raise RuntimeError('#2391 positive control failed')
                        write_source_atomic(ROOT, case.path, changed.encode())
                        compiled = run(case.mutation + '-compile', ['--no-run'])
                        row['compiled'] = compiled.returncode == 0
                        if not row['compiled']:
                            raise RuntimeError('#2391 mutant did not compile')
                        if compile_only:
                            row.update(status='COMPILED', reason='mutant cargo test --no-run passed')
                        else:
                            row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
                    except Exception as error:
                        row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                    finally:
                        phase = 'restore'
                        write_source_atomic(ROOT, case.path, originals[case.path])
                        if case.path.read_bytes() != originals[case.path]:
                            raise RuntimeError('#2391 source restoration failed')
                        phase = 'body'
                    if not compile_only:
                        try:
                            if not passed(run(case.mutation + '-restored', args), case.test):
                                raise RuntimeError('#2391 restored positive control failed')
                        except Exception as error:
                            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                    record(row)
            except BaseException as error:
                body_error = error
                raise
            phase = 'exit'
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for case in selected_rows[len(results):]:
            record(invalid(case, reason))
        publish_summary(out, results)
        if (phase in ('restore', 'exit') or not isinstance(error, Exception)
                or (body_error is not None and error is not body_error)):
            return 2
        return 1
    publish_summary(out, results)
    # The shared exit contract (#2391 fix-r1): zero rows is never 0 (`0/0
    # killed` is not a pass), and every row must be KILLED — COMPILED under
    # --compile-only.
    return verdict_exit(results, key='status',
                        killed='COMPILED' if compile_only else 'KILLED')


if __name__ == '__main__':
    raise SystemExit(main())
