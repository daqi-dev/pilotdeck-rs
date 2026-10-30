#!/usr/bin/env python3
"""#1125 ETXTBSY script-publication controls. Run after committing the candidate.

Usage: CARGO_TARGET_DIR=... python3 negative_controls.py OUT [mutation ...]
Every selected mutant compiles first; all results are reported before exit.

The fix keeps every write descriptor out of the test process: the body is
piped to a `/bin/sh -c 'cat >"$tmp"'` helper child, the helper is reaped
within a bounded deadline (killed and reaped past it), and only then are
chmod + rename applied. The bound stress test
`script_spawn_survives_concurrent_forks` writes the script through that
helper and immediately execs it while sibling threads fork continuously; a
mutant that moves the write back into this process must fail there with
ETXTBSY (`Os { code: 26, ... "Text file busy" }`), and only that failure
kills: the test's `#1125 etxtbsy` marker is emitted for raw os error 26
alone, so any other spawn failure (EAGAIN under fork pressure, a missing
binary, a non-zero exit) classifies as INVALID, never KILLED. The
`no-temp-cleanup` and deadline mutants are bound to the lib-only residue
unit tests instead: they are deterministic and platform-independent. macOS
uses posix_spawn and never shows the window, so the race mutants are only
meaningful on a Linux host; run them there (cb8 or CI).
"""
import argparse
import json
import os
import subprocess
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
helper = root / 'src/pilotdeck-gateway/tests/common/scratch.rs'
source = root / 'src/pilotdeck-gateway/src/runtime/tools/builtin/grep.rs'
stress = 'runtime::tools::builtin::grep::tests::script_spawn_survives_concurrent_forks'
residue = 'test_scratch_publication::failed_publication_removes_the_temporary_name'
stalled = 'test_scratch_publication::a_stalled_helper_is_killed_and_cleaned_up'
unread = 'test_scratch_publication::a_helper_that_never_reads_hits_the_write_deadline'
cases = [
    # The literal pre-fix shape: Scratch::script itself writes and chmods
    # the exec'd path in the test process, reopening the wide write window.
    ('script-direct-write', source,
     '''            crate::test_scratch::write_executable(&path, body, 0o755)
                .expect("#1125 publish script by helper");''',
     '''            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&path, body).expect("#1125 write script");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("#1125 chmod script");''',
     stress, '#1125 etxtbsy'),
    # The helper spawn segment regressed to an in-process write of the
    # temporary name (the fix-r1 shape); every caller is exposed again. The
    # anchor is only that spawn segment, so unrelated edits to the helper
    # body cannot stale it (#1125 fix-r2 review P2).
    ('helper-in-process-write', helper,
     '''    write_executable_bounded(path, body, mode, HELPER_DEADLINE, |temporary| {
        let mut helper = process::Command::new("/bin/sh");
        helper
            .arg("-c")
            .arg("cat >\\"$1\\"")
            .arg("write-executable")
            .arg(temporary)
            .stdin(process::Stdio::piped());
        helper
    })''',
     '''    write_executable_bounded(path, body, mode, HELPER_DEADLINE, |temporary| {
        std::fs::write(temporary, body).expect("#1125 mutant writes the temporary itself");
        let mut helper = process::Command::new("/bin/sh");
        helper
            .arg("-c")
            .arg("cat >/dev/null")
            .arg("write-executable")
            .stdin(process::Stdio::piped());
        helper
    })''',
     stress, '#1125 etxtbsy'),
    # A failed publication must not leave the `.tmp-` name behind; bound to
    # the residue unit test, which fails deterministically (no race, so it
    # also holds on macOS).
    ('no-temp-cleanup', helper,
     '''    if published.is_err() {
        // Every error exit after the helper may have created the file
        // removes the temporary name again (#1125 fix-r1 review P2).
        let _ = fs::remove_file(&temporary);
    }
    published''',
     '''    published''',
     residue, '#1125 temp residue'),
    # A helper that overruns its deadline must be killed and reaped, not
    # leaked (#1125 fix-r2 review P2); bound to the stalled-helper unit
    # test, whose SIGKILL liveness probe fails deterministically.
    ('deadline-kill-removed', helper,
     '''    if exceeded {
        // The helper cannot be waited out: terminate and reap it so no
        // process keeps running in this test's name, then fall through to
        // the temporary-name cleanup below.
        let _ = helper.kill();
        let _ = helper.wait();
    }''',
     '''    let _ = &helper;''',
     stalled, '#1125 stalled helper must be killed and reaped'),
    # The overrun must surface as ErrorKind::TimedOut, never as a bare
    # helper failure; bound to the never-reading-helper unit test.
    ('deadline-error-kind', helper,
     '''    io::Error::new(
        io::ErrorKind::TimedOut,''',
     '''    io::Error::new(
        io::ErrorKind::Other,''',
     unread, '#1125 deadline error must be TimedOut'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('out', type=Path)
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    unknown = set(args.mutations) - {row[0] for row in cases}
    if unknown:
        parser.error(f'unknown mutations: {sorted(unknown)}')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    args.out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, test, compile_only=False):
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
            (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            suffix = ['--no-run'] if compile_only else [test, '--', '--exact']
            result = run_owned(command + suffix, cwd=root, env=env, timeout=900)
            (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for value in (error.stdout, error.stderr):
                    raw += value.decode(errors='replace') if isinstance(value, bytes) else value or ''
            with (args.out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion, status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, [helper, source]) as snapshots:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: expected one source anchor')
                    if not args.compile_only and not passed(run(name + '-positive', test), test):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        if run(name + '-compile', test, compile_only=True).returncode != 0:
                            reason = 'mutant failed to compile'
                        elif args.compile_only:
                            status, reason = 'COMPILED', 'mutant cargo test --no-run passed'
                        else:
                            status, reason = classify(run(name + '-negative', test),
                                Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not args.compile_only and not passed(run(name + '-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, test, assertion, status, reason)
    except Exception as error:
        reason = f'source guard: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', reason)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    (args.out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(not results or any(row['status'] != success for row in results))


if __name__ == '__main__':
    if sys.argv[1:] == ['--check-anchors']:
        stale = []
        for name, path, old, _new, _test, _assertion in cases:
            count = path.read_text().count(old)
            if count != 1:
                stale.append((name, count))
        if stale:
            raise SystemExit(f'STALE ANCHORS: {stale}')
        print(f'{len(cases)} anchors: ok')
        raise SystemExit(0)
    raise SystemExit(main())
