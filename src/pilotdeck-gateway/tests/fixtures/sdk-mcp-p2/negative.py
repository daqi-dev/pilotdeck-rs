#!/usr/bin/env python3
"""#347/#401 (MCP P2 batch 1) negative controls.

Temporary product mutations over the fixes for #347 (the `projectKey`
TypeError, `clearSdkSessionState`'s MCP fields, and the one shared
session→project table) and #401 (user cancel vs timeout on a shared MCP
client). Each mutant names the regression test whose `#347`/`#401`-tagged
assertion message it must turn red: one that fails to compile is INVALID, one
whose test stays green is SURVIVED, and a stale anchor (count != 1) aborts the
run. Every mutant is compile-verified (`cargo check --tests`) before its test
runs. The executor runs ALL mutations, restoring each source byte-for-byte,
then summarizes; any non-KILLED result exits nonzero.

Run post-commit from a clean tree (the mutation guard requires the gateway
product sources to equal HEAD):

    CARGO_TARGET_DIR=<shared build tree> \
      python3 tests/fixtures/sdk-mcp-p2/negative.py <fresh-output-dir> [name ...]

The caller must export CARGO_TARGET_DIR; this script error-exits without it.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime'

# (name, file under src/pilotdeck-gateway/src/runtime, old, new,
#  cargo test target, test filter, the assertion message that must go red)
MUTATIONS = [
    # ── #347① projectKey ─────────────────────────────────────────────────
    ('project-key-nonstring-fallback', 'mcp/sdk.rs',
     'Some(value) => Err(SdkMcpError::type_error(format!(\n'
     '            "The \\"paths[0]\\" argument must be of type string. Received {}",\n'
     '            crate::metadata::argument_received(value).unwrap_or_else(|| "[Object]".into())\n'
     '        ))),',
     'Some(_) => Ok(None),',
     '--test=sdk_mcp', 'non_string_project_key_throws_the_node_type_error',
     '#347 a non-string projectKey must reject setMcpServersForSdk'),
    ('project-key-message-text', 'mcp/sdk.rs',
     'must be of type string. Received {}',
     'must be a string. Received {}',
     '--test=sdk_mcp', 'non_string_project_key_throws_the_node_type_error',
     '#347 number projectKey message'),
    ('null-key-throws', 'mcp/sdk.rs',
     'None | Some(JsValue::Undefined) | Some(JsValue::Null) => Ok(None),',
     'None | Some(JsValue::Undefined) => Ok(None),',
     '--test=sdk_mcp', 'nullish_project_key_keeps_the_fallback_project',
     '#347 null projectKey must fall back'),
    # ── #347② clear + read entries ────────────────────────────────────────
    ('clear-keeps-servers', 'mcp/sdk.rs',
     'lock(&self.servers).retain(|(key, _)| !same_key(key, session_key));',
     '// mutant: keep the cleared session\'s server table',
     '--test=sdk_mcp', 'clear_sdk_session_state_drops_only_the_given_session',
     "#347 clear keeps session t's server table"),
    ('clear-keeps-overrides', 'mcp/sdk.rs',
     'lock(&self.overrides).retain(|(existing, _)| existing != key);',
     '// mutant: keep the cleared session\'s permission-mode overrides',
     '--test=sdk_mcp', 'clear_sdk_session_state_drops_only_the_given_session',
     "#347 clear drops session s's permission-mode overrides"),
    ('clear-keeps-rules', 'mcp/sdk.rs',
     'lock(&self.rules).retain(|(existing, _)| existing != key);',
     '// mutant: keep the cleared session\'s generated rules',
     '--test=sdk_mcp', 'clear_sdk_session_state_drops_only_the_given_session',
     "#347 clear drops session s's generated rules"),
    ('clear-keeps-project-entry', 'mcp/sdk.rs',
     'self.options.projects.delete(session_key);',
     '// mutant: keep the cleared session\'s shared project entry',
     '--test=sdk_mcp', 'clear_sdk_session_state_drops_only_the_given_session',
     "#347 clear drops only session s's shared project entry"),
    ('read-returns-empty', 'mcp/sdk.rs',
     'self.servers_for(session_key)\n'
     '            .into_iter()\n'
     '            .map(|(name, entry)| (name, server_value(&entry)))\n'
     '            .collect()',
     'Vec::new()',
     '--test=sdk_mcp', 'session_mcp_servers_reads_the_table_in_insertion_order',
     "#347 the read keeps the set call's insertion order"),
    # ── #347③ the shared session→project table ────────────────────────────
    ('registry-skips-shared-write', 'sdk_session/registry.rs',
     'self.projects\n'
     '            .set(&JsValue::String(session_key.clone()), path_js(&root));',
     'let _ = (session_key, root);',
     '--test=sdk_mcp', 'session_to_project_table_is_shared_with_the_session_registry',
     '#347 setSdkSessionConfig records the session root'),
    ('mcp-write-not-shared', 'mcp/sdk.rs',
     'self.options.projects.set(&session_key, project_root);',
     'let _ = (&session_key, &project_root);',
     '--test=sdk_mcp', 'session_to_project_table_is_shared_with_the_session_registry',
     '#347 both slices append to the one shared table in insertion order'),
    ('set-appends-duplicate', 'sdk_session/projects.rs',
     'Some((_, root)) => *root = project_root,',
     'Some(_) => {}',
     '--lib', 'set_replaces_in_place_and_keeps_insertion_order',
     '#347 set replaces an existing key in place, like the TS Map'),
    ('session-project-none', 'sdk_session/registry.rs',
     'self.projects.project(&JsValue::String(session_key.clone()))',
     'let _ = session_key;\n        None',
     '--test=sdk_mcp', 'session_to_project_table_is_shared_with_the_session_registry',
     '#347 setSdkSessionConfig records the session root'),
    # ── #401 user cancel vs timeout ────────────────────────────────────────
    ('timeout-code-ignored', 'mcp/client.rs',
     'if error.code == Some(McpErrorCode::Number(-32001.0)) || is_timeout_message(&error.message)',
     'if false && error.code == Some(McpErrorCode::Number(-32001.0))\n'
     '            || is_timeout_message(&error.message)',
     '--test=mcp_client', 'user_cancel_mid_request_shares_the_timeout_recycle_path',
     '#401 a mid-request user cancel surfaces as the timeout code, like the TS baseline'),
    ('no-recycle', 'mcp/client.rs',
     'self.recycle_transport_after_timeout(current.as_ref());',
     '// mutant: no transport recycle after the timeout code',
     '--test=mcp_client', 'user_cancel_mid_request_shares_the_timeout_recycle_path',
     '#401 the recycle marks the client error'),
    ('preaborted-check-removed', 'mcp/wire.rs',
     'if signal.is_some_and(McpAbortSignal::is_aborted) {\n'
     '            return Err(McpClientError::with_code(\n'
     '                "AbortError",\n'
     '                McpErrorCode::Number(20.0),\n'
     '                ABORT_MESSAGE.into(),\n'
     '            ));\n'
     '        }',
     '',
     '--test=mcp_client', 'pre_aborted_signal_fails_only_the_caller',
     '#401 a pre-aborted signal is a plain call failure, not the timeout path'),
    ('abort-message-text', 'mcp/wire.rs',
     'const ABORT_MESSAGE: &str = "This operation was aborted";',
     'const ABORT_MESSAGE: &str = "aborted";',
     '--test=mcp_client', 'pre_aborted_signal_fails_only_the_caller',
     '#401 the DOMException text is preserved'),
]


def main():
    selected = sys.argv[2:]
    mutations = MUTATIONS
    if selected:
        mutations = [m for m in MUTATIONS if m[0] in selected]
        unknown = set(selected) - {m[0] for m in mutations}
        assert not unknown, f'unknown mutation name(s): {sorted(unknown)}'
    if not os.environ.get('CARGO_TARGET_DIR'):
        sys.exit('CARGO_TARGET_DIR must be exported by the caller '
                 '(the shared slot-aware build tree)')
    env = os.environ.copy()
    env.setdefault('CARGO_PROFILE_DEV_DEBUG', 'line-tables-only')
    env.setdefault('CARGO_PROFILE_TEST_DEBUG', 'line-tables-only')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    check_command = ['cargo', 'check', '--manifest-path', 'src/Cargo.toml',
                     '-p', 'pilotdeck-gateway', '--tests']
    results = []
    with mutating_sources(
        root,
        sorted({source / filename for _, filename, *_ in mutations}),
    ) as snapshots:
        for name, filename, old, new, target, test, message in mutations:
            path = source / filename
            original = snapshots[path]
            before = hashlib.sha256(original).hexdigest()
            content = original.decode()
            count = content.count(old)
            if count != 1:
                results.append(dict(name=name, status='INVALID',
                                    reason=f'stale anchor: count {count} != 1'))
                print(f'{name}: INVALID stale anchor count {count}', flush=True)
                continue
            changed = content.replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=filename, tofile=filename)))
            try:
                # Compile first: a mutant that does not build is INVALID, not
                # evidence.
                wait_for_slot(root / 'scripts/cargo-slots.sh')
                path.write_text(changed)
                check = run_owned(check_command, cwd=root, env=env, timeout=900)
                (out / f'{name}.check.log').write_text(check.stdout + check.stderr)
                if check.returncode != 0:
                    results.append(dict(name=name, status='INVALID',
                                        reason='mutant does not compile'))
                    print(f'{name}: INVALID does not compile', flush=True)
                    continue
                wait_for_slot(root / 'scripts/cargo-slots.sh')
                result = run_owned(
                    ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                     '-p', 'pilotdeck-gateway', target, '--', test],
                    cwd=root, env=env, timeout=900)
                log = result.stdout + result.stderr
                (out / f'{name}.log').write_text(log)
                killed = (result.returncode == 101
                          and f'{test} ... FAILED' in log
                          and message in log)
                results.append(dict(
                    name=name,
                    status='KILLED' if killed else 'SURVIVED',
                    reason='' if killed else
                    f'rc={result.returncode}; tagged message not observed',
                    message=message,
                    sourceSha256=before,
                ))
                print(f'{name}: {"KILLED" if killed else "SURVIVED"}', flush=True)
            finally:
                path.write_bytes(original)
                assert hashlib.sha256(path.read_bytes()).hexdigest() == before
            (out / 'summary.json').write_text(
                json.dumps(results, ensure_ascii=False, indent=2) + '\n')
    not_killed = [result for result in results if result['status'] != 'KILLED']
    print(f'{len(results) - len(not_killed)}/{len(results)} killed; '
          f'every source restored byte-for-byte', flush=True)
    if not_killed:
        for result in not_killed:
            print(f"{result['name']}: {result['status']} {result.get('reason', '')}",
                  flush=True)
        sys.exit(1)


if __name__ == '__main__':
    main()
