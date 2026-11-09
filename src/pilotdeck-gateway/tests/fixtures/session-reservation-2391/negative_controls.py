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
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
# The two sources the rows mutate; the assertion messages also live in CONFIG
# (the `methods/config.rs` test module), so that file is scanned for pins too.
REPLACEMENT = ROOT / 'src/pilotdeck-gateway/src/runtime/replacement.rs'
CONFIG = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/config.rs'
SOURCE_TEXTS = (REPLACEMENT, CONFIG)
CFG = 'native_process::methods::config::tests::'
mutations = [
    # The guard's Drop stops releasing: the pre-#2391 leak restored — a
    # request dropped at an await point strands the reservation.
    ('guard-drop-leak', REPLACEMENT,
     'self.coordinator.release_transcript_write(&key);',
     'let _ = self.coordinator;',
     CFG + 'session_model_set_dropped_mid_write_releases_the_reservation',
     '#2391 a dropped request releases its transcript-write reservation'),
    # The set call site releases before the reserved body runs: the parked
    # request holds nothing (the reservation no longer spans the write).
    ('set-guard-early-release', CONFIG,
     'let reserved = reserve_model_update(self, params, "change the session model")?;\n'
     '        let result = self.session_model_set_reserved(params).await;\n'
     '        drop(reserved);\n'
     '        result',
     'drop(reserve_model_update(self, params, "change the session model")?);\n'
     '        let result = self.session_model_set_reserved(params).await;\n'
     '        result',
     CFG + 'session_model_set_dropped_mid_write_releases_the_reservation',
     '#2391 the parked request holds the reservation'),
    # The clear call site mirrors set's.
    ('clear-guard-early-release', CONFIG,
     'let reserved = reserve_model_update(self, params, "clear the session model")?;\n'
     '        let result = self.session_model_clear_reserved(params).await;\n'
     '        drop(reserved);\n'
     '        result',
     'drop(reserve_model_update(self, params, "clear the session model")?);\n'
     '        let result = self.session_model_clear_reserved(params).await;\n'
     '        result',
     CFG + 'session_model_clear_dropped_mid_write_releases_the_reservation',
     '#2391 the parked clear request holds the reservation'),
]
# Every bound assertion lives verbatim in the scanned sources; each needs its
# own pin or the completeness rule refuses the run (#2065 shape).
LITERAL_ANCHORS = [
    (CONFIG, '#2391 a dropped request releases its transcript-write reservation'),
    (CONFIG, '#2391 the parked request holds the reservation'),
    (CONFIG, '#2391 the parked clear request holds the reservation'),
]


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
            raise SystemExit(f'#2391 scanned source unreadable: {path.name}: {error}')
    for case in cases:
        if case.path not in texts:
            raise SystemExit('#2391 row mutates a file outside SOURCE_TEXTS: '
                             f'{case.mutation} -> {case.path.name}')
    for path, anchor in LITERAL_ANCHORS:
        if path not in texts:
            raise SystemExit(f'#2391 literal anchor declared outside SOURCE_TEXTS: {path.name}')
        if anchor not in texts[path]:
            raise SystemExit(f'#2391 literal anchor not in {path.name}: {anchor!r}')
        if not any(anchor == assertion
                   for case in bound_rows for assertion in _assertions(case)):
            raise SystemExit(f'#2391 literal anchor unbound to any row: {anchor!r}')
    pinned = set(LITERAL_ANCHORS)
    for case in cases:
        for assertion in _assertions(case):
            unregistered = [path.name for path, text in texts.items()
                            if assertion in text and (path, assertion) not in pinned]
            if unregistered:
                raise SystemExit('#2391 unpinned literal anchor '
                                 f'(verbatim in {", ".join(unregistered)}): {assertion!r}')


def mutate(case, original):
    count = original.count(case.old)
    if count != 1 or case.old == case.new:
        raise ValueError(f'#2391 {case.mutation}: stale anchor (expected 1, got {count})')
    return original.replace(case.old, case.new, 1)


PUBLISH_DIR = ROOT / '.negctl-publish'


def write_atomic(path, data):
    """Publish the source in one rename so a concurrent reader never sees a
    half-written file. The temp lives in `.negctl-publish` at the repository
    root (outside the `src` / `.cargo` / `rust-toolchain*` scan areas), so a
    SIGKILL residue is not read back as an untracked build input; the mode
    comes from the source (#1692 §9, same shape as native-config)."""
    PUBLISH_DIR.mkdir(parents=True, exist_ok=True)
    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'
    mode = path.stat().st_mode & 0o7777
    try:
        with tmp.open('wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    finally:
        tmp.unlink(missing_ok=True)


def main():
    args = sys.argv[1:]
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    every_row = [Case(*row) for row in mutations]
    selected_rows = every_row
    if len({case.mutation for case in every_row}) != len(every_row):
        raise SystemExit('#2391 duplicate mutation name in the table')
    if args == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#2391 {len(selected_rows)} mutation anchors OK; '
              f'{len(LITERAL_ANCHORS)} literal anchors OK')
        return 0
    if not args:
        raise SystemExit('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#2391 CARGO_TARGET_DIR must be supplied by the caller')
    for key in ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT'):
        env.pop(key, None)
    selected = args[1:]
    if (len(selected) != len(set(selected))
            or set(selected) - {case.mutation for case in selected_rows}):
        raise SystemExit('#2391 unknown/duplicate mutation name')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    check_literal_anchors(selected_rows, bound_rows=every_row)
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
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
                        write_atomic(case.path, changed.encode())
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
                        write_atomic(case.path, originals[case.path])
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
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not results or any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
