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

Writes `<mode>-r<n>.log` per sample plus `summary.json`; exits non-zero when a
sample fails or a receipt is missing. The tree must be committed: `HEAD` and
the `src/` cleanliness are recorded in the summary.
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
from negative_process import run_owned, wait_for_slot

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
    for line in stderr.splitlines():
        if 'maximum resident set size' in line:
            fields = line.replace(':', ' ').split()
            value = next(field for field in fields if field.isdigit())
            return int(value) * (1 if DARWIN else 1024)
    raise ValueError('no maximum resident set size line in timing stderr')


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
    args.out.mkdir(parents=True, exist_ok=False)
    out = args.out
    clean = subprocess.run(['git', 'status', '--porcelain', '--', 'src'],
                           cwd=root, capture_output=True, text=True, check=True)
    provenance = dict(
        head=subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=root,
                            capture_output=True, text=True, check=True).stdout.strip(),
        src_dirty=bool(clean.stdout.strip()), host=platform.node(),
        time_tool='time -l' if DARWIN else 'time -v', rounds=args.rounds)
    print(f'provenance {json.dumps(provenance)}', flush=True)
    executable = build(env, out)
    print(f'binary {executable}', flush=True)
    command = ['/usr/bin/time', '-l' if DARWIN else '-v', executable,
               '--ignored', '--exact', TEST, '--nocapture', '--test-threads=2']
    samples, failures = {}, []
    for mode in args.modes:
        samples[mode] = []
        for round_index in range(args.rounds):
            run_env = dict(env, PDRS_PROBE=mode)
            result = run_owned(command, cwd=root, env=run_env, timeout=600)
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
    baseline = minima.get('baseline')
    summary = dict(provenance=provenance, executable=executable, test=TEST,
                   samples=samples, minima=minima, failures=failures,
                   delta_from_baseline={mode: value - baseline
                                        for mode, value in minima.items()}
                   if baseline is not None else {})
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    for mode in minima:
        delta = summary['delta_from_baseline'].get(mode)
        print(f'{mode:16s} min_peak_rss={minima[mode]:>12} B'
              + (f'  delta={delta:+} B' if delta is not None else ''), flush=True)
    if failures:
        print('\n'.join(failures), file=sys.stderr)
    return 1 if failures or len(minima) != len(args.modes) else 0


if __name__ == '__main__':
    raise SystemExit(main())
