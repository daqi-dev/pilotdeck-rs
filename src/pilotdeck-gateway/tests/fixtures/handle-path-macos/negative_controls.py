#!/usr/bin/env python3
"""#1990: mutations that pin handle_path.rs `mod macos` itself (82-112).

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation compiles, fails at its bound nonce receipt, and passes again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
The two rows target the macOS module body directly — the existing
attach-followups-a rows pin only the services.rs caller, so this body had no
mutation on any platform. The bound tests are macOS-only and so are the rows:
on any other host each row is skipped with evidence (anchor present, module
cfg-eliminated), never recorded as KILLED, and the run exits 1 — a SKIPPED row
is not a green run (`specs/negative-controls/README.md` invariant 2). The
Linux-side zero discriminative power is proven separately by
`_tmp/20261107-issue-1990/survive-experiment.py`, which mutates the same two
anchors on a pristine HEAD tree and records SURVIVED for all five Linux
handle_path tests.
"""
import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parents[1]
HANDLE_PATH = ROOT / 'src/pilotdeck-gateway/src/handle_path.rs'
sys.path.insert(0, str(FIXTURES))
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                              tree_lock, write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered, verdict_exit)
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

mutations = [
    # Swallow the kernel F_GETPATH error and answer an empty Ok path: the
    # pipe fixture then resolves "successfully", so the getpath-step test
    # fails at its bound receipt. The is_err() shape alone could not tell
    # which step was bypassed; the raw-errno assertion can.
    ('macos-getpath-error-swallowed', HANDLE_PATH,
     '        let resolved = rustix::fs::getpath(file)?;',
     '''        let resolved = match rustix::fs::getpath(file) {
            Ok(path) => path,
            Err(_) => return Ok(PathBuf::new()),
        };''',
     'handle_path::tests::handle_path_non_filesystem_error_comes_from_getpath',
     '#1990 macOS must surface the kernel F_GETPATH error itself, not a later-stage failure'),
    # Replace the UTF-8 validation with a lossy decode: a non-UTF-8 leaf name
    # (APFS preserves the raw bytes) then resolves to a U+FFFD spelling that
    # was never opened, and the non-UTF-8 test fails at its bound receipt.
    ('macos-utf8-lossy', HANDLE_PATH,
     '''        String::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the handle path is not UTF-8"))''',
     '        Ok(PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))',
     'handle_path::tests::handle_path_rejects_a_non_utf8_name',
     '#1990 macOS must reject a non-UTF-8 handle path as invalid data'),
]

# #1990 fix-r1: the denominator is this literal, never `len(mutations)`.
# `registered()` compares the two in order and refuses on any drift, so a
# dropped, renamed, duplicated or reordered row is a startup refusal (rc 2)
# instead of a silently shorter run that still reports "N/N killed"
# (`specs/negative-controls/README.md` invariant 1).
EXPECTED_NAMES = (
    'macos-getpath-error-swallowed',
    'macos-utf8-lossy',
)


def test_body(test, texts=None):
    if texts is None:
        texts = {HANDLE_PATH: HANDLE_PATH.read_text()}
    # Tests live inside `mod tests` at 4-space indent; the function's own
    # closing brace is the first `^    }` after its `fn` line, so a sibling's
    # assertion cannot satisfy this binding.
    pattern = (r'^    (?:async )?fn ' + re.escape(test.rsplit('::', 1)[-1])
               + r'\([^\n]*\).*?^    }')
    bodies = re.findall(pattern, texts[HANDLE_PATH], re.M | re.S)
    return bodies[0] if len(bodies) == 1 else ''


def check_binding_scope():
    fixture = ('    fn target() {\n    }\n'
               '    fn sibling() {\n        assert!(false, "only sibling");\n    }\n')
    if 'only sibling' in test_body('target', {HANDLE_PATH: fixture}):
        raise RuntimeError('#1990 binding checker accepted sibling assertion')
    if 'only sibling' not in test_body('sibling', {HANDLE_PATH: fixture}):
        raise RuntimeError('#1990 binding checker lost its own assertion')


def stale_names():
    stale = []
    # #2795 fix-r3（sibling P1 同族跟改，manager-gateway fix-r2 先例）：锚点核对
    # 也要持树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能把变异体
    # 留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败沿用本入口
    # 既有 refuse 习惯（具名 rc 2），stale 判定消息保持原样。核对发生在
    # 变异轮（mutating_sources）之前，顺序取锁无同进程嵌套。
    try:
        with tree_lock(ROOT, timeout=60):
            for name, path, old, new, test, assertion in mutations:
                text = path.read_text()
                body = test_body(test)
                if (text.count(old) != 1 or old == new or
                        not body or f'"{assertion}"' not in body):
                    stale.append(name)
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        refuse(f'#1990 tree lock or anchor read failed: '
               f'{type(error).__name__}: {error}')
    return stale


def skip_rows(selected, output):
    """Record an explicit per-row SKIP on any non-Darwin host, with evidence.

    A KILLED verdict requires the mutation to be compiled *and* judged; on
    Linux the whole `mod macos` is cfg-eliminated, so a run here can prove
    neither. Stale anchors are still refused (they are textual), and each row
    records the anchor count it saw, the platform, and the cfg guard. The
    exit code is the shared `verdict_exit`: SKIPPED is never 0, so a consumer
    that greens on the exit code cannot read this as a finished run.
    """
    results = []
    for name, _path, old, _new, test, assertion in selected:
        count = HANDLE_PATH.read_text().count(old)
        guard = HANDLE_PATH.read_text().count('#[cfg(target_os = "macos")]')
        if count != 1:
            refuse(f'#1990 stale anchor for {name}: {count} matches')
        reason = (f'macOS-only module body; host is {sys.platform}, the module '
                  f'is compiled out by #[cfg(target_os = "macos")] (guard '
                  f'present {guard}x); anchor present {count}x in the source '
                  f'text, so it is not stale; judge on a macOS host')
        results.append(dict(name=name, test=test, assertion=assertion,
                            status='SKIP', reason=reason,
                            platform=sys.platform, anchorCount=count))
        print(f'{name}: SKIP: {reason}', flush=True)
    counts = {'SKIP': len(results)}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts,
         'platform': sys.platform,
         'note': 'no verdict claimed off-macOS; see summary rows for the '
                 'per-anchor evidence'}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(f'summary: SKIP={counts["SKIP"]}, SURVIVED=0, INVALID=0', flush=True)
    return verdict_exit(results, key='status')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    # #1990 fix-r1 (review P2): the binding self-check runs before any
    # mutation, so its failure is a "never started" refusal (rc 2) — the
    # pre-`mutating_sources` boundary every guard below already follows
    # (`specs/negative-controls/README.md`) — not an uncaught traceback a
    # host would read as "ran and not green" (1). Same class of fix as
    # #2441's sdk-mcp-session-tools fixture in this same tree.
    try:
        check_binding_scope()
    except RuntimeError as error:
        refuse(str(error))
    # Every refusal below happens before any mutation, so it is rc 2 ("this
    # run never started") and not rc 1 ("ran, not green") — the four-way table
    # in `specs/negative-controls/README.md`.
    try:
        registered(mutations, EXPECTED_NAMES)
    except ValueError as error:
        refuse(str(error))
    # #2468 r3 (review r7 sibling#1/boundary#1): every guard below reads
    # argv/env only, so it is refused before the stale-anchor scan touches
    # live sources — same class as the sdk-mcp-session-tools fixture. A full
    # mutation round holds the tree lock with `handle_path.rs` mutated on
    # disk; a concurrent caller with an unknown name or an unusable
    # CARGO_TARGET_DIR would otherwise hit the stale-anchor refusal first,
    # naming none of the real cause. `--check` keeps its own argv contract
    # (no output, no names), and the non-Darwin `skip_rows` path still
    # requires no target (cargo never starts there). The output-path
    # precheck is a fast fail on the same condition `claim_dir` owns below
    # (an atomic `mkdir(exist_ok=False)`), so a path created after it is
    # still refused.
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not args.check:
        if not selected or (args.selected and len(selected) != len(args.selected)):
            refuse('#1990 unknown or repeated mutation')
        if not args.output:
            refuse('#1990 output directory required')
        if sys.platform == 'darwin':
            if missing_target():
                refuse('#1990 set CARGO_TARGET_DIR to a dedicated target directory')
            refuse_unusable_target()
            if Path(args.output).exists():
                refuse(f'#1990 output path already exists: {Path(args.output).resolve()}')
    stale = stale_names()
    if stale:
        refuse('#1990 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#1990 {len(mutations)} unique anchors and assertion bindings: OK')
        return 0
    if sys.platform != 'darwin':
        # Nothing below this line can be judged off-macOS; cargo never starts,
        # so a caller-owned target is not required here. The evidence
        # directory is still claimed the shared way (an existing one is a
        # refusal, not a verdict).
        return skip_rows(selected, claim_dir(args.output))
    output = claim_dir(args.output)
    env = dict(os.environ)
    for key in [ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'PDRS_MUTANT', 'LIVE_E2E_MUTANT']:
        env.pop(key, None)
    guarded = [HANDLE_PATH, Path(__file__).resolve(),
               FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_suite.py']
    results = []
    guarded_sha256 = {}

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway', '--lib']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded) as originals:
            guarded_sha256 = {str(path.relative_to(ROOT)): hashlib.sha256(originals[path]).hexdigest() for path in guarded}
            # #2468 r2 (review r6 concurrency#2/boundary#3): publish and
            # restore every mutated source through `write_source_atomic` (a
            # single rename, never a truncating in-place write), so a SIGKILL
            # mid-write-back cannot leave an empty or half-written
            # `handle_path.rs` that even `git checkout` cannot recover (#1903
            # residue class). The mode is snapshotted from the source itself,
            # so the new inode of the temp file does not become a mode-only
            # `git diff` under `core.filemode=true`.
            modes = {path: path.stat().st_mode & 0o7777
                     for _, path, _, _, _, _ in mutations}
            for name, path, old, new, test, assertion in selected:
                item = dict(name=name, test=test, assertion=assertion,
                            status='INVALID', reason='not run', compiled=False)
                try:
                    if not args.compile_only and not passed(run(name + '-positive', env, test), test):
                        raise RuntimeError('positive bound test failed')
                    write_source_atomic(ROOT, path,
                                        originals[path].decode().replace(old, new, 1).encode(),
                                        mode=modes[path])
                    if run(name + '-compile', env, test, compile_only=True).returncode:
                        raise RuntimeError('mutant did not compile; not a runtime kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='mutated source compiled')
                    else:
                        receipt = ReceiptRun(output, name, [(test, assertion)])
                        result = run(name + '-negative', receipt.environment(env), test)
                        killed, reason, hits = judge(result, '', [(test, assertion)], receipt)
                        item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'),
                                    reason=reason, hits=hits)
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    write_source_atomic(ROOT, path, originals[path], mode=modes[path])
                try:
                    if not args.compile_only:
                        if not passed(run(name + '-restored', env, test), test):
                            raise RuntimeError('restored bound test failed')
                        item['restoredPass'] = True
                except Exception as error:
                    item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
                results.append(item)
                print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            results.append(dict(name=name, test=test, assertion=assertion,
                                status='INVALID', reason=reason, compiled=False))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
          f'INVALID={counts["INVALID"]}', flush=True)
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
