#!/usr/bin/env python3
"""#2294 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.
`--check` only proves each mutation anchor is unique; it cannot prove the row's
assertion string ever reaches a panic payload, so a full run is the only proof
that a row can go KILLED.

Rows bind one mutation to one lib test (run with `--exact`); the kill evidence
is that test's own panic payload carrying its `#2294` message. The five rows
are the prev-plan §5 targets: the lock gate removed from the gate chain
(g10), the stop order swapped (cron no longer drains before Always-On), the
reload rebuilding an unchanged family, describe advertising the always-on
family with no manager bound, and a fired tick keeping the discovery lock.
The bound replays need no Node: they read the committed oracle bytes, so
every row runs on both platforms.
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
gates = root / 'src/pilotdeck-gateway/src/runtime/always_on/gates.rs'
automation = root / 'src/pilotdeck-gateway/src/native_process/assembly/automation.rs'
capabilities = root / 'src/pilotdeck-gateway/src/native_process/capabilities.rs'
scheduler = root / 'src/pilotdeck-gateway/src/runtime/always_on/scheduler.rs'
oracle = root / 'src/pilotdeck-gateway/tests/fixtures/always-on-2294/oracle.json'
GATES_REPLAY = 'runtime::always_on::gates::tests::replays_the_gate_chain_golden'
B2_REPLAY = 'native_process::assembly::automation::tests::b2_start_stop_order'
B6_REPLAY = 'native_process::assembly::automation::tests::b6_reload_cron_only'
NOT_ADVERTISED = (
    'native_process::methods::always_on::tests::'
    'always_on_methods_are_not_advertised_without_a_manager'
)
T1_REPLAY = 'runtime::always_on::scheduler::tests::t1_fires_with_state_and_lock_bookkeeping'
mutations = [
    # The gate chain's lock gate is removed, so g10's held lock answers ok
    # instead of blocked/lock_busy (a held discovery lock no longer short-
    # circuits the chain — the "period arrived but the gate opened anyway"
    # failure the plan's first target names).
    ('gate-chain-lock-gate-removed', gates,
     '    if *lock_held {\n'
     '        return blocked(GateBlockReason::LockBusy);\n'
     '    }\n'
     '\n'
     '    if fresh.is_empty() {\n',
     '    if fresh.is_empty() {\n',
     GATES_REPLAY,
     '#2294 g10-lock-held: the gate chain answer matches the TS baseline'),
    # stopGeneration stops Always-On before cron, so b2's lifecycle log ends
    # ao.stop, cron.stop instead of the reverse startup order the TS comment
    # pins ("producer dependencies drain in reverse startup order").
    ('stop-order-swapped', automation,
     '        if changes.cron_changed\n'
     '            && let Some(cron) = &generation.cron\n'
     '            && let Err(error) = cron.clone().stop().await\n'
     '        {\n'
     '            failures.push(error);\n'
     '        }\n'
     '        if changes.always_on_changed\n'
     '            && let Some(always_on) = &generation.always_on\n'
     '            && let Err(error) = always_on.clone().stop().await\n'
     '        {\n'
     '            failures.push(error);\n'
     '        }\n',
     '        if changes.always_on_changed\n'
     '            && let Some(always_on) = &generation.always_on\n'
     '            && let Err(error) = always_on.clone().stop().await\n'
     '        {\n'
     '            failures.push(error);\n'
     '        }\n'
     '        if changes.cron_changed\n'
     '            && let Some(cron) = &generation.cron\n'
     '            && let Err(error) = cron.clone().stop().await\n'
     '        {\n'
     '            failures.push(error);\n'
     '        }\n',
     B2_REPLAY,
     '#2294 lifecycle order'),
    # stage() rebuilds the unchanged family: the reuse guard is inverted, so a
    # cron-only reload constructs a second Always-On provider (b6's factory
    # counts and log both diverge; the factory counts assert first).
    ('reload-rebuilds-unchanged-family', automation,
     '            Some(previous) if !changes.always_on_changed => previous.always_on.clone(),',
     '            Some(previous) if changes.always_on_changed => previous.always_on.clone(),',
     B6_REPLAY,
     '#2294 factory counts'),
    # The wired probe answers true without a manager, so describe advertises
    # the three always-on methods on a gateway whose control is not bound —
    # the false capability claim the not-advertised test exists to kill.
    ('advertise-without-manager', capabilities,
     'fn always_on(requests: &Requests) -> bool {\n'
     '    requests.always_on.is_some()\n'
     '}\n',
     'fn always_on(_requests: &Requests) -> bool {\n'
     '    true\n'
     '}\n',
     NOT_ADVERTISED,
     '#2294 always_on_apply must not be advertised without an always-on manager'),
    # A fired tick keeps the discovery lock, so t1's lockAfter stays true
    # against the golden's false (the bookkeeping half of the fire path).
    ('fired-tick-keeps-the-lock', scheduler,
     '                if result.outcome == "no_plan" {\n'
     '                    self.ensure_dormancy_watcher();\n'
     '                }\n'
     '                release_discovery_lock(&self.deps.paths);\n',
     '                if result.outcome == "no_plan" {\n'
     '                    self.ensure_dormancy_watcher();\n'
     '                }\n',
     T1_REPLAY,
     '#2294 t1-fired: the full fire path matches the TS baseline'),
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
        print('\n'.join(errors) if errors else f'#2294 {len(mutations)} unique anchors checked')
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
    guarded = list(dict.fromkeys([gates, automation, capabilities, scheduler, oracle,
        Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py',
        Path(__file__).resolve().parents[1] / 'negative_panic.py']))

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
