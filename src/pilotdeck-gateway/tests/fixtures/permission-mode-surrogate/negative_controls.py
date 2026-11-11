#!/usr/bin/env python3
"""#1485: permission-mode UTF-16 control; requires committed product sources."""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, tree_lock
from negative_suite import Case, invalidate_completed, run_cases

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/native_process/methods/config.rs'
controls = [
    ('lossy-mode-message', source,
     '"INVALID_PERMISSION_MODE",\n                JsString::from_utf16(units),',
     '"INVALID_PERMISSION_MODE",\n                JsString::from(String::from_utf16_lossy(&units)),',
     'native_process::methods::config::tests::permission_mode_error_preserves_utf16',
     '#1485 mode UTF-16 preserved:'),
]


def mutate(content, scope, replacements):
    for old, new in replacements:
        count = content.count(old)
        if count != 1:
            raise ValueError(f'#1485 stale anchor: expected 1, found {count}: {old!r}')
        content = content.replace(old, new, 1)
    return content


def audit():
    if [row[0] for row in controls] != ['lossy-mode-message']:
        raise ValueError('#1485 mutation inventory changed')
    # #2795 fix-r3（sibling P1 同族跟改，manager-gateway fix-r2 先例）：锚点核对
    # 也要持树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能把变异体
    # 留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败是拒跑（2），
    # 不是宿主会读成 rc 1 的裸 traceback。audit 与后续变异轮是顺序关系，
    # 锁先释放再由 mutating_sources 重取，无同进程嵌套。
    try:
        with tree_lock(root, timeout=60):
            for _, path, old, new, _, _ in controls:
                mutate(path.read_text(), None, [(old, new)])
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        print(f'--audit tree lock or read failed: '
              f'{type(error).__name__}: {error}', file=sys.stderr)
        return 2
    return 0


def compile_controls(out, env):
    results = []
    try:
        with mutating_sources(root, [source]) as originals:
            for name, path, old, new, _, _ in controls:
                row = {'mutation': name, 'status': 'INVALID', 'reason': ''}
                try:
                    path.write_text(mutate(originals[path].decode(), None, [(old, new)]))
                    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
                    (out / f'{name}-compile.slots.log').write_text(slot.stdout + slot.stderr)
                    run = run_owned(
                        ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                         '-p', 'pilotdeck-gateway', '--lib', '--no-run'],
                        cwd=root, env=env, timeout=600,
                    )
                    (out / f'{name}-compile.log').write_text(run.stdout + run.stderr)
                    row.update(returncode=run.returncode,
                               status='COMPILED' if run.returncode == 0 else 'INVALID',
                               reason='' if run.returncode == 0 else 'mutant did not compile')
                except Exception as error:
                    row['reason'] = f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(originals[path])
                results.append(row)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, *_ in controls[len(results):]:
            results.append({'mutation': name, 'status': 'INVALID', 'reason': reason})
    (out / 'compile-summary.json').write_text(json.dumps(results, indent=2) + '\n')
    for row in results:
        print(json.dumps(row), flush=True)
    return all(row['status'] == 'COMPILED' for row in results)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audit', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    args = parser.parse_args()
    if audit():  # A stale anchor is an error, never SURVIVED.
        return 2
    if args.audit:
        print('#1485 anchors: 1/1 unique')
        return 0
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    if not args.output:
        parser.error('provide a fresh evidence directory')
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',
               CARGO_PROFILE_TEST_DEBUG='line-tables-only', CARGO_TERM_QUIET='false')
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=False)
    compiled = compile_controls(out, env)
    if args.compile_only:
        return int(not compiled)
    bound_cases = [Case(name, path, old, new, test, assertion)
                   for name, path, old, new, test, assertion in controls]
    status = run_cases(root, bound_cases, out, env, mutate, timeout=600)
    return int(not compiled or status != 0)


if __name__ == '__main__':
    raise SystemExit(main())
