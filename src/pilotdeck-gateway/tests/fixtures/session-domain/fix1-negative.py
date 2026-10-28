#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative replacement chain, no mutation table
"""#562 repair 1: behavioral mutants, bounded process groups, one launcher target."""
import os
from pathlib import Path
import signal
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'
assert Path(os.environ['CARGO_TARGET_DIR']).resolve() == SRC / 'target'
OUT = REPO / '_tmp/20261023-issue-222-domain-log/fix1-negative'
OUT.mkdir(parents=True, exist_ok=True)
ADAPTER = SRC / 'pilotdeck-gateway/src/storage/transcript_adapter.rs'
with mutating_sources(REPO, [ADAPTER]) as snapshots:
    original = snapshots[ADAPTER].decode()
    selected = set(sys.argv[1:])
    killed = []


    def stop(signum, frame):
        raise SystemExit(f'interrupted by signal {signum}')


    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)


    def run(name, red, test='storage::transcript_adapter_tests::real_ts_scenarios_match_outcomes_calls_and_bytes'):
        slot = subprocess.run([str(REPO / 'scripts/cargo-slots.sh')], timeout=10, capture_output=True, text=True)
        (OUT / f'{name}-slots.log').write_text(slot.stdout + slot.stderr)
        assert slot.returncode == 0, 'no cargo slot; rerun later'
        env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0', CARGO_INCREMENTAL='0')
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
        a = original.index('fn validate_hydrated_message(')
        b = original.index('fn message_supported(', a)
        body = original[a:b]
        changes = [
            ('skip-message-check', '                validate_hydrated_message(field(entry, "message"))?;', '                let _ = validate_hydrated_message;'),
            ('wrong-some-property', body, body.replace('property_error(content, "some")', 'property_error(content, "content")')),
            ('wrong-nonarray-error', body, body.replace('"message.content.some is not a function"', '"content.some is not a function"')),
            ('skip-null-block', body, body.replace('return Err(property_error(block, "type"));', 'continue;')),
            ('lose-short-circuit', body, body.replace('            break;', '            continue;')),
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
            a = original.index('        for entry in entries {', original.index('        // Hydration runs every'))
            b = original.index('        // WebHistory reads', a)
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
