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
KILLED. Those two race mutants run against the lib (`--lib`); every other
mutant is bound to a test of the dedicated publication integration binary
(`--test scratch_publication`), which owns its own process: the
child-subreaper lease the stranger guards take is process-wide, and in the
shared lib test process it deterministically broke the parallel grep group
tests (#1155 fix-r1 review P1; probe: 5 of 7 grep group tests
watchdog-expired in each of 3 dense runs). macOS uses posix_spawn and
never shows the write window, so the two race mutants are a Linux contract
(expected SURVIVED elsewhere), and so are the #1155 scan and reaping
mutants, which need `/proc` and `PR_SET_CHILD_SUBREAPER`: the scan's
access-mode filter (`boundary-write-end-filter-removed`), the fdinfo
fail-closed branches (`boundary-fdinfo-read-error` /
`boundary-fdinfo-parse-error`, #1155 fix-r1 review opus row 1, plus
`boundary-fdinfo-unknown-passed` for the release re-check, #1155 fix-r2b
review boundary P2), the subreaper lease (`stranger-subreaper-removed`),
the group reaping (`stranger-reap-removed`), and the pid-mode guard's
kill (`stranger-pid-kill-removed`, #1155 fix-r2b review boundary P3 —
the pid-mode test itself is Linux-only). The other fourteen must kill on
every unix host — `stranger-retention-reverted` included: bash (the macOS `/bin/sh`)
keeps the pipe behind an explicit `<&0`, so `started` still appears, but
the reverted helper writes no `saw` marker and the #1155 r4 saw assertion
kills it (under dash, the Linux `/bin/sh`, a background command's stdin
is /dev/null even behind an explicit `<&0`, so `started` never appears
and the hook's expect fails first) — the two failure points are one anchor
list, a list being a set of alternatives. The #1141 mutants target the
dead-helper stranger test's write-end evidence: the publication itself
now scans `/proc` at the return boundary (#1155 r3 review P2-1) — before
it returns — for any fd of its pipe open for writing, with the scan's
error taking precedence over the `TimedOut` the test expects, so any
mutant that leaks a write end — the writer's give-up
(`deadline-write-end-kept-open`) or the parent's own copy
(`parent-write-end-held-past-return`, fix-r2 review P2-1) — fails the
deadline-kind assertion with the scan's message in its payload on Linux,
while on a host without `/proc` the same leak falls through to the
stranger's later marker verdict, a not-ready read end; both failure
points are the assertion list those mutants are bound to (#1155 fix-r1
review DeepSeek P1-1). The writer's own give-up
(`writer-deadline-removed`, bound to that same dead-helper test since
fix-r1) is unix-wide: the writer-exit assert fires with no scan involved.
And the guards: the dead-helper test's group kill
(`stranger-guard-kill-removed`, fix-r1 review P1-1), plus the
spawn-window panic test, which proves the guard owns the group from the
spawn on (`spawn-hook-removed`) and still ends the stranger after an
in-flight panic (`stranger-panic-guard-kill-removed`, fix-r2 review
P1-1). The stranger parks in a bounded sleep that only that guard may
end. The fix-r3 handshake mutants prove the stranger must provably hold
the read end before anything else runs: the probe publishes `started`
only after `fstat(0)` shows a FIFO, and both the helper and the test's
`spawned` hook wait for that signal, so a mutant that inverts the
confirmation (`stranger-confirmation-inverted`) or drops the fd-3
retention save (`stranger-retention-reverted`) leaves the publication
blocked at the hook's expect. The #1155 mutants cover the four new
contracts: the deadline starts only after the `spawned` hook's handshake
(`expiry-hoisted-before-spawn`, r4 review P2 — the 8-second handshake
must not eat the write budget; a deadline that starts too early gives up
on the first blocked write, so the measured floor fails, or the
already-past expiry collapses the writer-exit grace and that assert fires
first — one anchor list for the two outcomes); the helper's and the
test's started-wait caps are one derived constant
(`started-cap-hardcoded-short`, r4 review P2 — a re-hardcoded 5-second
wall clock gives up 3 s of real time before the 8-second delayed `started`
on any host, so the helper dies without its saw marker), and a `started`
that appears after the cap is not recorded as seen at all
(`saw-write-skips-the-cap`, #1155 fix-r2b review Codex P1-1 — the saw
write re-validates the same deadline the loop watched, so even a poll
paused across the cap that resumes to find the file already there
refuses it; the late-started test controls the pause instead of gambling
on load); the
return-boundary scan flags write ends only
(`boundary-write-end-filter-removed`, r3 review P2-1 — without the
access-mode filter the stranger's read end fails the healthy path; bound
to the scan's own payload, #1155 fix-r1 review DeepSeek P2-2); and the
test — never PID 1 — reaps the stranger (`stranger-subreaper-removed` and
`stranger-reap-removed`, r3 review P2-2 — without the subreaper lease the
strict NOWAIT `waitid` confirmation reports ECHILD; without the
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
publication = root / 'src/pilotdeck-gateway/tests/scratch_publication.rs'
stress = 'runtime::tools::builtin::grep::tests::script_spawn_survives_concurrent_forks'
residue = 'failed_publication_removes_the_temporary_name'
stalled = 'a_stalled_helper_is_killed_and_cleaned_up'
unread = 'a_helper_that_never_reads_hits_the_write_deadline'
dead_stranger = 'a_dead_helper_with_a_stranger_reader_still_closes_the_write_end'
panic_stranger = 'a_panic_while_the_stranger_runs_still_ends_it'
late_started = 'a_started_past_the_helper_cap_is_not_seen_after_a_paused_poll'
pid_stranger = 'a_stranger_on_the_read_end_still_hits_the_write_deadline'
fdinfo_unit = 'an_unreadable_fdinfo_fails_closed_only_behind_a_live_fd'
# #1155 fix-r1 review P1: the two ETXTBSY races stay bound to the lib stress
# test; every publication mutant runs the dedicated integration binary, whose
# process owns the subreaper lease instead of the shared lib test process.
lib_target = ['--lib']
publication_target = ['--test', 'scratch_publication']
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
     lib_target, stress, '#1125 etxtbsy'),
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
     lib_target, stress, '#1125 etxtbsy'),
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
     publication_target, residue, '#1125 temp residue'),
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
     publication_target, stalled, ['#1125 stalled helper kill must be reported',
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
     publication_target, unread, '#1125 deadline error must be TimedOut'),
    # The writer must share the deadline: with the give-up removed it stays
    # blocked on a read end a stranger keeps open, and the parent's own
    # deadline cannot end it — only the writer-exit guard catches that
    # (#1125 fix-r3 review P2-2). #1155 fix-r1 review P2-1: rebound to the
    # dead-helper group test (the old pid-mode test is now a Linux-only
    # contract); its stranger holds the read end the same way, and the
    # writer-exit assert needs no `/proc`, so this kills on every unix host.
    ('writer-deadline-removed', helper,
     '''                if Instant::now() >= expiry {
                    return Err(helper_deadline_error(deadline));
                }''',
     '''                if false {
                    return Err(helper_deadline_error(deadline));
                }''',
     publication_target, dead_stranger, '#1125 helper writer did not exit by its deadline'),
    # The give-up must also close this process's copy of the write end: the
    # mutant leaks a clone past the deadline. #1155 r3 review P2-1: on Linux
    # the leak is caught before the return — the publication's own /proc
    # scan finds the surviving write end and fails the publication with it,
    # and the scan's error takes precedence over the TimedOut the test
    # expects — so the deadline-kind assertion is the kill, with the scan's
    # message inside its panic payload. On a host without `/proc` the scan
    # does not run, the publication returns TimedOut, and the stranger's
    # later marker verdict (`write-end-still-held`) is the failure point
    # instead — one anchor list, both platforms deterministic (#1155 fix-r1
    # review DeepSeek P1-1).
    ('deadline-write-end-kept-open', helper,
     '''                if Instant::now() >= expiry {
                    return Err(helper_deadline_error(deadline));
                }''',
     '''                if Instant::now() >= expiry {
                    use std::os::fd::AsFd;
                    std::mem::forget(stdin.as_fd().try_clone_to_owned());
                    return Err(helper_deadline_error(deadline));
                }''',
     publication_target, dead_stranger,
     ['#1141 deadline error must be TimedOut',
      '#1141 the write end must be closed by the time the publication returns']),
    # #1141 fix-r2 review P2-1 (Codex): a write end held past the publication
    # must fail the return-boundary observation, whatever holds it. The
    # writer-side leak above covers the give-up clone; this one leaks the
    # parent's own dup before the writer even starts — the review's shape of
    # a copy surviving the return boundary — and holds it through the whole
    # return boundary scan, which is what kills it on Linux (#1155 r3
    # review P2-1); without `/proc` the stranger's marker verdict kills
    # instead, the same anchor list as above (#1155 fix-r1 review DeepSeek
    # P1-1).
    ('parent-write-end-held-past-return', helper,
     '''    set_nonblocking(&mut stdin)?;
    let (sent, done) = mpsc::channel();''',
     '''    set_nonblocking(&mut stdin)?;
    use std::os::fd::AsFd;
    std::mem::forget(stdin.as_fd().try_clone_to_owned());
    let (sent, done) = mpsc::channel();''',
     publication_target, dead_stranger,
     ['#1141 deadline error must be TimedOut',
      '#1141 the write end must be closed by the time the publication returns']),
    # #1141 fix-r1 review P1-1: the stranger must be ended by the test's own
    # guard, never left parked behind the pipe. The mutant drops the guard's
    # group kill, so the parked stranger outlives the whole test: the
    # reparented-confirmation reports it never died, and the end() expect
    # of the dead-helper test is the assertion that kills.
    ('stranger-guard-kill-removed', publication,
     '''            let _ = rustix::process::kill_process_group(group, Signal::KILL);''',
     '''            let _ = group;''',
     publication_target, dead_stranger,
     "#1141 the stranger must be killed and reaped by the test's guard"),
    # #1141 fix-r2 review P1-1 (Codex): the same kill, bound to the panic
    # test — a publication that panics while the stranger runs must still
    # end it, so the guard's end() after the caught unwind is the assertion
    # this mutant breaks.
    ('stranger-panic-guard-kill-removed', publication,
     '''            let _ = rustix::process::kill_process_group(group, Signal::KILL);''',
     '''            let _ = group;''',
     publication_target, panic_stranger,
     '#1141 the stranger must still be killed and confirmed gone after a panic'),
    # #1141 fix-r2 review P1-1: the spawn id handover itself. The
    # mutant never invokes the `spawned` hook, so no panic ever happens in
    # the window and the publication runs to its ordinary deadline error:
    # the caught-Ok expect_err is the first thing that can fail, and the
    # guard's ownership assertion right after is what keeps a hook that
    # stops halfway from passing vacuously.
    ('spawn-hook-removed', helper,
     '''    spawned(helper_id);''',
     '''    let _ = (helper_id, &spawned);''',
     publication_target, panic_stranger, '#1141 the publication must panic while the stranger is running'),
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
     publication_target, dead_stranger,
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
    # test's saw assertion kills there too. One anchor list covers the two
    # hosts (#1155 fix-r1 review, opus row 3).
    ('stranger-retention-reverted', publication,
     '''            "exec 3<&0
               end=$(( $(date +%s) + {wait_secs} ))
               python3 -c '{STRANGER_PROBE}' \\"$1\\" \\"$2\\" \\"$3\\" {delay_ms} <&3 &
               while [ ! -e \\"$3\\" ] && [ \\"$(date +%s)\\" -lt \\"$end\\" ]; do /bin/sleep {poll_secs}; done
               if [ -e \\"$3\\" ] && [ \\"$(date +%s)\\" -lt \\"$end\\" ]; then printf saw >\\"$4.staging\\"; mv \\"$4.staging\\" \\"$4\\"; fi
               exit 0"''',
     '''            "python3 -c '{STRANGER_PROBE}' \\"$1\\" \\"$2\\" \\"$3\\" 0 <&0 &
               exit 0"''',
     publication_target, dead_stranger,
     ['#1141 the stranger must confirm it holds the pipe read end before the publication writes',
      "#1155 the helper must observe the stranger's started signal before it dies"]),
    # #1155 r4 review P2: the write deadline must start after the `spawned`
    # hook's handshake, not before the spawn. The mutant hoists the expiry
    # back to the r4 shape, so the dead-helper test's 8-second started
    # handshake consumes the whole write budget: the writer gives up on its
    # first blocked write — elapsed since the hook's return is ~0, far under
    # the deadline floor — or the writer-exit race assert fires first when
    # the already-past expiry collapses the grace; either assertion kills
    # (one anchor list, both deterministic).
    ('expiry-hoisted-before-spawn', helper,
     '''    spawned(helper_id);
    // #1155 r4 review P2: the expiry starts here, after the handover, so a
    // `spawned` hook that waits for the helper's descendants to settle (the
    // stranger confirming the read end) does not eat into the write budget.
    let expiry = Instant::now() + deadline;''',
     '''    let expiry = Instant::now() + deadline;
    spawned(helper_id);''',
     publication_target, dead_stranger,
     ['#1155 the write phase must last its full deadline',
      '#1125 helper writer did not exit by its deadline']),
    # #1155 r4 review P2: the helper's started-wait cap is derived from the
    # test-side `STRANGER_STARTED_TIMEOUT` — one cap, not two. The mutant
    # re-hardcodes the old 5 s on the helper's wall clock (the wait itself
    # is `date +%s`-based, so the bound is real time and load cannot
    # stretch it, #1155 fix-r1 review), so against the 8-second delayed
    # `started` the helper gives up first, dies without leaving its `saw`
    # marker, and the dead-helper test's saw assertion kills; the
    # publication itself still behaves, which is exactly the r4 finding
    # (the drift was invisible to every other assertion). #1155 fix-r2b
    # review: the derivation now adds the `+ 1` truncation compensation —
    # the anchor covers it — and the saw write re-validates the deadline,
    # so the 5 s mutant is refused there too even if a paused poll resumes
    # to find the file already present.
    ('started-cap-hardcoded-short', publication,
     '''    STRANGER_STARTED_TIMEOUT.as_secs() + 1''',
     '''    5''',
     publication_target, dead_stranger,
     "#1155 the helper must observe the stranger's started signal before it dies"),
    # #1155 fix-r2b review (Codex P1-1): a started that appears after the
    # helper's cap must not be recorded as seen — even when the helper's poll
    # was paused across the deadline so the loop resumes to find the file
    # already there (the file-exists short-circuit skipped the time check).
    # The mutant drops the deadline re-check at the saw write, so the
    # late-started test — one ten-second poll round, cap 1 s, started at the
    # 2 s delay, all controlled, no load gamble — records the late started as
    # seen and its saw assertion kills, deterministically on any unix host.
    ('saw-write-skips-the-cap', publication,
     '''               if [ -e \\"$3\\" ] && [ \\"$(date +%s)\\" -lt \\"$end\\" ]; then printf saw''',
     '''               if [ -e \\"$3\\" ]; then printf saw''',
     publication_target, late_started,
     "#1155 a started past the helper's cap must not be recorded as seen after a paused poll"),
    # #1155 r3 review P2-1: the scan must only flag write ends. The mutant
    # drops the access-mode filter, so the stranger's read end — a
    # `pipe:[inode]` fd like any other — fails the scan on the healthy path.
    # #1155 fix-r1 review DeepSeek P2-2: bound to the scan's own payload —
    # the deadline-kind assertion of the dead-helper test kills with the
    # scan's message inside it — so the binding no longer depends on which
    # downstream assertion the scan error displaces.
    ('boundary-write-end-filter-removed', publication,
     '''                if access & 0o3 == 0 {''',
     '''                if false {''',
     publication_target, dead_stranger, '#1155 a write end of the publication pipe'),
    # #1155 fix-r1 review opus row 1: the scan must fail closed. A fdinfo
    # that cannot be read is an unknown — not a read end — and only the fd's
    # own disappearance (ENOENT between the readlink and the read) proves
    # the copy was released. The mutant appends a path separator to the
    # fdinfo path, so the read fails deterministically with ENOTDIR on the
    # healthy path (the stranger's read end), and the fail-closed branch
    # fails the scan there.
    ('boundary-fdinfo-read-error', publication,
     '''"/proc/{pid}/fdinfo/{}"''',
     '''"/proc/{pid}/fdinfo/{}/"''',
     publication_target, dead_stranger, '#1155 reading the fdinfo'),
    # #1155 fix-r1 review opus row 1: the other unknown — a fdinfo whose
    # flags line cannot be parsed must fail the scan too, not pass as a
    # read end. The mutant breaks the prefix the parser looks for, so no
    # line ever matches and the parse branch fails on the healthy path.
    ('boundary-fdinfo-parse-error', publication,
     '''.strip_prefix("flags:")''',
     '''.strip_prefix("flagsX:")''',
     publication_target, dead_stranger, 'has no parsable flags line'),
    # #1155 fix-r2b review (boundary P2 / concurrency P3): an fdinfo read
    # that fails behind an fd that still resolves to the pipe must fail the
    # scan closed — the read can fail for reasons other than the fd's own
    # disappearance (ESRCH in teardown, EACCES behind a setuid exec; cb8
    # probe), so the release is proven by the fd being gone, re-read inside
    # `fdinfo_unreadable`. The mutant makes the re-read never confirm a live
    # fd, so the unit test's live-fd half fails at its expect while its
    # gone-fd half pins the release side (Linux contract: /proc).
    ('boundary-fdinfo-unknown-passed', publication,
     '''        .is_some_and(|target| target.to_string_lossy() == wanted)''',
     '''        .is_some_and(|target| false)''',
     publication_target, fdinfo_unit,
     '#1155 an unreadable fdinfo behind a live fd must fail the scan closed'),
    # #1155 r3 review P2-2: without the subreaper lease the orphaned
    # stranger lands on PID 1, so the guard's `waitid` finds no child of
    # this process — an ECHILD — and the strict Linux path turns that into
    # an error instead of leaving the collection to init.
    ('stranger-subreaper-removed', publication,
     '''            rustix::process::set_child_subreaper(Some(mine))?;''',
     '''            let _ = mine;''',
     publication_target, dead_stranger,
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
     publication_target, dead_stranger,
     "#1141 the stranger's process group survived the guard's kill"),
    # #1155 fix-r2b review (boundary P3): the pid-mode guard's kill itself
    # had no negative control — its stranger now parks (sleep 30) instead of
    # self-exiting at 5 s, so a guard that never kills leaves it alive and
    # the reparented-confirmation times out with "did not die by the guard's
    # kill"; the end() expect of the pid-mode test is the assertion that
    # kills (Linux contract: the pid-mode test is Linux-only). The kill-error
    # propagation half of the fix-r1 tightening stays defense-in-depth
    # without a mutant of its own: under the subreaper lease the stranger's
    # zombie pins its pid, so a real kill cannot fail ESRCH on any path this
    # binary can reach.
    ('stranger-pid-kill-removed', publication,
     '''        rustix::process::kill_process(stranger, Signal::KILL).map_err(|error| {
            io::Error::other(format!(
                "#1155 the guard's stranger kill could not be sent: {error}"
            ))
        })?;''',
     '''        let _ = stranger;''',
     publication_target, pid_stranger,
     "#1155 the stranger must be killed and reaped by the test's guard, not PID 1's"),
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
    base = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
            '-p', 'pilotdeck-gateway']
    results = []

    def run(label, target, test, compile_only=False):
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
            (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            suffix = target + (['--no-run'] if compile_only else [test, '--', '--exact'])
            result = run_owned(base + suffix, cwd=root, env=env, timeout=900)
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
            for name, path, old, new, target, test, assertion in selected:
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: expected one source anchor')
                    if not args.compile_only and not passed(run(name + '-positive', target, test), test):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        if run(name + '-compile', target, test, compile_only=True).returncode != 0:
                            reason = 'mutant failed to compile'
                        elif args.compile_only:
                            status, reason = 'COMPILED', 'mutant cargo test --no-run passed'
                        else:
                            status, reason = classify(run(name + '-negative', target, test),
                                Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not args.compile_only and not passed(run(name + '-restored', target, test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, test, assertion, status, reason)
    except Exception as error:
        reason = f'source guard: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _path, _old, _new, _target, test, assertion in selected[len(results):]:
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
        for name, path, old, _new, _target, _test, _assertion in cases:
            count = path.read_text().count(old)
            if count != 1:
                stale.append((name, count))
        if stale:
            raise SystemExit(f'STALE ANCHORS: {stale}')
        print(f'{len(cases)} anchors: ok')
        raise SystemExit(0)
    raise SystemExit(main())
