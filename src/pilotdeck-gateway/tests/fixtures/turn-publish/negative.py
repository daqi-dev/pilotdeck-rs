#!/usr/bin/env python3
"""Temporarily mutate the owned implementation, require assertion failure, restore.

No tests/oracle are changed. Run alone: the shared tree flock plus a HEAD
byte baseline prevents another mutation run from adopting a mutant. Every mutation and the original SHA are recorded before
writing. finally restores the exact bytes, including on failed controls.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources

fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
repo = rust.parent
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
source = rust / 'pilotdeck-gateway/src/runtime/turn/publish.rs'
connection = rust / 'pilotdeck-gateway/src/runtime/connection/mod.rs'
with mutating_sources(repo, [source, connection]) as snapshots:
    originals = {source: snapshots[source], connection: snapshots[connection]}
    env = dict(os.environ, CARGO_NET_OFFLINE='true')
    env.setdefault('CARGO_TARGET_DIR', str(rust / 'target'))
    assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
    prefix = 'runtime::turn::publish::tests::'
    mutants = [
        ('drop-event', source,
         '                        state.queue.push_back(event);',
         '                        let _ = event;', 'ordered_durable_terminal_after_session_end'),
        ('duplicate-event', source,
         '                        state.queue.push_back(event);',
         '                        state.queue.push_back(event.clone()); state.queue.push_back(event);',
         'ordered_durable_terminal_after_session_end'),
        ('visible-before-durable', source,
         '        let future = registry.append(&self.run, &event);',
         '        state.queue.push_back(event.clone());\n        let future = registry.append(&self.run, &event);',
         'ordered_durable_terminal_after_session_end'),
        ('early-terminal-before-session-end', source,
         '        if is(&event, "type", "turn_completed") {',
         '        if false && is(&event, "type", "turn_completed") {',
         'ordered_durable_terminal_after_session_end'),
        ('missing-result-unknown', source,
         '                Err(_) if !state.failure_published => {',
         '                Err(_) if false => {', 'append_failure_then_success_single_unknown'),
        ('repeated-result-unknown', source,
         '                    state.failure_published = true;',
         '                    state.failure_published = false;', 'all_appends_fail_only_one_unknown'),
        ('close-with-append-pending', source,
         '\n        self.publication.wait_for(&writes).await;\n',
         '\n        let _ = writes;\n',
         'close_waits_for_pending_append'),
        ('duplicate-replay-lost', source,
         '        Ok(Some(events)) => TurnAcceptance::Replay(events),',
         '        Ok(Some(_events)) => TurnAcceptance::Replay(vec![]),', 'duplicate_replays_durable_events'),
        ('conflict-treated-as-unknown', source,
         '            let conflict = error.code() == Some("conflict");',
         '            let conflict = false;', 'conflict_does_not_start_pump'),
        ('accept-failure-as-conflict', source,
         '            let conflict = error.code() == Some("conflict");',
         '            let conflict = true;', 'accept_failure_result_unknown'),
        ('preflight-failure-exposes-original', source,
         '        return persistence_failure(&run.run_id, true);',
         '        return event;', 'preflight_registry_failure'),
        ('reverse-visible-order', source,
         '                        state.queue.push_back(event);',
         '                        state.queue.push_front(event);', 'file_registry_publication_preserves_fifo_across_workers'),
        ('late-append-not-awaited', source,
         '                self.publication.wait_for(&writes).await;\n                let _pump = self.pump.lock().unwrap();',
         '                let _ = writes;\n                let _pump = self.pump.lock().unwrap();',
         'late_event_while_terminal_append_pending'),
        ('duplicate-terminal-suppressed', source,
         '            state.deferred.push(event);',
         '            if state.deferred.is_empty() { state.deferred.push(event); }',
         'duplicate_terminal_is_registry_failure'),
        ('ws-seq-confused-with-module', connection,
         '            out.insert("seq".into(), JsValue::Number(stream.0 as f64));',
         '            out.insert("seq".into(), JsValue::Number((stream.0 + 71) as f64));',
         'ordered_durable_terminal_after_session_end'),
        ('ws-final-too-early', connection,
         '            out.insert("final".into(), JsValue::Bool(false));',
         '            out.insert("final".into(), JsValue::Bool(true));',
         'ordered_durable_terminal_after_session_end'),
        ('storage-failure-exposes-original', source,
         '                Err(_) if !state.failure_published => {',
         '                Err(_) if !state.failure_published => { state.queue.push_back(event);',
         'real_storage_persist_failure'),
        ('external-emitter-skipped', source,
         '        self.coordinator\n            .emit(&self.publication.run.session_key, event)',
         '        let _ = event; false', 'ordered_durable_terminal_after_session_end'),
        ('file-adapter-skips-durable', source,
         '        self.call(move |registry| registry.append(&run, &event))',
         '        self.call(move |_registry| { let _ = (run, event); Ok(()) })',
         'file_registry_publication_preserves_fifo_across_workers'),
    ]
    summary = []
    try:
        for name, path, before, after, test in mutants:
            assert originals[path].decode().count(before) == 1, (name, 'anchor is not unique')
        (output / 'negative-original-sha256.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(b).hexdigest() for p,b in originals.items()}, indent=2)+'\n')
        for name, path, before, after, test in [('baseline', None, None, None, '')] + mutants:
            if path:
                assert path.read_bytes() == originals[path], f'{path} changed outside this run'
                path.write_text(originals[path].decode().replace(before, after, 1))
            slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True)
            while slots.returncode == 1:
                time.sleep(30)
                slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True)
            command = ['cargo','test','--offline','--manifest-path',str(rust / 'Cargo.toml'),'-p','pilotdeck-gateway','--lib',prefix+test,'--','--nocapture']
            process = subprocess.Popen(command, cwd=repo, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True)
            try:
                raw, _ = process.communicate(timeout=240)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                raw, _ = process.communicate()
            finally:
                if path: path.write_bytes(originals[path])
            valid = (process.returncode == 0 and 'test result: ok' in raw) if name == 'baseline' else (
                process.returncode == 101 and 'test result: FAILED' in raw and 'could not compile' not in raw and prefix+test in raw
                and ('panicked at' in raw or 'assertion' in raw))
            (output / f'negative-{name}.log').write_text('COMMAND '+ ' '.join(command)+'\n'+slots.stdout+slots.stderr+raw+f'\nexit={process.returncode}\n')
            summary.append(dict(name=name, file=str(path.relative_to(repo)) if path else None, test=prefix+test, before=before, after=after, exit=process.returncode, valid=valid))
            (output/'negative-summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2)+'\n')
            print(f'{name}: {"PASS" if valid else "INVALID"} exit={process.returncode}', flush=True)
            if not valid: raise SystemExit(1)
    finally:
        for path, original in originals.items():
            # These paths belong exclusively to this run while the lock exists.
            path.write_bytes(original)
            assert path.read_bytes() == original
    print(f'{len(mutants)} compiled implementation mutants killed; original bytes restored', flush=True)
