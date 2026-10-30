#!/usr/bin/env python3
"""#1125 ETXTBSY script-publication controls. Run after committing the candidate.

Usage: CARGO_TARGET_DIR=... python3 negative_controls.py OUT [mutation ...]
Every selected mutant compiles first; all results are reported before exit.

The fix keeps every write descriptor out of the test process: the body is
piped to a `/bin/sh -c 'exec cat >"$tmp"'` helper child, the writer shares
the deadline through a non-blocking write end (so a stalled read end cannot
wedge it), and a guard owns the helper from the spawn on — on every exit it
kills and reaps the helper, reports that through the `reaped` hook, and
removes the temporary name — before chmod + rename publish the file. The
bound stress test `script_spawn_survives_concurrent_forks` writes the
script through that helper and immediately execs it while sibling threads
fork continuously; a mutant that moves the write back into this process
must fail there with ETXTBSY (`Os { code: 26, ... "Text file busy" }`),
and only that failure kills: the test's `#1125 etxtbsy` marker is emitted
for raw os error 26 alone, so any other spawn failure (EAGAIN under fork
pressure, a missing binary, a non-zero exit) classifies as INVALID, never
KILLED. The cleanup, deadline, and writer mutants are bound to the
lib-only publication unit tests instead: they are deterministic and
unix-only like the module itself. macOS uses posix_spawn and never shows
the window, so the two race mutants are a Linux contract (expected
SURVIVED elsewhere); the other four must kill on every unix host.
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
stranger = 'test_scratch_publication::a_stranger_on_the_read_end_still_hits_the_write_deadline'
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
    # The helper shape regressed to the fix-r1 mutation: the parent itself
    # writes the FINAL path with the write descriptor open until the end of
    # the function, right before the caller execs. That shape was proven
    # killable on cb8 (probe c: 28 ETXTBSY in 300 pressured iterations); the
    # fix-r2 variant that only wrote the temporary name closed its fd too
    # early and SURVIVED on the host, so the anchor is again the whole
    # body of `write_executable_bounded` (#1125 fix-r3 review P2).
    ('helper-in-process-write', helper,
     '''    let name = path
        .file_name()
        .expect("#1125 executable file name")
        .to_string_lossy();
    let temporary = path.with_file_name(format!("{name}.tmp-{}", uuid::Uuid::new_v4()));
    let expiry = Instant::now() + deadline;
    let helper = build_helper(&temporary).spawn()?;
    // #1125 fix-r3 review P2-1: the guard owns the helper from the spawn on,
    // so every later exit — an error return, a deadline overrun, or a panic
    // unwinding through this function — kills and reaps it and removes the
    // temporary name. Killing an already-reaped child is a no-op in std, so
    // a pid recycled after the wait loop reaped the helper is never
    // signalled.
    let mut guard = ReapGuard {
        helper: Some(helper),
        temporary: temporary.clone(),
        reaped: Some(reaped),
    };
    let mut stdin = guard
        .helper
        .as_mut()
        .expect("#1125 reap guard holds the helper")
        .stdin
        .take()
        .expect("#1125 helper stdin is piped");
    set_nonblocking(&mut stdin)?;
    let (sent, done) = mpsc::channel();
    let payload = body.as_bytes().to_vec();
    // #1125 fix-r3 review P2-1: a writer that cannot start is an ordinary
    // error return, not a panic unwinding past the cleanup: the dropped
    // closure closes the pipe's write end, so the helper still sees EOF,
    // and the guard reaps it and removes the temporary name.
    let writer = thread::Builder::new()
        .name("#1125 scratch writer".into())
        .spawn(move || {
            let written = write_payload_by_deadline(&mut stdin, &payload, expiry, deadline);
            let _ = sent.send(written);
            // `stdin` drops with the closure: closing the write end is what
            // lets the helper see EOF, and a broken pipe was reported by
            // `written`, never by hanging on the helper.
        });
    let written = match writer {
        Err(error) => Err(error),
        Ok(writer) => {
            let reported = done.recv_timeout(expiry.saturating_duration_since(Instant::now()));
            // #1125 fix-r3 review P2-2: the writer shares the expiry, so it
            // — and with it the last copy of the pipe's write end — must be
            // gone shortly after the expiry at the latest, whatever
            // happened to the helper or to anyone else holding the read
            // end. The grace absorbs one poll interval of lag.
            let writer_gone = expiry + WRITER_GRACE;
            while !writer.is_finished() && Instant::now() < writer_gone {
                thread::sleep(HELPER_POLL);
            }
            assert!(
                writer.is_finished(),
                "#1125 helper writer did not exit by its deadline"
            );
            match reported {
                Ok(written) => written,
                Err(mpsc::RecvTimeoutError::Timeout) => Err(helper_deadline_error(deadline)),
                // The writer can only end without reporting by panicking,
                // which still drops the pipe's write end; success is never
                // assumed.
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
                    "#1125 helper writer ended without reporting",
                )),
            }
        }
    };
    let waited = if matches!(&written, Err(error) if error.kind() == io::ErrorKind::TimedOut) {
        // The write phase itself overran: the helper cannot be waited out,
        // the guard below ends it instead.
        Err(helper_deadline_error(deadline))
    } else {
        let helper = guard
            .helper
            .as_mut()
            .expect("#1125 reap guard holds the helper");
        loop {
            match helper.try_wait() {
                Ok(Some(status)) if status.success() => break Ok(()),
                Ok(Some(status)) => {
                    break Err(io::Error::other(format!(
                        "#1125 helper for {} exited with {status}",
                        temporary.display()
                    )));
                }
                Ok(None) if Instant::now() < expiry => thread::sleep(HELPER_POLL),
                Ok(None) => break Err(helper_deadline_error(deadline)),
                // A wait error leaves the helper's state unknown; the guard
                // still kills and reaps it before this returns (#1125
                // fix-r3 review P3: every non-disarmed exit reaps).
                Err(error) => break Err(error),
            }
        }
    };
    let published = written.and(waited).and_then(|()| {
        #[cfg(unix)]
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
        #[cfg(not(unix))]
        let _ = mode;
        fs::rename(&temporary, path)
    });
    if published.is_ok() {
        // The wait loop reaped the helper and the rename consumed the
        // temporary name; the guard has nothing left to do.
        guard.disarm();
    }
    published
}
''',
     '''    let mut file = fs::File::create(path)?;
    io::Write::write_all(&mut file, body.as_bytes())?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    #[cfg(not(unix))]
    let _ = mode;
    drop(file);
    Ok(())
}''',
     stress, '#1125 etxtbsy'),
    # A failed publication must not leave the `.tmp-` name behind; the
    # guard's Drop is the single cleanup point, so the mutant deletes its
    # remove line. Bound to the residue unit test, which fails
    # deterministically (no race, so it also holds on macOS).
    ('no-temp-cleanup', helper,
     '''            if let Some(report) = self.reaped.take() {
                report(waited);
            }
        }
        let _ = fs::remove_file(&self.temporary);''',
     '''            if let Some(report) = self.reaped.take() {
                report(waited);
            }
        }''',
     residue, '#1125 temp residue'),
    # An overrun helper must be killed and reaped by the guard, not leaked;
    # the mutant drops the guard's kill + wait + report, so the stalled
    # test never receives its kill report (#1125 fix-r3 review P2).
    ('deadline-kill-removed', helper,
     '''        if let Some(mut helper) = self.helper.take() {
            let waited = match helper.kill() {
                Ok(()) => helper.wait(),
                Err(error) => Err(error),
            };
            if let Some(report) = self.reaped.take() {
                report(waited);
            }
        }''',
     '''        self.helper = None;
        self.reaped = None;''',
     stalled, ['#1125 stalled helper kill must be reported',
               '#1125 stalled helper must be killed and reaped']),
    # The overrun must surface as ErrorKind::TimedOut, never as a bare
    # helper failure; every construction site (the writer's give-up, the
    # recv backstop, the wait overrun) calls this one function, so one
    # mutant covers them all. Bound to the never-reading-helper unit test.
    ('deadline-error-kind', helper,
     '''    io::Error::new(
        io::ErrorKind::TimedOut,''',
     '''    io::Error::new(
        io::ErrorKind::Other,''',
     unread, '#1125 deadline error must be TimedOut'),
    # The writer must share the deadline: with the give-up removed it stays
    # blocked on a read end a stranger keeps open, and the parent's own
    # deadline cannot end it — only the writer-exit guard catches that
    # (#1125 fix-r3 review P2-2). Bound to the stranger-on-the-read-end test.
    ('writer-deadline-removed', helper,
     '''                if Instant::now() >= expiry {
                    return Err(helper_deadline_error(deadline));
                }''',
     '''                if false {
                    return Err(helper_deadline_error(deadline));
                }''',
     stranger, '#1125 helper writer did not exit by its deadline'),
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
