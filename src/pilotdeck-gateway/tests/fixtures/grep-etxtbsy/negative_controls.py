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
SURVIVED elsewhere), and so are the #1155 scan and reaping mutants, which
need `/proc` and `PR_SET_CHILD_SUBREAPER`; the other thirteen must kill on
every unix host — `stranger-retention-reverted` included: bash (the macOS
`/bin/sh`) keeps the pipe behind an explicit `<&0`, so `started` still
appears, but the reverted helper writes no `saw` marker and the #1155 r4
saw assertion kills it (under dash, the Linux `/bin/sh`, a background
command's stdin is /dev/null even behind an explicit `<&0`, so `started`
never appears and the hook's expect fails first). The
#1141 mutants target the dead-helper stranger test's write-end evidence:
the publication itself now scans `/proc` at the return boundary (#1155
r3 review P2-1) — before it returns — for any fd of its pipe open for
writing, with the scan's error taking precedence over the `TimedOut` the
test expects, so any mutant that leaks a write end — the writer's give-up
(`deadline-write-end-kept-open`) or the parent's own copy
(`parent-write-end-held-past-return`, fix-r2 review P2-1) — fails the
deadline-kind assertion with the scan's message in its payload (the
stranger's later marker verdict, a 0-byte read versus a not-ready read
end, remains the second witness) — and the guards: the dead-helper
test's group kill (`stranger-guard-kill-removed`, fix-r1 review P1-1),
plus the spawn-window panic test, which proves the guard owns the group
from the spawn on (`spawn-hook-removed`) and still ends the stranger
after an in-flight panic (`stranger-panic-guard-kill-removed`, fix-r2
review P1-1). The stranger parks in a bounded sleep that only that guard
may end. The fix-r3 handshake mutants prove the stranger must provably
hold the read end before anything else runs: the probe publishes
`started` only after `fstat(0)` shows a FIFO, and both the helper and the
test's `spawned` hook wait for that signal, so a mutant that inverts the
confirmation (`stranger-confirmation-inverted`) or drops the fd-3
retention save (`stranger-retention-reverted`) leaves the publication
blocked at the hook's expect. The #1155 mutants cover the four new
contracts: the deadline starts only after the `spawned` hook's handshake
(`expiry-hoisted-before-spawn`, r4 review P2 — the 6-second handshake
must not eat the write budget, and a deadline that starts too early lands
under the measured floor); the helper's and the test's started-wait caps
are one derived constant (`started-cap-hardcoded-short`, r4 review P2 —
a re-hardcoded 5 s gives up before the 6-second delayed started, so the
helper dies without its saw marker); the return-boundary scan flags write
ends only (`boundary-write-end-filter-removed`, r3 review P2-1 — without
the access-mode filter the stranger's read end fails the healthy path);
and the test — never PID 1 — reaps the stranger (`stranger-subreaper-removed`
and `stranger-reap-removed`, r3 review P2-2 — without the subreaper lease
the strict NOWAIT `waitid` confirmation reports ECHILD; without the
`waitpgid` group collection the zombies keep the group non-empty forever,
because under the lease there is no init fallback).
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
publication = root / 'src/pilotdeck-gateway/src/test_scratch_publication.rs'
stress = 'runtime::tools::builtin::grep::tests::script_spawn_survives_concurrent_forks'
residue = 'test_scratch_publication::failed_publication_removes_the_temporary_name'
stalled = 'test_scratch_publication::a_stalled_helper_is_killed_and_cleaned_up'
unread = 'test_scratch_publication::a_helper_that_never_reads_hits_the_write_deadline'
stranger = 'test_scratch_publication::a_stranger_on_the_read_end_still_hits_the_write_deadline'
dead_stranger = ('test_scratch_publication::'
                 'a_dead_helper_with_a_stranger_reader_still_closes_the_write_end')
panic_stranger = 'test_scratch_publication::a_panic_while_the_stranger_runs_still_ends_it'
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
    // #1141 fix-r2 review P1-1: the handover happens while the guard already
    // owns the helper but before any later step — taking the stdin, starting
    // the writer, the write itself, the waits — can fail or panic, so a panic
    // raised by the hook itself still unwinds through the guard's cleanup.
    let helper_id = guard
        .helper
        .as_ref()
        .expect("#1125 reap guard holds the helper")
        .id();
    spawned(helper_id);
    // #1155 r4 review P2: the expiry starts here, after the handover, so a
    // `spawned` hook that waits for the helper's descendants to settle (the
    // stranger confirming the read end) does not eat into the write budget.
    let expiry = Instant::now() + deadline;
    let mut stdin = guard
        .helper
        .as_mut()
        .expect("#1125 reap guard holds the helper")
        .stdin
        .take()
        .expect("#1125 helper stdin is piped");
    // #1155 r3 review P2-1: the pipe's inode identifies every fd of the
    // pipe in the return-boundary scan — both ends share it — and it must
    // be taken before the writer owns the write end.
    let pipe = pipe_inode(&stdin)?;
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
    // #1155 r3 review P2-1: the return boundary. `returning` proves — while
    // this function still owns the guard, so an error can still clean up —
    // that no write end of the pipe survives the return; its error takes
    // precedence over `published` so a leak surfaces even on a deadline
    // failure the test already expects.
    let boundary = returning(pipe);
    if boundary.is_ok() && published.is_ok() {
        // The wait loop reaped the helper and the rename consumed the
        // temporary name; the guard has nothing left to do.
        guard.disarm();
    }
    boundary.and(published)
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
    # The give-up must also close this process's copy of the write end: the
    # mutant leaks a clone past the deadline. #1155 r3 review P2-1: the leak
    # is now caught before the return — the publication's own /proc scan
    # finds the surviving write end and fails the publication with it, and
    # the scan's error takes precedence over the TimedOut the test expects —
    # so the deadline-kind assertion in the dead-helper test is the kill,
    # with the scan's message inside its panic payload; the stranger's later
    # marker verdict (`write-end-still-held`) is the second witness.
    ('deadline-write-end-kept-open', helper,
     '''                if Instant::now() >= expiry {
                    return Err(helper_deadline_error(deadline));
                }''',
     '''                if Instant::now() >= expiry {
                    use std::os::fd::AsFd;
                    std::mem::forget(stdin.as_fd().try_clone_to_owned());
                    return Err(helper_deadline_error(deadline));
                }''',
     dead_stranger, '#1141 deadline error must be TimedOut'),
    # #1141 fix-r2 review P2-1 (Codex): a write end held past the publication
    # must fail the return-boundary observation, whatever holds it. The
    # writer-side leak above covers the give-up clone; this one leaks the
    # parent's own dup before the writer even starts — the review's shape of
    # a copy surviving the return boundary — and holds it through the whole
    # return boundary scan, which is what kills it now (#1155 r3 review
    # P2-1, same rebind as above).
    ('parent-write-end-held-past-return', helper,
     '''    set_nonblocking(&mut stdin)?;
    let (sent, done) = mpsc::channel();''',
     '''    set_nonblocking(&mut stdin)?;
    use std::os::fd::AsFd;
    std::mem::forget(stdin.as_fd().try_clone_to_owned());
    let (sent, done) = mpsc::channel();''',
     dead_stranger, '#1141 deadline error must be TimedOut'),
    # #1141 fix-r1 review P1-1: the stranger must be ended by the test's own
    # guard, never left parked behind the pipe. The mutant drops the guard's
    # group kill, so the parked stranger outlives the whole test and the
    # group never empties: only the dead-helper test's reap assertion kills.
    ('stranger-guard-kill-removed', publication,
     '''        super::process_group::kill(Some(group), Signal::KILL);''',
     '''        let _ = group;''',
     dead_stranger, "#1141 the stranger must be killed and reaped by the test's guard"),
    # #1141 fix-r2 review P1-1 (Codex): the same kill, bound to the panic
    # test — a publication that panics while the stranger runs must still
    # end it, so the guard's end() after the caught unwind is the assertion
    # this mutant breaks.
    ('stranger-panic-guard-kill-removed', publication,
     '''        super::process_group::kill(Some(group), Signal::KILL);''',
     '''        let _ = group;''',
     panic_stranger, '#1141 the stranger must still be killed and confirmed gone after a panic'),
    # #1141 fix-r2 review P1-1 (Codex): the spawn id handover itself. The
    # mutant never invokes the `spawned` hook, so no panic ever happens in
    # the window and the publication runs to its ordinary deadline error:
    # the caught-Ok expect_err is the first thing that can fail, and the
    # guard's ownership assertion right after is what keeps a hook that
    # stops halfway from passing vacuously.
    ('spawn-hook-removed', helper,
     '''    spawned(helper_id);''',
     '''    let _ = (helper_id, &spawned);''',
     panic_stranger, '#1141 the publication must panic while the stranger is running'),
    # #1141 fix-r3 (Linux cb8): the stranger must confirm it holds the pipe
    # before anything else runs — the probe publishes `started` only once
    # `fstat(0)` shows a FIFO, and both the helper and the test's `spawned`
    # hook wait for that signal, so the writer cannot start and the helper
    # cannot die before the read end is provably held. The mutant inverts
    # the confirmation, so no `started` ever appears and the hook's expect
    # is the assertion that kills, on every unix host.
    ('stranger-confirmation-inverted', publication,
     '''if not stat.S_ISFIFO(os.fstat(0).st_mode):''',
     '''if stat.S_ISFIFO(os.fstat(0).st_mode):''',
     dead_stranger,
     '#1141 the stranger must confirm it holds the pipe read end before the publication writes'),
    # #1141 fix-r3 (Linux cb8): the fd-3 save is what holds the read end
    # across dash's async-rule /dev/null — dash replaces a background
    # command's stdin even behind an explicit `<&0`, which lost the read
    # end on Linux and killed the writer with EPIPE instead of its
    # deadline. The mutant reverts the helper to the r2 shape (no save, no
    # started handshake, no saw marker), so on Linux `started` never appears
    # and the spawned hook's expect kills; bash keeps the pipe behind `<&0`,
    # so `started` does appear on macOS — but the reverted helper never
    # writes the `saw` marker (#1155 r4 review P2), so the dead-helper
    # test's saw assertion kills there too.
    ('stranger-retention-reverted', publication,
     '''            "exec 3<&0
               python3 -c '{STRANGER_PROBE}' \\"$1\\" \\"$2\\" \\"$3\\" {delay_ms} <&3 &
               i=0
               while [ ! -e \\"$3\\" ] && [ \\"$i\\" -lt {polls} ]; do i=$((i+1)); /bin/sleep 0.05; done
               if [ -e \\"$3\\" ]; then printf saw >\\"$4.staging\\"; mv \\"$4.staging\\" \\"$4\\"; fi
               exit 0"''',
     '''            "python3 -c '{STRANGER_PROBE}' \\"$1\\" \\"$2\\" \\"$3\\" 0 <&0 &
               exit 0"''',
     dead_stranger,
     '#1141 the stranger must confirm it holds the pipe read end before the publication writes'),
    # #1155 r4 review P2: the write deadline must start after the `spawned`
    # hook's handshake, not before the spawn. The mutant hoists the expiry
    # back to the r4 shape, so the dead-helper test's 6-second started
    # handshake consumes the whole write budget: the writer gives up on its
    # first blocked write — elapsed since the hook's return is ~0, far under
    # the deadline floor — or the writer-exit race assert fires first when
    # the already-past expiry collapses the grace; either assertion kills.
    ('expiry-hoisted-before-spawn', helper,
     '''    spawned(helper_id);
    // #1155 r4 review P2: the expiry starts here, after the handover, so a
    // `spawned` hook that waits for the helper's descendants to settle (the
    // stranger confirming the read end) does not eat into the write budget.
    let expiry = Instant::now() + deadline;''',
     '''    let expiry = Instant::now() + deadline;
    spawned(helper_id);''',
     dead_stranger,
     ['#1155 the write phase must last its full deadline',
      '#1125 helper writer did not exit by its deadline']),
    # #1155 r4 review P2: the helper's started-wait cap is derived from the
    # test-side `STRANGER_STARTED_TIMEOUT` — one cap, not two. The mutant
    # re-hardcodes the old 5 s (100 polls), so against the 6-second delayed
    # `started` the helper gives up first, dies without leaving its `saw`
    # marker, and the dead-helper test's saw assertion kills; the
    # publication itself still behaves, which is exactly the r4 finding
    # (the drift was invisible to every other assertion).
    ('started-cap-hardcoded-short', publication,
     '''    STRANGER_STARTED_TIMEOUT.as_millis() / 50''',
     '''    100''',
     dead_stranger,
     "#1155 the helper must observe the stranger's started signal before it dies"),
    # #1155 r3 review P2-1: the scan must only flag write ends. The mutant
    # drops the access-mode filter, so the stranger's read end — a
    # `pipe:[inode]` fd like any other — fails the scan on the healthy path,
    # and the dead-helper test's deadline-kind assertion kills with the
    # scan's message in its payload.
    ('boundary-write-end-filter-removed', publication,
     '''                if access & 0o3 == 0 {''',
     '''                if false {''',
     dead_stranger, '#1141 deadline error must be TimedOut'),
    # #1155 r3 review P2-2: without the subreaper lease the orphaned
    # stranger lands on PID 1, so the guard's `waitpid` finds no child of
    # this process — an ECHILD — and the strict Linux path turns that into
    # an error instead of leaving the collection to init.
    ('stranger-subreaper-removed', publication,
     '''            rustix::process::set_child_subreaper(Some(mine))?;''',
     '''            let _ = mine;''',
     dead_stranger,
     "#1155 the stranger was not reparented to the test's guard"),
    # #1155 r3 review P2-2: the reaping itself. With the subreaper lease up
    # there is no init fallback — nobody else collects the zombie — so the
    # mutant that never waits leaves it in the process group forever and the
    # group-empty confirmation times out.
    ('stranger-reap-removed', publication,
     '''        #[cfg(target_os = "linux")]
        while let Ok(Some(_)) =
            rustix::process::waitpgid(group, rustix::process::WaitOptions::NOHANG)
        {}''',
     '''        #[cfg(target_os = "linux")]
        let _ = group;''',
     dead_stranger,
     "#1141 the stranger's process group survived the guard's kill"),
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
        with mutating_sources(root, [helper, source, publication]) as snapshots:
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
