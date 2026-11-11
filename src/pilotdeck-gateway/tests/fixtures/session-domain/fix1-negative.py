#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative replacement chain, no mutation table
"""#562 repair 1: behavioral mutants, bounded process groups, one launcher target."""
import os
from pathlib import Path
import signal
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                               wait_for_slot)

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'
if not os.environ.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
OUT = REPO / '_tmp/20261023-issue-222-domain-log/fix1-negative'
OUT.mkdir(parents=True, exist_ok=True)
ADAPTER = SRC / 'pilotdeck-gateway/src/storage/transcript_adapter.rs'
# #1281 (same exception as the session-factory runners): PDRS_NEGATIVE_BASELINE
# lets an uncommitted sandbox candidate run against its exact working tree;
# committed hosts keep the default HEAD.
with mutating_sources(REPO, [ADAPTER], baseline=os.environ.get('PDRS_NEGATIVE_BASELINE') or 'HEAD') as snapshots:
    original = snapshots[ADAPTER].decode()
    selected = set(sys.argv[1:])
    killed = []


    def stop(signum, frame):
        raise SystemExit(f'interrupted by signal {signum}')


    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)


    def run(name, red, test='storage::transcript_adapter_tests::real_ts_scenarios_match_outcomes_calls_and_bytes'):
        # #1908: the shared wait replaces the single 300 s slot sample; the
        # budget now comes from the shared constant above the single-sample cap.
        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
        (OUT / f'{name}-slots.log').write_text(slot.stdout + slot.stderr)
        assert slot.returncode in (0, 69), 'no cargo slot; rerun later'
        env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG=os.environ.get('CARGO_PROFILE_DEV_DEBUG', 'line-tables-only'), CARGO_PROFILE_TEST_DEBUG=os.environ.get('CARGO_PROFILE_TEST_DEBUG', 'line-tables-only'), CARGO_INCREMENTAL='0')
        with (OUT / f'{name}.log').open('w') as log:
            child = subprocess.Popen(['cargo', 'test', '-p', 'pilotdeck-gateway', '--lib', test, '--', '--exact', '--nocapture'], cwd=SRC, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                code = child.wait(timeout=240)
            except BaseException:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
                raise
        text = (OUT / f'{name}.log').read_text()
        assert 'running 1 test' in text, f'{name}: exit={code}; compilation/zero-test failure is not evidence'
        assert ('test result: FAILED' in text and code != 0) if red else ('test result: ok. 1 passed' in text and code == 0), name
        if red:
            killed.append(name)
        print(f'{name}: {"KILLED" if red else "PASS"}; exit={code}; reaped={child.poll() is not None}', flush=True)


    try:
        run('baseline', False)
        # #2043 fix-r1: impl-r1 extracted the hydrate `.some` walk into
        # `message_contains_tool_result` (with the shared `cannot_read`
        # constructor and a `return Ok(true)` short circuit), so the five body
        # mutants follow that function's text now; `validate_hydrated_message`
        # itself is only the `Ok(())` projection left for `skip-message-check`.
        a = original.index('fn message_contains_tool_result(')
        b = original.index('fn validate_hydrated_message(', a)
        body = original[a:b]
        changes = [
            ('skip-message-check', '                validate_hydrated_message(field(entry, "message"))?;', '                let _ = validate_hydrated_message;'),
            ('wrong-some-property', body, body.replace('cannot_read(content, "some")', 'cannot_read(content, "content")')),
            ('wrong-nonarray-error', body, body.replace('"message.content.some is not a function"', '"content.some is not a function"')),
            ('skip-null-block', body, body.replace('return Err(cannot_read(block, "type"));', 'continue;')),
            ('lose-short-circuit', body, body.replace('return Ok(true);', 'continue;')),
            ('reject-primitive-blocks', body, body.replace('    for block in blocks {', '    for block in blocks {\n        if !nullish(block) && !plain_object(block) { return Err(AdapterError::Type("block must be an object".into())); }')),
        ]
        for name, before, after in changes:
            if selected and name not in selected:
                continue
            assert original.count(before) == 1 and before != after, name
            ADAPTER.write_text(original.replace(before, after))
            run(name, True)
            ADAPTER.write_text(original)
        if not selected or 'wrong-reducer-order' in selected:
            # #1985 r4: locate the hydrate passes by the calls they make. The
            # agent-transcript cell (controlBoundary, turnSummary, metadata) runs
            # before WebHistory's fileArtifact/tokenUsage passes, and the old
            # `// WebHistory reads` comment anchor disappeared in d39affa51.
            opens = '        for entry in entries {\n'
            first = opens + '            if is_str(field(entry, "type"), "control_boundary") {\n                control_boundary_supported(entry)?;'
            web = opens + '            if is_durable_message_entry(entry) {'
            assert original.count(first) == 1 and original.count(web) == 1
            a = original.index(first)
            b = original.index(web, a)
            c = original.index('        Ok(())', b)
            ADAPTER.write_text(original[:a] + original[b:c] + original[a:b] + original[c:])
            run('wrong-reducer-order', True)
            ADAPTER.write_text(original)
        if not selected or 'read-outside-writer-lock' in selected:
            # Recreate the actual old race by moving lock acquisition past the read.
            before = '        let inner = self.transcript.lock();\n        let read = self.read_transcript(None)?;'
            after = '        let read = self.read_transcript(None)?;\n        let inner = self.transcript.lock();'
            assert original.count(before) == 1
            ADAPTER.write_text(original.replace(before, after))
            lock_test = 'storage::transcript_adapter::restore_tests::restore_serializes_read_and_state_installation_with_append'
            run('read-outside-writer-lock', True, lock_test)
        ADAPTER.write_text(original)
        run('restored-message', False)
        run('restored-lock', False, 'storage::transcript_adapter::restore_tests::restore_serializes_read_and_state_installation_with_append')
        expected = len(selected) if selected else 8
        assert len(killed) == expected, f'unknown selection or missing mutant: {selected}'
        print(f'PASS: {len(killed)} / {expected} selected repair-1 mutants killed', flush=True)
    finally:
        ADAPTER.write_text(original)
