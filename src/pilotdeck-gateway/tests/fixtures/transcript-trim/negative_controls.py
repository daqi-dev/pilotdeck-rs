#!/usr/bin/env python3
"""#1244 negative controls for the merged transcript trim; run all, then summarize.

#1244 removed the untested same-shape `js_trim_units` copy from
`storage/transcript_adapter.rs`; the storage transcript path now trims through
the shared `runtime::memory::js_trim_units` / `js_is_space_unit` pair that
#1226 pins. These rows prove both sides of that merge:

- dropping a code unit from the shared set fails the #1226 exhaustive lib pin;
- rewiring the storage-side `non_empty_string` consumer (no trim at all, or
  Rust `char::is_whitespace` instead of the JS set — the historical fork
  shape) fails the new #1244 exhaustive lib pin in
  `storage::transcript_adapter::trim_tests`;
- an off-by-one in the shared `js_trim_units` slicing fails the same #1244
  pin (the exhaustive storage consumer also pins the slice semantics).

Each mutant is applied to one committed source file, compile-checked, run
against its bound test, and restored byte-for-byte. The shared executor
(`negative_suite.run_cases`) records KILLED / SURVIVED / INVALID with a reason
for every row and exits non-zero if any row is not KILLED; `--audit` refuses
stale anchors before anything runs.

The files this lane adds (`transcript-trim/`, the `trim_tests` module and the
merged call sites) are not in HEAD yet, so ``mutating_sources`` refuses to open
its window until the delivery is committed; the coordinator runs this table
on the host afterwards. Run from the worktree root with a caller-supplied
``CARGO_TARGET_DIR``:

    CARGO_TARGET_DIR=/abs/cargo-target \
    python3 src/pilotdeck-gateway/tests/fixtures/transcript-trim/negative_controls.py out/
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, tree_lock
from negative_suite import Case, invalidate_completed, run_cases

root = Path(__file__).resolve().parents[5]
memory = root / 'src/pilotdeck-gateway/src/runtime/memory/mod.rs'
adapter = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
# Whole-file, unique anchors understood by anchor_selfcheck.py.
controls = [
    # The shared set itself: dropping one measured unit drifts against the
    # #1226 exhaustive pin (and, through the merge, every storage caller).
    ('shared-unit-00a0-dropped', memory,
     '            | 0x00A0\n',
     '',
     'runtime::memory::gateway::tests::js_is_space_unit_pins_the_ts_trim_whitespace_set',
     '#1226 js_is_space_unit drifted from the TS trim whitespace set'),
    # The storage consumer must keep trimming: dropping the filter is the
    # crudest fork (whitespace-only strings become "non-empty").
    ('storage-trim-dropped', adapter,
     'fn non_empty_string(value: &JsValue) -> Option<&JsString> {\n'
     '    value\n'
     '        .as_string()\n'
     '        .filter(|s| !js_trim_units(s.units()).is_empty())\n'
     '}\n',
     'fn non_empty_string(value: &JsValue) -> Option<&JsString> {\n'
     '    value.as_string()\n'
     '}\n',
     'storage::transcript_adapter::trim_tests::non_empty_string_pins_the_js_trim_whitespace_set',
     '#1244 non_empty_string drifted from the TS trim whitespace set'),
    # The historical fork shape: a locally rewritten predicate using Rust's
    # White_Space instead of the JS trim set (U+0085 / U+FEFF differ).
    ('storage-trim-rust-whitespace', adapter,
     'fn non_empty_string(value: &JsValue) -> Option<&JsString> {\n'
     '    value\n'
     '        .as_string()\n'
     '        .filter(|s| !js_trim_units(s.units()).is_empty())\n'
     '}\n',
     'fn non_empty_string(value: &JsValue) -> Option<&JsString> {\n'
     '    value\n'
     '        .as_string()\n'
     '        .filter(|s| !s.units().iter().all(|&u| char::from_u32(u32::from(u)).is_some_and(|c| c.is_whitespace())))\n'
     '}\n',
     'storage::transcript_adapter::trim_tests::non_empty_string_pins_the_js_trim_whitespace_set',
     '#1244 non_empty_string drifted from the TS trim whitespace set'),
    # The exhaustive storage consumer also pins the shared slice: `..=end`
    # keeps the last non-space unit; `..end` eats it.
    ('shared-trim-end-off-by-one', memory,
     '    &units[start..=end]\n',
     '    &units[start..end]\n',
     'storage::transcript_adapter::trim_tests::non_empty_string_pins_the_js_trim_whitespace_set',
     '#1244 non_empty_string drifted from the TS trim whitespace set'),
]


def mutate(content, scope, replacements):
    for old, new in replacements:
        count = content.count(old)
        if count != 1:
            raise ValueError(f'#1244 stale anchor count={count}, expected=1: {old!r}')
        content = content.replace(old, new, 1)
    return content


def audit(rows):
    # #2795 fix-r3（sibling P1 同族跟改，manager-gateway fix-r2 先例）：锚点核对
    # 也要持树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能把变异体
    # 留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败是拒跑（2），
    # 不是宿主会读成 rc 1 的裸 traceback。audit 与后续变异轮是顺序关系，
    # 锁先释放再由 mutating_sources 重取，无同进程嵌套。
    try:
        with tree_lock(root, timeout=60):
            for _, path, old, new, _, _ in rows:
                mutate(path.read_text(), None, [(old, new)])
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        print(f'--audit tree lock or read failed: '
              f'{type(error).__name__}: {error}', file=sys.stderr)
        return 2
    return 0


def compile_controls(rows, out, env):
    results = []
    try:
        with mutating_sources(root, list(dict.fromkeys(row[1] for row in rows))) as originals:
            for name, path, old, new, _, _ in rows:
                result = {'mutation': name, 'compiled': False, 'status': 'INVALID', 'reason': ''}
                try:
                    path.write_text(mutate(originals[path].decode(), None, [(old, new)]))
                    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
                    (out / f'{name}-compile.slots.log').write_text(slot.stdout + slot.stderr)
                    run = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                                     '-p', 'pilotdeck-gateway', '--lib', '--no-run'],
                                    cwd=root, env=env, timeout=600)
                    (out / f'{name}-compile.log').write_text(run.stdout + run.stderr)
                    result.update(compiled=run.returncode == 0, returncode=run.returncode,
                                  status='COMPILED' if run.returncode == 0 else 'INVALID')
                except Exception as error:
                    result['reason'] = f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(originals[path])
                results.append(result)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, *_ in rows[len(results):]:
            results.append({'mutation': name, 'compiled': False, 'status': 'INVALID',
                            'reason': reason})
    (out / 'compile-summary.json').write_text(json.dumps(results, indent=2) + '\n')
    for result in results:
        print(json.dumps(result), flush=True)
    return all(result['compiled'] and result['status'] == 'COMPILED' for result in results)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audit', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    rows = controls
    if args.selected:
        unknown = set(args.selected) - {row[0] for row in rows}
        if unknown:
            parser.error(f'unknown mutations: {sorted(unknown)}')
        rows = [row for row in rows if row[0] in args.selected]
    if audit(rows):  # Stale anchors are errors, never survivors.
        return 2
    if args.audit:
        print(json.dumps([{'mutation': row[0], 'anchor': 'unique'} for row in rows], indent=2))
        return 0
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    if not args.output:
        parser.error('provide a fresh evidence directory')
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only',
               CARGO_TERM_QUIET='false')
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=False)
    compiled = compile_controls(rows, out, env)
    if args.compile_only:
        return int(not compiled)
    bound_cases = [Case(name, path, old, new, test, assertion)
                   for name, path, old, new, test, assertion in rows]
    status = run_cases(root, bound_cases, out, env, mutate, timeout=600)
    return int(not compiled or status != 0)


if __name__ == '__main__':
    raise SystemExit(main())
