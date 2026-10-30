#!/usr/bin/env python3
"""#1071: commit first, then compile and run every native-process mutation.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound nonce receipt, and pass again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
"""
import argparse
import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/native_process.rs'
FAMILIES = ROOT / 'src/pilotdeck-gateway/src/native_process/families.rs'
ASSEMBLY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
TOOLS = ROOT / 'src/pilotdeck-gateway/src/native_process/tools.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/tests.rs'
MAIN = ROOT / 'src/pilotdeck-gateway/src/main.rs'
PROCESS_TEST = ROOT / 'src/pilotdeck-gateway/tests/native_process.rs'
WS_TEST = ROOT / 'src/pilotdeck-gateway/src/native_ws/tests.rs'
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

mutations = [
    ('a4-opt-in-guard', MAIN,
     'let mut args = std::env::args_os().skip(1);',
     'let mut args = std::env::args_os().skip(1).skip(0);',
     'native_ws::tests::default_mode_unchanged', '#1071 exact explicit opt-in branch'),
    ('a4-default-bytes', MAIN,
     '"127.0.0.1:28789"', '"127.0.0.1:0"',
     'native_ws::tests::default_mode_unchanged', '#749 A4 executable remains unchanged'),
    ('signal-shutdown-disconnected', SOURCE,
     'server.run(stop, options.drain_budget).await',
     '{ let _ = stop; server.run(std::future::pending::<()>(), options.drain_budget).await }',
     'executable_native_and_a4', '#1071 executable native and A4 oracle'),
    ('non-utf8-environment', SOURCE,
     'value.to_string_lossy().into_owned()',
     'value.to_str().expect("#1071 invalid environment mutation").to_owned()',
     'native_process::tests::explicit_options_and_environment', '#1071 non UTF-8 environment does not panic'),
    ('configured-modules', ASSEMBLY,
     '.is_some_and(|modules| !modules.own_entries().is_empty())',
     '.is_some_and(|modules| modules.own_entries().is_empty())',
     'native_process::tests::production_profile_and_registration', '#1071 unsupported module bindings refused'),
    ('disconnected-cancel-signal', ASSEMBLY,
     'ports.turn(identity, recorder, signal)',
     'ports.turn(identity, recorder, { let _ = signal; crate::runtime::hooks::Cancellation::new() })',
     'executable_native_and_a4', '#1071 executable native and A4 oracle'),
    ('native-entry-disconnected', MAIN,
     'if pilotdeck_gateway::native_process::native_requested(args.next().as_deref()) {',
     'if false && pilotdeck_gateway::native_process::native_requested(args.next().as_deref()) {',
     'executable_native_and_a4', '#1071 executable native and A4 oracle'),
    ('opt-in', SOURCE,
     'first == Some(std::ffi::OsStr::new("--native"))',
     'first == Some(std::ffi::OsStr::new("--other"))',
     'native_process::tests::explicit_options_and_environment', '#1071 explicit native switch'),
    ('isolated-config', SOURCE,
     'inherited.remove("PILOTDECK_CONFIG_PATH");',
     'inherited.remove("UNUSED_CONFIG_PATH");',
     'native_process::tests::explicit_options_and_environment', '#1071 inherited authority removed'),
    ('separate-roots', SOURCE,
     'if home.starts_with(&project) || project.starts_with(&home) {',
     'if false && (home.starts_with(&project) || project.starts_with(&home)) {',
     'native_process::tests::explicit_options_and_environment', '#1071 separated roots'),
    ('bounded-drain', SOURCE,
     'if drain_ms == 0 || drain_ms > 300_000 {',
     'if false && (drain_ms == 0 || drain_ms > 300_000) {',
     'native_process::tests::explicit_options_and_environment', '#1071 invalid configuration rejected'),
    ('loopback', SOURCE,
     'listen.ip() != Ipv4Addr::LOCALHOST',
     'false && listen.ip() != Ipv4Addr::LOCALHOST',
     'native_process::tests::explicit_options_and_environment', '#1071 invalid configuration rejected'),
    ('ambient-state', SOURCE,
     'if env.get(key).is_some_and(|v| !v.is_empty()) {',
     'if false && env.get(key).is_some_and(|v| !v.is_empty()) {',
     'native_process::tests::explicit_options_and_environment', '#1071 ambient state refused'),
    ('disposed-lsp', FAMILIES,
     'self.disposed.store(true, Ordering::Release);',
     'self.disposed.store(false, Ordering::Release);',
     'native_process::tests::default_lsp_matches_ts', '#1071 disposed LSP rejects bind'),
    ('lsp-error', FAMILIES,
     '"LSP_WORKSPACE_REQUIRED",',
     '"LSP_WORKSPACE_MISSING",',
     'native_process::tests::default_lsp_matches_ts', '#1071 LSP oracle'),
    ('title-truncation', FAMILIES,
     'if text.len() > 1200 {',
     'if text.len() > 1201 {',
     'native_process::tests::title_matches_ts_requests_and_results', '#1071 title request oracle'),
    ('title-cancel-wait', FAMILIES,
     '_ = input.signal.cancelled() => { abort.abort(); response.await.ok() },',
     '_ = input.signal.cancelled() => { abort.abort(); None },',
     'native_process::tests::title_waits_for_provider_after_abort', '#1071 title waits after abort'),
    ('title-timeout-wait', FAMILIES,
     '_ = tokio::time::sleep(timeout) => { abort.abort(); response.await.ok() },',
     '_ = tokio::time::sleep(timeout) => { abort.abort(); None },',
     'native_process::tests::title_waits_for_provider_after_abort', '#1071 title waits after abort'),
    ('project-marker', ASSEMBLY,
     'std::fs::hard_link(&temporary, &marker)',
     'Ok(())',
     'native_process::tests::production_profile_and_registration', '#1071 project visible to metadata'),
    ('configured-mcp', ASSEMBLY,
     'if !mcp.servers.own_entries().is_empty() || !mcp.diagnostics.is_empty() {',
     'if false && (!mcp.servers.own_entries().is_empty() || !mcp.diagnostics.is_empty()) {',
     'native_process::tests::production_profile_and_registration', '#1071 unsupported MCP refused'),
    ('configured-contributions', ASSEMBLY,
     'if entries.count() != 0 {',
     'if entries.count() == usize::MAX {',
     'native_process::tests::production_profile_and_registration', '#1071 unsupported contributions refused'),
    ('configured-subagents', ASSEMBLY,
     '.is_some_and(|v| !v.is_undefined())',
     '.is_some_and(|v| v.is_undefined())',
     'native_process::tests::production_profile_and_registration', '#1071 unsupported subagents refused'),
    ('empty-allowlist', TOOLS,
     'allowed.as_ref().is_none_or(|names| names.contains(name))',
     'allowed.as_ref().is_none_or(|_names| true)',
     'native_process::tests::tool_policy_and_turn_environment', '#1071 empty tool allowlist enforced'),
    ('denylist', TOOLS,
     '!denied.as_ref().is_some_and(|names| names.contains(name))',
     '!denied.as_ref().is_some_and(|_names| false)',
     'native_process::tests::tool_policy_and_turn_environment', '#1071 tool denylist enforced'),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    stale = []
    for name, path, old, new, test, assertion in mutations:
        text = path.read_text()
        tests = TESTS.read_text() + PROCESS_TEST.read_text() + WS_TEST.read_text()
        if (text.count(old) != 1 or old == new or
                f'fn {test.rsplit("::", 1)[-1]}(' not in tests or f'"{assertion}"' not in tests):
            stale.append(name)
    if stale:
        raise SystemExit('#1071 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#1071 {len(mutations)} unique anchors and assertion bindings: OK')
        return 0
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not selected or (args.selected and len(selected) != len(args.selected)):
        raise SystemExit('#1071 unknown or repeated mutation')
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1071 output and caller CARGO_TARGET_DIR required')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for key in [ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'PDRS_MUTANT', 'LIVE_E2E_MUTANT']:
        env.pop(key, None)
    guarded = [SOURCE, FAMILIES, ASSEMBLY, TOOLS, TESTS, MAIN, PROCESS_TEST, WS_TEST, Path(__file__).resolve(), FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_suite.py',
               ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs']
    results = []

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway']
        command += ['--test', 'native_process'] if test == 'executable_native_and_a4' else ['--lib']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        run_env = dict(run_env)
        if test == 'executable_native_and_a4':
            run_env['PDRS_1071_PROCESS_OUTPUT'] = str(output / (label + '-process'))
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded) as originals:
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
        {'controls': results, 'counts': counts}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
