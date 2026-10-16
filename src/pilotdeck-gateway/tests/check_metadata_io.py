#!/usr/bin/env python3
"""Re-record default Node metadata errors, verify retained Native Rust, restore."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import shutil
import selectors
import stat
import subprocess
import time
import tomllib


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


SOURCE_FILES = (
    'src/pilotdeck-gateway/src/metadata.rs', 'src/pilotdeck-gateway/src/metadata/tests.rs',
    'src/pilotdeck-gateway/src/pairing/tests.rs', 'src/pilotdeck-gateway/tests/record_metadata_io.mts',
    'src/pilotdeck-gateway/tests/check_metadata_io.py', 'src/pilotdeck-gateway/tests/test_metadata_io_driver.py',
    'src/pilotdeck-gateway/tests/baseline_source_guard.mjs', 'src/pilotdeck-gateway/tests/test_baseline_source_guard.mjs',
    'src/pilotdeck-gateway/tests/fixtures/metadata/describe-io-errors.json',
    '.github/workflows/ci.yml', 'rust-toolchain.toml', 'src/Cargo.toml', 'src/Cargo.lock',
    'src/pilotdeck-gateway/Cargo.toml',
)


def source_snapshot(root):
    result = {}
    for name in SOURCE_FILES:
        path = root / name
        if not path.is_file() or path.is_symlink():
            raise RuntimeError('required source missing or redirected: ' + name)
        result[name] = sha(path)
    return result


def rustup_snapshot(root):
    rows = []
    def visit(path):
        if not os.path.lexists(path):
            rows.append((str(path.relative_to(root)), 'missing'))
            return
        value = path.lstat()
        kind = 'link' if stat.S_ISLNK(value.st_mode) else 'dir' if stat.S_ISDIR(value.st_mode) else 'file'
        rows.append((str(path.relative_to(root)), kind, value.st_size, value.st_mtime_ns,
                     os.readlink(path) if kind == 'link' else None))
        if kind == 'dir':
            for child in sorted(path.iterdir()):
                visit(child)
    for name in ('toolchains', 'downloads', 'tmp'):
        visit(root / name)
    return dict(roots=['toolchains', 'downloads', 'tmp'], entries=len(rows), sha256=hashlib.sha256(json.dumps(rows).encode()).hexdigest())


def check_receipts(text, marker, ids):
    actual = re.findall(r'\b' + re.escape(marker) + r' (\S+)', text)
    if len(actual) != 10 or len(ids) != 10 or set(actual) != set(ids) or any(not value for value in ids):
        raise RuntimeError(marker + ': expected ten unique actual-case receipts')


def permission_checkpoint(stdout, expected_scratch):
    rows = [json.loads(line.removeprefix('PDRS_METADATA_PERMISSION '))
            for line in stdout.split('\n') if line.startswith('PDRS_METADATA_PERMISSION ')]
    if not rows:
        return None
    first = rows[0]
    plan = {key: first[key] for key in ('baselineCommit', 'owner', 'scratch')}
    if plan['baselineCommit'] != 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85':
        raise RuntimeError('permission baseline mismatch')
    scratch = Path(plan['scratch']).resolve(strict=True)
    if scratch != expected_scratch.resolve(strict=True) or not plan['owner']:
        raise RuntimeError('permission owner root mismatch')
    if (scratch/'.metadata-oracle-owner').read_text() != plan['owner']:
        raise RuntimeError('permission owner marker mismatch')
    plan['permissions'] = []
    for row in rows:
        if any(row[key] != first[key] for key in ('baselineCommit','owner','scratch')):
            raise RuntimeError('mixed permission owners')
        item = row['permission']; path = Path(item['path'])
        if not path.is_absolute() or path.is_symlink() or not path.resolve(strict=True).is_relative_to(scratch):
            raise RuntimeError('permission path outside owner')
        if type(item['mode']) is not int or not 0 <= item['mode'] <= 0o777:
            raise RuntimeError('invalid permission mode')
        plan['permissions'].append(dict(path=str(path),mode=item['mode']))
    paths = [item['path'] for item in plan['permissions']]
    if len(paths) > 3 or len(paths) != len(set(paths)):
        raise RuntimeError('duplicate or excess permission plans')
    return dict(content=None,oracle=None,plan=plan,restoreArgument=json.dumps(plan))


def freeze_oracle(stdout, expected_scratch):
    """Checkpoint comes from the recorder pipe, never from a mutable file."""
    rows = [line.removeprefix('PDRS_METADATA_ORACLE ') for line in stdout.split('\n')
            if line.startswith('PDRS_METADATA_ORACLE ')]
    if len(rows) != 1:
        raise RuntimeError('one recorder memory checkpoint required')
    serialized = json.loads(rows[0])
    if not isinstance(serialized, str):
        raise RuntimeError('invalid recorder memory checkpoint')
    oracle = json.loads(serialized)
    plan = {key: oracle[key] for key in ('baselineCommit', 'owner', 'scratch')}
    if plan['baselineCommit'] != 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85':
        raise RuntimeError('checkpoint baseline mismatch')
    scratch = Path(plan['scratch']).resolve(strict=True)
    if scratch != expected_scratch.resolve(strict=True):
        raise RuntimeError('checkpoint scratch mismatch')
    if not plan['owner'] or (scratch/'.metadata-oracle-owner').read_text() != plan['owner']:
        raise RuntimeError('checkpoint owner mismatch')
    permissions = oracle['permissions']
    if len(permissions) != 3 or len({item['path'] for item in permissions}) != 3:
        raise RuntimeError('three unique checkpoint permission targets required')
    plan['permissions'] = []
    for item in permissions:
        path = Path(item['path'])
        if not path.is_absolute() or path.is_symlink() or not path.resolve(strict=True).is_relative_to(scratch):
            raise RuntimeError('checkpoint target outside owner')
        if type(item['mode']) is not int or not 0 <= item['mode'] <= 0o777:
            raise RuntimeError('invalid checkpoint mode')
        plan['permissions'].append(dict(path=str(path), mode=item['mode']))
    return dict(content=serialized.encode('utf-8'), oracle=oracle, plan=plan,
                restoreArgument=json.dumps(plan))


def unchanged_oracle(fixture, snapshot):
    if snapshot['content'] is None:
        raise RuntimeError('recording has no complete oracle checkpoint')
    if fixture.read_bytes() != snapshot['content']:
        raise RuntimeError('oracle bytes changed after recorder checkpoint')


def check_restore(fixture, oracle):
    scratch = Path(oracle['scratch']).resolve(strict=True)
    if not oracle.get('owner') or (scratch / '.metadata-oracle-owner').read_text() != oracle['owner']:
        raise RuntimeError('restore owner mismatch')
    permissions = oracle['permissions']
    if not 1 <= len(permissions) <= 3 or len({item['path'] for item in permissions}) != len(permissions):
        raise RuntimeError('one to three unique permission targets required')
    for item in permissions:
        path = Path(item['path'])
        if not path.resolve(strict=True).is_relative_to(scratch) or path.is_symlink():
            raise RuntimeError('restore target outside owner')
        if type(item['mode']) is not int or stat.S_IMODE(path.stat().st_mode) != item['mode']:
            raise RuntimeError('permission not restored: ' + str(path))
    receipt = json.loads(Path(str(fixture) + '.cleanup.json').read_text())
    if receipt.get('owner') != oracle['owner'] or type(receipt.get('restored')) is not int or receipt['restored'] != len(permissions):
        raise RuntimeError('restore receipt owner/count mismatch')
    return receipt


def reject_protected_paths(paths, home):
    for location in paths:
        for protected in (home/'.pilotdeck', home/'.cargo', home/'.rustup', home/'ws/PilotDeck-rust-program-20261009'):
            if location.resolve().is_relative_to(protected.resolve()):
                raise RuntimeError('protected output/target: ' + str(location))


def validate_output_target(target, output):
    if target.is_relative_to(output) or output.is_relative_to(target):
        raise RuntimeError('target and output must be disjoint')
    if target.exists() and not target.is_dir():
        raise RuntimeError('fresh target must be a directory')
    if target.exists() and any(target.iterdir()):
        raise RuntimeError('fresh target required; prior incident target products are not admitted')


def external_git(executable, roots):
    executable = executable.resolve(strict=True)
    for start in roots:
        start = start.resolve()
        for root in (start, *start.parents):
            if root == start or (root/'.git').exists():
                if executable.is_relative_to(root):
                    raise RuntimeError('workspace-local Git rejected: ' + str(executable))
    return executable


def toolchain_preflight(toolchain, original, root):
    """Read existing installation only. Never launch rustup, Cargo or a proxy."""
    home = Path(original['HOME'])
    if not home.is_absolute():
        raise RuntimeError('original HOME must be absolute')
    cargo_home = Path(original.get('CARGO_HOME', home / '.cargo')).resolve()
    rustup_home = Path(original.get('RUSTUP_HOME', home / '.rustup')).resolve()
    toolchain = toolchain.resolve(strict=True)
    channel = tomllib.loads((root / 'rust-toolchain.toml').read_text())['toolchain']['channel']
    if toolchain.parent != rustup_home / 'toolchains' or not toolchain.name.startswith(channel + '-'):
        raise RuntimeError('toolchain must be the existing pinned installation in original RUSTUP_HOME')
    host = toolchain.name[len(channel)+1:]
    components_file = toolchain / 'lib/rustlib/components'
    components = components_file.read_text().splitlines()
    for component in ('cargo-', 'rustc-', 'rust-std-', 'clippy-preview-', 'rustfmt-preview-'):
        if component + host not in components:
            raise RuntimeError('incomplete installed component: ' + component + host)
    if not (toolchain / 'lib/rustlib' / host / 'lib').is_dir():
        raise RuntimeError('installed host standard library missing')
    binaries = {}
    for name in ('cargo', 'rustc', 'rustdoc', 'cargo-clippy', 'clippy-driver', 'rustfmt'):
        path = toolchain / 'bin' / name
        if not path.is_file() or path.resolve().parent != toolchain / 'bin' or not os.access(path, os.X_OK):
            raise RuntimeError('missing or redirected original executable: ' + str(path))
        binaries[name] = dict(path=str(path), sha256=sha(path), mtime=path.stat().st_mtime)
    if not cargo_home.is_dir():
        raise RuntimeError('original CARGO_HOME missing')
    return dict(home=str(home), cargoHome=str(cargo_home), rustupHome=str(rustup_home),
                toolchain=str(toolchain), channel=channel, host=host, components=components,
                componentsSha256=sha(components_file), binaries=binaries)


def environments(original, preflight, scratch, target, node_executable=None, git_executable=None):
    # Build-affecting Cargo/Rust flags, proxy and wrapper aliases never flow from
    # the caller. Locale and ordinary temp settings are the only inherited keys.
    allowed = ('LANG', 'LC_ALL', 'LC_CTYPE', 'TZ', 'TMPDIR', 'SYSTEMROOT', 'WINDIR')
    cargo = {key: original[key] for key in allowed if key in original}
    cargo.update(HOME=preflight['home'], CARGO_HOME=preflight['cargoHome'], RUSTUP_HOME=preflight['rustupHome'],
                 RUSTUP_AUTO_INSTALL='0', RUSTC=preflight['binaries']['rustc']['path'],
                 RUSTDOC=preflight['binaries']['rustdoc']['path'], RUSTC_WRAPPER='', RUSTC_WORKSPACE_WRAPPER='',
                 CARGO_BUILD_RUSTC_WRAPPER='', CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='',
                 CARGO_TARGET_DIR=str(target), CARGO_BUILD_JOBS='2',
                 PATH=str(Path(preflight['toolchain'])/'bin') + os.pathsep + os.defpath)
    node = {key: original[key] for key in allowed if key in original}
    node.update(HOME=str(scratch/'isolation/home'), TMPDIR=str(scratch/'isolation/tmp'),
                PILOT_HOME=str(scratch/'isolation/home'), PILOTDECK_SKIP_BROWSER_OPEN='1', SKIP_BROWSER_OPEN='1',
                PATH=os.pathsep.join(dict.fromkeys([str(Path(value).parent) for value in
                     (node_executable, git_executable) if value] + os.defpath.split(os.pathsep))))
    for key in ('XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME'):
        node[key] = str(scratch/'isolation/xdg')
    if git_executable:
        node['PDRS_METADATA_GIT'] = str(git_executable)
    return cargo, node


def check_postflight(result, root, toolchain, original, preflight, sources_before, rustup_before,
                     node_executable, node_before, node_home, git_executable=None, git_before=None):
    result['sourcesAfter'] = source_snapshot(root)
    result['toolsAfter'] = toolchain_preflight(toolchain, original, root)
    result['rustupAfter'] = rustup_snapshot(Path(preflight['rustupHome']))
    if result['sourcesAfter'] != sources_before:
        raise RuntimeError('source changed during verification')
    if result['toolsAfter'] != {key: value for key, value in preflight.items() if key in result['toolsAfter']}:
        raise RuntimeError('original toolchain changed during verification')
    if git_executable is not None:
        result['gitAfter'] = dict(path=str(git_executable), sha256=sha(git_executable))
        if result['gitAfter']['sha256'] != git_before:
            raise RuntimeError('Git executable changed during verification')
    if result['rustupAfter'] != rustup_before or sha(node_executable) != node_before:
        raise RuntimeError('monitored rustup subtrees or Node executable changed')
    for name in ('.cargo', '.rustup'):
        if os.path.lexists(node_home/name):
            raise RuntimeError('tool cache appeared in isolated Node HOME: ' + name)


def stop_group(child):
    """Only for children created with start_new_session=True (pid == pgid)."""
    try:
        os.killpg(child.pid, signal.SIGKILL)
        return False
    except ProcessLookupError:
        return True  # legitimate exit-between-poll-and-kill race


def run_bounded(command, cwd, environment, log, *, deadline=90, limit=1024*1024, discard_stderr=False):
    start = time.monotonic()
    reason, wait_error, code, gone = None, None, None, False
    captured = bytearray()
    written = 0
    with log.open('xb') as stream:
        child = subprocess.Popen(command, cwd=cwd, env=environment, stdout=subprocess.PIPE,
                                 stderr=subprocess.DEVNULL if discard_stderr else subprocess.STDOUT,
                                 stdin=subprocess.DEVNULL, start_new_session=True)
        try:
            with selectors.DefaultSelector() as selector:
                os.set_blocking(child.stdout.fileno(), False)
                selector.register(child.stdout, selectors.EVENT_READ)
                pipe_open = True
                while pipe_open or child.poll() is None:
                    if time.monotonic()-start > deadline:
                        reason = 'timeout'
                        break
                    if not pipe_open:
                        time.sleep(0.025)
                        continue
                    for key, _ in selector.select(0.025):
                        try:
                            chunk = os.read(key.fd, min(65536, limit-written+1))
                        except BlockingIOError:
                            continue
                        if not chunk:
                            selector.unregister(child.stdout)
                            pipe_open = False
                            break
                        keep = min(len(chunk), limit-written)
                        stream.write(chunk[:keep]); captured.extend(chunk[:keep]); written += keep
                        if keep != len(chunk):
                            reason = 'output limit'
                            break
                    if reason:
                        break
            if reason:
                gone = stop_group(child)
            try:
                code = child.wait(timeout=5)
            except subprocess.TimeoutExpired as exc:
                wait_error = str(exc)
                reason = reason or 'wait timeout'
                gone = stop_group(child) or gone
                try:
                    code = child.wait(timeout=5)
                except subprocess.TimeoutExpired as second:
                    wait_error += '\n' + str(second)
        finally:
            if child.poll() is None:
                gone = stop_group(child) or gone
                try:
                    code = child.wait(timeout=5)
                except subprocess.TimeoutExpired as exc:
                    reason, wait_error = reason or 'wait timeout', str(exc)
            # The reaped leader can leave same-session descendants behind.
            # A successful group kill here proves the group was still present.
            if code is not None and reason is None:
                already_gone = stop_group(child)
                gone = gone or already_gone
                if not already_gone:
                    reason = reason or 'surviving descendants'
            # Preserve already-written permission checkpoints on timeout/kill.
            # Read only the remaining output budget plus one detection byte.
            while written <= limit:
                try:
                    chunk = os.read(child.stdout.fileno(), min(65536, limit-written+1))
                except BlockingIOError:
                    break
                if not chunk:
                    break
                keep = min(len(chunk), limit-written)
                stream.write(chunk[:keep]); captured.extend(chunk[:keep]); written += keep
                if keep != len(chunk):
                    reason = reason or 'output limit'
                    break
            child.stdout.close()
    return dict(argv=list(map(str, command)), exit=code, reason=reason, waitError=wait_error,
                groupAlreadyGone=gone, reaped=code is not None, pid=child.pid,
                seconds=round(time.monotonic()-start,3), log=str(log), sha256=sha(log),
                stdout=captured.decode('utf-8', errors='replace'))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target', type=Path, required=True, help='new independently owned Cargo target')
    parser.add_argument('--toolchain', type=Path, required=True, help='existing pinned original Rust installation')
    parser.add_argument('--preflight-only', action='store_true')
    args = parser.parse_args(argv)
    repo = Path(__file__).resolve().parents[2]
    root = repo.parent
    original = dict(os.environ)
    preflight = toolchain_preflight(args.toolchain, original, root)
    sources_before = source_snapshot(root)
    rustup_before = rustup_snapshot(Path(preflight['rustupHome']))
    node_executable = Path(shutil.which('node', path=original.get('PATH')) or '').resolve(strict=True)
    git_executable = Path(shutil.which('git', path=original.get('PATH')) or '').resolve(strict=True)
    for executable in (node_executable, git_executable):
        if not executable.is_file() or not os.access(executable, os.X_OK):
            raise RuntimeError('required original Node/Git executable missing')
    git_executable = external_git(git_executable, (root, args.baseline))
    git_before = sha(git_executable)
    node_before = sha(node_executable)
    target = args.target.resolve()
    reject_protected_paths((target, args.output), Path(preflight['home']))
    output = args.output.resolve()
    validate_output_target(target, output)
    output.mkdir(parents=True, exist_ok=False)
    scratch = output / 'recording'
    cargo_env, node_env = environments(original, preflight, scratch, target, node_executable, git_executable)
    whitelist = ('HOME', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_AUTO_INSTALL', 'RUSTC', 'RUSTDOC',
                 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER',
                 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER', 'CARGO_TARGET_DIR', 'CARGO_BUILD_JOBS', 'PATH')
    preflight['cargoEnvironment'] = {key: cargo_env[key] for key in whitelist}
    preflight['nodeHome'] = node_env['HOME']
    preflight['cargoArgvPrefix'] = [preflight['binaries']['cargo']['path']]
    preflight['sourcesBefore'] = sources_before
    preflight['rustupBefore'] = rustup_before
    preflight['nodeExecutable'] = dict(path=str(node_executable), sha256=node_before)
    preflight['gitExecutable'] = dict(path=str(git_executable), sha256=git_before)
    preflight['nodeEnvironment'] = node_env
    preflight['driverSha256'] = sha(Path(__file__).resolve())
    (output / 'preflight.json').write_text(json.dumps(preflight, indent=2)+'\n')
    if args.preflight_only:
        print(json.dumps({'preflight': str(output / 'preflight.json'), 'noProcessesStarted': True}))
        return 0
    for name in ('home', 'xdg', 'tmp'):
        (scratch / 'isolation' / name).mkdir(parents=True)
    fixture = output / 'metadata-io.json'
    recorder = repo / 'pilotdeck-gateway/tests/record_metadata_io.mts'
    runs = []
    snapshot = None

    def run(name, command, *, deadline=90, environment=None, discard_stderr=False):
        nonlocal snapshot
        if snapshot is not None and name in ('replay', 'dynamic-snapshot', 'native-retained'):
            unchanged_oracle(fixture, snapshot)
        log = output / (name + '.log')
        actual_environment = cargo_env if environment is None else environment
        result = run_bounded(command, root, actual_environment, log, deadline=deadline, discard_stderr=discard_stderr)
        stdout = result.pop('stdout')
        result['name'] = name
        result['stderrPolicy'] = 'discarded' if discard_stderr else 'merged'
        result['environment'] = dict(actual_environment)
        runs.append(result)
        if name == 'record':
            snapshot = permission_checkpoint(stdout, scratch)
            if result['exit'] == 0 and not result['reason']:
                full_snapshot = freeze_oracle(stdout, scratch)
                if snapshot is None:
                    snapshot = full_snapshot
                    raise RuntimeError('pre-chmod permission checkpoints missing')
                if snapshot['plan'] != full_snapshot['plan']:
                    raise RuntimeError('oracle and pre-chmod permission plans disagree')
                snapshot = full_snapshot
                unchanged_oracle(fixture, snapshot)
        elif name in ('replay', 'dynamic-snapshot', 'native-retained'):
            unchanged_oracle(fixture, snapshot)
        if result['exit'] != 0 or result['reason'] or not result['reaped']:
            raise RuntimeError(f'{name}: exit={result["exit"]}, reason={result["reason"]}; see {log}')
        return stdout

    success, error = False, None
    try:
        baseline = args.baseline.resolve()
        node = [str(node_executable), '--import', str(root / 'scripts/gateway-parity/watcher.mjs'), '--import',
                str(baseline / 'node_modules/tsx/dist/loader.mjs'), str(recorder)]
        run('record', node + [str(baseline), str(fixture), str(scratch)], environment=node_env)
        oracle = snapshot['oracle']
        ids = {row['id'] for row in oracle['cases']}
        if len(oracle['cases']) != 10 or len(ids) != 10 or any(row['status'] != 'RUN' for row in oracle['cases']):
            raise RuntimeError('ten unique RUN cases required; NOT_RUN never passes')
        cargo = [preflight['binaries']['cargo']['path'], 'test', '--manifest-path', str(repo / 'Cargo.toml'),
                 '-p', 'pilotdeck-gateway', '--lib', '--locked', '--offline']
        replay = run('replay', cargo + ['metadata_io', '--', '--nocapture'])
        if 'test result: ok. 3 passed; 0 failed; 2 ignored;' not in replay:
            raise RuntimeError('expected three mandatory replay tests')
        cargo_env['PDRS_METADATA_IO_LIVE'] = str(fixture)
        dynamic = run('dynamic-snapshot', cargo +
                      ['metadata::tests::metadata_numeric_dynamic_snapshot_full_rpc_matches_node',
                       '--', '--ignored', '--exact', '--nocapture'])
        if 'test result: ok. 1 passed; 0 failed; 0 ignored;' not in dynamic:
            raise RuntimeError('expected exactly one dynamic Snapshot test')
        check_receipts(dynamic, 'PDRS_NUMERIC_SNAPSHOT_OK', ids)
        native = run('native-retained', cargo + ['metadata_io_native_retained', '--', '--ignored', '--nocapture'])
        if 'test result: ok. 2 passed; 0 failed; 0 ignored;' not in native:
            raise RuntimeError('expected exactly two retained Native tests')
        for marker, text in [('PDRS_NATIVE_METADATA_OK', native), ('PDRS_METADATA_PAIR_OK', native),
                             ('PDRS_METADATA_PAIR_OK', replay)]:
            check_receipts(text, marker, ids)
        success = True
    except Exception as exc:
        error = str(exc)
    finally:
        oracle_guard = None
        if snapshot is not None:
            oracle_guard = dict(originalPermissions=snapshot['plan']['permissions'],
                                artifactUnchanged=True, completeOracle=snapshot['content'] is not None, restored=False)
            try:
                unchanged_oracle(fixture, snapshot)
            except Exception as exc:
                oracle_guard['artifactUnchanged'] = False
                success, error = False, (error or '') + '\noracle: ' + str(exc)
            try:
                run('restore', node + ['--restore', str(fixture), snapshot['restoreArgument']],
                    deadline=15, environment=node_env)
            except Exception as exc:
                success, error = False, (error or '') + '\ncleanup: ' + str(exc)
            try:
                oracle_guard['receipt'] = check_restore(fixture, snapshot['plan'])
                oracle_guard['restored'] = True
            except Exception as exc:
                success, error = False, (error or '') + '\nrestore-check: ' + str(exc)
        postflight = {}
        try:
            check_postflight(postflight, root, args.toolchain, original, preflight, sources_before,
                             rustup_before, node_executable, node_before, Path(node_env['HOME']), git_executable, git_before)
        except Exception as exc:
            success, error = False, (error or '') + '\npostflight: ' + str(exc)
        source_head = None
        try:
            source_head = run('source-head', [str(git_executable), '--no-replace-objects',
                                             'rev-parse', 'HEAD'], deadline=10, discard_stderr=True).strip()
            if not re.fullmatch('[0-9a-f]{40}', source_head):
                raise RuntimeError('source HEAD must be exactly forty lowercase hex digits')
        except Exception as exc:
            source_head = None
            success, error = False, (error or '') + '\nsource-head: ' + str(exc)
        if snapshot is not None and snapshot['content'] is not None:
            try:
                unchanged_oracle(fixture, snapshot)
            except Exception as exc:
                oracle_guard['artifactUnchanged'] = False
                success, error = False, (error or '') + '\nfinal-oracle: ' + str(exc)
        manifest = dict(success=success, error=error, runs=runs, preflight=preflight, postflight=postflight,
                        sourceHead=source_head, sources=sources_before, oracleGuard=oracle_guard)
        (output / 'verification.json').write_text(json.dumps(manifest, indent=2)+'\n')
    print(json.dumps({'success': success, 'error': error, 'output': str(output)}))
    return 0 if success else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(json.dumps({'success': False, 'preflightError': str(exc)}))
        raise SystemExit(1)
