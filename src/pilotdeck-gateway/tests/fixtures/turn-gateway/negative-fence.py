#!/usr/bin/env python3
"""Run exclusively in this tree using the launcher's one existing target.

A mutation is killed only by its named assertion. Compile errors and timeouts
are INVALID, recorded in summary.json; timeout kills the entire process group.
"""
import hashlib
import json
import re
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import mutating_sources, run_owned, target_environment
from negative_selection import select
from negative_source import assertion_message_present

fixture = Path(__file__).resolve().parent
workspace = fixture.parents[3]
repo = workspace.parent
env = target_environment(repo)
env['CARGO_NET_OFFLINE'] = 'true'
turn = workspace / 'pilotdeck-gateway/src/runtime/turn'
gateway = turn / 'gateway.rs'
submit = turn / 'submit.rs'
tests = turn / 'gateway_tests.rs'
submit_tests = turn / 'submit_tests.rs'
session = workspace / 'pilotdeck-gateway/src/runtime/session/mod.rs'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
for key in ['PDRS_GATEWAY_CASE', 'PDRS_SUBMIT_CAPABILITY']:
    env.pop(key, None)
with mutating_sources(repo, [gateway, submit, tests, submit_tests, session]) as snapshots:
    original = {p: snapshots[p] for p in [gateway, submit, tests, submit_tests, session]}
    text = original[gateway].decode()
    start = text.index('            let wait_for = {')
    end = text.index('            if let Some(mut done) = wait_for {', start)
    lookup = text[start:end]
    yield_step = text[text.index('            // Router.abort has returned.'):start]
    assert text.count(yield_step) == 1
    fence_test = 'abort_waits_for_latest_real_ts_session_fence'
    ws_test = 'real_ts_gateway_frames_registry_replay_and_concurrency'
    cap_test = 'unavailable_capabilities_are_explicitly_refused'

    def fence_marker(case):
        return f'#813 abort fence: {case}: results must match'

    def mutation(name, path, before, after, marker, test=fence_test):
        assert original[path].decode().count(before) == 1, (name, 'anchor not unique')
        # #813 fix-r8: a bare scenario name matches any failure in that case,
        # including a different assertion. Each mutant names the complete
        # message of the assertion it must trip.
        assert marker.startswith('#813 '), (name, 'marker must be a #813 assertion message', marker)
        assert marker != '#813 assertion' and not marker.startswith('#813 assertion `left ='), \
            (name, 'generic assertion diagnostic is not evidence')
        return dict(name=name, path=path, modified=original[path].decode().replace(before, after, 1).encode(),
                    marker=marker, target=marker, test=test)

    mutants = [
        mutation('max-not-current-slot', gateway,
            'drains\n                    .current\n                    .get(key)\n                    .and_then(|id| drains.active.get(id))',
            'drains.active.iter().filter(|(_, (run, _))| run.as_ref().is_some_and(|run| &run.session_key == key)).max_by_key(|(id, _)| *id).map(|(_, entry)| entry)',
            fence_marker('completed-slot-does-not-refill')),
        mutation('begin-keeps-old-slot', gateway, 'self.current.insert(run.session_key.clone(), id);',
            'self.current.entry(run.session_key.clone()).or_insert(id);', fence_marker('latest-session-fence-only')),
        mutation('obsolete-drop-clears-current-slot', gateway,
            '&& drains.current.get(&run.session_key) == Some(&self.id)', '',
            fence_marker('obsolete-drop-preserves-current-slot')),
        mutation('other-session-selected', gateway, 'drains\n                    .current\n                    .get(key)',
            'drains.current.values().max()', fence_marker('other-session-does-not-replace')),
        mutation('snapshot-before-router-await', gateway, yield_step + lookup,
            lookup + yield_step, fence_marker('replacement-during-abort')),
        mutation('snapshot-after-owner-drain', gateway, yield_step,
            '            self.0.submitter.owner.abort_turn(key, run, reason).await.map_err(|error| RequestError::new(error.to_string()))?;\n' ,
            fence_marker('ungated-cleanup-replacement'), 'abort_snapshots_before_ungated_cleanup_replacement'),
        mutation('admission-only-matching', session,
            'run_id.is_none() || cancelled || active.as_ref() == run_id',
            'run_id.is_none() || (cancelled && false) || active.as_ref() == run_id',
            fence_marker('admission-cleared-before-consumer')),
        mutation('stale-run-check-skipped', session,
            'run_id.is_none() || cancelled || active.as_ref() == run_id',
            '(run_id.is_none() || cancelled || active.as_ref() == run_id) || true',
            fence_marker('stale-run-does-not-wait')),
        mutation('pre-admission-overwrites-slot', submit,
            '        admitted(&run);', '', fence_marker('rejected-run-does-not-replace-fence')),
        mutation('requested-run-filter', gateway,
            '.and_then(|id| drains.active.get(id))',
            '.and_then(|id| drains.active.get(id)).filter(|(identity, _)| run.is_none_or(|run| identity.as_ref().is_some_and(|submitted| &submitted.run_id == run)))',
            fence_marker('replacement-during-abort')),
        mutation('consumer-wait-skipped', gateway, 'let _ = done.wait_for(|done| *done).await;',
            'let _ = &mut done;', fence_marker('admission-cleared-before-consumer')),
        mutation('observation-does-not-wait', tests,
            'if self.0.lock().unwrap().len() >= frames && (!ended || consumer.load(Ordering::SeqCst))',
            'if true || (self.0.lock().unwrap().len() >= frames && (!ended || consumer.load(Ordering::SeqCst)))',
            '#813 transport observation: must wait for delivery before the receive completes', 'transport_observation_waits_for_delivery_and_receive_completion'),
    ]
    # Move the observer to identity resolution, before acceptance/admission, so a
    # busy/preflight submission can overwrite a valid current consumer slot.
    m = next(m for m in mutants if m['name'] == 'pre-admission-overwrites-slot')
    s = m['modified'].decode()
    anchor = '        let acceptance = RunAcceptInput {'
    assert s.count(anchor) == 1
    m['modified'] = s.replace(anchor, '        admitted(&run);\n' + anchor, 1).encode()
    # That mutation is exercised through the real WS busy case as well: manual
    # fence replay deliberately calls only the completion-begin observer.
    m['test'] = ws_test
    m['marker'] = m['target'] = '#813 abort-after-busy: step \"abort\": gateway frames, active and ended must match'

    # The two filter controls must fail through the named test's own
    # `assert!(selected > 0, "<message>")`. The marker is that assertion
    # message, not a code fragment; verify the literal is still in the test
    # source before trusting any verdict derived from it (#813 fix-r3).
    filter_assertions = {
        'PDRS_GATEWAY_CASE': ('#813 gateway case filter: must match at least one scenario', tests),
        'PDRS_SUBMIT_CAPABILITY': ('#813 submit capability filter: must match at least one scenario', submit_tests),
    }
    for key, (message, path) in filter_assertions.items():
        # Relax whitespace only outside the string literal: rustfmt may split
        # the assertion across lines, but the message itself is data (#813
        # fix-r4). `str.split()` used to normalize `must  match` to `must
        # match` and let that drift through.
        assert assertion_message_present(original[path].decode(), message), \
            (key, 'filter assertion message drifted')

    selected = [dict(name='baseline', test=fence_test, marker=None)] + mutants
    for key, test in [('PDRS_GATEWAY_CASE', ws_test), ('PDRS_SUBMIT_CAPABILITY', cap_test)]:
        message, _path = filter_assertions[key]
        for label, value in [('unknown', 'does-not-exist'), ('empty', '')]:
            selected.append(dict(name=f'{key.lower()}-{label}', test=test,
                marker=message, filter=(key, value)))
    selected = select(
        selected, sys.argv[2:], name=lambda row: row['name'])
    command = ['cargo', 'test', '--offline', '--manifest-path', str(workspace / 'Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--lib']
    summary = []
    try:
        for control in selected:
            assert all(p.read_bytes() == data for p, data in original.items()), 'concurrent source edit'
            slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], capture_output=True, text=True, timeout=10)
            slot_deadline = time.monotonic() + 1800
            while slots.returncode == 1:
                assert time.monotonic() < slot_deadline, 'cargo slots unavailable for 30 minutes'
                time.sleep(20)
                slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], capture_output=True, text=True, timeout=10)
            assert slots.returncode == 0
            modified = dict(original)
            if 'path' in control:
                modified[control['path']] = control['modified']
                control['path'].write_bytes(control['modified'])
            run_env = dict(env)
            if 'filter' in control:
                key, value = control['filter']
                run_env[key] = value
            module = 'submit' if control['test'] == cap_test else 'gateway'
            test_name = f"runtime::turn::{module}::tests::{control['test']}"
            cmd = command + [test_name, '--', '--exact']
            timed_out = False
            try:
                run = run_owned(cmd, cwd=repo, env=run_env, timeout=300)
                raw = run.stdout + run.stderr
                returncode = run.returncode
            except subprocess.TimeoutExpired as error:
                timed_out = True
                raw = (error.output or '') + (error.stderr or '')
                returncode = -signal.SIGKILL
            finally:
                for path, data in original.items():
                    assert path.read_bytes() == modified[path], f'concurrent edit preserved: {path}'
                    if modified[path] != data:
                        path.write_bytes(data)
            marker = control['marker']
            result = re.search(r'^test result: ok\. (\d+) passed; 0 failed;', raw, re.MULTILINE)
            baseline_ok = (returncode == 0 and result is not None
                and int(result.group(1)) == 1
                and re.search(r'^test ' + re.escape(test_name) + r' \.\.\. ok$', raw, re.MULTILINE) is not None)
            valid = not timed_out and (baseline_ok if marker is None else (
                returncode == 101 and 'test result: FAILED' in raw
                and 'could not compile' not in raw
                and named_test_killed(raw, test_name, marker, control.get('target'))))
            (out / f'{control["name"]}.log').write_text('COMMAND ' + ' '.join(cmd) + '\n' + slots.stdout + slots.stderr
                + raw + f'\nexit={returncode} timed_out={timed_out}\n')
            summary.append(dict(name=control['name'], exit=returncode, timed_out=timed_out, valid=valid, marker=marker))
            (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
            print(f'{control["name"]}: {"PASS" if valid else "INVALID"} exit={returncode} timed_out={timed_out}', flush=True)
            if not valid and marker is None:
                # A subset run still needs the unmutated copy to pass. Without
                # that proof no mutant verdict counts, so the whole run is
                # INVALID instead of reporting per-mutant results.
                raise SystemExit(1)
            # Continue after an invalid/timeout mutant, retaining all evidence.
    finally:
        assert all(p.read_bytes() == data for p, data in original.items()), 'original bytes not restored'
        (out / 'restored-sha256.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(data).hexdigest()
            for p, data in original.items()}, indent=2) + '\n')
    raise SystemExit(0 if all(row['valid'] for row in summary) else 1)
