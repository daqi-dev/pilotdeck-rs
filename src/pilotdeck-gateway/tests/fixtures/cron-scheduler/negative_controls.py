#!/usr/bin/env python3
"""#2295 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.
`--check` proves each mutation anchor is unique and that the README manifest
hand-copies (mutation count, Rows list, gold-cell counts, bound test names)
still equal this file's `mutations` table, `oracle.json`, and
`scheduler_tests.rs`; it cannot prove the row's assertion string ever reaches
a panic payload, so a full run is the only proof that a row can go KILLED.

Rows bind one mutation to one lib test (run with `--exact`); the kill evidence
is that test's own panic payload carrying its `#2295` message. Four rows bind
`runtime::cron::scheduler_tests::scheduler_replays_the_ts_gold` — the gold
replay reads the committed oracle bytes, needs no Node, and each row breaks a
different cell first: poke-no-reschedule breaks `poke-fires-early` (the poke
disarms instead of re-arming, so the 250ms fast-forward never fires),
tick-not-rescheduled breaks `tick-failure-recovers` (the restore is followed
by an advance that can no longer tick), isdue-inverted and fire-not-spawned
break `due-once-fires` (the due seed never fires / fires into a warn). The
stop row cannot bind the replay: manager.stop() drops the runtime and the
scheduler thread exits through its weak-upgrade check before any post-stop
tick could run, so the gold observation would match anyway — it binds
`start_after_stop_does_not_revive`, whose first deterministic failure is the
armed-delay assertion right after the restart attempt (the mutated stop leaves
the deadline armed, so `armed_tick_delay_ms()` is `Some` whether or not a tick
fired first). The remaining rows pin the observation seams and the fix-r1
liveness properties the replay cannot see: start-arms-nothing pins
`armed_tick_delay_ms` after start, delay-skipped pins the constant-60s
deferral on the direct scheduler, re-register-clobbers-stop pins the
pre-register-window merge, tick-panic-kills-thread pins the panic fold into
the warn, panic-unregister-gone pins the fire's unwind unregister. The fix-r2
rows pin the review's two findings: failed-start-resets-running breaks the
running latch the rejected start must keep (the second start re-runs the
recalculation against the still-broken store), failed-start-loses-seat moves
the spawn back after the recalculation (the reviewed bug: a failed start
leaves the deadline without a consumer, so the poked tick never fires),
floorless-idle-poll drops the MIN_TIMER_MS floor from the arm helper (a 0ms
poll arms at "now", either read as Some(0) or raced to an unarmed moment, and
hot-loops the now seam past the 700ms bound).
"""
from pathlib import Path
import json
import os
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, declares_test, passed

root = Path(__file__).resolve().parents[5]
scheduler = root / 'src/pilotdeck-gateway/src/runtime/cron/scheduler.rs'
manager = root / 'src/pilotdeck-gateway/src/runtime/cron/manager.rs'
fire = root / 'src/pilotdeck-gateway/src/runtime/cron/fire.rs'
tests = root / 'src/pilotdeck-gateway/src/runtime/cron/scheduler_tests.rs'
oracle = root / 'src/pilotdeck-gateway/tests/fixtures/cron-scheduler/oracle.json'
REPLAY = 'runtime::cron::scheduler_tests::scheduler_replays_the_ts_gold'
RESTART = 'runtime::cron::scheduler_tests::start_after_stop_does_not_revive'
ARMED = 'runtime::cron::scheduler_tests::start_arms_the_idle_poll_and_poke_floors_at_250ms'
CAPACITY = 'runtime::cron::scheduler_tests::capacity_defers_by_the_constant_sixty_seconds'
PREREGISTER = 'runtime::cron::scheduler_tests::stop_in_the_pre_register_window_survives_the_re_register'
TICKPANIC = 'runtime::cron::scheduler_tests::tick_panic_folds_into_a_warn_and_re_arms'
SUBMITPANIC = 'runtime::cron::scheduler_tests::submit_panic_still_unregisters'
FAILEDSTART = 'runtime::cron::scheduler_tests::poke_after_a_failed_start_recalculation_fires_the_due_task'
FLOOR = 'runtime::cron::scheduler_tests::idle_poll_below_the_floor_arms_at_the_floor'
mutations = [
    # The runtime's stop never stops the scheduler, so the armed poll survives
    # the stop; the restart-holding test's first deterministic failure is the
    # armed-delay assertion right after the restart attempt (the replay
    # cannot bind this: manager.stop() drops the runtime and the thread exits
    # through its weak-upgrade check before ticking).
    ('stop-not-wired', manager,
     '    pub fn stop(&self) -> Result<(), CronError> {\n'
     '        if let Some(scheduler) = self.bound_scheduler() {\n'
     '            scheduler.stop();\n'
     '        }',
     '    pub fn stop(&self) -> Result<(), CronError> {\n'
     '        if let Some(scheduler) = self.bound_scheduler() {\n'
     '            let _ = scheduler;\n'
     '        }',
     RESTART,
     '#2295 the stopped scheduler must not re-arm'),
    # A same-runId re-register goes back to whole-entry replacement, so the
    # stop the pre-register window test requests before opening the gate is
    # clobbered and the terminal record reads "completed".
    ('re-register-clobbers-stop', manager,
     '            Some(existing) => {\n'
     '                let stop_requested = existing.stop_requested || run.stop_requested;\n'
     '                *existing = run;\n'
     '                existing.stop_requested = stop_requested;\n'
     '            }',
     '            Some(existing) => *existing = run,',
     PREREGISTER,
     "#2295 a stop requested in the pre-register window must survive the fire's re-register"),
    # The caught panic is resumed instead of folded, killing the scheduler
    # thread, so the warn the test waits for never appears.
    ('tick-panic-kills-thread', scheduler,
     '                Err(panic) => Err(CronError::error(JsString::from(panic_message(&panic)))),',
     '                Err(panic) => std::panic::resume_unwind(panic),',
     TICKPANIC,
     '#2295 timed out waiting for the panicked tick to fold into a warn'),
    # The unwind guard is born disarmed, so a panicking submit seam skips the
    # unregister and the run leaks into the registry.
    ('panic-unregister-gone', fire,
     '        let mut unregister = UnregisterOnUnwind {\n'
     '            unregister: &self.deps.unregister_active_run,\n'
     '            run_id,\n'
     '            armed: true,\n'
     '        };',
     '        let mut unregister = UnregisterOnUnwind {\n'
     '            unregister: &self.deps.unregister_active_run,\n'
     '            run_id,\n'
     '            armed: false,\n'
     '        };',
     SUBMITPANIC,
     '#2295 a panicking submit seam must not leak the active run'),
    # The poke disarms instead of re-arming at the 250ms floor, so the
    # poke-fires-early cell's fast-forward never ticks.
    ('poke-no-reschedule', scheduler,
     '        state.deadline = Some(Instant::now() + Duration::from_millis(MIN_TIMER_MS));',
     '        state.deadline = None;',
     REPLAY,
     '#2295 gold poke-fires-early observation final: gatewayCalls differ from the TS gold'),
    # The tick never re-arms, so the restore in tick-failure-recovers is
    # followed by an advance that can no longer fire the due seed.
    ('tick-not-rescheduled', scheduler,
     '            if !state.stopped\n'
     '                && state.running\n'
     '                && inner.deps.config.enabled\n'
     '                && state.deadline.is_none()\n'
     '            {',
     '            if false {',
     REPLAY,
     '#2295 gold tick-failure-recovers observation final: gatewayCalls differ from the TS gold'),
    # start leaves the timer seat empty, so the armed-delay observation has
    # nothing to read (the replay would also miss fires, but this row pins the
    # observation seam directly).
    ('start-arms-nothing', scheduler,
     '                if !state.stopped && state.running && enabled && state.deadline.is_none() {',
     '                if false {',
     ARMED,
     '#2295 start must arm the idle poll'),
    # isDue inverts, so the due seed of the first gold cell never fires.
    ('isdue-inverted', scheduler,
     '        ComputedRunAt::Valid(ms) if ms <= now_ms',
     '        ComputedRunAt::Valid(ms) if ms > now_ms',
     REPLAY,
     '#2295 gold due-once-fires observation final: gatewayCalls differ from the TS gold'),
    # The capacity branch never defers, so the direct-scheduler test's NoFire
    # port runs and the "over the limit" guard trips.
    ('delay-skipped', scheduler,
     '        if (deps.active_run_count)() as f64 >= deps.config.max_concurrent_runs {',
     '        if false && (deps.active_run_count)() as f64 >= deps.config.max_concurrent_runs {',
     CAPACITY,
     '#2295 a task over the limit must not fire'),
    # The fire thread body never submits, so the first gold cell records a
    # "cron fire failed" warn instead of the turn.
    ('fire-not-spawned', scheduler,
     '                let result = fire.run_task(&task, &fire_run_id);',
     '                let result: Result<(), CronError> = Err(CronError::error('
     'JsString::from("#2295 fire not spawned")));',
     REPLAY,
     '#2295 gold due-once-fires observation final: gatewayCalls differ from the TS gold'),
    # fix-r2: the rejected start resets running, so the second start re-runs
    # the recalculation against the still-broken store and fails instead of
    # being the running-gate no-op.
    ('failed-start-resets-running', scheduler,
     '            // The rejected start leaves the seat waiting unarmed — the exact\n'
     '            // TS state, revived by a later poke.\n'
     '            Err(error) => Err(error),',
     '            // The rejected start leaves the seat waiting unarmed — the exact\n'
     '            // TS state, revived by a later poke.\n'
     '            Err(error) => {\n'
     '                state.running = false;\n'
     '                Err(error)\n'
     '            }',
     FAILEDSTART,
     '#2295 the running gate must make the second start a no-op despite the broken store'),
    # fix-r2: the seat spawns only after a successful recalculation (the
    # reviewed bug), so the failed start leaves the poked deadline without a
    # consumer and the due task never fires.
    ('failed-start-loses-seat', scheduler,
     '            state.running = true;\n'
     '            // The seat spawns before the recalculation (the stopped gate\n'
     '            // above just ran under the same lock, so no join can be missed);\n'
     '            // a stop that lands while the lock is down for the recalculation\n'
     '            // takes this handle with it and joins it, so no seat is leaked.\n'
     '            if state.thread.is_none() {\n'
     '                let inner = self.inner.clone();\n'
     '                match thread::Builder::new()\n'
     '                    .name("cron-scheduler".into())\n'
     '                    .spawn(move || scheduler_thread(inner))\n'
     '                {\n'
     '                    Ok(handle) => state.thread = Some(handle),\n'
     '                    Err(error) => {\n'
     '                        // A spawn failure (EAGAIN/resource exhaustion)\n'
     '                        // must not latch `running`: the gate above would\n'
     '                        // swallow every later start() retry with a\n'
     '                        // spurious Ok.\n'
     '                        state.running = false;\n'
     '                        drop(state);\n'
     '                        self.inner.signal.notify_all();\n'
     '                        return Err(CronError::error(JsString::from(error.to_string())));\n'
     '                    }\n'
     '                }\n'
     '            }\n'
     '        }\n'
     '        let recalc = recalculate_all_next_runs(&self.inner.deps);\n'
     '        let mut state = lock(&self.inner.state);\n'
     '        match recalc {\n'
     '            // The rejected start leaves the seat waiting unarmed — the exact\n'
     '            // TS state, revived by a later poke.\n'
     '            Err(error) => Err(error),\n'
     '            Ok(()) => {\n',
     '            state.running = true;\n'
     '        }\n'
     '        let recalc = recalculate_all_next_runs(&self.inner.deps);\n'
     '        let mut state = lock(&self.inner.state);\n'
     '        match recalc {\n'
     '            // The rejected start leaves the seat waiting unarmed — the exact\n'
     '            // TS state, revived by a later poke.\n'
     '            Err(error) => Err(error),\n'
     '            Ok(()) => {\n'
     '                if state.thread.is_none() && !state.stopped {\n'
     '                    let inner = self.inner.clone();\n'
     '                    match thread::Builder::new()\n'
     '                        .name("cron-scheduler".into())\n'
     '                        .spawn(move || scheduler_thread(inner))\n'
     '                    {\n'
     '                        Ok(handle) => state.thread = Some(handle),\n'
     '                        Err(error) => {\n'
     '                            state.running = false;\n'
     '                            drop(state);\n'
     '                            self.inner.signal.notify_all();\n'
     '                            return Err(CronError::error(JsString::from(error.to_string())));\n'
     '                        }\n'
     '                    }\n'
     '                }\n',
     FAILEDSTART,
     '#2295 timed out waiting for the poked tick to fire the due task after the failed start'),
    # fix-r2: the arm helper drops the MIN_TIMER_MS floor, so a 0ms poll arms
    # at "now" — read back as Some(0) (or raced to an unarmed moment) and the
    # 700ms window hot-loops the now seam.
    ('floorless-idle-poll', scheduler,
     '    Instant::now().checked_add(Duration::from_millis(idle_poll_ms.max(MIN_TIMER_MS)))',
     '    Instant::now().checked_add(Duration::from_millis(idle_poll_ms))',
     FLOOR,
     ['#2295 start must arm the floored idle poll',
      '#2295 an idle poll below the floor must arm at the 250ms floor, got 0ms',
      '#2295 the floored idle poll must not hot-loop']),
]


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


readme = Path(__file__).with_name('README.md')


def check_manifest():
    """Every hand-copied number must equal the artifact it describes.

    The README's mutation count and `Rows:` list, its gold-cell counts, and
    the `--exact` test name of each mutation, are hand-maintained copies of
    this file's `mutations` table, of `oracle.json`, and of
    `scheduler_tests.rs`. A copy that drifts is invisible until the row it
    names is deleted, so `--check` derives them from the sources.
    """
    errors = []
    # The count lives in the negative_controls.py bullet of this README's
    # Chinese file list (cron-schedule has a per-section counts table instead).
    # Exactly one: a duplicated bullet whose copy drifted must not hide behind
    # whichever copy re.search happened to hit first.
    text = readme.read_text(encoding='utf-8')
    found = re.findall(r'`negative_controls\.py` — (\d+) 变异负控', text)
    if not found:
        errors.append('README: no count row for negative_controls.py mutations')
    elif len(found) != 1:
        errors.append(f'README: expected exactly one negative_controls.py count row, '
                      f'found {len(found)}')
    elif int(found[0]) != len(mutations):
        errors.append(f'README: negative_controls.py says {found[0]} mutations, '
                      f'script has {len(mutations)}')
    # The list lives in its own paragraph (`Rows: `a`, `b`, …`), so scope the
    # scan to that paragraph: prose elsewhere in the README that mentions the
    # list would otherwise be swept into `listed` and compared as rows.
    blocks = [block for block in re.split(r'\n\s*\n', text) if 'Rows:' in block]
    listed = (re.findall(r'`([a-z0-9-]+)`', blocks[0].split('Rows:', 1)[1])
              if len(blocks) == 1 else [])
    if len(blocks) != 1:
        errors.append(f'README: expected exactly one Rows paragraph, found {len(blocks)}')
    expected_rows = [row[0] for row in mutations]
    if listed != expected_rows:
        errors.append(f"README: Rows list {listed} != mutations {expected_rows}")
    # The gold-cell count is another hand copy: the README file bullet says
    # `oracle.json — N 格金样` and the sh-block comment says `含 N 格字节比对`;
    # oracle.json's cells are the artifact both copies describe.
    oracle_payload = json.loads(oracle.read_text())
    # 修复轮 r6 P2（DS sibling#2，always-on 侧 fix-r3 空 oracle 拒绝的同族）：
    # oracle 被清空成 {"cells": []}（或整份 {} / 缺 cells 键）且 README 两处
    # 金样计数同步清零时，下面的 0==0 核对全部空心通过——先拒绝空清单本身，
    # --check 对被清空的 oracle.json 不再绿灯。
    cells = len(oracle_payload.get('cells') or [])
    if not cells:
        errors.append('oracle.json: the gold-cell manifest is empty')
    for pattern, where in ((r'`oracle\.json` — (\d+) 格金样', 'file list'),
                           (r'含 (\d+) 格字节比对', 'replay comment')):
        found = re.findall(pattern, text)
        if not found:
            errors.append(f'README: no gold-cell count row ({where})')
        elif len(found) != 1:
            errors.append(f'README: expected exactly one gold-cell count row ({where}), found {len(found)}')
        elif int(found[0]) != cells:
            errors.append(f'README: gold cells at {where} say {found[0]}, oracle has {cells}')
    # declares_test (negative_suite, #2064 r10) blanks comments and string
    # literals before matching, so a binding that only a comment carrier
    # satisfies is rejected instead of passing --check hollowly.
    scheduler_test_source = tests.read_text()
    for row in mutations:
        ident = row[4].rsplit('::', 1)[-1]
        if not declares_test(scheduler_test_source, ident):
            errors.append(f'{row[0]}: bound test {ident} is not defined in scheduler_tests.rs')
    return errors


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check_anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        errors.extend(check_manifest())
        print('\n'.join(errors) if errors else
               f'#2295 {len(mutations)} unique anchors and fixture manifest checked')
        return int(bool(errors))
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    if not args:
        raise SystemExit('provide NEW_OUT and optional mutation names, or --check')
    names = args[1:]
    selected = [row for row in mutations if not names or row[0] in names]
    if not selected or (names and len(selected) != len(names)):
        raise SystemExit('unknown or duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    summary = []
    fixture = Path(__file__).resolve().parent
    guarded = list(dict.fromkeys([scheduler, manager, fire, tests, oracle, Path(__file__).resolve(),
        fixture / 'generate.mts', fixture / 'probe.mts', fixture / 'README.md',
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py']))

    def save(name, text):
        with (out / name).open('x') as stream:
            stream.write(text)

    def run(label, test, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        save(label + '.slots.log', slot.stdout + slot.stderr)
        # negative_suite needs libtest's named result lines, including positives.
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--lib']
        if compile:
            command.append('--no-run')
        else:
            command += [test, '--', '--exact', '--show-output']
        save(label + '.command.json', json.dumps(command) + '\n')
        try:
            result = run_owned(command, cwd=root / 'src', env=env, timeout=300)
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes)
                              else value or '' for value in (error.stdout, error.stderr))
            save(label + '.log', raw + f'\n{type(error).__name__}: {error}\n')
            raise
        save(label + '.log', result.stdout + result.stderr)
        return result

    for row in selected:
        name, path, old, new, test, assertion = row
        item = dict(name=name, test=test, assertion=assertion,
                    status='INVALID', reason='not executed', compiled=False)
        try:
            # No caller-supplied baseline, dirty-file snapshot, or alternate git.
            with mutating_sources(root, guarded) as originals:
                original = originals[path].decode()
                check_anchor(row, original)
                if not compile_only and not passed(run(name + '-positive', test), test):
                    raise RuntimeError('bound positive control did not pass')
                try:
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', test, compile=True).returncode != 0:
                        raise RuntimeError('mutant failed compilation; not a runtime kill')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED', reason='cargo test --no-run passed')
                    else:
                        case = Case(name, path, old, new, test, assertion)
                        status, reason = classify(run(name + '-negative', test), case)
                        item.update(status=status, reason=reason)
                finally:
                    # Bytes come only from the HEAD-validated guard; it also
                    # restores and verifies every guarded source on exit.
                    path.write_bytes(originals[path])
                if not passed(run(name + '-restored', test), test):
                    raise RuntimeError('bound restored control did not pass')
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        summary.append(item)
        print(f"{name}: {item['status']}: {item['reason']}", flush=True)
    save('summary.json', json.dumps(summary, indent=2) + '\n')
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED', 'SKIPPED')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    success = 'COMPILED' if compile_only else 'KILLED'
    return int(not summary
               or not counts[success]
               or any(row['status'] not in (success, 'SKIPPED') for row in summary))


if __name__ == '__main__':
    raise SystemExit(main())
