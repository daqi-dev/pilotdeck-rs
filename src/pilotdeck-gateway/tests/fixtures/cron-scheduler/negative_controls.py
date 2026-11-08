#!/usr/bin/env python3
"""#2295 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.
`--check` only proves each mutation anchor is unique; it cannot prove the row's
assertion string ever reaches a panic payload, so a full run is the only proof
that a row can go KILLED.

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
`start_after_stop_does_not_revive`, which holds the runtime Arc through the
stop and asserts on the post-restart window (poll 100ms, sleep 300ms). The
remaining two rows pin the observation seams the replay cannot see:
start-arms-nothing pins `armed_tick_delay_ms` after start, delay-skipped pins
the constant-60s deferral on the direct scheduler.
"""
from pathlib import Path
import json
import os
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
scheduler = root / 'src/pilotdeck-gateway/src/runtime/cron/scheduler.rs'
manager = root / 'src/pilotdeck-gateway/src/runtime/cron/manager.rs'
tests = root / 'src/pilotdeck-gateway/src/runtime/cron/scheduler_tests.rs'
oracle = root / 'src/pilotdeck-gateway/tests/fixtures/cron-scheduler/oracle.json'
REPLAY = 'runtime::cron::scheduler_tests::scheduler_replays_the_ts_gold'
RESTART = 'runtime::cron::scheduler_tests::start_after_stop_does_not_revive'
ARMED = 'runtime::cron::scheduler_tests::start_arms_the_idle_poll_and_poke_floors_at_250ms'
CAPACITY = 'runtime::cron::scheduler_tests::capacity_defers_by_the_constant_sixty_seconds'
mutations = [
    # The runtime's stop never stops the scheduler, so the armed poll survives
    # the stop; the restart-holding test sees the fire window reopen (the
    # replay cannot: manager.stop() drops the runtime and the thread exits
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
     '#2295 nothing may fire after the restart attempt'),
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
]


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check_anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        print('\n'.join(errors) if errors else f'#2295 {len(mutations)} unique anchors checked')
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
    guarded = list(dict.fromkeys([scheduler, manager, tests, oracle, Path(__file__).resolve(),
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
