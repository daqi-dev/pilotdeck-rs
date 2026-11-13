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
REGISTRY = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/registry.rs'
PROCESS_TEST = ROOT / 'src/pilotdeck-gateway/tests/sdk_mcp_session_tools.rs'
sys.path.insert(0, str(FIXTURES))
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                               tree_lock, write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                                refuse_unusable_target, registered, verdict_exit)
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
    # `>=` is the TS limit check; `>` lets one runtime past an integer cap.
    # #2442 fix-r1 (review P1): the cap's enforcement point moved from the
    # `session_mcp_tools` pre-check (tools.rs, TS-aligned fast path) into
    # `register_below_cap`'s critical section — flipping only the pre-check
    # is unobservable (both refusals emit the same `limit_diagnostic()` and
    # the fake factory has no recording side effect), so the mutation is
    # re-anchored to the surviving recheck and rebound to the race test
    # that reaches it past the pre-check.
    ('per-session-limit-off', REGISTRY,
     'if total_entries(&state.by_session) as f64 >= cap {',
     'if total_entries(&state.by_session) as f64 > cap {',
     'native_process::tools::tests::session_mcp_tools_refuses_the_sixth_runtime_when_a_racer_registers_after_the_check',
     '#2442 the over-cap compose starts no per-session runtime'),
    # #1433 fix-r1 (review P2#4): the limit must compare the snapshot's f64
    # cap without truncation — casting back to usize floors 2.5 to 2, so the
    # third registration under a 2.5 cap is refused (TS `2 >= 2.5` is false,
    # SessionMcpRuntimeBundle.ts:70, Node v22.22.0).
    ('per-session-limit-truncated', TOOLS,
     'as f64 >= options.max_per_session_instances {',
     'as usize >= options.max_per_session_instances as usize {',
     'native_process::tools::tests::session_mcp_tools_allows_the_third_runtime_under_a_fractional_limit',
     '#1433 a 2.5 cap still admits the third per-session runtime'),
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

# #1433 fix-r1 (review P1-1): the denominator is this literal, never
# `len(mutations)`. `registered()` compares the two in order and refuses on
# any drift, so a dropped, renamed, duplicated or reordered row is a startup
# refusal (rc 2) instead of a silently shorter run that still reports
# "N/N killed" (`specs/negative-controls/README.md` invariant 1).
EXPECTED_NAMES = (
    'enabled-filter-dropped',
    'shared-conflict-replaced',
    'per-session-replace-lost',
    'release-order-swapped',
    'per-session-limit-off',
    'per-session-limit-truncated',
    'start-failure-propagated',
    'turn-registry-drops-mcp',
    'composition-reads-no-servers',
)

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
    # #2441: the binding self-check runs before any mutation, so its failure is
    # a "never started" refusal (rc 2) — the pre-`mutating_sources` boundary
    # every guard below already follows (`specs/negative-controls/README.md`)
    # — not an uncaught traceback a host would read as "ran and not green" (1).
    try:
        check_binding_scope()
    except RuntimeError as error:
        refuse(str(error))
    # #1433 fix-r1 (review P1-1): every refusal below happens before any
    # mutation, so it is rc 2 ("this run never started", printed by
    # `negative_selection.refuse`) and not rc 1 ("ran, not green") — the
    # four-way table in `specs/negative-controls/README.md`.
    try:
        registered(mutations, EXPECTED_NAMES)
    except ValueError as error:
        refuse(f'mutation roster drifted: {error}')
    # #2468: the caller-side name errors are refused from argv alone, before
    # the stale-anchor scan reads live sources. A full mutation round holds the
    # tree lock with `tools.rs` mutated on disk; a concurrent CI guards run
    # that passes an unknown name used to hit the stale-anchor refusal first,
    # so its stderr named no unknown mutation and `assertIn(UNKNOWN, stderr)`
    # failed without anything being wrong with the guards themselves. The name
    # checks read no source bytes, so their verdict cannot depend on the tree
    # state; taking the tree lock on the refusal path instead would queue that
    # CI run behind the whole mutation round. `--check` keeps its own argv
    # contract (no names; the anchor scan below is the check).
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    unknown = sorted(set(args.selected) - {row[0] for row in mutations})
    if not args.check and unknown:
        refuse(f'#1433 unknown mutation names: {unknown}')
    if not args.check and args.selected and len(selected) != len(args.selected):
        repeated = sorted({name for name in args.selected
                           if args.selected.count(name) > 1})
        refuse(f'#1433 repeated mutation names: {repeated}')
    # #2468 r2 (review r6 concurrency#1): the argv/env-only guards below are
    # also refused from their inputs alone, before the stale-anchor scan
    # reads live sources. A full mutation round holds the tree lock with
    # `tools.rs` mutated on disk; a concurrent CI guards run with an unusable
    # target (or an already-claimed output directory) used to hit the
    # stale-anchor refusal first, so its stderr named no CARGO_TARGET_DIR /
    # "already exists" cause and the guards matrix went red with nothing
    # wrong. These three checks read no source bytes — same class as the
    # name checks above. The target is admitted by the one shared rule
    # (absent, empty, blank, dot, root, repo root, inside `src`) *before* the
    # evidence directory exists, so a rejected target cannot leave a
    # directory that reads like a finished run; `claim_dir` below still makes
    # the real (atomic) claim, so a directory created between this precheck
    # and the claim is still refused fail-closed.
    if not args.check:
        if not args.output:
            refuse('#1433 an output directory is required')
        if missing_target():
            refuse('#1433 set CARGO_TARGET_DIR to the caller-owned build tree')
        refuse_unusable_target()
        if Path(args.output).exists():
            refuse(f'#1433 output directory already exists: {Path(args.output).resolve()}')
    # #2795 fix-r4（评审 sibling P1 同族跟改，fix-r3 mcp-http-encoding 先例）：
    # 锚点预检（含 test_body 的字典推导裸读）也要持树锁读——兄弟变异运行器在
    # 自己的 write 与 restore 之间可能把变异体留在盘上，无锁读会把核对错在
    # 它不拥有的源上；锁忙 / 读失败是拒跑（2）。本窗口只包只读预检面，与后续
    # mutating_sources 是顺序关系，锁先释放再重取，无同进程嵌套。
    try:
        with tree_lock(ROOT, timeout=60):
            stale = []
            for name, path, old, new, test, assertion in mutations:
                text = path.read_text()
                body = test_body(test)
                if (text.count(old) != 1 or old == new or
                        not body or f'"{assertion}"' not in body):
                    stale.append(name)
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        refuse(f'tree lock or anchor read failed: '
               f'{type(error).__name__}: {error}')
    if stale:
        refuse('#1433 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#1433 {len(EXPECTED_NAMES)} unique anchors and assertion bindings: OK')
        return 0
    # The target and output-directory guards already ran above (argv/env
    # only, before the stale scan); `claim_dir` here makes the atomic claim.
    output = claim_dir(args.output)
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
            # #2468: the inner publish/restore pair below must be one atomic
            # rename, never a truncating in-place write — a SIGKILL mid
            # write-back used to leave a half-written `tools.rs` that the next
            # preflight then refuses the whole tree for, while an unlocked
            # plain `cargo test` could read the mutant. `mutating_sources`
            # yields bytes only, so the mode is recorded here, inside the
            # lease that has already proven those bytes equal HEAD: passing it
            # explicitly keeps a mutant that deleted its target from
            # restoring to the 0o644 fallback (a mode-only diff).
            modes = {path: path.stat().st_mode & 0o7777
                     for _, path, _, _, _, _ in mutations}
            for name, path, old, new, test, assertion in selected:
                item = dict(name=name, test=test, assertion=assertion,
                            status='INVALID', reason='not run', compiled=False)
                try:
                    if not args.compile_only and not passed(run(name + '-positive', env, test), test):
                        raise RuntimeError('positive bound test failed')
                    write_source_atomic(
                        ROOT, path,
                        originals[path].decode().replace(old, new, 1).encode(),
                        mode=modes[path])
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
                    write_source_atomic(ROOT, path, originals[path],
                                        mode=modes[path])
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
    # #1433 fix-r1 (review P1-1): exactly one verdict per selected name. The
    # roster literal alone only proves the source table is intact; a row whose
    # flow is skipped entirely would leave every remaining verdict KILLED and
    # `verdict_exit` green, so the recorded names are compared against the
    # static roster before anything that reads as success is published. A
    # mismatch publishes no summary.json — only a file whose name says what
    # happened (the memory-gateway r5/r6 shape).
    expected = [name for name in EXPECTED_NAMES
                if not args.selected or name in args.selected]
    recorded = [row['name'] for row in results]
    if recorded != expected:
        message = f'verdict roster mismatch: recorded={recorded} expected={expected}'
        print(message, file=sys.stderr)
        (output / 'roster-mismatch.json').write_text(json.dumps(
            {'verdict': 'ROSTER_MISMATCH', 'expected': expected,
             'recorded': recorded, 'rows': results,
             'counts': counts, 'guardedSha256': guarded_sha256}, indent=2) + '\n')
        return 1
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    # The shared exit contract: zero rows is never 0 (`0/0 killed` is not a
    # pass), and every row must be KILLED — COMPILED under --compile-only.
    return verdict_exit(results, key='status',
                        killed='COMPILED' if args.compile_only else 'KILLED')


if __name__ == '__main__':
    raise SystemExit(main())
