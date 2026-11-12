#!/usr/bin/env python3
"""#2392: --check; [--compile-only] NEW_OUTPUT [mutation ...]. Commit first.

`deleteSession`'s trailing `clearSdkSessionState` wiring, over the
`session_delete` method. Each mutation restores one shape of the pre-#2392
gap (a deleted session's registry rows outliving its files) and is bound to
the test that must die:

- `mcp-clear-removed`: the whole clear is gone — the exact gap the issue
  reported.
- `thinking-clear-removed`: only the thinking override survives the delete.
- `output-style-clear-removed`: only the output-style selection survives the
  delete (the TS clear drops the whole `sdkSessionConfigs` row that carries
  it, ProjectRuntimeRegistry.ts:779).
- `clear-before-not-found`: the clear runs before the transcript check, so a
  `SESSION_NOT_FOUND` refusal also wipes the rows it must keep.
- `output-style-clear-before-not-found`: only the output-style clear runs
  before the transcript check, so the refusal wipes that row alone.
- `thinking-clear-before-not-found`: only the thinking clear runs before
  the transcript check, so the refusal wipes that row alone.

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
SESSION_DIR = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/session_dir.rs'
SOURCE_TEXTS = (SESSION_DIR,)
CFG = 'native_process::methods::session_dir::tests::'
CLEAR = ('self.mcp\n'
         '            .config()\n'
         '            .clear_sdk_session_state(&JsValue::String(key.clone()));\n'
         '        self.session_overrides.clear_thinking(&key);\n'
         '        self.session_overrides.clear_output_style(&key);')
mutations = [
    # The whole clear is gone: the pre-#2392 gap restored — a deleted session's
    # registry rows outlive its files.
    ('mcp-clear-removed', SESSION_DIR,
     CLEAR,
     'let _ = &key;',
     CFG + 'session_delete_clears_the_sdk_session_state',
     "#2392 delete drops the session's MCP permission overrides"),
    # Only the thinking override survives the delete.
    ('thinking-clear-removed', SESSION_DIR,
     'self.session_overrides.clear_thinking(&key);',
     'let _ = self.session_overrides.thinking(&key);',
     CFG + 'session_delete_clears_the_sdk_session_state',
     "#2392 delete drops the session's thinking override"),
    # Only the output-style selection survives the delete (fix-r1): the TS
    # clear deletes the whole `sdkSessionConfigs` row that carries it.
    ('output-style-clear-removed', SESSION_DIR,
     'self.session_overrides.clear_output_style(&key);',
     'let _ = self.session_overrides.output_style(&key);',
     CFG + 'session_delete_clears_the_sdk_session_state',
     "#2392 delete drops the session's output-style selection"),
    # The clear runs before the transcript check: a SESSION_NOT_FOUND refusal
    # also wipes the rows it must keep. The early block covers all three
    # groups (MCP, thinking, output-style), so the MCP assertion dies first.
    ('clear-before-not-found', SESSION_DIR,
     '// Close the live session first so the writer drains before we delete.\n'
     '        self.owner\n'
     '            .close(&key)\n'
     '            .await\n'
     '            .map_err(|e| RequestError::new(e.to_string()))?;',
     '// Close the live session first so the writer drains before we delete.\n'
     '        self.owner\n'
     '            .close(&key)\n'
     '            .await\n'
     '            .map_err(|e| RequestError::new(e.to_string()))?;\n'
     '        self.mcp\n'
     '            .config()\n'
     '            .clear_sdk_session_state(&JsValue::String(key.clone()));\n'
     '        self.session_overrides.clear_thinking(&key);\n'
     '        self.session_overrides.clear_output_style(&key);',
     CFG + 'session_delete_not_found_keeps_the_sdk_session_state',
     "#2392 the refusal keeps the session's MCP permission overrides"),
    # The output-style clear alone runs before the transcript check (fix-r2,
    # the third half's own row): the refusal wipes that row while the MCP,
    # rules and thinking rows survive, so the test dies at its own assertion.
    ('output-style-clear-before-not-found', SESSION_DIR,
     '// Close the live session first so the writer drains before we delete.\n'
     '        self.owner\n'
     '            .close(&key)\n'
     '            .await\n'
     '            .map_err(|e| RequestError::new(e.to_string()))?;',
     '// Close the live session first so the writer drains before we delete.\n'
     '        self.owner\n'
     '            .close(&key)\n'
     '            .await\n'
     '            .map_err(|e| RequestError::new(e.to_string()))?;\n'
     '        self.session_overrides.clear_output_style(&key);',
     CFG + 'session_delete_not_found_keeps_the_sdk_session_state',
     "#2392 the refusal keeps the session's output-style selection"),
    # The thinking clear alone runs before the transcript check (fix-r3, the
    # second half's own row, the symmetric twin of the output-style one): the
    # refusal wipes that row while the MCP, rules and output-style rows
    # survive, so the test dies at its own assertion.
    ('thinking-clear-before-not-found', SESSION_DIR,
     '// Close the live session first so the writer drains before we delete.\n'
     '        self.owner\n'
     '            .close(&key)\n'
     '            .await\n'
     '            .map_err(|e| RequestError::new(e.to_string()))?;',
     '// Close the live session first so the writer drains before we delete.\n'
     '        self.owner\n'
     '            .close(&key)\n'
     '            .await\n'
     '            .map_err(|e| RequestError::new(e.to_string()))?;\n'
     '        self.session_overrides.clear_thinking(&key);',
     CFG + 'session_delete_not_found_keeps_the_sdk_session_state',
     "#2392 the refusal keeps the session's thinking override"),
]
# Every bound assertion lives verbatim in the scanned source; each needs its
# own pin or the completeness rule refuses the run (#2065 shape).
LITERAL_ANCHORS = [
    (SESSION_DIR, "#2392 delete drops the session's MCP permission overrides"),
    (SESSION_DIR, "#2392 delete drops the session's thinking override"),
    (SESSION_DIR, "#2392 delete drops the session's output-style selection"),
    (SESSION_DIR, "#2392 the refusal keeps the session's MCP permission overrides"),
    (SESSION_DIR, "#2392 the refusal keeps the session's thinking override"),
    (SESSION_DIR, "#2392 the refusal keeps the session's output-style selection"),
]

# The denominator is this literal, never `len(mutations)` (#2391 fix-r1,
# specs/negative-controls/README.md invariant 1): `registered()` compares the
# two in order and refuses on any drift, so a dropped, renamed, duplicated or
# reordered row is a startup refusal (rc 2) instead of a silently shorter run
# that still reports "N/N killed".
EXPECTED_NAMES = (
    'mcp-clear-removed',
    'thinking-clear-removed',
    'output-style-clear-removed',
    'clear-before-not-found',
    'output-style-clear-before-not-found',
    'thinking-clear-before-not-found',
)


def _assertions(case):
    # closure-r5 (DS sibling#2, P2): sync of the native-config closure-r4
    # guard into this family's copy of the accessor. Every consumer iterates
    # per element, so an EMPTY list row produces no anchors: it joins no
    # literal-anchor loop, the row still counts in `--check`'s printed total
    # and the run stays green while the kill round could only ever read it
    # INVALID. Refuse the empty-list shape cleanly (rc 2), and the shapes
    # that would TypeError on the next line anyway (a non-str element, a
    # non-str scalar assertion). This family is outside the five-family
    # ROW_PINS machine check, but its `--check` is in CI.
    assertion = case.assertion
    if isinstance(assertion, list):
        if not assertion:
            refuse(f'#2392 empty assertion list: {case.mutation}')
        for element in assertion:
            if not isinstance(element, str):
                refuse(f'#2392 non-str assertion element: {case.mutation}')
        return assertion
    if not isinstance(assertion, str):
        refuse(f'#2392 non-str assertion: {case.mutation}')
    return [assertion]


def check_literal_anchors(cases, bound_rows=None):
    """`cases` are validated; a pin may bind to any of `bound_rows`."""
    bound_rows = cases if bound_rows is None else bound_rows
    texts = {}
    for path in SOURCE_TEXTS:
        try:
            texts[path] = path.read_text()
        except OSError as error:
            refuse(f'#2392 scanned source unreadable: {path.name}: {error}')
    for case in cases:
        if case.path not in texts:
            refuse('#2392 row mutates a file outside SOURCE_TEXTS: '
                   f'{case.mutation} -> {case.path.name}')
    for path, anchor in LITERAL_ANCHORS:
        if path not in texts:
            refuse(f'#2392 literal anchor declared outside SOURCE_TEXTS: {path.name}')
        if anchor not in texts[path]:
            refuse(f'#2392 literal anchor not in {path.name}: {anchor!r}')
        if not any(anchor == assertion
                   for case in bound_rows for assertion in _assertions(case)):
            refuse(f'#2392 literal anchor unbound to any row: {anchor!r}')
    pinned = set(LITERAL_ANCHORS)
    for case in cases:
        for assertion in _assertions(case):
            unregistered = [path.name for path, text in texts.items()
                            if assertion in text and (path, assertion) not in pinned]
            if unregistered:
                refuse('#2392 unpinned literal anchor '
                       f'(verbatim in {", ".join(unregistered)}): {assertion!r}')


def mutate(case, original):
    count = original.count(case.old)
    if count != 1 or case.old == case.new:
        raise ValueError(f'#2392 {case.mutation}: stale anchor (expected 1, got {count})')
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
        print(f'#2392 {len(selected_rows)} mutation anchors OK; '
              f'{len(LITERAL_ANCHORS)} literal anchors OK')
        return 0
    if not args:
        refuse('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT [mutation ...]')
    selected = args[1:]
    unknown = sorted(set(selected) - {case.mutation for case in selected_rows})
    if unknown:
        refuse(f'#2392 unknown mutation names: {unknown}')
    if len(selected) != len(set(selected)):
        repeated = sorted({name for name in selected if selected.count(name) > 1})
        refuse(f'#2392 repeated mutation names: {repeated}')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    check_literal_anchors(selected_rows, bound_rows=every_row)
    # The target is admitted by the shared rule (absent, empty, blank, dot,
    # root, repo root, inside `src`) *before* the evidence directory exists,
    # so a rejected target cannot leave a directory that reads like a
    # finished run.
    if missing_target():
        refuse('#2392 set CARGO_TARGET_DIR to the caller-owned build tree')
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
                            raise RuntimeError('#2392 positive control failed')
                        write_source_atomic(ROOT, case.path, changed.encode())
                        compiled = run(case.mutation + '-compile', ['--no-run'])
                        row['compiled'] = compiled.returncode == 0
                        if not row['compiled']:
                            raise RuntimeError('#2392 mutant did not compile')
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
                            raise RuntimeError('#2392 source restoration failed')
                        phase = 'body'
                    if not compile_only:
                        try:
                            if not passed(run(case.mutation + '-restored', args), case.test):
                                raise RuntimeError('#2392 restored positive control failed')
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
