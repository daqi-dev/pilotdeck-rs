#!/usr/bin/env python3
"""#1433: commit first, then compile and run every session-MCP mutation.

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
TOOLS = ROOT / 'src/pilotdeck-gateway/src/native_process/tools.rs'
PROCESS_TEST = ROOT / 'src/pilotdeck-gateway/tests/sdk_mcp_session_tools.rs'
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

mutations = [
    # The production read face of `session_mcp_servers` must project only the
    # enabled entries (TS getSdkMcpServers filters `server.enabled`).
    ('enabled-filter-dropped', TOOLS,
     '''        if !crate::runtime::mcp::sdk::js_truthy(&enabled) {
            continue;
        }''',
     '        let _ = &enabled;',
     'native_process::tools::tests::session_mcp_tools_skips_disabled_servers',
     '#1433 a disabled session server composes no tools'),
    # Shared-lease tools keep existing names (SessionMcpRuntimeBundle.ts:62);
    # making them replace the builtin inverts the TS skip-on-conflict.
    ('shared-conflict-replaced', TOOLS,
     '''            if !tools.has(&name) {
                tools
                    .register(tool.clone())
                    .expect("#1433 shared session MCP tool name is unique");
            }''',
     '''            if tools.has(&name) {
                tools.replace(tool.clone()).expect("#1433 shared replaces");
            } else {
                tools
                    .register(tool.clone())
                    .expect("#1433 shared session MCP tool name is unique");
            }''',
     'native_process::tools::tests::register_keeps_shared_names_and_replaces_per_session_names',
     '#1433 a shared session MCP tool keeps the existing builtin name'),
    # Per-session tools replace same names (SessionMcpRuntimeBundle.ts:140);
    # skipping the conflict leaves the builtin in place.
    ('per-session-replace-lost', TOOLS,
     '''            if tools.has(&name) {
                tools
                    .replace(tool.clone())
                    .expect("#1433 per-session MCP tool replaces its own name");
            } else {
                tools
                    .register(tool.clone())
                    .expect("#1433 per-session MCP tool name is unique");
            }''',
     '''            if !tools.has(&name) {
                tools
                    .register(tool.clone())
                    .expect("#1433 per-session MCP tool name is unique");
            }''',
     'native_process::tools::tests::register_keeps_shared_names_and_replaces_per_session_names',
     '#1433 a per-session MCP tool replaces the same-name builtin'),
    # Release order: per-session before shared (the reverse of composition).
    ('release-order-swapped', TOOLS,
     '''        if let Some(registration) = self.per_session
            && let Err(error) = registration.dispose().await
        {
            failures.push(error);
        }
        if let Some(lease) = self.shared
            && let Err(error) = lease.release().await
        {
            failures.push(error);
        }''',
     '''        if let Some(lease) = self.shared
            && let Err(error) = lease.release().await
        {
            failures.push(error);
        }
        if let Some(registration) = self.per_session
            && let Err(error) = registration.dispose().await
        {
            failures.push(error);
        }''',
     'native_process::tools::tests::session_mcp_release_stops_per_session_before_shared',
     '#1433 release order: per-session MCP before shared MCP'),
    # `>=` is the TS limit check; `>` lets one runtime past the cap.
    ('per-session-limit-off', TOOLS,
     'if options.per_session_runtimes.size() >= options.max_per_session_instances {',
     'if options.per_session_runtimes.size() > options.max_per_session_instances {',
     'native_process::tools::tests::session_mcp_tools_respects_the_per_session_limit',
     '#1433 the limit skips the per-session server'),
    # A per-session start failure degrades to a diagnostic; propagating it
    # takes the whole session down instead.
    ('start-failure-propagated', TOOLS,
     '''        Err(error) => {
            diagnostic(
                format!("Per-session MCP startup failed for {session_key}"),
                Some(error),
            );
            return Ok((tools, release));
        }''',
     '''        Err(error) => {
            return Err(error);
        }''',
     'native_process::tools::tests::session_mcp_tools_degrades_per_session_start_failures',
     '#1433 composition survives the start failure'),
    # The turn registry must carry the composed tools; dropping the register
    # call is the original #1433 defect (tools silently missing).
    ('turn-registry-drops-mcp', TOOLS,
     '    mcp.register(&tools);',
     '    let _ = mcp;',
     'native_process::tools::tests::native_turn_registry_carries_the_session_mcp_tools',
     '#1433 the turn registry carries the session MCP tool'),
    # The composition must read the enabled table; an empty table is the
    # silent-miss defect on the production path (S3 face 1 over real stdio
    # children).
    ('composition-reads-no-servers', TOOLS,
     'let servers = enabled_session_mcp_servers(options.mcp_config, options.session_key);',
     'let servers = JsObject::new();',
     'discovery_composes_shared_and_per_session_tools',
     '#1433 the shared lease tool is discovered'),
]

EXTERNAL = {'discovery_composes_shared_and_per_session_tools'}
LIB = {test for _, _, _, _, test, _ in mutations} - EXTERNAL


def test_body(test, texts=None):
    if texts is None:
        texts = {path: path.read_text() for path in [TOOLS, PROCESS_TEST]}
    path = PROCESS_TEST if test in EXTERNAL else TOOLS
    # The unit tests live inside `mod tests` (indented) and the external
    # tests at the crate top level, so the function ends at a closing brace
    # with the function's own indentation — deeper (nested block) braces
    # never end it, and a sibling's assertion cannot satisfy it.
    pattern = (r'^(?P<indent>[ \t]*)(?:async )?fn ' + re.escape(test.rsplit('::', 1)[-1])
               + r'\([^\n]*\).*?^(?P=indent)}')
    bodies = [match.group(0)
              for match in re.finditer(pattern, texts[path], re.M | re.S)]
    return bodies[0] if len(bodies) == 1 else ''


def check_binding_scope():
    fixture = ('fn target() {\n}\n'
               'fn sibling() {\n assert!(false, "only sibling");\n}\n')
    texts = {PROCESS_TEST: fixture, TOOLS: ''}
    if 'only sibling' in test_body('target', texts):
        raise RuntimeError('#1433 binding checker accepted sibling assertion')
    indented = 'mod tests {\n    async fn target() {\n    }\n' \
               '    async fn sibling() {\n        assert!(false, "only sibling");\n    }\n}\n'
    texts[TOOLS] = indented
    if 'only sibling' not in test_body('tests::sibling', texts):
        raise RuntimeError('#1433 binding checker lost its own assertion')


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
        raise SystemExit('#1433 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#1433 {len(mutations)} unique anchors and assertion bindings: OK')
        return 0
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not selected or (args.selected and len(selected) != len(args.selected)):
        raise SystemExit('#1433 unknown or repeated mutation')
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1433 output and caller CARGO_TARGET_DIR required')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for key in [ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'PDRS_MUTANT', 'LIVE_E2E_MUTANT']:
        env.pop(key, None)
    guarded = [TOOLS, PROCESS_TEST,
               ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs',
               ROOT / 'src/pilotdeck-gateway/src/native_process.rs',
               ROOT / 'src/pilotdeck-gateway/src/native_process/tests.rs',
               ROOT / 'src/pilotdeck-gateway/src/native_process/review_tests.rs',
               ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/registry.rs',
               ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/sdk.rs',
               ROOT / 'src/pilotdeck-gateway/src/runtime/project/tools_mcp.rs',
               ROOT / 'src/pilotdeck-gateway/src/lib.rs',
               ROOT / 'src/pilotdeck-gateway/src/test_process.rs',
               ROOT / 'src/pilotdeck-gateway/src/process_group.rs',
               ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
               ROOT / 'src/pilotdeck-gateway/tests/common/exact_receipt.rs',
               ROOT / 'src/pilotdeck-gateway/tests/common/scratch.rs',
               ROOT / 'src/pilotdeck-gateway/tests/fixtures/mcp-client/mock_server.mjs',
               Path(__file__).resolve(),
               FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_suite.py']
    results = []
    guarded_sha256 = {}

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway']
        command += (['--test', 'sdk_mcp_session_tools'] if test in EXTERNAL else ['--lib'])
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded) as originals:
            guarded_sha256 = {str(path.relative_to(ROOT)): hashlib.sha256(originals[path]).hexdigest()
                              for path in guarded}
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
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
