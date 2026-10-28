#!/usr/bin/env python3
"""#347/#401 (MCP P2 batch 1 + fix-r1/fix-r2) negative controls.

Temporary product mutations over the fixes for #347 (the `projectKey`
TypeError, `clearSdkSessionState`'s MCP fields, the one shared
session→project table, the clear/set critical section and the constructor
`name` projections) and #401 (user cancel vs timeout on a shared MCP
client). Each mutant names the regression test whose `#347`/`#401`-tagged
assertion message it must turn red; the kill check runs that test with
`--exact` and accepts the message only inside the test's own panic block,
so a failing sibling with a shared filter suffix or message can never
count (fix-r1, review P1-4). A mutant that does not compile is INVALID, one
whose test stays green is SURVIVED, and a stale anchor (count != 1) is
INVALID without skipping later mutants. Every mutant is compile-verified (`cargo check --tests`) before
its test runs. The executor runs ALL mutations, restoring each source
byte-for-byte, then summarizes; any non-KILLED result exits nonzero.

The boundary mutants bind the actual try_lock outcome in the cfg(test)
SDK unit module. Delayed rows pause AFTER the start marker and BEFORE the
real acquisition probe. Both Acquired and WouldBlock are reported; no sleep
or is_finished observation is accepted as contention evidence. Controls use
the same post-marker delay without the product mutation.

Run post-commit from a clean tree (the mutation guard requires the gateway
product sources to equal HEAD):

    CARGO_TARGET_DIR=<shared build tree> \
      python3 tests/fixtures/sdk-mcp-p2/negative.py <fresh-output-dir> [name ...]

`--classify-selftest <log-file>` evaluates the kill predicate against a
saved libtest log (no mutation) and exits nonzero if the predicate is
wrong; the review's synthetic sibling-failure log must classify as False.

The caller must export CARGO_TARGET_DIR; this script error-exits without it.
"""
import difflib
import json
import os
import subprocess
import time
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import passed

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime'

# (name, file under src/pilotdeck-gateway/src/runtime, old, new,
#  cargo test target, full test name for --exact, the assertion message that
#  must go red inside that test's own panic block)
MUTATIONS = [
    # ── #347① projectKey ─────────────────────────────────────────────────
    ('project-key-nonstring-fallback', 'mcp/sdk.rs',
     'Some(value) => Err(SdkMcpError::type_error_with_code(\n'
     '            "ERR_INVALID_ARG_TYPE",\n'
     '            format!(\n'
     '                "The \\"paths[0]\\" argument must be of type string. Received {}",\n'
     '                crate::metadata::argument_received(value)'
     '.unwrap_or_else(|| "[Object]".into())\n'
     '            ),\n'
     '        )),',
     'Some(_) => Ok(None),',
     '--test=sdk_mcp', 'non_string_project_key_throws_the_node_type_error',
     '#347 a non-string projectKey must reject setMcpServersForSdk'),
    ('project-key-message-text', 'mcp/sdk.rs',
     'must be of type string. Received {}',
     'must be a string. Received {}',
     '--test=sdk_mcp', 'non_string_project_key_throws_the_node_type_error',
     '#347 number projectKey message'),
    ('project-key-code-dropped', 'mcp/sdk.rs',
     'Some(value) => Err(SdkMcpError::type_error_with_code(\n'
     '            "ERR_INVALID_ARG_TYPE",\n'
     '            format!(',
     'Some(value) => Err(SdkMcpError::type_error(\n'
     '            format!(',
     '--test=sdk_mcp', 'non_string_project_key_throws_the_node_type_error',
     "projectKey keeps Node's error code"),
    ('null-key-throws', 'mcp/sdk.rs',
     'None | Some(JsValue::Undefined) | Some(JsValue::Null) => Ok(None),',
     'None | Some(JsValue::Undefined) => Ok(None),',
     '--test=sdk_mcp', 'nullish_project_key_keeps_the_fallback_project',
     '#347 null projectKey must fall back'),
    # ── #347 fix-r1 (review P1-3): the constructor `name` projections ──────
    ('constructor-name-object-fallback', '../metadata.rs',
     'Some(name) => format!(\n'
     '                    "an instance of {}",\n'
     '                    String::from_utf16(&crate::js_value::to_string_units_checked(name)'
     '.ok()?)\n'
     '                        .ok()?\n'
     '                ),',
     'Some(_) => "[Object]".into(),',
     '--test=sdk_mcp', 'constructor_name_shapes_project_the_node_received_text',
     'name-object set message must match Node\'s String(name) conversion'),
    ('constructor-name-empty-string', '../metadata.rs',
     'Some(name) => format!(\n'
     '                    "an instance of {}",',
     'Some(JsValue::String(name)) if name.is_empty() => "[Object]".into(),\n'
     '                Some(name) => format!(\n'
     '                    "an instance of {}",',
     '--test=sdk_mcp', 'constructor_name_shapes_project_the_node_received_text',
     'name-empty-string set message must match Node\'s String(name) conversion'),
    # ── #347② clear + read entries ────────────────────────────────────────
    ('clear-keeps-servers', 'mcp/sdk.rs',
     'servers.retain(|(key, _)| !same_key(key, session_key));',
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
    # ── #347 fix-r1 (review P1-1): the clear/set critical section ──────────
    ('clear-commit-boundary', 'mcp/sdk.rs',
     'self.options.projects.delete(session_key);\n'
     '        drop(servers);',
     'drop(servers);\n'
     '        self.options.projects.delete(session_key);',
     '--lib', 'runtime::mcp::sdk::concurrency::clear_and_set_commits_share_one_critical_section',
     '#347 fix-r4b cell3 must observe WouldBlock at the real servers lock'),
    ('set-commit-boundary', 'mcp/sdk.rs',
     'self.options.projects.set(&session_key, project_root);\n'
     '        }\n'
     '        (self.options.close_session)(session_key, "sdk_mcp_servers_changed").await?;',
     '}\n'
     '        self.options.projects.set(&session_key, project_root);\n'
     '        (self.options.close_session)(session_key, "sdk_mcp_servers_changed").await?;',
     '--lib', 'runtime::mcp::sdk::concurrency::set_commit_cannot_outlive_the_shared_table_write',
     '#347 fix-r4b cell4 must observe WouldBlock at the real servers lock'),
    ('set-read-window-unlocked', 'mcp/sdk.rs',
     '            let mut next: Vec<(JsString, ServerEntry)> = Vec::new();\n            let mut errors: Vec<(JsString, &\'static str)> = Vec::new();\n            for (name, config) in own_entries(input.get("servers")) {\n                if reserved.iter().any(|existing| existing == &name) {\n                    errors.push((name, RESERVED_NAME));\n                    continue;\n                }\n                match check_config(&name, &config, self.options.environment.as_ref())? {\n                    ConfigCheck::Valid => {\n                        // TS calls `structuredClone(config)` for every accepted\n                        // entry; both it and the later `JSON.stringify(value)`\n                        // go through the shared wire-depth bound, including\n                        // new names.\n                        json_text(&config)?;\n                        let enabled = current\n                            .iter()\n                            .find(|(existing, _)| existing == &name)\n                            .map(|(_, entry)| entry.enabled.clone())\n                            .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))\n                            .unwrap_or(JsValue::Bool(true));\n                        next.push((\n                            name,\n                            ServerEntry {\n                                config: config.clone(),\n                                enabled,\n                            },\n                        ));\n                    }\n                    ConfigCheck::Diagnostic(message) => errors.push((name, message)),\n                }\n            }\n            if !errors.is_empty() {\n                return Ok(set_result(&[], &[], &errors));\n            }\n\n            let mut added_names: Vec<JsString> = Vec::new();\n            for (name, value) in &next {\n                // TS serializes every next entry, so a new name cannot skip\n                // the check that an existing name already performs.\n                let value_text = json_text(&server_value(value))?;\n                let changed = match current.iter().find(|(existing, _)| existing == name) {\n                    None => true,\n                    Some((_, previous)) => value_text != json_text(&server_value(previous))?,\n                };\n                if changed {\n                    added_names.push(name.clone());\n                }\n            }\n            added = added_names;\n            removed = current\n                .iter()\n                .filter(|(name, _)| !next.iter().any(|(candidate, _)| candidate == name))\n                .map(|(name, _)| name.clone())\n                .collect();\n\n',
     '            drop(servers);\n            let mut next: Vec<(JsString, ServerEntry)> = Vec::new();\n            let mut errors: Vec<(JsString, &\'static str)> = Vec::new();\n            for (name, config) in own_entries(input.get("servers")) {\n                if reserved.iter().any(|existing| existing == &name) {\n                    errors.push((name, RESERVED_NAME));\n                    continue;\n                }\n                match check_config(&name, &config, self.options.environment.as_ref())? {\n                    ConfigCheck::Valid => {\n                        // TS calls `structuredClone(config)` for every accepted\n                        // entry; both it and the later `JSON.stringify(value)`\n                        // go through the shared wire-depth bound, including\n                        // new names.\n                        json_text(&config)?;\n                        let enabled = current\n                            .iter()\n                            .find(|(existing, _)| existing == &name)\n                            .map(|(_, entry)| entry.enabled.clone())\n                            .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))\n                            .unwrap_or(JsValue::Bool(true));\n                        next.push((\n                            name,\n                            ServerEntry {\n                                config: config.clone(),\n                                enabled,\n                            },\n                        ));\n                    }\n                    ConfigCheck::Diagnostic(message) => errors.push((name, message)),\n                }\n            }\n            if !errors.is_empty() {\n                return Ok(set_result(&[], &[], &errors));\n            }\n\n            let mut added_names: Vec<JsString> = Vec::new();\n            for (name, value) in &next {\n                // TS serializes every next entry, so a new name cannot skip\n                // the check that an existing name already performs.\n                let value_text = json_text(&server_value(value))?;\n                let changed = match current.iter().find(|(existing, _)| existing == name) {\n                    None => true,\n                    Some((_, previous)) => value_text != json_text(&server_value(previous))?,\n                };\n                if changed {\n                    added_names.push(name.clone());\n                }\n            }\n            added = added_names;\n            removed = current\n                .iter()\n                .filter(|(name, _)| !next.iter().any(|(candidate, _)| candidate == name))\n                .map(|(name, _)| name.clone())\n                .collect();\n\n            let mut servers = lock(&self.servers);\n',
     '--lib', 'runtime::mcp::sdk::concurrency::set_and_toggle_do_not_lose_updates',
     '#347 fix-r4b toggle must observe WouldBlock at the real servers lock'),
    ('ts-cell3-servers-corrupted', '../../tests/fixtures/sdk-mcp-p2/oracle.json',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": true,\n      "projects": true,\n      "enabled": true,\n      "names": [\n        "b"\n      ],\n      "projectRoot": "/pdrs-fixture/project"\n    }',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": false,\n      "projects": true,\n      "enabled": true,\n      "names": [\n        "b"\n      ],\n      "projectRoot": "/pdrs-fixture/project"\n    }',
     '--lib', 'runtime::mcp::sdk::concurrency::clear_and_set_commits_share_one_critical_section',
     '#347 fix-r4b clear-then-set TS servers'),
    ('ts-cell3-projects-corrupted', '../../tests/fixtures/sdk-mcp-p2/oracle.json',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": true,\n      "projects": true,\n      "enabled": true,\n      "names": [\n        "b"\n      ],\n      "projectRoot": "/pdrs-fixture/project"\n    }',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": true,\n      "projects": false,\n      "enabled": true,\n      "names": [\n        "b"\n      ],\n      "projectRoot": "/pdrs-fixture/project"\n    }',
     '--lib', 'runtime::mcp::sdk::concurrency::clear_and_set_commits_share_one_critical_section',
     '#347 fix-r4b clear-then-set TS projects'),
    ('ts-cell3-enabled-corrupted', '../../tests/fixtures/sdk-mcp-p2/oracle.json',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": true,\n      "projects": true,\n      "enabled": true,\n      "names": [\n        "b"\n      ],\n      "projectRoot": "/pdrs-fixture/project"\n    }',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": true,\n      "projects": true,\n      "enabled": false,\n      "names": [\n        "b"\n      ],\n      "projectRoot": "/pdrs-fixture/project"\n    }',
     '--lib', 'runtime::mcp::sdk::concurrency::clear_and_set_commits_share_one_critical_section',
     '#347 fix-r4b clear-then-set TS enabled'),
    ('ts-cell3-names-corrupted', '../../tests/fixtures/sdk-mcp-p2/oracle.json',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": true,\n      "projects": true,\n      "enabled": true,\n      "names": [\n        "b"\n      ],\n      "projectRoot": "/pdrs-fixture/project"\n    }',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": true,\n      "projects": true,\n      "enabled": true,\n      "names": [\n        "wrong"\n      ],\n      "projectRoot": "/pdrs-fixture/project"\n    }',
     '--lib', 'runtime::mcp::sdk::concurrency::clear_and_set_commits_share_one_critical_section',
     '#347 fix-r4b clear-then-set TS exact names'),
    ('ts-cell3-projectRoot-corrupted', '../../tests/fixtures/sdk-mcp-p2/oracle.json',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": true,\n      "projects": true,\n      "enabled": true,\n      "names": [\n        "b"\n      ],\n      "projectRoot": "/pdrs-fixture/project"\n    }',
     '    {\n      "id": "clear-then-set",\n      "events": [\n        "clear.servers",\n        "clear.return",\n        "second.set"\n      ],\n      "servers": true,\n      "projects": true,\n      "enabled": true,\n      "names": [\n        "b"\n      ],\n      "projectRoot": "/wrong"\n    }',
     '--lib', 'runtime::mcp::sdk::concurrency::clear_and_set_commits_share_one_critical_section',
     '#347 fix-r4b clear-then-set TS project root'),
    ('start-error-swallowed', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     '.unwrap_or_else(|e| panic!("#347 fix-r4b {cell} start missing: {e}"))',
     '.unwrap_or(ProbeEvent::Started)',
     '--lib', 'runtime::mcp::sdk::concurrency::missing_observations_fail_closed',
     '#347 fix-r4b missing observation must identify its stage'),
    ('competition-error-swallowed', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     '.unwrap_or_else(|e| panic!("#347 fix-r4b {cell} competition missing: {e}"))',
     '.unwrap_or(ProbeEvent::WouldBlock)',
     '--lib', 'runtime::mcp::sdk::concurrency::missing_observations_fail_closed',
     '#347 fix-r4b missing observation must fail closed'),
    ('acquired-reported-blocked', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'Ok(guard) => {\n                let _ = probe.sender.send(ProbeEvent::Acquired);',
     'Ok(guard) => {\n                let _ = probe.sender.send(ProbeEvent::WouldBlock);',
     '--lib', 'runtime::mcp::sdk::concurrency::acquired_probe_retains_the_real_guard',
     '#347 fix-r4b unlocked probe must report Acquired'),
    ('poisoned-reported-blocked', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'Err(TryLockError::Poisoned(error)) => {\n                let _ = probe.sender.send(ProbeEvent::Acquired);',
     'Err(TryLockError::Poisoned(error)) => {\n                let _ = probe.sender.send(ProbeEvent::WouldBlock);',
     '--lib', 'runtime::mcp::sdk::concurrency::acquired_probe_retains_the_real_guard',
     '#347 fix-r4b unlocked probe must report Acquired'),
    ('unwind-release-disabled', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'release_gate(gate);', 'let _ = gate;',
     '--lib', 'runtime::mcp::sdk::concurrency::unwinding_releases_the_predecessor',
     '#347 fix-r4b unwind predecessor task failed'),
    ('cancel-release-disabled', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'release_gate(gate);', 'let _ = gate;',
     '--lib', 'runtime::mcp::sdk::concurrency::cancellation_releases_the_predecessor',
     '#347 fix-r4b cancelled predecessor task failed'),
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
     '--lib', 'runtime::sdk_session::projects::tests::'
     'set_replaces_in_place_and_keeps_insertion_order',
     '#347 set replaces an existing key in place, like the TS Map'),
    ('session-project-none', 'sdk_session/registry.rs',
     'self.projects.project(&JsValue::String(session_key.clone()))',
     'let _ = session_key;\n        None',
     '--test=sdk_mcp', 'session_to_project_table_is_shared_with_the_session_registry',
     '#347 setSdkSessionConfig records the session root'),
    # ── #347 fix-r1 (review P2-2): the paired clears ───────────────────────
    ('budget-clear-reads-after-delete', 'sdk_session/registry.rs',
     'if let Some(root) = self.session_project(session_key)\n'
     '            && let Some(budget) = &self.options.task_budget',
     'self.delete_project(session_key);\n'
     '        if let Some(root) = self.session_project(session_key)\n'
     '            && let Some(budget) = &self.options.task_budget',
     '--test=sdk_mcp', 'registry_clear_then_mcp_clear_clears_everything_in_order',
     "the registry clear's budget step still sees the shared entry"),
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

# Each delayed row changes the delay parameter that the real lock probe
# consumes after emitting Started. Full test names bind the cfg(test) module.
TEST_REL = '../../tests/fixtures/sdk-mcp-p2/concurrency.rs'
TEST_FILE = (source / TEST_REL).resolve()
MUTANTS = [
    ('clear-commit-boundary-delayed', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'clear_set_cell(Duration::ZERO).await;',
     'clear_set_cell(Duration::from_secs(3)).await;',
     'clear-commit-boundary',
     'runtime::mcp::sdk::concurrency::clear_and_set_commits_share_one_critical_section',
     '#347 fix-r4b cell3 must observe WouldBlock at the real servers lock'),
    ('set-commit-boundary-delayed', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'set_clear_cell(Duration::ZERO).await;',
     'set_clear_cell(Duration::from_secs(3)).await;',
     'set-commit-boundary',
     'runtime::mcp::sdk::concurrency::set_commit_cannot_outlive_the_shared_table_write',
     '#347 fix-r4b cell4 must observe WouldBlock at the real servers lock'),
    ('set-read-window-unlocked-delayed', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'toggle_cell(Duration::ZERO, true).await;',
     'toggle_cell(Duration::from_secs(3), true).await;',
     'set-read-window-unlocked',
     'runtime::mcp::sdk::concurrency::set_and_toggle_do_not_lose_updates',
     '#347 fix-r4b toggle must observe WouldBlock at the real servers lock'),
]
CONTROLS = [
    ('cell3-delayed-control', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'clear_set_cell(Duration::ZERO).await;',
     'clear_set_cell(Duration::from_secs(3)).await;',
     'runtime::mcp::sdk::concurrency::clear_and_set_commits_share_one_critical_section'),
    ('cell4-delayed-control', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'set_clear_cell(Duration::ZERO).await;',
     'set_clear_cell(Duration::from_secs(3)).await;',
     'runtime::mcp::sdk::concurrency::set_commit_cannot_outlive_the_shared_table_write'),
    ('toggle-delayed-control', '../../tests/fixtures/sdk-mcp-p2/concurrency.rs',
     'toggle_cell(Duration::ZERO, true).await;',
     'toggle_cell(Duration::from_secs(3), true).await;',
     'runtime::mcp::sdk::concurrency::set_and_toggle_do_not_lose_updates'),
]


def panic_block(log, test):
    """`test`'s own stdout block: libtest opens it with
    `---- {test} stdout ----` after the FAILED line and closes it at the
    next `---- ` section or the `test result:` summary. A message shared
    with a sibling test's failure does not appear here."""
    marker = f'---- {test} stdout ----'
    lines = log.splitlines()
    start = next((i for i, line in enumerate(lines) if line == marker), None)
    if start is None:
        return ''
    block = []
    for line in lines[start + 1:]:
        if line.startswith('---- ') or line.startswith('test result:'):
            break
        block.append(line)
    return '\n'.join(block)


def killed(log, returncode, test, message):
    """The target test itself failed (rc 101, its `--exact` FAILED line) and
    the tagged message is inside that test's own panic block."""
    return (returncode == 101
            and f'{test} ... FAILED' in log
            and message in panic_block(log, test))


# The review's synthetic log (P1-4): the sibling test (a registry test whose
# name merely shares the target's filter suffix) fails with the same
# assertion message; the target test passes. The predicate must not count it.
TARGET = ('runtime::sdk_session::projects::tests::'
          'set_replaces_in_place_and_keeps_insertion_order')
SIBLING = ('runtime::sdk_session::registry::tests::'
           'regression_set_replaces_in_place_and_keeps_insertion_order')
SELFTEST_MESSAGE = '#347 set replaces an existing key in place, like the TS Map'
SELFTEST_SIBLING_LOG = f'''running 2 tests
test {TARGET} ... ok
test {SIBLING} ... FAILED

failures:

---- {SIBLING} stdout ----
assertion failed: {SELFTEST_MESSAGE}
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    {SIBLING}

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
'''
# The same shape with the roles swapped: the target fails and its own panic
# block carries the message. The predicate must count this one.
SELFTEST_TARGET_LOG = f'''running 2 tests
test {TARGET} ... FAILED
test {SIBLING} ... ok

failures:

---- {TARGET} stdout ----
assertion failed: {SELFTEST_MESSAGE}
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    {TARGET}

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
'''
# Both fail, but the message only appears in the sibling's panic block: the
# FAILED line alone must not carry the kill, the message must be in the
# target's own block.
SELFTEST_BOTH_LOG = f'''running 2 tests
test {TARGET} ... FAILED
test {SIBLING} ... FAILED

failures:

---- {TARGET} stdout ----
assertion failed: some other assertion of the target test
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- {SIBLING} stdout ----
assertion failed: {SELFTEST_MESSAGE}
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    {TARGET}
    {SIBLING}

test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
'''


def classify_selftest(log):
    """The review's synthetic log (P1-4): a sibling with a suffix-shared
    filter and the same message must NOT count, the target test's own
    failure with the message must count, and a shared message that only
    appears in the sibling's block must not count either."""
    checks = [
        ('sibling failure must not count',
         not killed(log, 101, TARGET, SELFTEST_MESSAGE)),
        ('target failure must count',
         killed(SELFTEST_TARGET_LOG, 101, TARGET, SELFTEST_MESSAGE)),
        ('message outside the target block must not count',
         not killed(SELFTEST_BOTH_LOG, 101, TARGET, SELFTEST_MESSAGE)),
    ]
    for label, ok in checks:
        print(f'selftest {label}: {"ok" if ok else "FAILED"}')
        if not ok:
            return False
    return True


def main():
    if len(sys.argv) >= 2 and sys.argv[1] == '--classify-selftest':
        log = Path(sys.argv[2]).read_text() if len(sys.argv) > 2 else SELFTEST_SIBLING_LOG
        return 0 if classify_selftest(log) else 1
    if not os.environ.get('CARGO_TARGET_DIR'):
        sys.exit('CARGO_TARGET_DIR must be exported by the caller')
    env = os.environ.copy()
    env.setdefault('CARGO_PROFILE_DEV_DEBUG', 'line-tables-only')
    env.setdefault('CARGO_PROFILE_TEST_DEBUG', 'line-tables-only')
    sample = sys.argv[1] == '--bounded-sample'
    compile_only = sys.argv[1] == '--compile-only'
    offset = 2 if sample or compile_only else 1
    out = Path(sys.argv[offset]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = set(sys.argv[offset + 1:])
    command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway']
    check_command = ['cargo', 'check', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--tests']

    def run(label, argv, timeout=900):
        wait_for_slot(root / 'scripts/cargo-slots.sh')
        try:
            result = run_owned(argv, cwd=root, env=env, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raw = ''
            for stream in (error.output, error.stderr):
                raw += stream.decode(errors='replace') if isinstance(stream, bytes) else stream or ''
            (out / f'{label}.log').write_text(raw + '\nINVALID: process timeout\n')
            raise
        (out / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    # Reuse the shared exact-one-test success predicate. Keep this catalog's
    # executor for multi-file mutations, integration/lib targets and compile
    # preflight; its panic-block kill predicate is stricter than run_cases.
    results = []
    def record(name, status, reason, **extra):
        results.append(dict(name=name, status=status, reason=reason, **extra))
        (out / 'summary.json').write_text(json.dumps(results, ensure_ascii=False, indent=2) + '\n')
        print(f'{name}: {status}: {reason}', flush=True)

    # No source mutation: an intentionally unreleased gate exercises the same
    # toggle path and the runtime's real destructor in an owned subprocess.
    if sample:
        test = 'runtime::mcp::sdk::concurrency::unreleased_predecessor_fails_boundedly'
        message = '#347 fix-r4b toggle predecessor task failed'
        try:
            run('sample-compile', command + ['--lib', '--no-run']).check_returncode()
            start = time.monotonic()
            result = run('sample', command + ['--lib', test, '--', '--exact', '--ignored'], timeout=45)
            elapsed = round(time.monotonic() - start, 3)
            hit = killed(result.stdout + result.stderr, result.returncode, test, message)
            record('unreleased-predecessor', 'KILLED' if hit else 'INVALID',
                   'bounded tagged failure' if hit else 'missing bound failure',
                   elapsedSeconds=elapsed, test=test, message=message)
        except Exception as error:
            record('unreleased-predecessor', 'INVALID', str(error))
        return int(any(row['status'] != 'KILLED' for row in results))

    # Normalize the declarative catalog; mutations may affect product + test.
    by_name = {row[0]: row for row in MUTATIONS}
    jobs = []
    for name, filename, old, new, target, test, message in MUTATIONS:
        jobs.append((name, [(source / filename, old, new)], target, test, message, False))
    for name, filename, old, new, base, test, message in MUTANTS:
        _, product, before, after, target, _, _ = by_name[base]
        jobs.append((name, [(source / product, before, after), (source / filename, old, new)], target, test, message, False))
    for name, filename, old, new, test in CONTROLS:
        jobs.append((name, [(source / filename, old, new)], '--lib', test, '', True))
    if selected:
        unknown = selected - {row[0] for row in jobs}
        if unknown:
            sys.exit(f'unknown mutation names: {sorted(unknown)}')
        jobs = [row for row in jobs if row[0] in selected]
    paths = sorted({path for _, edits, *_ in jobs for path, _, _ in edits})
    try:
        with mutating_sources(root, paths) as snapshots:
            for name, edits, target, test, message, control in jobs:
                status, reason, start = 'INVALID', '', time.monotonic()
                changed = {}
                try:
                    for path, old, new in edits:
                        content = snapshots[path].decode()
                        count = content.count(old)
                        if count != 1:
                            raise ValueError(f'stale anchor {path}: {count} != 1')
                        changed[path] = content.replace(old, new, 1)
                    argv = command + [target, test, '--', '--exact']
                    if not compile_only and not passed(run(name + '-positive', argv), test):
                        raise ValueError('positive bound test did not pass')
                    patch = []
                    for path, content in changed.items():
                        original = snapshots[path].decode()
                        patch.extend(difflib.unified_diff(original.splitlines(True), content.splitlines(True), fromfile=str(path), tofile=str(path)))
                        path.write_text(content)
                    (out / f'{name}.patch').write_text(''.join(patch))
                    # Every mutant must compile, even when another one failed.
                    run(name + '-check', check_command).check_returncode()
                    if compile_only:
                        status, reason = 'COMPILED', 'runtime not requested'
                    else:
                        result = run(name, argv)
                        if control:
                            status = 'CONTROL-OK' if passed(result, test) else 'INVALID'
                            reason = 'delayed positive control' if status == 'CONTROL-OK' else 'control failed'
                        elif killed(result.stdout + result.stderr, result.returncode, test, message):
                            status, reason = 'KILLED', 'exact test and dedicated panic message'
                        elif passed(result, test):
                            status, reason = 'SURVIVED', 'bound test still passed'
                        else:
                            status, reason = 'INVALID', 'wrong panic, missing test, or build/process failure'
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    for path, _, _ in edits:
                        path.write_bytes(snapshots[path])
                        if path.read_bytes() != snapshots[path]:
                            raise RuntimeError(f'restoration failed: {path}')
                if not compile_only:
                    try:
                        if not passed(run(name + '-restored', command + [target, test, '--', '--exact']), test):
                            status, reason = 'INVALID', reason + '; restored test failed'
                    except Exception as error:
                        status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, status, reason, test=test, message=message, elapsedSeconds=round(time.monotonic()-start, 3))
    except Exception as error:
        # An unsafe source baseline/restoration stops writes, but accounts for
        # every job so it cannot silently truncate the requested suite.
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + f'; source guard: {error}')
        for name, _, _, test, message, _ in jobs[len(results):]:
            record(name, 'INVALID', f'source guard: {error}', test=test, message=message)
        (out / 'summary.json').write_text(json.dumps(results, ensure_ascii=False, indent=2) + '\n')
    good = {'COMPILED'} if compile_only else {'KILLED', 'CONTROL-OK'}
    unexpected = [row for row in results if row['status'] not in good]
    print(f'{len(results)-len(unexpected)}/{len(results)} as expected; KILLED/SURVIVED/INVALID recorded for every selected mutation', flush=True)
    return int(not results or bool(unexpected))

if __name__ == '__main__':
    sys.exit(main())
