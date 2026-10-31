#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1168 signal-guarantee source mutations, no product anchor
"""#1168 negative controls: every signal guarantee in run.py is load-bearing.

Applies one source mutation at a time to the committed run.py under
negative_process.mutating_sources and requires the targeted real-signal cell
in test_signals.py to fail with its own #1168 message (KILLED). Run from the
repository root after the branch is committed (mutating_sources only mutates
HEAD-identical sources), giving a fresh evidence directory:

    python3 -I src/pilotdeck-gateway/tests/fixtures/native-topology/negative_signals.py \
        /absolute/path/to/new-evidence-directory [mutation-name ...]

Without a mutation name every mutation runs (fix-r1 host-0/P1-3: the default
README command must work; unknown names are still an error exit).

Each mutation is judged per signal cell (PDRS_1168_ONLY_SIG narrows the grid
to one signal per run, fix-r1 P1-6): a signal cell is killed only when its
log carries exactly one failing assertion, that line starts with one of the
mutation's bound #1168 messages, and no unrelated ERROR is mixed in. A cell
that still passes is SURVIVED; anything else (wrong message, mixed result,
crash, timeout) is INVALID. Every mutant runs before the summary; any
non-KILLED verdict is a non-zero exit.

A stale anchor (the source drifted, an anchor no longer matches exactly once)
is an error exit, never a SURVIVED verdict; a mutant that stops compiling is
INVALID, caught by compiling the mutated source before it is written.
"""
import os
import subprocess
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned

ROOT = Path(__file__).resolve().parents[5]
HERE = Path(__file__).resolve().parent
TARGET = HERE / 'run.py'
TEST = HERE / 'test_signals.py'
SIGNALS = ('2', '15')  # SIGINT, SIGTERM, one verdict per signal cell

# (name, targeted cell, the single-line messages the mutant must die on — one
# per signal, [(anchor, replacement), ...]). The live-groups registration is
# deliberately mutated at BOTH of its sites: run_probe's explicit add and
# collect_descendants' group-leader add each suffice on their own, so removing
# only one survives. no-cancel-handler dies on different bound assertions per
# signal (SIGTERM kills the child outright, SIGINT survives to the report
# check with a message-less builtin KeyboardInterrupt), so both are registered.
mutations = [
    ('no-cancel-handler', 'test_probe_running',
     ('#1168 signal child completes',
      '#1168 a cancelled capture publishes a FAIL report naming the signal'),
     [('''    with cancel_signals():
        try:
            with parity_lock():
                return run(args)
        except KeyboardInterrupt as error:''',
       '''    try:
        with parity_lock():
            return run(args)
    except KeyboardInterrupt as error:''')]),
    ('spawn-window-unatomic', 'test_window_residue',
     ('#1168 residue keeps the lease marked cleanupPending',),
     [('''            with deferred_signals():
                child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True,''',
       '''            if True:
                child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True,''')]),
    ('cleanup-unshielded', 'test_probe_cleanup',
     ('#1168 cleanup is not interruptible, the audit is published and the lease released',),
     [('''        finally:
            # #1168: cleanup is the one window an interrupt must not abandon;
            # a pending cancellation is delivered only after the audit below.
            with deferred_signals():''',
       '''        finally:
            # #1168: cleanup is the one window an interrupt must not abandon;
            # a pending cancellation is delivered only after the audit below.
            if True:''')]),
    ('provider-cleanup-unshielded', 'test_cleanup_shutdown',
     ('#1168 provider cleanup is not interruptible',),
     [('''    finally:
        # #1168: provider cleanup is not interruptible; a pending cancellation
        # is delivered only after server_close and the report are published.
        with deferred_signals():''',
       '''    finally:
        # #1168: provider cleanup is not interruptible; a pending cancellation
        # is delivered only after server_close and the report are published.
        if True:''')]),
    ('live-groups-unregistered', 'test_cleanup_residue',
     ('#1168 residue keeps the lease marked cleanupPending',),
     [('''                LIVE_GROUPS.add(child.pid)''',
       '''                pass'''),
      ('''            # #1168: a descendant leading its own group escapes the probe's
            # group kill; register it so residue is published, never lost.
            if census[child]['pgid'] == child:
                LIVE_GROUPS.add(child)''',
       '''            # #1168: a descendant leading its own group escapes the probe's
            # group kill; register it so residue is published, never lost.
            if census[child]['pgid'] == child:
                pass''')]),
    ('no-mask-restore', 'test_probe_running',
     ('#1168 the probe inherits an unblocked signal mask',),
     [('''                child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True,
                                         preexec_fn=unblock_child_signals,''',
       '''                child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True,''')]),
    ('provider-window-unatomic', 'test_provider_bound',
     ('#1168 provider cleanup is not interruptible',),
     [('''        with deferred_signals():
            provider = Provider()''',
       '''        if True:
            provider = Provider()''')]),
    ('cancel-raises-twice', 'test_probe_spawn_window_mixed',
     ('#1168 mixed signals unwind exactly once and still reap the probe',),
     [('''        if isinstance(sys.exception(), KeyboardInterrupt):
            return''',
       '''        if isinstance(None, KeyboardInterrupt):
            return''')]),
    ('no-cancel-report', 'test_cleanup_close',
     ('#1168 a cancelled capture publishes a FAIL report naming the signal',),
     [('''            publish_cancelled(args.output, error)
            return 1''',
       '''            return 1''')]),
]


def select(requested):
    """Full mode with no arguments; unknown or duplicated names are errors."""
    if not requested:
        return list(mutations)
    selected = [row for row in mutations if row[0] in requested]
    unknown = set(requested) - {row[0] for row in selected}
    if unknown:
        raise SystemExit(f'unknown mutation name(s): {sorted(unknown)}')
    if len(selected) != len(requested):
        raise SystemExit(f'duplicated mutation name(s): {requested}')
    return selected


def selfcheck():
    """Entry-point self-test for the selection and judgment logic."""
    assert select([]) == mutations, 'default full mode must select every mutation'
    assert [row[0] for row in select(['no-cancel-report'])] == ['no-cancel-report']
    for bad in (['nope'], ['no-cancel-report', 'nope']):
        try:
            select(bad)
        except SystemExit as error:
            assert 'unknown mutation name(s)' in str(error), error
        else:
            raise AssertionError(f'unknown names accepted: {bad}')
    try:
        select(['no-cancel-report', 'no-cancel-report'])
    except SystemExit as error:
        assert 'duplicated mutation name(s)' in str(error), error
    else:
        raise AssertionError('duplicated names accepted')
    killed = 'AssertionError: False is not true : #1168 own message: detail\n'
    assert binding_failure(killed, ('#1168 own message',)) is None, killed
    for log, reason in (
        ('AssertionError: False is not true : #1168 other message\n', 'unbound message'),
        ('AssertionError: False is not true : #1168 own message\n'
         'AssertionError: False is not true : #1168 own message\n', 'mixed result'),
        ('AssertionError: False is not true : #1168 own message\nERROR: test_x\n', 'unrelated error'),
        ('', 'no failure at all'),
    ):
        detail = binding_failure(log, ('#1168 own message',))
        assert detail == reason, (log, detail)
    print(f'selfcheck OK: {len(mutations)} mutations, full mode selects all, '
          f'judgment rejects mixed or unbound results')
    return 0


def binding_failure(log, markers):
    """Return None when the whole log is exactly one bound assertion failure.

    fix-r1 P1-6: a substring hit anywhere in the log used to accept a target
    failure mixed with unrelated errors from the other signal cell. A kill now
    requires the failing AssertionError line itself to carry one of this
    mutation's bound messages, and nothing else to have failed or errored.
    """
    failures = [line for line in log.splitlines() if line.startswith('AssertionError:')]
    if any(line.startswith('ERROR:') for line in log.splitlines()):
        return 'unrelated error'
    if not failures:
        return 'no failure at all'
    if len(failures) > 1:
        return 'mixed result'
    # assertTrue renders "AssertionError: False is not true : <message>", so
    # the bound message is matched on the assertion line itself, not as a
    # prefix of the line and never as a substring of the whole log.
    if not any(marker in failures[0] for marker in markers):
        return 'unbound message'
    return None


def run_cell(out, name, test, sig=None):
    """Run one cell (one signal when sig is given); return (returncode, output)."""
    env = dict(os.environ, PDRS_1168_ONLY_SIG=sig) if sig else None
    command = [sys.executable, '-I', str(TEST), '-k', test]
    try:
        result = run_owned(command, cwd=ROOT, timeout=900, env=env)
    except subprocess.TimeoutExpired as error:
        log = (error.output or '') + (error.stderr or '') + \
            f'\n#1168 negative control: cell timed out after {error.timeout}s'
        (out / f'{name}.log').write_text(log)
        return None, log
    (out / f'{name}.log').write_text(result.stdout + result.stderr)
    return result.returncode, result.stdout + result.stderr


def main():
    if len(sys.argv) == 2 and sys.argv[1] == '--selfcheck':
        return selfcheck()
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_signals.py <new-evidence-dir> [mutation-name ...]')
    out = new_output_directory(sys.argv[1])
    selected = select(sys.argv[2:])
    records = []
    with mutating_sources(ROOT, [TARGET]) as snapshots:
        original = snapshots[TARGET]
        # Stale anchors are an error, not a SURVIVED verdict.
        for name, _test, _markers, pairs in selected:
            for anchor, _replacement in pairs:
                count = original.decode().count(anchor)
                if count != 1:
                    print(f'STALE ANCHOR {name}: {count} matches in run.py', file=sys.stderr)
                    raise SystemExit(2)
        baselines = sorted({row[1] for row in selected})
        for test in baselines:
            code, log = run_cell(out, f'baseline-{test}', test)
            print(f'baseline {test}: {"PASS" if code == 0 else "INVALID"} (exit {code})', flush=True)
            if code != 0:
                records.append((f'baseline-{test}', 'INVALID', f'exit {code}; see baseline-{test}.log'))
        for name, test, markers, pairs in selected:
            changed = original.decode()
            for anchor, replacement in pairs:
                changed = changed.replace(anchor, replacement, 1)
            try:
                compile(changed, str(TARGET), 'exec')
            except SyntaxError as error:
                records.append((name, 'INVALID', f'does not compile: {error}'))
                print(f'{name}: INVALID (does not compile: {error})', flush=True)
                continue
            TARGET.write_text(changed)
            verdicts = []
            try:
                for sig in SIGNALS:
                    code, log = run_cell(out, f'{name}-sig{sig}', test, sig)
                    if code == 0:
                        verdicts.append((sig, 'SURVIVED', 'the targeted cell still passed'))
                        continue
                    detail = binding_failure(log, markers)
                    if code == 1 and detail is None:
                        verdicts.append((sig, 'KILLED', 'failed on its own bound message'))
                    else:
                        verdicts.append((sig, 'INVALID', detail or f'exit {code}'))
            finally:
                TARGET.write_bytes(original)
            statuses = {status for _sig, status, _reason in verdicts}
            if statuses == {'KILLED'}:
                status = 'KILLED'
            elif 'SURVIVED' in statuses:
                status = 'SURVIVED'
            else:
                status = 'INVALID'
            reason = '; '.join(f'sig{sig}: {why}' for sig, _status, why in verdicts)
            records.append((name, status, reason))
            print(f'{name}: {status} ({reason})', flush=True)
    print('--- summary ---', flush=True)
    for name, status, reason in records:
        print(f'{status:9} {name}: {reason}')
    bad = [name for name, status, _ in records if status != 'KILLED']
    if bad:
        print(f'non-KILLED: {", ".join(bad)}')
        return 1
    print(f'all {len(records)} mutation(s) KILLED')
    return 0


if __name__ == '__main__':
    sys.exit(main())
