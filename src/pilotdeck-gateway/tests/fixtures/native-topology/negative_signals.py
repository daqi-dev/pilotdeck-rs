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

A stale anchor (the source drifted, an anchor no longer matches exactly once)
is an error exit, never a SURVIVED verdict; a mutant that stops compiling is
INVALID, caught by compiling the mutated source before it is written.
"""
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

# (name, targeted cell, the single-line message the mutant must die on, [(anchor, replacement), ...]).
# The live-groups registration is deliberately mutated at BOTH of its sites:
# run_probe's explicit add and collect_descendants' group-leader add each
# suffice on their own, so removing only one survives.
mutations = [
    ('no-cancel-handler', 'test_probe_running', '#1168 signal child completes',
     [('''    with cancel_signals():
        try:
            with parity_lock():
                return run(args)
        except KeyboardInterrupt:''',
       '''    try:
        with parity_lock():
            return run(args)
    except KeyboardInterrupt:''')]),
    ('spawn-window-unatomic', 'test_window_residue', '#1168 residue keeps the lease marked cleanupPending',
     [('''            with deferred_signals():
                child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True,''',
       '''            if True:
                child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True,''')]),
    ('cleanup-unshielded', 'test_probe_cleanup', '#1168 cleanup is not interruptible, the probe group is reaped',
     [('''        finally:
            # #1168: cleanup is the one window an interrupt must not abandon;
            # a pending cancellation is delivered only after the audit below.
            with deferred_signals():''',
       '''        finally:
            # #1168: cleanup is the one window an interrupt must not abandon;
            # a pending cancellation is delivered only after the audit below.
            if True:''')]),
    ('provider-cleanup-unshielded', 'test_cleanup_shutdown', '#1168 provider cleanup is not interruptible',
     [('''    finally:
        # #1168: provider cleanup is not interruptible; a pending cancellation
        # is delivered only after server_close and the report are published.
        with deferred_signals():''',
       '''    finally:
        # #1168: provider cleanup is not interruptible; a pending cancellation
        # is delivered only after server_close and the report are published.
        if True:''')]),
    ('live-groups-unregistered', 'test_cleanup_residue', '#1168 residue keeps the lease marked cleanupPending',
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
    ('no-mask-restore', 'test_probe_running', '#1168 the probe inherits an unblocked signal mask',
     [('''                child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True,
                                         preexec_fn=unblock_child_signals,''',
       '''                child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True,''')]),
    ('provider-window-unatomic', 'test_provider_bound', '#1168 provider cleanup is not interruptible',
     [('''        with deferred_signals():
            provider = Provider()''',
       '''        if True:
            provider = Provider()''')]),
]


def run_cell(out, name, test):
    """Run one targeted cell; return (returncode, output)."""
    result = run_owned([sys.executable, '-I', str(TEST), '-k', test], cwd=ROOT, timeout=900)
    (out / f'{name}.log').write_text(result.stdout + result.stderr)
    return result.returncode, result.stdout + result.stderr


def main():
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_signals.py <new-evidence-dir> [mutation-name ...]')
    out = new_output_directory(sys.argv[1])
    selected = [row for row in mutations if not sys.argv[2:] or row[0] in sys.argv[2:]]
    if len(selected) != len(sys.argv[2:]):
        unknown = set(sys.argv[2:]) - {row[0] for row in selected}
        raise SystemExit(f'unknown mutation name(s): {sorted(unknown)}')
    # No -q/--quiet anywhere: the log must carry the per-test verdict lines.
    command = [sys.executable, '-I', str(TEST)]
    records = []
    with mutating_sources(ROOT, [TARGET]) as snapshots:
        original = snapshots[TARGET]
        # Stale anchors are an error, not a SURVIVED verdict.
        for name, _test, _marker, pairs in selected:
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
        for name, test, marker, pairs in selected:
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
            try:
                code, log = run_cell(out, name, test)
            finally:
                TARGET.write_bytes(original)
            if code == 0:
                status, reason = 'SURVIVED', 'the targeted cell still passed'
            elif marker in log:
                status, reason = 'KILLED', 'failed on its own message'
            else:
                status, reason = 'INVALID', f'exit {code} without the binding message'
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
