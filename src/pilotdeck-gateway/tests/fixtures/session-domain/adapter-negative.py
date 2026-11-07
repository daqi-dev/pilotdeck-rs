#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative replacement chain, no mutation table
"""Full gateway mutation checks for store ordering and the former Node gaps."""
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
OUT = REPO / '_tmp/20261023-issue-222-domain-log/adapter-negative'
OUT.mkdir(parents=True, exist_ok=True)
ADAPTER = SRC / 'pilotdeck-gateway/src/storage/transcript_adapter.rs'
RECORDER = SRC / 'pilotdeck-gateway/src/storage/recorder.rs'
TEST = SRC / 'pilotdeck-gateway/src/storage/transcript_adapter_tests.rs'
with mutating_sources(REPO, [ADAPTER, RECORDER, TEST]) as snapshots:
    originals = {p: snapshots[p].decode() for p in [ADAPTER, RECORDER, TEST]}

    def stop(signum, frame):
        raise SystemExit(f'interrupted by signal {signum}')
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    def restore():
        for path, text in originals.items():
            path.write_text(text)

    def run(name, red):
        # #1908: the shared wait replaces the single 300 s slot sample; the
        # budget now comes from the shared constant above the single-sample cap.
        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=CARGO_SLOTS_WAIT_SECONDS)
        (OUT / f'{name}-slots.log').write_text(slot.stdout + slot.stderr)
        assert slot.returncode in (0, 69), 'no cargo slot; rerun later'
        env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG=os.environ.get('CARGO_PROFILE_DEV_DEBUG', 'line-tables-only'), CARGO_PROFILE_TEST_DEBUG=os.environ.get('CARGO_PROFILE_TEST_DEBUG', 'line-tables-only'))
        with (OUT / f'{name}.log').open('w') as log:
            child = subprocess.Popen(['cargo', 'test', '-p', 'pilotdeck-gateway', '--lib', 'storage::transcript_adapter_tests::real_ts_scenarios_match_outcomes_calls_and_bytes', '--', '--exact', '--nocapture'], cwd=SRC, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                code = child.wait(timeout=240)
            except BaseException:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
                raise
        text = (OUT / f'{name}.log').read_text()
        assert 'running 1 test' in text, f'{name}: compilation/zero-test failure is not evidence'
        assert ('test result: FAILED' in text and code != 0) if red else ('test result: ok. 1 passed' in text and code == 0), name
        print(f'{name}: {"KILLED" if red else "PASS"}', flush=True)

    try:
        run('baseline', False)
        # Restore the actual removed NodeHostRequired variant and its turn refusal.
        for path, text in originals.items():
            path.write_text(text.replace('ProjectionUnavailable', 'NodeHostRequired'))
        text = ADAPTER.read_text()
        # #297 r1 refactor moved the fold to the in-place advance form; the
        # #1985-era `domain.next(entry)` anchor no longer exists (re-anchored
        # per the #297 plan's shared-anchor section, r3).
        anchor = '        domain.advance(entry, undo).map_err(AdapterError::from)'
        assert text.count(anchor) == 1
        ADAPTER.write_text(text.replace(anchor, '''        if field(entry, "type").as_string().is_some_and(|t| t.equals_str("turn_started")) {
                return Err(AdapterError::NodeHostRequired("Session domain validation for turn_started entries requires the Node host.".into()));
            }
    ''' + anchor))
        run('restore-node-host-required', True)
        restore()
        before = originals[ADAPTER]
        # #1985 r2 ordering mutants: hydrate runs the registered projectors one
        # at a time over the whole log — agent-transcript metadata, then
        # WebHistory's fileArtifact message scan, then its tokenUsage
        # (`compactMetadata.postTokens`). Swapping two passes flips which error
        # the double-fault golden rows surface.
        P_METADATA = '''        for entry in entries {
            metadata_reduce_supported(entry)?;
        }
'''
        P_MESSAGE_LOOP = '''        for entry in entries {
            if is_durable_message_entry(entry) {
                validate_hydrated_message(field(entry, "message"))?;
            }
        }
'''
        P_ARTIFACT_LOOP = '''        for entry in entries {
            file_artifact_supported(entry)?;
        }
'''
        P_COMPACT = '''        for entry in entries {
            if is_str(field(entry, "type"), "control_boundary") {
                compact_budget_supported(entry)?;
            }
        }'''
        P_WEBHOOK_COMMENT = '''        // WebHistory's fileArtifact reducer (`WebHistoryProjections.ts:75-88`)
        // is registered before its tokenUsage reducer
        // (`WebHistoryProjections.ts:266-270`) and walks the log once. Per entry
        // it reads `message.content.some` (via messageContainsToolResult) on
        // durable messages and `entry.artifacts.length` on `file_artifacts`
        // entries, so the first malformed entry of either kind wins over a
        // missing `compactMetadata` (Codex #1985 r3 measured both orders).
'''
        P_FILEARTIFACT = '''        for entry in entries {
            if is_durable_message_entry(entry) {
                validate_hydrated_message(field(entry, "message"))?;
            }
            file_artifact_supported(entry)?;
        }
'''
        restore_passes = P_METADATA + P_WEBHOOK_COMMENT + P_FILEARTIFACT + P_COMPACT
        changes = [
          ('premature-malformed-restore-refusal', '        // Hydration runs every registered reducer over the whole log, reducer by', '''        for entry in entries {
            if field(entry, "type").as_string().is_some_and(|t| t.equals_str("session_metadata")) {
                let metadata = field(entry, "metadata");
                list_title_supported(&metadata.get("title").cloned(), &None)?;
            }
        }
        // Hydration runs every registered reducer over the whole log, reducer by'''),
          ('list-denials-not-array', '            if !matches!(denials, JsValue::Array(_)) {', '            if !js_iterable(denials) {'),
          ('restore-metadata-before-webhistory', restore_passes,
           P_WEBHOOK_COMMENT + P_FILEARTIFACT + P_COMPACT + P_METADATA),
          ('restore-message-after-posttok', restore_passes,
           P_METADATA + P_WEBHOOK_COMMENT + P_COMPACT + P_FILEARTIFACT),
          ('restore-artifact-read-missing', '            file_artifact_supported(entry)?;\n', ''),
          ('restore-artifact-array-only', '    let artifacts = field(entry, "artifacts");\n    if nullish(artifacts) {', '    let artifacts = field(entry, "artifacts");\n    if !matches!(artifacts, JsValue::Array(_)) {'),
          ('restore-artifact-pass-split', P_FILEARTIFACT, P_MESSAGE_LOOP + P_ARTIFACT_LOOP),
          ('list-title-trim-dropped', '''        Some(_) => {
            return Err(AdapterError::Type(
                "metadata.title?.trim is not a function".into(),
            ));
        }''', '        Some(_) => false,'),
          ('list-null-result-refusal', '    let result = field(entry, "result");\n    if nullish(result) {\n        return Err(cannot_read(result, "usage"));\n    }', '    let result = field(entry, "result");\n    if nullish(result) {\n        return Err(AdapterError::ProjectionUnavailable("result missing".into()));\n    }'),
          # #297 r1 refactor: adapter_domain now returns Result<()> over the
          # in-place advance, so skipping validation is an early Ok(()) tail —
          # the old `Ok(domain.clone())` no longer compiles against the new
          # signature (plan §共享锚点更新).
          ('adapter-skips-domain-validation', anchor, '        Ok(())'),
          # #297 r1 refactor: the pre-append `domain.clone()` publish moved into
          # the undo journal — the equivalent observable is now "roll the domain
          # back after a successful append" (the disarm `drop(undo)` replaced by
          # an undoing rollback), leaving the writer behind the state the log
          # already holds (anthropic r2 P1#3 suggestion). The append-failure
          # rollback deletion is the issue-297-domain-fold fixture's
          # plan-pinned mutant, so this row keeps the distinct post-commit half.
          # r4 (MM r3 boundary/simplify, DS r3 sibling): renamed from the r1-era
          # `domain-published-before-store` — the old name described the removed
          # pre-store publish; this row pins the post-commit rollback.
          ('domain-rolled-back-after-store', '        drop(undo);',
           '        inner.recorder_domain.rollback(&mut undo);'),
        ]
        for name, old, new in changes:
            assert before.count(old) == 1, (name, before.count(old))
            ADAPTER.write_text(before.replace(old, new))
            run(name, True)
            restore()
        a = before.index('    fn restore_entries(')
        b = before.index('    /// JsonlTranscriptWriter.close', a)
        segment = before[a:b]
        assert segment.count('inner.recorder_domain = domain;') == 1
        ADAPTER.write_text(before[:a] + segment.replace('inner.recorder_domain = domain;', 'inner.recorder_domain = DomainState::default();') + before[b:])
        run('restore-loses-domain-state', True)
        restore()
        run('restored', False)
        print('PASS: 13 / 13 adapter mutants killed', flush=True)
    finally:
        restore()
