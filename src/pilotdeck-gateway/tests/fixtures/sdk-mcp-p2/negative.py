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
whose test stays green is SURVIVED, and a stale anchor (count != 1) aborts
the run. Every mutant is compile-verified (`cargo check --tests`) before
its test runs. The executor runs ALL mutations, restoring each source
byte-for-byte, then summarizes; any non-KILLED result exits nonzero.

The two boundary mutants (`clear-commit-boundary` / `set-commit-boundary`)
are killed by matrix-cell tests that consume the bounded-window outcome and
confirm the in-window completion before opening the other gate, so the two
writes' order is pinned by the gates instead of thread wakeup order
(#347 fix-r2, review P1). #347 fix-r3 (review P1): those tests now record a
start marker as the second operation's first statement and only open the
window once it is confirmed, so an expired window is lock-wait evidence
rather than a timing guess; the `-delayed` mutants re-run each boundary
mutant with the spawned operation's start delayed past the window (the
"not yet scheduled" interleaving the review showed could let it through) and
must still die with the same bound message, while the matching `-control`
entries apply the same delay with no mutation and must stay green (the delay
alone must not break the fixed shape).

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
     'let mut servers = lock(&self.servers);\n'
     '        servers.retain(|(key, _)| !same_key(key, session_key));\n'
     '        if let JsValue::String(key) = session_key {\n'
     '            lock(&self.overrides).retain(|(existing, _)| existing != key);\n'
     '            lock(&self.rules).retain(|(existing, _)| existing != key);\n'
     '        }\n'
     '        self.options.projects.delete(session_key);\n'
     '        drop(servers);',
     'lock(&self.servers).retain(|(key, _)| !same_key(key, session_key));\n'
     '        if let JsValue::String(key) = session_key {\n'
     '            lock(&self.overrides).retain(|(existing, _)| existing != key);\n'
     '            lock(&self.rules).retain(|(existing, _)| existing != key);\n'
     '        }\n'
     '        self.options.projects.delete(session_key);',
     '--test=sdk_mcp', 'clear_and_set_commits_share_one_critical_section',
     'the shared session→project entry must not lag the server table'),
    ('set-commit-boundary', 'mcp/sdk.rs',
     'self.options.projects.set(&session_key, project_root);\n'
     '        }\n'
     '        (self.options.close_session)(session_key, "sdk_mcp_servers_changed").await?;',
     '}\n'
     '        self.options.projects.set(&session_key, project_root);\n'
     '        (self.options.close_session)(session_key, "sdk_mcp_servers_changed").await?;',
     '--test=sdk_mcp', 'set_commit_cannot_outlive_the_shared_table_write',
     'the clear must not leave the commit\'s shared-table entry behind'),
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

# The test file the delayed rows below patch (relative to `source`, like
# the product anchors above).
TEST_REL = '../../tests/sdk_mcp.rs'
TEST_FILE = (source / TEST_REL).resolve()

# #347 fix-r3 (review P1): delayed re-runs of the two boundary mutants. Each
# row patches the test file to sleep 3s — past the 2s completion window —
# before the spawned operation's start marker, i.e. exactly the "clear/set
# not yet scheduled" interleaving under which the pre-fix-r3 tests let both
# mutants through; the row also names the product mutation it must be run
# with. Kill predicate unchanged: the same bound message must appear in the
# same test's own panic block.
MUTANTS = [
    ('clear-commit-boundary-delayed', '../../tests/sdk_mcp.rs',
     'set_started.record();',
     'tokio::time::sleep(Duration::from_secs(3)).await;\n'
     '        set_started.record();',
     'clear-commit-boundary',
     'clear_and_set_commits_share_one_critical_section',
     'the shared session→project entry must not lag the server table'),
    ('set-commit-boundary-delayed', '../../tests/sdk_mcp.rs',
     'clear_started.record();',
     'std::thread::sleep(Duration::from_secs(3));\n'
     '        clear_started.record();',
     'set-commit-boundary',
     'set_commit_cannot_outlive_the_shared_table_write',
     "the clear must not leave the commit's shared-table entry behind"),
]

# #347 fix-r3: the paired positive controls — the same 3s start delay with
# no product mutation. The delayed start must not break the fixed shape, or
# the delayed mutants above would prove nothing (a false kill). These must
# come back green.
CONTROLS = [
    ('cell3-delayed-control', '../../tests/sdk_mcp.rs',
     'set_started.record();',
     'tokio::time::sleep(Duration::from_secs(3)).await;\n'
     '        set_started.record();',
     'clear_and_set_commits_share_one_critical_section'),
    ('cell4-delayed-control', '../../tests/sdk_mcp.rs',
     'clear_started.record();',
     'std::thread::sleep(Duration::from_secs(3));\n'
     '        clear_started.record();',
     'set_commit_cannot_outlive_the_shared_table_write'),
    ('toggle-delayed-control', '../../tests/sdk_mcp.rs',
     'toggle_started.record();',
     'tokio::time::sleep(Duration::from_secs(3)).await;\n'
     '        toggle_started.record();',
     'set_and_toggle_do_not_lose_updates'),
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
        log = Path(sys.argv[2]).read_text() if len(sys.argv) > 2 \
            else SELFTEST_SIBLING_LOG
        sys.exit(0 if classify_selftest(log) else 1)
    selected = sys.argv[2:]
    mutations = MUTATIONS
    delayed = MUTANTS
    controls = CONTROLS
    if selected:
        mutations = [m for m in MUTATIONS if m[0] in selected]
        delayed = [m for m in MUTANTS if m[0] in selected]
        controls = [m for m in CONTROLS if m[0] in selected]
        unknown = set(selected) - ({m[0] for m in mutations}
                                   | {m[0] for m in delayed}
                                   | {m[0] for m in controls})
        assert not unknown, f'unknown mutation name(s): {sorted(unknown)}'
    by_name = {m[0]: m for m in MUTATIONS}
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
    product_paths = {source / filename for _, filename, *_ in mutations}
    for entry in delayed:
        product_paths.add(source / by_name[entry[4]][1])
    paths = sorted(product_paths)
    if delayed or controls:
        paths.append(TEST_FILE)
    with mutating_sources(root, paths) as snapshots:
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
                     '-p', 'pilotdeck-gateway', target, '--',
                     '--exact', test],
                    cwd=root, env=env, timeout=900)
                log = result.stdout + result.stderr
                (out / f'{name}.log').write_text(log)
                hit = killed(log, result.returncode, test, message)
                results.append(dict(
                    name=name,
                    status='KILLED' if hit else 'SURVIVED',
                    reason='' if hit else
                    'rc={}; tagged message not in the test\'s own panic block'.format(
                        result.returncode),
                    message=message,
                    sourceSha256=before,
                ))
                print(f'{name}: {"KILLED" if hit else "SURVIVED"}', flush=True)
            finally:
                path.write_bytes(original)
                assert hashlib.sha256(path.read_bytes()).hexdigest() == before
        # #347 fix-r3: the delayed boundary mutants — product mutation plus a
        # test-file patch that delays the second operation's start past the
        # window. Same compile-then-run discipline and the same kill
        # predicate as above.
        for name, _test_rel, test_old, test_new, base, test, message in delayed:
            entry = by_name[base]
            filename, old, new, target = entry[1], entry[2], entry[3], entry[4]
            path = source / filename
            original = snapshots[path]
            before = hashlib.sha256(original).hexdigest()
            test_original = snapshots[TEST_FILE]
            test_before = hashlib.sha256(test_original).hexdigest()
            content = original.decode()
            test_content = test_original.decode()
            count = content.count(old)
            test_count = test_content.count(test_old)
            if count != 1 or test_count != 1:
                results.append(dict(name=name, status='INVALID',
                                    reason=f'stale anchor: product {count}, '
                                           f'test {test_count}'))
                print(f'{name}: INVALID stale anchor product {count} '
                      f'test {test_count}', flush=True)
                continue
            changed = content.replace(old, new, 1)
            test_changed = test_content.replace(test_old, test_new, 1)
            (out / f'{name}.patch').write_text(
                ''.join(difflib.unified_diff(
                    content.splitlines(True), changed.splitlines(True),
                    fromfile=filename, tofile=filename))
                + ''.join(difflib.unified_diff(
                    test_content.splitlines(True), test_changed.splitlines(True),
                    fromfile='tests/sdk_mcp.rs', tofile='tests/sdk_mcp.rs')))
            try:
                wait_for_slot(root / 'scripts/cargo-slots.sh')
                path.write_text(changed)
                TEST_FILE.write_text(test_changed)
                check = run_owned(check_command, cwd=root, env=env, timeout=900)
                (out / f'{name}.check.log').write_text(check.stdout + check.stderr)
                if check.returncode != 0:
                    results.append(dict(name=name, status='INVALID',
                                        reason='delayed mutant does not compile'))
                    print(f'{name}: INVALID does not compile', flush=True)
                    continue
                wait_for_slot(root / 'scripts/cargo-slots.sh')
                result = run_owned(
                    ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                     '-p', 'pilotdeck-gateway', target, '--',
                     '--exact', test],
                    cwd=root, env=env, timeout=900)
                log = result.stdout + result.stderr
                (out / f'{name}.log').write_text(log)
                hit = killed(log, result.returncode, test, message)
                results.append(dict(
                    name=name,
                    status='KILLED' if hit else 'SURVIVED',
                    reason='' if hit else
                    'rc={}; tagged message not in the test\'s own panic block'.format(
                        result.returncode),
                    message=message,
                    sourceSha256=before,
                ))
                print(f'{name}: {"KILLED" if hit else "SURVIVED"}', flush=True)
            finally:
                path.write_bytes(original)
                TEST_FILE.write_bytes(test_original)
                assert hashlib.sha256(path.read_bytes()).hexdigest() == before
                assert hashlib.sha256(TEST_FILE.read_bytes()).hexdigest() \
                    == test_before
        # #347 fix-r3: the paired controls — the same start delay with no
        # product mutation must leave the tests green.
        for name, _test_rel, test_old, test_new, test in controls:
            test_original = snapshots[TEST_FILE]
            test_before = hashlib.sha256(test_original).hexdigest()
            test_content = test_original.decode()
            test_count = test_content.count(test_old)
            if test_count != 1:
                results.append(dict(name=name, status='INVALID',
                                    reason=f'stale anchor: test {test_count}'))
                print(f'{name}: INVALID stale anchor test {test_count}',
                      flush=True)
                continue
            test_changed = test_content.replace(test_old, test_new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                test_content.splitlines(True), test_changed.splitlines(True),
                fromfile='tests/sdk_mcp.rs', tofile='tests/sdk_mcp.rs')))
            try:
                wait_for_slot(root / 'scripts/cargo-slots.sh')
                TEST_FILE.write_text(test_changed)
                check = run_owned(check_command, cwd=root, env=env, timeout=900)
                (out / f'{name}.check.log').write_text(check.stdout + check.stderr)
                if check.returncode != 0:
                    results.append(dict(name=name, status='INVALID',
                                        reason='control patch does not compile'))
                    print(f'{name}: INVALID does not compile', flush=True)
                    continue
                wait_for_slot(root / 'scripts/cargo-slots.sh')
                result = run_owned(
                    ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                     '-p', 'pilotdeck-gateway', '--test=sdk_mcp', '--',
                     '--exact', test],
                    cwd=root, env=env, timeout=900)
                log = result.stdout + result.stderr
                (out / f'{name}.log').write_text(log)
                ok = (result.returncode == 0 and f'{test} ... ok' in log
                      and 'test result: ok' in log)
                results.append(dict(
                    name=name,
                    status='CONTROL-OK' if ok else 'CONTROL-FAILED',
                    reason='' if ok else f'rc={result.returncode}',
                ))
                print(f'{name}: {"CONTROL-OK" if ok else "CONTROL-FAILED"}',
                      flush=True)
            finally:
                TEST_FILE.write_bytes(test_original)
                assert hashlib.sha256(TEST_FILE.read_bytes()).hexdigest() \
                    == test_before
        # Written after the loop (fix-r1, review boundary #3): the last
        # mutant skipping the tail must not truncate the evidence.
        (out / 'summary.json').write_text(
            json.dumps(results, ensure_ascii=False, indent=2) + '\n')
    unexpected = [result for result in results
                  if result['status'] not in ('KILLED', 'CONTROL-OK')]
    print(f'{len(results) - len(unexpected)}/{len(results)} as expected; '
          f'every source restored byte-for-byte', flush=True)
    if unexpected:
        for result in unexpected:
            print(f"{result['name']}: {result['status']} {result.get('reason', '')}",
                  flush=True)
        sys.exit(1)


if __name__ == '__main__':
    main()
