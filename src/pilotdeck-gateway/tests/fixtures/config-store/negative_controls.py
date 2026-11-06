#!/usr/bin/env python3
"""#1736 / #1739 / #1899 HEAD-only controls: compile, fail at the bound assertion, restore.

Usage: negative_controls.py --check
       negative_controls.py NEW_OUT [mutation ...]
Commit the tests and this script first. CARGO_TARGET_DIR is caller-owned.
Every selected row is reported, including anchor/build/source-guard failures.

Rows bind one mutation to one named test in ``config::store::tests`` (a lib
test, run with ``--exact``); the kill evidence is that test's own panic payload
carrying its ``#1736`` / ``#1739`` / ``#1794`` / ``#1899`` message. Four rows pin
the watcher bounds (the debounce ceiling through both the watcher-level test and
the ledger's literal two-hour pin, the poll-interval ceiling, and the
``checked_add`` overflow fallback in ``deadline_after``); two pin the metadata
short-circuit (the skipped content read on unchanged metadata, and the ctime
bump that carries a same-length mtime-restored rewrite through it); two pin
#1899's watcher-stop evidence (the join-based check reverted to
``JoinHandle::is_finished()``, which is also true for a task that died by
panic; a panicking watcher loop that an ``is_finished()``-only check accepts as
a clean stop; a watcher that returns cleanly right after spawn; one that exits
on its first in-loop tick, before ``stop`` and before ``drop(store)``; and one
that never stops at all — all of them are #1650's silent-death path).
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
source = root / 'src/pilotdeck-gateway/src/config/store.rs'
tests = root / 'src/pilotdeck-gateway/src/config/store/tests.rs'
mutations = [
    # #1736: the debounce ceiling is removed, so a representable over-long
    # value passes through and the reload lands at the requested two hours
    # instead of the clamped one — the bound test's "inside the bound"
    # assertion fails.
    ('debounce-ceiling-removed', source,
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce.min(MAX_DEBOUNCE)\n'
     '}',
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce\n'
     '}',
     'config::store::tests::overlong_debounce_is_clamped_and_reloads_within_the_bound',
     '#1736 an overlong debounce lands on MAX_DEBOUNCE, not on the requested value'),
    # #1794 P1-2: the same clamp removal, bound to the ledger's literal
    # two-hour value instead of `2 * MAX_DEBOUNCE` — raising the ceiling to
    # Node's own `2^31 - 1` ms limit is the entry's convergence path and this
    # row is what turns that from a silent behaviour change into a red test.
    ('two-hour-debounce-ceiling-removed', source,
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce.min(MAX_DEBOUNCE)\n'
     '}',
     'fn effective_debounce(options: &WatchOptions) -> Duration {\n'
     '    options.debounce\n'
     '}',
     'config::store::tests::two_hour_debounce_is_clamped_to_the_one_hour_ceiling',
     '#1794 a two-hour debounce the baseline honours verbatim lands on the one-hour ceiling'),
    # #1736: the poll-interval ceiling is removed (back to #1650's floor-only
    # clamp), so a `Duration::MAX` period hits tokio's `Instant::far_future`
    # fallback — the ticker fires once and never again, the change written
    # after watcher start is never sampled, and no reload happens.
    ('poll-ceiling-removed', source,
     'fn effective_poll_interval(options: &WatchOptions) -> Duration {\n'
     '    options\n'
     '        .poll_interval\n'
     '        .clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)\n'
     '}',
     'fn effective_poll_interval(options: &WatchOptions) -> Duration {\n'
     '    options.poll_interval.max(MIN_POLL_INTERVAL)\n'
     '}',
     'config::store::tests::huge_poll_interval_is_clamped_and_keeps_sampling',
     '#1736 a huge poll interval is clamped and the change is still sampled'),
    # #1736: `deadline_after` adds unchecked again, so an unrepresentable
    # duration panics inside the bound test's `catch_unwind` and the expect
    # on its result fails at the #1736 marker.
    ('deadline-add-unchecked', source,
     '    now.checked_add(duration)\n'
     '        .unwrap_or_else(|| now.checked_add(MAX_DEBOUNCE).unwrap_or(now))',
     '    now + duration',
     'config::store::tests::deadline_after_handles_unrepresentable_durations',
     '#1736 deadline_after must not panic on an unrepresentable duration'),
    # #1739: the metadata short-circuit is removed, so every sample reads the
    # config file's content again and the read-count assertion fails.
    ('metadata-short-circuit-removed', source,
     '        let next_metadata = fingerprint(&self.path, self.directory, self.target.as_deref(), false);\n'
     '        if next_metadata == self.metadata_fingerprint {\n'
     '            return false;\n'
     '        }\n'
     '        let next = fingerprint(&self.path, self.directory, self.target.as_deref(), true);\n'
     '        self.metadata_fingerprint = next_metadata;\n'
     '        std::mem::replace(&mut self.fingerprint, next) != next',
     '        let next = fingerprint(&self.path, self.directory, self.target.as_deref(), true);\n'
     '        std::mem::replace(&mut self.fingerprint, next) != next',
     'config::store::tests::unchanged_metadata_skips_the_content_read',
     '#1739 50 metadata-identical samples must not read the config file content'),
    # #1739: ctime is dropped from the metadata tuple, so a same-length rewrite
    # with mtime restored leaves the metadata sample identical and the
    # short-circuit hides the change from the watcher.
    ('ctime-dropped-from-metadata', source,
     '            (meta.ctime(), meta.ctime_nsec(), meta.ino(), meta.dev()).hash(hasher);',
     '            (meta.ino(), meta.dev()).hash(hasher);',
     'config::store::tests::restored_mtime_rewrite_between_samples_is_seen',
     '#1739 a same-length rewrite with mtime restored is still seen (ctime)'),
    # #1899: the watcher-stop evidence goes back to `JoinHandle::is_finished()`,
    # which tokio 1.48 also reports for a task that died by panic (the harness
    # stores `JoinError::panic` and marks the task complete), so the regression
    # test's `expect_err` fails at its own marker: a panicked watcher is again
    # accepted as a clean stop — #1650's silent hot-reload death.
    ('is-finished-stop-evidence-restored', tests,
     '    match tokio::time::timeout(STOP_TIMEOUT, task).await {\n'
     '        Err(_) => Err(format!(\n'
     '            "the watcher task did not stop within {STOP_TIMEOUT:?}"\n'
     '        )),\n'
     '        Ok(Ok(())) => Ok(()),\n'
     '        Ok(Err(err)) if err.is_panic() => Err(format!("the watcher task panicked: {err}")),\n'
     '        Ok(Err(err)) => Err(format!("the watcher task was cancelled: {err}")),\n'
     '    }',
     '    if task.is_finished() {\n'
     '        Ok(())\n'
     '    } else {\n'
     '        Err("the watcher task did not stop".to_owned())\n'
     '    }',
     'config::store::tests::panicked_watcher_task_is_not_a_clean_stop',
     '#1899 a panicked watcher must not look like a clean stop'),
    # #1899: the watcher loop panics on its first poll — the #1650 silent-death
    # path. The drop test must fail instead of passing on a task that is merely
    # "finished": the injected panic kills it at the first liveness check, and
    # the later ones are listed because they are the same defect seen one step
    # apart (`classify` accepts the first marker that shows up).
    ('watch-loop-panic-injected', source,
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     '    panic!("#1899 injected: simulated watch_loop failure");\n'
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     'config::store::tests::dropping_the_handle_or_the_store_ends_the_task',
     ['#1899 the watcher must still be running before the handle is dropped',
      '#1899 the watcher must still be running before the store is dropped',
      '#1899 dropping the store must stop the watcher cleanly']),
    # #1899 review P2-2: a watcher that returns cleanly right after spawn also
    # joins with `Ok(())`, so the drop-the-store evidence only means something
    # once the watcher is proven alive before the drop. This mutant is that
    # counterexample — the silent-death shape #1650 is about, with a clean exit
    # instead of a panic — and it must die at the liveness assertion.
    ('watch-loop-clean-early-exit', source,
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     '    return;\n'
     '    let mut ticker = tokio::time::interval(effective_poll_interval(&options));',
     'config::store::tests::dropping_the_handle_or_the_store_ends_the_task',
     ['#1899 the watcher must still be running before the handle is dropped',
      '#1899 the watcher must still be running before the store is dropped']),
    # #1899 review P2 (boundary): a liveness check taken at spawn time proves
    # nothing about a watcher that returns on its first in-loop tick — still
    # #1650's silent death, one tick later. Each check advances one poll interval
    # first, and this mutant is what that advance is there to catch.
    ('watch-loop-exits-after-first-tick', source,
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                let mut changed = false;',
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                return;\n'
     '                let mut changed = false;',
     'config::store::tests::dropping_the_handle_or_the_store_ends_the_task',
     ['#1899 the watcher must still be running before the handle is dropped',
      '#1899 the watcher must still be running before the store is dropped']),
    # #1899 review P2 (sibling): the same mutant against `WatchHandle::stop`,
    # which aborts and swallows the `JoinError` — `weak_count == 0` and "no
    # reload" alone would accept a watcher that died before the stop.
    ('watch-loop-exits-after-first-tick-before-stop', source,
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                let mut changed = false;',
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                return;\n'
     '                let mut changed = false;',
     'config::store::tests::stop_cancels_pending_reload_and_leaves_no_task',
     '#1899 the watcher must still be running before stop'),
    # #1899 review P2 (concurrency): every other row dies before the join, so
    # the timeout arm of `watcher_stop` (and with it the "90 s clears the 60 s
    # poll ceiling" invariant) had no receipt. Removing the owner-gone return
    # makes the watcher keep ticking after `drop(store)`: the suspended clock
    # walks the whole 90 s at no wall-clock cost and the test fails at the
    # clean-stop assertion instead of hanging or being called a clean stop.
    ('watch-loop-never-stops', source,
     '                if inner.strong_count() == 0 {\n'
     '                    return;\n'
     '                }\n'
     '                let mut changed = false;',
     '                let mut changed = false;',
     'config::store::tests::dropping_the_handle_or_the_store_ends_the_task',
     '#1899 dropping the store must stop the watcher cleanly'),
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
        print('\n'.join(errors) if errors else f'#1736/#1739/#1794/#1899 {len(mutations)} unique anchors checked')
        return int(bool(errors))
    if not args:
        raise SystemExit('provide NEW_OUT and optional mutation names, or --check')
    names = args[1:]
    selected = [row for row in mutations if not names or row[0] in names]
    if not selected or (names and len(selected) != len(names)):
        raise SystemExit('unknown or duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    # cb8's zh_CN default locale has produced false reds in libtest output;
    # an explicit caller choice is never overridden.
    env.setdefault('LANG', 'en_US.UTF-8')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    summary = []
    guarded = list(dict.fromkeys([source, tests, Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py']))

    def save(name, text):
        with (out / name).open('x') as stream:
            stream.write(text)

    def run(label, test, *, compile=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        save(label + '.slots.log', slot.stdout + slot.stderr)
        # negative_suite needs libtest's named result lines, including
        # positives; no -q (cargo forwards it to libtest and the named
        # `test ... FAILED` line disappears, #942 lesson).
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
        name, path, old, new, test, assertion = row[:6]
        item = dict(name=name, test=test, assertion=assertion,
                    status='INVALID', reason='not executed', compiled=False)
        try:
            # No caller-supplied baseline, dirty-file snapshot, or alternate git.
            with mutating_sources(root, guarded) as originals:
                original = originals[path].decode()
                check_anchor(row, original)
                if not passed(run(name + '-positive', test), test):
                    raise RuntimeError('bound positive control did not pass')
                try:
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', test, compile=True).returncode != 0:
                        raise RuntimeError('mutant failed compilation; not a runtime kill')
                    item['compiled'] = True
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
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    success = 'KILLED'
    return int(not summary
               or not counts[success]
               or any(row['status'] != success for row in summary))


if __name__ == '__main__':
    raise SystemExit(main())
