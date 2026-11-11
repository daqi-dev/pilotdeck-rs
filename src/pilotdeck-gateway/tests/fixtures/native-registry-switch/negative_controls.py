#!/usr/bin/env python3
"""#1369: commit first, then compile and run every registry-switch mutation.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound nonce receipt, and pass again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
"""
import argparse
import json
import hashlib
import re
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parents[1]
ASSEMBLY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
REQUESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/requests.rs'
# #1352: the advertised capability table moved out of `requests.rs`.
CAPABILITIES = ROOT / 'src/pilotdeck-gateway/src/native_process/capabilities.rs'
RUNS_METHODS = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/runs.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/tests.rs'
PROCESS_TEST = ROOT / 'src/pilotdeck-gateway/tests/native_registry_switch.rs'
PROCESS = Path(__file__).resolve().parent / 'process.mts'
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

mutations = [
    # The switch itself must resolve "off" to the off state: flipping the
    # parse to On changes the stderr receipt first, before any turn runs.
    ('registry-switch-ignored', ASSEMBLY,
     '"off" => Ok(Some(RunRegistrySwitch::Off)),',
     '"off" => Ok(Some(RunRegistrySwitch::On)),',
     'executable_native_registry_switch', '#1369 executable off registry receipt'),
    # The off state must not open the persistent registry: swapping the arms
    # gives off a real registry, so describe_server lists run reads first.
    ('registry-switch-opens-when-off', ASSEMBLY,
     '''let runs = match registry_switch {
            Some(RunRegistrySwitch::Off) => None,
            Some(RunRegistrySwitch::On) | None => Some(Arc::new(FileRegistry::new(
                FileRunRegistry::open(options.home.join("runs.json"))?,
            )?)),
        };''',
     '''let runs = match registry_switch {
            Some(RunRegistrySwitch::Off) => Some(Arc::new(FileRegistry::new(
                FileRunRegistry::open(options.home.join("runs.json"))?,
            )?)),
            Some(RunRegistrySwitch::On) | None => Some(Arc::new(FileRegistry::new(
                FileRunRegistry::open(options.home.join("runs.json"))?,
            )?)),
        };''',
     'executable_native_registry_switch', '#1369 executable describe_server registry gating'),
    # Production default: an unset switch keeps the registry on.
    ('registry-default-off', ASSEMBLY,
     'let Some(value) = env.get("PILOTDECK_NATIVE_RUN_REGISTRY") else {\n        return Ok(None);\n    };',
     'let Some(value) = env.get("PILOTDECK_NATIVE_RUN_REGISTRY") else {\n        return Ok(Some(RunRegistrySwitch::Off));\n    };',
     'native_process::tests::run_registry_switch_matrix', '#1369 unset registry switch keeps the production default'),
    # Fail closed: any other value, including the empty string, is rejected.
    ('registry-invalid-accepted', ASSEMBLY,
     '''_ => Err(
            "native run registry switch (PILOTDECK_NATIVE_RUN_REGISTRY) must be exactly on or off."
                .into(),
        ),''',
     '_ => Ok(None),',
     'native_process::tests::run_registry_switch_matrix', '#1369 invalid registry switch value rejected'),
    # TS InProcessGateway.ts:2093-2095: without a registry, describeServer
    # drops the run capabilities. #1321 在 `.into_iter()` 与守卫 filter 之间插入
    # upload 能力的 `.chain([...])`；锚点改从守卫 filter 起算，变异字节不变。
    ('registry-capabilities-ungated', CAPABILITIES,
     '''fn runs(requests: &Requests) -> bool {
    requests.runs.is_some()
}''',
     '''fn runs(_requests: &Requests) -> bool {
    true
}''',
     'native_process::tests::requests_registry_switch_gates_run_reads', '#1369 describe_server gates run capabilities'),
    # TS InProcessGateway.ts:2525-2544: a configured registry still serves
    # run reads; the reader must not be forced to None.
    ('registry-reader-always-none', RUNS_METHODS,
     '''let registry: Option<&dyn read::TurnRunReader> = self
                .runs
                .as_deref()
                .map(|runs| runs as &dyn read::TurnRunReader);''',
     'let registry: Option<&dyn read::TurnRunReader> = None;',
     'native_process::tests::requests_registry_switch_gates_run_reads', '#1369 run_get with a registry resolves'),
    # The construction receipt is the #1368 registry boundary witness: the
    # switch must state its resolved state on stderr at startup. Re-anchored
    # to the #2558 fix-r2 shape (the same receipt line, now through the
    # non-panicking `console_log_line` writer instead of `eprintln!`).
    ('registry-receipt-not-published', ASSEMBLY,
     '''super::channels::console_log_line(
            &mut std::io::stderr(),
            &format!("native run registry {state}"),
        );''',
     'let _ = state;',
     'executable_native_registry_switch', '#1369 executable off registry receipt'),
]


def test_body(test, texts=None):
    if texts is None:
        texts = {path: path.read_text() for path in [TESTS, PROCESS_TEST]}
    path = TESTS if test.startswith('native_process::tests::') else PROCESS_TEST
    # These files use top-level test functions. A column-zero closing brace is
    # the end of this function, so a sibling's assertion cannot satisfy it.
    pattern = r'^(?:async )?fn ' + re.escape(test.rsplit('::', 1)[-1]) + r'\([^\n]*\).*?^}'
    bodies = re.findall(pattern, texts[path], re.M | re.S)
    return bodies[0] if len(bodies) == 1 else ''


def check_binding_scope():
    fixture = 'fn target() {\n}\nfn sibling() {\n assert!(false, "only sibling");\n}\n'
    if 'only sibling' in test_body('target', {PROCESS_TEST: fixture}):
        raise RuntimeError('#1369 binding checker accepted sibling assertion')
    if 'only sibling' not in test_body('sibling', {PROCESS_TEST: fixture}):
        raise RuntimeError('#1369 binding checker lost its own assertion')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    check_binding_scope()
    stale = []
    for name, path, old, new, test, assertion in mutations:
        text = path.read_text()
        body = test_body(test)
        if (text.count(old) != 1 or old == new or
                not body or f'"{assertion}"' not in body):
            stale.append(name)
    if stale:
        raise SystemExit('#1369 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#1369 {len(mutations)} unique anchors and assertion bindings: OK')
        return 0
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not selected or (args.selected and len(selected) != len(args.selected)):
        raise SystemExit('#1369 unknown or repeated mutation')
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1369 output and caller CARGO_TARGET_DIR required')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for key in [ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'PDRS_MUTANT', 'LIVE_E2E_MUTANT']:
        env.pop(key, None)
    # #1352: CAPABILITIES is mutated by `registry-capabilities-ungated`, so it
    # must be inside the guarded window too — `originals[path]` is keyed by this
    # list, and a missing entry raises KeyError in the per-mutant `finally` and
    # takes the whole fixture down instead of failing one row.
    guarded = [ASSEMBLY, CAPABILITIES, REQUESTS, RUNS_METHODS, TESTS, PROCESS_TEST, PROCESS, Path(__file__).resolve(),
               FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_suite.py',
               ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
               ROOT / 'src/pilotdeck-gateway/src/lib.rs',
               ROOT / 'src/pilotdeck-gateway/src/test_process.rs']
    results = []
    guarded_sha256 = {}
    sidecar_sha256 = None

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway']
        command += (['--test', 'native_registry_switch']
                    if test == 'executable_native_registry_switch' else ['--lib'])
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        run_env = dict(run_env)
        if test == 'executable_native_registry_switch':
            run_env['PDRS_1369_PROCESS_OUTPUT'] = str(output / (label + '-process'))
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded) as originals:
            guarded_sha256 = {str(path.relative_to(ROOT)): hashlib.sha256(originals[path]).hexdigest() for path in guarded}
            if any(row[4] == 'executable_native_registry_switch' for row in selected):
                slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
                (output / 'sidecar-build.slots.log').write_text(slot.stdout + slot.stderr)
                command = ['cargo', 'build', '--offline', '--manifest-path', 'src/Cargo.toml',
                           '-p', 'pilotdeck-transport', '--bin', 'pilotdeck-agent-loop-sidecar']
                (output / 'sidecar-build.command.json').write_text(json.dumps(command) + '\n')
                built = run_owned(command, cwd=ROOT, env=env, timeout=900)
                (output / 'sidecar-build.log').write_text(built.stdout + built.stderr)
                if built.returncode:
                    raise RuntimeError('#1369 current-source sidecar build failed')
                sidecar = Path(env['CARGO_TARGET_DIR']) / 'debug/pilotdeck-agent-loop-sidecar'
                sidecar_sha256 = hashlib.sha256(sidecar.read_bytes()).hexdigest()
            for name, path, old, new, test, assertion in selected:
                item = dict(name=name, test=test, assertion=assertion,
                            status='INVALID', reason='not run', compiled=False)
                try:
                    if not args.compile_only and not passed(run(name + '-positive', env, test), test):
                        raise RuntimeError('positive bound test failed')
                    path.write_text(originals[path].decode().replace(old, new, 1))
                    if run(name + '-compile', env, test, compile_only=True).returncode:
                        raise RuntimeError('mutant did not compile; not a runtime kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='mutated source compiled')
                    else:
                        receipt = ReceiptRun(output, name, [(test, assertion)])
                        result = run(name + '-negative', receipt.environment(env), test)
                        killed, reason, hits = judge(result, '', [(test, assertion)], receipt)
                        item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'),
                                    reason=reason, hits=hits)
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not args.compile_only:
                        if not passed(run(name + '-restored', env, test), test):
                            raise RuntimeError('restored bound test failed')
                        item['restoredPass'] = True
                except Exception as error:
                    item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
                results.append(item)
                print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            results.append(dict(name=name, test=test, assertion=assertion,
                                status='INVALID', reason=reason, compiled=False))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256, 'sidecarSha256': sidecar_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
