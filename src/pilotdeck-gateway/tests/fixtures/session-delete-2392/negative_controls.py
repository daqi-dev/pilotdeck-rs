#!/usr/bin/env python3
"""#2392: --check; [--compile-only] NEW_OUTPUT [mutation ...]. Commit first.

`deleteSession`'s trailing `clearSdkSessionState` wiring, over the
`session_delete` method. Each mutation restores one shape of the pre-#2392
gap (a deleted session's registry rows outliving its files) and is bound to
the test that must die:

- `mcp-clear-removed`: the whole clear is gone — the exact gap the issue
  reported.
- `thinking-clear-removed`: only the thinking override survives the delete.
- `clear-before-not-found`: the clear runs before the transcript check, so a
  `SESSION_NOT_FOUND` refusal also wipes the rows it must keep.

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
SESSION_DIR = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/session_dir.rs'
SOURCE_TEXTS = (SESSION_DIR,)
CFG = 'native_process::methods::session_dir::tests::'
CLEAR = ('self.mcp\n'
         '            .config()\n'
         '            .clear_sdk_session_state(&JsValue::String(key.clone()));\n'
         '        self.session_overrides.clear_thinking(&key);')
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
    # The clear runs before the transcript check: a SESSION_NOT_FOUND refusal
    # also wipes the rows it must keep.
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
     '        self.session_overrides.clear_thinking(&key);',
     CFG + 'session_delete_not_found_keeps_the_sdk_session_state',
     "#2392 the refusal keeps the session's MCP permission overrides"),
]
# Every bound assertion lives verbatim in the scanned source; each needs its
# own pin or the completeness rule refuses the run (#2065 shape).
LITERAL_ANCHORS = [
    (SESSION_DIR, "#2392 delete drops the session's MCP permission overrides"),
    (SESSION_DIR, "#2392 delete drops the session's thinking override"),
    (SESSION_DIR, "#2392 the refusal keeps the session's MCP permission overrides"),
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
            raise SystemExit(f'#2392 scanned source unreadable: {path.name}: {error}')
    for case in cases:
        if case.path not in texts:
            raise SystemExit('#2392 row mutates a file outside SOURCE_TEXTS: '
                             f'{case.mutation} -> {case.path.name}')
    for path, anchor in LITERAL_ANCHORS:
        if path not in texts:
            raise SystemExit(f'#2392 literal anchor declared outside SOURCE_TEXTS: {path.name}')
        if anchor not in texts[path]:
            raise SystemExit(f'#2392 literal anchor not in {path.name}: {anchor!r}')
        if not any(anchor == assertion
                   for case in bound_rows for assertion in _assertions(case)):
            raise SystemExit(f'#2392 literal anchor unbound to any row: {anchor!r}')
    pinned = set(LITERAL_ANCHORS)
    for case in cases:
        for assertion in _assertions(case):
            unregistered = [path.name for path, text in texts.items()
                            if assertion in text and (path, assertion) not in pinned]
            if unregistered:
                raise SystemExit('#2392 unpinned literal anchor '
                                 f'(verbatim in {", ".join(unregistered)}): {assertion!r}')


def mutate(case, original):
    count = original.count(case.old)
    if count != 1 or case.old == case.new:
        raise ValueError(f'#2392 {case.mutation}: stale anchor (expected 1, got {count})')
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
        raise SystemExit('#2392 duplicate mutation name in the table')
    if args == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#2392 {len(selected_rows)} mutation anchors OK; '
              f'{len(LITERAL_ANCHORS)} literal anchors OK')
        return 0
    if not args:
        raise SystemExit('usage: negative_controls.py --check | [--compile-only] NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#2392 CARGO_TARGET_DIR must be supplied by the caller')
    for key in ('PDRS_NEGATIVE_RECEIPTS', 'PDRS_NEGATIVE_PARENT_TEST', 'PDRS_MUTANT'):
        env.pop(key, None)
    selected = args[1:]
    if (len(selected) != len(set(selected))
            or set(selected) - {case.mutation for case in selected_rows}):
        raise SystemExit('#2392 unknown/duplicate mutation name')
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
                            raise RuntimeError('#2392 positive control failed')
                        write_atomic(case.path, changed.encode())
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
                        write_atomic(case.path, originals[case.path])
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
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not results or any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
