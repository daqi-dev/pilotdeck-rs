#!/usr/bin/env python3
"""#983 HEAD-only CLI controls. Every mutant compiles before its bound test.

Usage: --check | --node-runtime NEW_OUTPUT_DIR | [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Requires caller CARGO_TARGET_DIR. New/uncommitted source deliberately refuses.
"""
import difflib
import json
import os
from pathlib import Path
import shutil
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, judge

root = Path(__file__).resolve().parents[5]
transport = root / 'src/pilotdeck-cli/src/sdk/transport.rs'
sessions = root / 'src/pilotdeck-cli/src/sdk/sessions.rs'
pretty = root / 'src/pilotdeck-cli/src/main.rs'
limits_oracle = root / 'src/pilotdeck-cli/tests/fixtures/sdk-transport-limits/oracle.json'
sdk = root / 'src/pilotdeck-cli/src/sdk.rs'
lease_tests = root / 'src/pilotdeck-cli/tests/sdk_sessions_list_exit.rs'
MATRIX = 'sessions_list_matches_ts_matrix'
LIFECYCLE = 'sessions_list_transport_boundaries'
oracle = root / 'src/pilotdeck-cli/tests/fixtures/sdk-sessions-list/oracle.json'
mutations = [
    ('hello-version', transport, '"protocolVersion": pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION', '"protocolVersion": "0.0"', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('websocket-default-limits', transport, 'WebSocketConfig::default()\n        .max_frame_size(Some(MAX_RECEIVE_BYTES))\n        .max_message_size(Some(MAX_RECEIVE_BYTES))', 'WebSocketConfig::default()', 'sessions_list_accepts_ts_large_frames', '#983 TS native accepts 16777217 byte frame'),
    ('websocket-default-message-limit', transport, '.max_message_size(Some(MAX_RECEIVE_BYTES))', '.max_message_size(Some(64 * 1024 * 1024))', 'sessions_list_accepts_ts_large_frames', '#983 TS native accepts 104857601 byte frame'),
    ('websocket-ws-payload-limit', transport, '.max_frame_size(Some(MAX_RECEIVE_BYTES))', '.max_frame_size(Some(100 * 1024 * 1024))', 'sessions_list_accepts_ts_large_frames', '#983 TS native accepts 104857601 byte frame'),
    ('websocket-unlimited-frame', transport, '.max_frame_size(Some(MAX_RECEIVE_BYTES))', '.max_frame_size(None)', 'sessions_list_rejects_extreme_frame_header', '#983 hostile frame 4611686018427387904 rejected cleanly before payload'),
    ('websocket-unlimited-message', transport, '.max_message_size(Some(MAX_RECEIVE_BYTES))', '.max_message_size(None)', 'sessions_list_rejects_fragmented_message_over_cap', '#983 fragmented message 134217729 bytes rejected cleanly'),
    ('transport-limits-coverage', limits_oracle, '"implementation": "node-native",\n      "frameBytes": 16777217', '"implementation": "ws-default",\n      "frameBytes": 16777217', 'sessions_list_accepts_ts_large_frames', '#983 native large-frame fixture coverage'),
    ('transport-limits-extraction', limits_oracle, '"constructorArgCount": 1', '"constructorArgCount": 2', 'sessions_list_transport_limits_regenerates_identically', '#983 identical TS fixture sdk-transport-limits'),
    ('hello-client-name', transport, '"clientName": "sdk"', '"clientName": "cli"', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('hello-client-version', transport, '"clientVersion": "pilotdeck-cli"', '"clientVersion": "wrong-cli"', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('hello-token', transport, '"token": connection.auth_token', '"token": "wrong-token"', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('rpc-method', sessions, '"list_sessions", params', '"wrong_method", params', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('project-param', sessions, 'params["projectKey"] = project.into();', 'params["wrongProject"] = project.into();', MATRIX, '#983 hello and request wire sessions-list--flag-repeat'),
    ('request-ok', transport, '== Some(true)', '== Some(false)', MATRIX, '#983 TS output and exit sessions-default--argv'),
    ('rpc-error-message', transport, 'string_field(value, "message")', 'string_field(value, "missingMessage")', MATRIX, '#983 TS output and exit sessions-default--gateway-rpc-error'),
    ('list-projection', sessions, '.get("sessions")', '.get("missingSessions")', MATRIX, '#983 TS output and exit sessions-default--argv'),
    ('list-default', sessions, 'map_or("list", String::as_str)', 'map_or("unknown", String::as_str)', MATRIX, '#983 expected gateway lifecycle sessions-default--argv'),
    ('pretty-indent', pretty, '&"  ".repeat(depth)', '&" ".repeat(depth)', MATRIX, '#983 TS output and exit sessions-default--argv'),
    ('close-handshake', transport, 'SinkExt::close(&mut socket)', 'socket.flush()', MATRIX, '#983 terminal connection closed sessions-default--argv'),
    ('unreachable-message', transport, 'error("Gateway connection failed.")', 'error("Gateway unreachable.")', MATRIX, '#983 TS output and exit sessions-default--gateway-unreachable'),
    ('auth-close-reason', transport, 'frame.reason.to_string()', 'format!("{frame:?}")', MATRIX, '#983 TS output and exit sessions-default--gateway-auth-rejected'),
    ('request-correlation', transport, '.as_deref() != Some(&id)', '.as_deref() == Some("")', LIFECYCLE, '#983 transport boundary output wrong-id-and-ping'),
    ('connect-and-hello-deadline', transport, 'CONNECT_TIMEOUT: Duration = Duration::from_secs(10)', 'CONNECT_TIMEOUT: Duration = Duration::from_secs(1)', LIFECYCLE, '#983 transport deadline duration upgrade-never'),
    ('rpc-deadline', transport, 'REQUEST_TIMEOUT: Duration = Duration::from_secs(30)', 'REQUEST_TIMEOUT: Duration = Duration::from_secs(1)', LIFECYCLE, '#983 transport deadline duration rpc-never'),
    ('close-fallback', transport, '"Gateway WebSocket closed."\n                } else', '"wrong close"\n                } else', LIFECYCLE, '#983 transport boundary output rpc-close'),
    ('invalid-json-error', transport, 'parse_json(&raw).map_err(|_| error("Invalid Gateway JSON frame."))', 'parse_json(&raw).map_err(|_| error("wrong JSON error"))', LIFECYCLE, '#983 transport boundary output rpc-invalid-json'),
    ('invalid-binary-error', transport, 'Some(Ok(_)) => return Err(error("Invalid Gateway JSON frame."))', 'Some(Ok(_)) => return Err(error("wrong binary error"))', LIFECYCLE, '#983 transport boundary output rpc-binary'),
    ('rpc-error-default', transport, 'unwrap_or("Gateway request failed.")', 'unwrap_or("wrong default error")', LIFECYCLE, '#983 transport boundary output error-default'),
    ('rpc-error-empty', transport, 'message.as_deref().unwrap_or("Gateway request failed.")', 'message.as_deref().filter(|s| !s.is_empty()).unwrap_or("Gateway request failed.")', LIFECYCLE, '#983 transport boundary output error-empty'),
    ('nullish-sessions', sessions, '!matches!(value, JsValue::Null | JsValue::Undefined)', '!matches!(value, JsValue::Undefined)', LIFECYCLE, '#983 transport boundary output null-sessions'),
    ('scalar-sessions', sessions, '.unwrap_or(&empty);', '.filter(|value| matches!(value, JsValue::Array(_))).unwrap_or(&empty);', LIFECYCLE, '#983 transport boundary output false-sessions'),
    ('null-result-error', sessions, 'if matches!(result, JsValue::Null | JsValue::Undefined)', 'if false && matches!(result, JsValue::Null | JsValue::Undefined)', LIFECYCLE, '#983 transport boundary output null-result'),
    ('pong-payload', transport, '.flush()', '.send(Message::Pong(vec![0].into()))', LIFECYCLE, '#983 exact pong wrong-id-and-ping'),
    ('codec-number-order', sessions, 'let raw = stringify_json(value)', 'let raw = stringify_json(&JsValue::Null)', MATRIX, '#983 TS output and exit sessions-default--argv'),
    ('original-extraction', oracle, '"nodeVersion": "v22.22.0"', '"nodeVersion": "v0.0.0"', 'sessions_list_oracle_regenerates_identically', '#983 identical TS fixture sdk-sessions-list'),
    ('lease-no-wait', lease_tests, 'if !waiting {', 'if !waiting { return Err(error); } if !waiting {', 'parity_lock_waits_for_predecessor', '#983 occupied lease waits before acquisition'),
    ('lease-timeout-kind', lease_tests, 'io::ErrorKind::TimedOut,', 'io::ErrorKind::Other,', 'parity_lock_times_out_without_reclaiming', '#983 occupied lease fails explicitly at deadline'),
    ('lease-late-acquisition', lease_tests, 'if waiting && Instant::now() >= deadline {', 'if waiting && Instant::now() >= deadline && path.exists() {', 'parity_lock_does_not_acquire_after_deadline', '#983 expired waiter cannot acquire released lease'),
    ('lease-owner-publish', lease_tests, '"pid": std::process::id(),', '"pid": 0,', 'parity_lock_publishes_and_releases_owned_lease', '#983 parity lease publishes complete ownership'),
    ('lease-owned-release', lease_tests, 'fs::remove_dir(&self.path)?;', 'if false { fs::remove_dir(&self.path)?; }', 'parity_lock_publishes_and_releases_owned_lease', '#983 parity lease releases owned directory'),
    ('lease-owner-check', lease_tests, '&& fs::read(self.path.join("owner.json"))? == self.owner', '&& !fs::read(self.path.join("owner.json"))?.is_empty()', 'parity_lock_preserves_replaced_owner', '#983 changed ownership prevents lease release'),
    ('lease-directory-check', lease_tests, '(metadata.dev(), metadata.ino()) == self.identity', 'metadata.dev() == self.identity.0', 'parity_lock_preserves_replaced_directory', '#983 changed directory identity prevents lease release'),
    ('lease-guard-check', lease_tests, 'guard.try_lock().map_err(io::Error::from)?;', 'let _ = guard.try_lock().map_err(io::Error::from);', 'parity_lock_guard_wait_is_bounded', '#983 occupied guard shares acquisition deadline'),
    ('lease-cancel-reservation', lease_tests, 'tokio::time::sleep_until(', 'fs::write(path.join("owner.json"), b"wrong waiter")?; tokio::time::sleep_until(', 'parity_lock_cancelled_wait_preserves_owner', '#983 cancelled waiter preserves predecessor ownership'),
    ('lease-release-retry', lease_tests, 'if error.kind() == io::ErrorKind::WouldBlock', 'if error.kind() == io::ErrorKind::Other', 'parity_lock_release_waits_for_guard', '#983 release retries a briefly occupied guard'),
    ('lease-release-deadline', lease_tests, 'std::time::Instant::now() + Duration::from_secs(2)', 'std::time::Instant::now() + Duration::from_secs(0)', 'parity_lock_release_guard_deadline_is_bounded', '#983 release guard exhausts bounded retry before failure'),
    ('lease-guard-stable', lease_tests, 'fs::remove_dir(&self.path)?;', 'fs::remove_dir(&self.path)?; let mut name = self.path.as_os_str().to_owned(); name.push(".guard"); fs::remove_file(Path::new(&name))?;', 'parity_lock_publishes_and_releases_owned_lease', '#983 parity guard inode remains stable'),
    ('lease-owner-timestamp', lease_tests, 'std::time::SystemTime::now()', 'std::time::UNIX_EPOCH', 'parity_lock_publishes_and_releases_owned_lease', '#983 parity lease publishes complete ownership'),
    ('lease-owner-token', lease_tests, 'uuid::Uuid::new_v4().simple().to_string()', '"invalid".to_owned()', 'parity_lock_publishes_and_releases_owned_lease', '#983 parity lease publishes complete ownership'),
    ('lease-guard-hardlink', lease_tests, 'metadata.nlink() != 1', 'metadata.nlink() == 0', 'parity_lock_rejects_guard_hardlinks', '#983 parity guard rejects multiple hardlinks'),
    ('lease-guard-symlink', lease_tests, 'libc::O_NOFOLLOW | libc::O_NONBLOCK', 'libc::O_NONBLOCK', 'parity_lock_rejects_guard_symlinks', '#983 parity guard rejects symlinks'),
    ('lease-guard-type', lease_tests, '!metadata.is_file()', 'metadata.is_dir()', 'parity_lock_rejects_guard_fifo', '#983 parity guard rejects nonregular files'),
]


def check(row):
    count = row[1].read_text().count(row[2])
    if count != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale anchor count={count}, expected 1')


def node_runtime_controls(out, env, run, passed):
    """Environment mutants: real extraction, no edits to uncommitted sources."""
    node = env['NODE22'] if 'NODE22' in env else shutil.which('node', path=env.get('PATH'))
    if not node:
        raise RuntimeError('#983 Node v22.22.0 must be available through NODE22 or PATH')
    node = shutil.which(node, path=env.get('PATH')) or node
    version = run_owned([node, '--version'], env=env, timeout=30)
    (out / 'real-node.stdout').write_text(version.stdout)
    (out / 'real-node.stderr').write_text(version.stderr)
    if version.returncode != 0 or version.stdout != 'v22.22.0\n':
        raise RuntimeError('#983 runtime controls require real Node v22.22.0')
    test = 'sessions_list_oracle_regenerates_identically'
    rows = [
        ('path-version', False, 'v22.22.1', 0,
         '#983 selected Node must report v22.22.0 before extraction'),
        ('override-version', True, 'v22.22.1', 0,
         '#983 selected Node must report v22.22.0 before extraction'),
        ('version-exit', True, 'v22.22.0', 23,
         '#983 selected Node --version must exit successfully'),
    ]

    def environment(label, override, reported, code):
        directory = out / label
        directory.mkdir()
        marker = directory / 'generator.called'

        def wrapper(path, reported, code):
            # Only --version is varied. Generation always executes real Node.
            path.write_text(f'#!{sys.executable}\n' +
                'import os, sys\nfrom pathlib import Path\n' +
                f'if sys.argv[1:] == ["--version"]:\n    print({reported!r})\n    sys.exit({code})\n' +
                f'marker = Path({str(marker)!r})\n' +
                'temporary = marker.with_suffix(".tmp")\n' +
                'temporary.write_text("generator invoked\\n")\ntemporary.replace(marker)\n' +
                f'os.execv({node!r}, [{node!r}, *sys.argv[1:]])\n')
            path.chmod(0o700)

        wrapper(directory / 'node', 'v0.0.0' if override else reported,
                0 if override else code)
        changed = dict(env)
        changed['PATH'] = str(directory) + os.pathsep + env.get('PATH', '')
        changed.pop('NODE22', None)
        if override:
            wrapper(directory / 'explicit-node', reported, code)
            changed['NODE22'] = str(directory / 'explicit-node')
        return changed, marker

    results = []
    for name, override, reported, code, assertion in rows:
        item = dict(name=name, test=test, assertion=assertion, status='INVALID',
                    reason='not run', compiled=False, restoredPass=False)
        try:
            positive, marker = environment(name + '-positive-env', override, 'v22.22.0', 0)
            if not passed(run(name + '-positive', test, positive), test) or not marker.is_file():
                raise RuntimeError('positive bound test did not extract through selected Node')
            negative, marker = environment(name + '-negative-env', override, reported, code)
            if run(name + '-compile', test, negative, compile=True).returncode != 0:
                raise RuntimeError('environment mutant compile failed; not evidence')
            item['compiled'] = True
            receipt = ReceiptRun(out, name + '-negative', [(test, assertion)])
            result = run(name + '-negative', test, receipt.environment(negative))
            killed, reason, _ = judge(result, '', [(test, assertion)], receipt)
            item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'), reason=reason)
            if marker.exists():
                item.update(status='INVALID', reason='generator ran before Node validation completed')
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        try:
            restored, marker = environment(name + '-restored-env', override, 'v22.22.0', 0)
            item['restoredPass'] = passed(run(name + '-restored', test, restored), test) and marker.is_file()
            if not item['restoredPass']:
                raise RuntimeError('restored bound test did not extract through selected Node')
        except Exception as error:
            item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
        results.append(item)
        print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = {status: sum(row['status'] == status for row in results)
              for status in ['KILLED', 'SURVIVED', 'INVALID']}
    (out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(any(row['status'] != 'KILLED' for row in results))


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        failures = []
        for row in mutations:
            try:
                check(row)
            except Exception as error:
                failures.append(str(error))
        print('\n'.join(failures) if failures else f'#983 {len(mutations)} anchors checked')
        return int(bool(failures))
    compile_only = '--compile-only' in args
    node_runtime = '--node-runtime' in args
    args = [arg for arg in args if arg not in ('--compile-only', '--node-runtime')]
    if not args:
        raise SystemExit('usage: --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('unknown/duplicate mutation selection')
    if node_runtime and (compile_only or len(args) != 1):
        raise SystemExit('--node-runtime requires exactly one new output directory')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    env.pop(ReceiptRun.ENV, None)
    env.pop(ReceiptRun.PARENT_ENV, None)
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-cli','--test','sdk_sessions_list_exit']

    def run(label, test, run_env, compile=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        argv = command + (['--no-run'] if compile else [test,'--','--exact','--include-ignored','--show-output','--test-threads=1'])
        result = run_owned(argv,cwd=root,env=run_env,timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    def passed(result, test):
        return (result.returncode == 0 and f'test {test} ... ok' in result.stdout
                and 'test result: ok. 1 passed; 0 failed;' in result.stdout)

    if node_runtime:
        return node_runtime_controls(out, env, run, passed)

    guarded = [*sorted((root / 'src/pilotdeck-cli/src').rglob('*.rs')),
               root / 'src/pilotdeck-cli/Cargo.toml', root / 'src/Cargo.lock', Path(__file__).resolve(),
               root / 'src/pilotdeck-cli/tests/sdk_sessions_list_exit.rs',
               oracle, root / 'src/pilotdeck-cli/tests/fixtures/sdk-sessions-list/generate.mjs',
               limits_oracle, root / 'src/pilotdeck-cli/tests/fixtures/sdk-transport-limits/generate.mjs',
               Path(__file__).resolve().parents[1] / 'negative_process.py',
               Path(__file__).resolve().parents[1] / 'negative_suite.py']
    results = []
    for name,path,old,new,test,assertion in selected:
        item = dict(name=name,test=test,status='INVALID',reason='not run',compiled=False)
        try:
            check((name,path,old,new,test,assertion))
            with mutating_sources(root,guarded) as originals:
                original = originals[path].decode()
                try:
                    if not compile_only and not passed(run(name+'-positive',test,env),test):
                        raise RuntimeError('positive bound test failed')
                    changed = original.replace(old,new,1)
                    (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(original.splitlines(True),changed.splitlines(True),fromfile=str(path.relative_to(root)),tofile=str(path.relative_to(root)))))
                    path.write_text(changed)
                    if run(name+'-compile',test,env,compile=True).returncode != 0:
                        raise RuntimeError('mutant compile failed; not evidence')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED',reason='mutant no-run passed')
                    else:
                        receipt = ReceiptRun(out,name+'-negative',[(test,assertion)])
                        result = run(name+'-negative',test,receipt.environment(env))
                        killed,reason,_ = judge(result,'',[(test,assertion)],receipt)
                        item.update(status='KILLED' if killed else ('SURVIVED' if passed(result,test) else 'INVALID'),reason=reason)
                finally:
                    path.write_bytes(originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError('restoration mismatch')
                if not compile_only and not passed(run(name+'-restored',test,env),test):
                    item.update(status='INVALID',reason=item['reason']+'; restored bound test failed')
        except Exception as error:
            item.update(status='INVALID',reason=f'{type(error).__name__}: {error}')
        results.append(item)
        print(f'{name}: {item["status"]}: {item["reason"]}',flush=True)
    counts = {status:sum(row['status']==status for row in results) for status in ['KILLED','SURVIVED','INVALID','COMPILED']}
    (out / 'summary.json').write_text(json.dumps(dict(controls=results,counts=counts),indent=2)+'\n')
    print(json.dumps(counts),flush=True)
    return int(any(row['status'] != ('COMPILED' if compile_only else 'KILLED') for row in results))

if __name__ == '__main__':
    sys.exit(main())
