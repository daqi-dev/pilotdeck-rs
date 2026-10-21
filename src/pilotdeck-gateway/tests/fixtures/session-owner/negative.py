#!/usr/bin/env python3
"""Compile real product mutations in an owned /tmp workspace; never edit the checkout."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def main():
    rust = Path(__file__).resolve().parents[4]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    assert os.environ.get('PDRS_BASELINE'), 'PDRS_BASELINE is required for the shared replay guard'
    assert os.environ.get('CARGO_HOME'), 'CARGO_HOME is required'
    env = dict(os.environ, CARGO_NET_OFFLINE='true', CARGO_TARGET_DIR=str(rust / 'target'))
    owner = 'pilotdeck-gateway/src/runtime/session/owner.rs'
    admission = 'pilotdeck-gateway/src/runtime/session/admission.rs'
    fixture = 'pilotdeck-gateway/tests/fixtures/session-owner/oracle.json'
    module = 'runtime::session::owner::tests::'
    mutants = [
        ('old-handle-is-current', owner,
         '.get(key)\n            .is_some_and(|record| record.handle.same_identity(handle))',
         '.get(key)\n            .is_some_and(|_record| { let _ = handle; true })',
         'dirty_identity_and_publication_matches_ts', 'oldCurrent'),
        ('global-factory-wait', owner,
         'fn ticket(state: &mut State, key: &JsString) -> Ticket {',
         'fn ticket(state: &mut State, key: &JsString) -> Ticket {\n'
         '    let _ = key; let global = JsString::from("global"); let key = &global;',
         'slow_factory_is_per_session_matches_ts', 'controlled lifecycle must progress independently'),
        ('ignore-maintenance', admission,
         '|| state.maintenance.contains(session_key)', '|| false',
         'maintenance_blocks_turn_and_idle_matches_ts', 'maintenance_blocks_turn_and_idle: value'),
        ('ignore-stop-new-publication', owner,
         'let publication_error = {\n            let mut state = self.0.state.lock().unwrap();\n            if state.stopped {',
         'let publication_error = {\n            let mut state = self.0.state.lock().unwrap();\n            if false {',
         'shutdown_during_creation_matches_ts', 'expected failure'),
        ('unchecked-live-abort', owner,
         '.map_or(!exact_completion_owner, |active| active != run)',
         '.map_or({ let _ = exact_completion_owner; false }, |_active| false)',
         'live_sink_rechecks_owner_after_abort_decision_across_threads', 'live replacement hits: 100/100'),
        ('stale-end-releases-readmitted-reservation', admission,
         '.is_some_and(|current| Arc::ptr_eq(current, &reservation.0))',
         '.is_some_and(|current| { let _ = (current, reservation); true })',
         'stale_end_after_close_and_same_run_readmission_preserves_new_reservation',
         'stale end_turn released the readmitted same-run reservation'),
        ('old-close-clears-new-reservation', owner,
         '.end_identity(&key, identity.as_ref());', '.end_turn(&key, { let _ = identity; None }, None);',
         'close_and_completion_cleanup_preserve_later_same_run_reservation',
         'old close released a new reservation with the same runId'),
        ('old-complete-releases-new-owner', owner,
         'let current = self.0.turns.completion.is_current(key, handle);',
         'self.0.turns.admission.end_turn(key, None, None);\n        let current = self.0.turns.completion.is_current(key, handle);',
         'close_and_completion_cleanup_preserve_later_same_run_reservation',
         'assertion failed: owner.has_active_turn(&key)'),
    ]
    originals = {name: (rust / name).read_text() for name in {m[1] for m in mutants} | {fixture}}
    oracle = json.loads(originals[fixture])
    source_hash = oracle['sourceSha256']['src/gateway/SessionRouter.ts']
    mutants.append(('tampered-source-hash', fixture, source_hash, '0' * 64,
                    'dirty_identity_and_publication_matches_ts',
                    'oracle hash for src/gateway/SessionRouter.ts does not match'))
    for name, file, before, after, test, marker in mutants:
        assert originals[file].count(before) == 1, (name, originals[file].count(before))
    results = []
    with tempfile.TemporaryDirectory(prefix='pdrs-session-owner-negative-', dir='/tmp') as directory:
        scratch = Path(directory) / 'src'
        # Same workspace, dependencies and manifests; no new dependency or mock
        # copy of project/refresh behavior is introduced by this harness.
        shutil.copytree(rust, scratch, ignore=shutil.ignore_patterns('target', '.git', 'node_modules', '.env*'))
        shutil.copytree(rust.parent / 'specs', Path(directory) / 'specs')
        for name, file, before, after, test, marker in [('baseline', None, None, None, '', 'test result: ok')] + mutants:
            for path, original in originals.items():
                content = original
                if path == file:
                    assert content.count(before) == 1, (name, content.count(before))
                    content = content.replace(before, after, 1)
                (scratch / path).write_text(content)
            gate = subprocess.run(['bash', '-c', 'until bash /Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh; do sleep 30; done'],
                                  capture_output=True, text=True, check=True)
            command = ['cargo', 'test', '--offline', '--manifest-path', str(scratch / 'Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--lib', module + test, '--', '--nocapture']
            result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=240)
            raw = result.stdout + result.stderr
            (output / f'negative-{name}.log').write_text(gate.stdout + gate.stderr + raw + f'\nexit={result.returncode}\n')
            valid = marker in raw and (result.returncode == 0 if name == 'baseline' else
                    result.returncode == 101 and 'test result: FAILED' in raw and 'could not compile' not in raw)
            results.append(dict(name=name, file=file, before=before, after=after, test=module + test,
                                exit=result.returncode, marker=marker, valid=valid))
            (output / 'negative-summary.json').write_text(json.dumps(results, ensure_ascii=False, indent=2) + '\n')
            print(f'{name}: {"PASS" if valid else "INVALID"} exit={result.returncode}', flush=True)
            if not valid:
                raise SystemExit(1)
    print('all mutations compiled and failed their targeted assertion; /tmp workspace removed')


if __name__ == '__main__':
    main()
