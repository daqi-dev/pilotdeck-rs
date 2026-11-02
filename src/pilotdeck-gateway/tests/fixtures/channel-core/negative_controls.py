#!/usr/bin/env python3
"""#1327 channel lifecycle negative controls: OUT [mutation ...].

New sources must first be committed by the coordinator. No local mutation,
compile receipt or KILLED claim bypasses mutating_sources' HEAD guard.
Each selected mutation compiles before runtime classification; all rows run
before a summary is published. CARGO_TARGET_DIR is supplied by the caller.
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
source = root / 'src/pilotdeck-gateway/src/channel'
tests = root / 'src/pilotdeck-gateway/src/channel/tests.rs'
cases = [
    ('config-enabled-gate', source / 'loader.rs',
     'if !js_truthy(cfg.get("enabled")) {',
     'if true {',
     'channel::tests::loader_matches_real_ts', '#1327 loader oracle'),
    ('config-order', source / 'loader.rs',
     '    "telegram",\n    "discord",',
     '    "discord",\n    "telegram",',
     'channel::tests::loader_matches_real_ts', '#1327 loader oracle'),
    ('nullish-password', source / 'loader.rs',
     'if nullish(&token) {',
     'if false {',
     'channel::tests::loader_matches_real_ts', '#1327 loader oracle'),
    ('load-error-format', source / 'loader.rs',
     'error.to_js_string()',
     'error.message()',
     'channel::tests::loader_matches_real_ts', '#1327 loader oracle'),
    ('missing-module-continue', source / 'loader.rs',
     '                continue;\n            }\n        };',
     '                break;\n            }\n        };',
     'channel::tests::registry_missing_and_replacement', '#1327 registry continue and replace'),
    ('disable-skipped', source / 'server.rs',
     'if managed.insert(key.clone()) && !desired.contains(&key) {',
     'if managed.insert(key.clone()) && false && !desired.contains(&key) {',
     'channel::tests::lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('managed-dedup', source / 'server.rs',
     'if managed.insert(key.clone()) && !desired.contains(&key) {',
     'if (managed.insert(key.clone()) || true) && !desired.contains(&key) {',
     'channel::tests::lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('close-admission', source / 'server.rs',
     '            state.closing = true;',
     '            state.closing = false;',
     'channel::tests::lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('late-start-stop-reason', source / 'server.rs',
     'begin_stop(handle, "server-shutdown").await?;',
     'begin_stop(handle, "hot-reload").await?;',
     'channel::tests::lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('rollback-skipped', source / 'server.rs',
     'if let Some(previous) = previous {',
     'if let Some(previous) = previous.filter(|_| false) {',
     'channel::tests::lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('stop-restoration', source / 'server.rs',
     'if !state.running.iter().any(|item| item.key == key) {',
     'if false && !state.running.iter().any(|item| item.key == key) {',
     'channel::tests::lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('close-errors-dropped', source / 'server.rs',
     'let result = if errors.is_empty() {',
     'let result = if true {',
     'channel::tests::adapter_panics_do_not_strand_lifecycle', '#1327 panic stop rejection'),
    ('report-order-bypassed', source / 'server.rs',
     'if reports.dispatching {',
     'if false {',
     'channel::tests::cross_thread_status_order', '#1327 cross-thread status order'),
    ('status-callback-skipped', source / 'server.rs',
     'if let Some(report) = &self.deps.report_status {',
     'if let Some(report) = &None::<StatusReporter> {',
     'channel::tests::status_callback_can_reenter_close', '#1327 reentrant callback initiated close'),
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
    names = {row[0] for row in cases}
    if set(args.mutations) - names or len(set(args.mutations)) != len(args.mutations):
        parser.error('unknown or duplicate mutations')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    args.out.mkdir(parents=True, exist_ok=False)
    # No -q (#942): cargo forwards it to libtest as --quiet, which drops the
    # per-test lines the shared passed/classify criteria match.
    command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def write(path, value):
        with path.open('x') as stream:
            stream.write(value)

    def run(label, test, *, compile_only=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
        write(args.out / f'{label}.slots.log', slot.stdout + slot.stderr)
        suffix = ['--no-run'] if compile_only else [test, '--', '--exact', '--show-output', '--format=pretty']
        try:
            result = run_owned(command + suffix, cwd=root, env=env, timeout=300)
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for value in (error.stdout, error.stderr):
                    raw += value.decode(errors='replace') if isinstance(value, bytes) else value or ''
            write(args.out / f'{label}.error.txt', raw + f'\n{type(error).__name__}: {error}\n')
            raise
        write(args.out / f'{label}.stdout', result.stdout)
        write(args.out / f'{label}.stderr', result.stderr)
        return result

    # Guard the mutation sources, the binding assertions, the generators and
    # oracles, and this script itself. No dirty/new-file exception and no
    # shadow repo.
    guarded = list(source.glob('*.rs')) + [tests, Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / 'negative_process.py',
        Path(__file__).resolve().parents[1] / 'negative_suite.py']
    for fixture in ('channel-loader', 'channel-lifecycle'):
        guarded.extend(p for p in (Path(__file__).resolve().parents[1] / fixture).rglob('*')
                       if p.is_file())
    for name, path, old, new, test, assertion in selected:
        item = dict(mutation=name, test=test, assertion=assertion,
                    status='INVALID', compiled=False, restoredPass=False, reason='not run')
        try:
            with mutating_sources(root, guarded) as snapshots:
                original = snapshots[path].decode()
                try:
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: stale anchor; expected exactly one match')
                    if not args.compile_only and not passed(run(name + '-positive', test), test):
                        raise RuntimeError('bound positive test did not pass')
                    path.write_text(original.replace(old, new, 1))
                    if run(name + '-compile', test, compile_only=True).returncode != 0:
                        raise RuntimeError('mutant did not compile; never a kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='cargo test --no-run passed')
                    else:
                        status, reason = classify(run(name + '-negative', test),
                                                  Case(name, path, old, new, test, assertion))
                        item.update(status=status, reason=reason)
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(snapshots[path])
                    if path.read_bytes() != snapshots[path]:
                        raise RuntimeError('byte restoration failed')
                if not args.compile_only:
                    item['restoredPass'] = passed(run(name + '-restored', test), test)
                    if not item['restoredPass']:
                        item.update(status='INVALID', reason=item['reason'] + '; restored test failed')
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(item)
        write(args.out / f'{name}.result.json', json.dumps(item, indent=2) + '\n')
        print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = {status: sum(row['status'] == status for row in results)
              for status in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
    write(args.out / 'summary.json.tmp', json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    (args.out / 'summary.json.tmp').replace(args.out / 'summary.json')
    print(json.dumps(counts), flush=True)
    expected = 'COMPILED' if args.compile_only else 'KILLED'
    return int(not results or any(row['status'] != expected for row in results))


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
