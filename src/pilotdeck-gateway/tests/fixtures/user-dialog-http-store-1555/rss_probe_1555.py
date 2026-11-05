#!/usr/bin/env python3
"""#1555 peak-RSS measurement: one mode per process under `/usr/bin/time`.

Issue #1555's acceptance line asks for a real peak-memory measurement of the
two resource-bound cells this change touches. `header_budget_peak_rss` in
`http_store_tests.rs` starts the real loopback server, sends one head shape and
prints a receipt line; this driver runs that test once per mode in its own
process so the timing tool reports that process's peak RSS, repeats `--rounds`
times and keeps the minimum (external memory pressure only ever inflates a
sample, the same rule `html_budget_probe.rs` uses).

    CARGO_TARGET_DIR=/abs/per-tree/target python3 rss_probe_1555.py OUT_DIR

Writes `<mode>-r<n>.log` per sample plus `summary.json`. The tree must be
committed and clean: the driver holds the shared tree lock (the same `flock`
the negative controls hold while they mutate sources in place) for the whole
build and measurement window, and a dirty product input is refused with
`status=INVALID` and exit code 2 instead of being recorded as `src_dirty=false`.
"""
# anchor-selfcheck: unsupported peak-RSS measurement driver: builds the lib test
# binary once, then times one already-written test process per head shape; it
# never rewrites product source, so there is no source replacement table.
import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (SourceBaselineError, _assert_workspace_sources,
                              _git, run_owned, tree_lock, wait_for_slot)

root = Path(__file__).resolve().parents[5]
TEST = 'user_dialog::http_store::http_store_tests::header_budget_peak_rss'
MODES = ('baseline', 'fields-4097', 'fields-max', 'ows-90000', 'fields-max-16x')
DARWIN = platform.system() == 'Darwin'


def build(env, out):
    """Build the `--lib` test binary once and return its path."""
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=1800)
    (out / 'build.slots.log').write_text(slot.stdout + slot.stderr)
    command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib', '--no-run',
               '--message-format=json']
    result = run_owned(command, cwd=root, env=env, timeout=3600)
    (out / 'build.log').write_text(result.stdout + result.stderr)
    result.check_returncode()
    executables = set()
    for line in result.stdout.splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        # cargo names the lib target after the crate (`pilotdeck_gateway`), not
        # the package (`pilotdeck-gateway`); accept either spelling.
        if (row.get('reason') == 'compiler-artifact'
                and row.get('target', {}).get('name', '').replace('-', '_') == 'pilotdeck_gateway'
                and row['target'].get('kind') == ['lib']
                and row.get('executable')):
            executables.add(row['executable'])
    if len(executables) != 1:
        raise SystemExit(f'expected one --lib executable, found {sorted(executables)}')
    return executables.pop()


def peak_rss(stderr):
    """Return the peak RSS in bytes from the timing tool's report.

    The two tools spell the line differently: BSD `/usr/bin/time -l` prints
    lowercase `maximum resident set size` in bytes, GNU `time -v` prints
    `Maximum resident set size (kbytes):` in kilobytes. Match case-insensitively
    or every Linux sample is dropped as "no maximum resident set size line".
    """
    for line in stderr.splitlines():
        if 'maximum resident set size' in line.lower():
            fields = line.replace(':', ' ').split()
            value = next(field for field in fields if field.isdigit())
            return int(value) * (1 if DARWIN else 1024)
    raise ValueError('no maximum resident set size line in timing stderr')


def workspace_dirty(root):
    """Return the shared guard's complaint, or None when inputs match HEAD.

    This delegates to `negative_process._assert_workspace_sources`, the same
    rule the mutating controls already run under this lock. A private
    `git status -- src` here saw neither `.cargo/`, nor the rust-toolchain pin,
    nor the index's assume-unchanged / skip-worktree bits, nor an ignored
    untracked build input -- exactly the "source changed, summary still says
    `src_dirty=false`" cases fix-r2 exists to stop. One definition, one place.
    """
    try:
        _assert_workspace_sources(root, 'HEAD')
    except SourceBaselineError as error:
        return str(error)
    return None


def summary_for(provenance, status, **over):
    """Build the summary from one skeleton so both exits share a shape."""
    summary = dict(provenance=provenance, status=status, executable=None,
                   test=TEST, samples={}, minima={}, delta_from_baseline={},
                   failures=[])
    summary.update(over)
    return summary


def write_summary(out, summary):
    """Persist one summary to `OUT_DIR/summary.json`.

    Every exit path goes through here, including the two refusals, so a reader
    can tell "measured nothing" from "crashed before writing": the file exists
    in the first case. A missing file is never a passing record.
    """
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--modes', nargs='*', default=list(MODES))
    parser.add_argument('--rounds', type=int, default=3)
    args = parser.parse_args()
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    unknown = set(args.modes) - set(MODES)
    if unknown:
        parser.error(f'unknown modes: {sorted(unknown)}')
    # Zero rounds measures nothing, yet the old shape still wrote
    # `status=OK` with an empty `minima` -- a reader takes that for a passing
    # acceptance item. Refuse the invocation instead of writing it.
    if args.rounds < 1:
        parser.error('--rounds must be at least 1')
    if not args.modes:
        parser.error('--modes must name at least one mode')
    args.out.mkdir(parents=True, exist_ok=False)
    out = args.out
    # #1555 fix-r2: the negative controls mutate product sources in place under
    # this same tree lock. Building or measuring without it could compile a
    # mutant while the summary still recorded `src_dirty=false`; a mutant that
    # shrinks hyper's per-parse slot vectors (`max-headers-back-to-4096`) then
    # measures a *lower* peak — a false PASS for the acceptance item. Hold the
    # kernel `flock` for the whole build+measurement window and refuse a tree
    # that is already dirty instead of recording the bit and continuing.
    #
    # `starting` is printed before the lock is requested and `provenance` only
    # after it is held, so a control that holds the lock can tell a blocked
    # driver from a slow interpreter instead of guessing from a quiet window.
    print(f'starting {json.dumps({"pid": os.getpid()})}', flush=True)
    with tree_lock(root, timeout=3600):
        dirty = workspace_dirty(root)
        provenance = dict(
            head=_git(root, 'rev-parse', 'HEAD').decode().strip(),
            src_dirty=dirty is not None, host=platform.node(),
            time_tool='time -l' if DARWIN else 'time -v', rounds=args.rounds)
        print(f'provenance {json.dumps(provenance)}', flush=True)
        if dirty is not None:
            summary = summary_for(provenance, 'INVALID', failures=[
                'src/ is dirty: the peak-RSS probe only measures a committed '
                'tree, and it must hold the shared tree lock against the '
                f'negative controls ({dirty})'])
            write_summary(out, summary)
            print(summary['failures'][0], file=sys.stderr)
            return 2
        try:
            executable = build(env, out)
        except (subprocess.SubprocessError, OSError, RuntimeError,
                SystemExit) as error:
            summary = summary_for(
                provenance, 'INVALID',
                build_log=str(out / 'build.log'),
                failures=[f'build failed: {type(error).__name__}: {error}'])
            write_summary(out, summary)
            print(summary['failures'][0], file=sys.stderr)
            return 2
        print(f'binary {executable}', flush=True)
        command = ['/usr/bin/time', '-l' if DARWIN else '-v', executable,
                   '--ignored', '--exact', TEST, '--nocapture', '--test-threads=2']
        samples, failures = {}, []
        for mode in args.modes:
            samples[mode] = []
            for round_index in range(args.rounds):
                run_env = dict(env, PDRS_PROBE=mode)
                try:
                    result = run_owned(command, cwd=root, env=run_env, timeout=600)
                except subprocess.TimeoutExpired:
                    failures.append(f'{mode} r{round_index + 1}: timed out after 600s')
                    continue
                log = out / f'{mode}-r{round_index + 1}.log'
                log.write_text(result.stdout + '\n--- stderr ---\n' + result.stderr)
                receipt = [line for line in result.stdout.splitlines()
                           if line.startswith(f'mode={mode} ')]
                if result.returncode != 0 or not receipt:
                    failures.append(f'{mode} r{round_index + 1}: rc={result.returncode} '
                                    f'receipt={receipt}')
                    continue
                if 'test result: ok. 1 passed' not in result.stdout:
                    failures.append(f'{mode} r{round_index + 1}: the filtered test did not run')
                    continue
                try:
                    samples[mode].append(peak_rss(result.stderr))
                except ValueError as error:
                    failures.append(f'{mode} r{round_index + 1}: {error}')
                print(f'{receipt[0]} rss={samples[mode][-1] if samples[mode] else "?"}',
                      flush=True)
        minima = {mode: min(values) for mode, values in samples.items() if values}
        # `status` follows the minima, not only the failure list: a mode with
        # no usable sample is a missing acceptance measurement even when every
        # round merely "continued".
        missing = [mode for mode in args.modes if mode not in minima]
        if missing and not failures:
            failures.append(f'no usable sample for: {sorted(missing)}')
        baseline = minima.get('baseline')
        summary = summary_for(
            provenance, 'OK' if not failures else 'FAILED',
            executable=executable, samples=samples, minima=minima,
            failures=failures,
            delta_from_baseline={mode: value - baseline
                                 for mode, value in minima.items()}
            if baseline is not None else {})
        write_summary(out, summary)
        for mode in minima:
            delta = summary['delta_from_baseline'].get(mode)
            print(f'{mode:16s} min_peak_rss={minima[mode]:>12} B'
                  + (f'  delta={delta:+} B' if delta is not None else ''), flush=True)
        if failures:
            print('\n'.join(failures), file=sys.stderr)
        return 0 if summary['status'] == 'OK' else 1


if __name__ == '__main__':
    raise SystemExit(main())
