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
    '.github/workflows/ci.yml', 'rust-toolchain.toml', 'src/Cargo.toml', 'src/Cargo.lock',
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


def check_restore(fixture):
    oracle = json.loads(fixture.read_text())
    scratch = Path(oracle['scratch']).resolve(strict=True)
    if not oracle.get('owner') or (scratch / '.metadata-oracle-owner').read_text() != oracle['owner']:
        raise RuntimeError('restore owner mismatch')
    permissions = oracle['permissions']
    receipt = json.loads(Path(str(fixture) + '.cleanup.json').read_text())
    if len(permissions) != 3 or len({item['path'] for item in permissions}) != 3:
        raise RuntimeError('three unique permission targets required')
    if receipt.get('owner') != oracle['owner'] or type(receipt.get('restored')) is not int or receipt['restored'] != 3:
        raise RuntimeError('restore receipt owner/count mismatch')
    for item in permissions:
        path = Path(item['path'])
        if not path.resolve(strict=True).is_relative_to(scratch) or path.is_symlink():
            raise RuntimeError('restore target outside owner')
        if type(item['mode']) is not int or stat.S_IMODE(path.stat().st_mode) != item['mode']:
            raise RuntimeError('permission not restored: ' + str(path))
    return receipt


def reject_protected_paths(paths, home):
    for location in paths:
        for protected in (home/'.pilotdeck', home/'.cargo', home/'.rustup', home/'ws/PilotDeck-rust-program-20261009'):
            if location.resolve().is_relative_to(protected.resolve()):
                raise RuntimeError('protected output/target: ' + str(location))


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
    return cargo, node


def check_postflight(result, root, toolchain, original, preflight, sources_before, rustup_before,
                     node_executable, node_before, node_home):
    result['sourcesAfter'] = source_snapshot(root)
    result['toolsAfter'] = toolchain_preflight(toolchain, original, root)
    result['rustupAfter'] = rustup_snapshot(Path(preflight['rustupHome']))
    if result['sourcesAfter'] != sources_before:
        raise RuntimeError('source changed during verification')
    if result['toolsAfter'] != {key: value for key, value in preflight.items() if key in result['toolsAfter']}:
        raise RuntimeError('original toolchain changed during verification')
    if result['rustupAfter'] != rustup_before or sha(node_executable) != node_before:
        raise RuntimeError('monitored rustup subtrees or Node executable changed')
    for name in ('.cargo', '.rustup'):
        if os.path.lexists(node_home/name):
            raise RuntimeError('tool cache appeared in isolated Node HOME: ' + name)


def stop_group(child):
    try:
        os.killpg(child.pid, signal.SIGKILL)
        return False
    except ProcessLookupError:
        return True  # legitimate exit-between-poll-and-kill race


def run_bounded(command, cwd, environment, log, *, deadline=90, limit=1024*1024):
    start = time.monotonic()
    reason, wait_error, code, gone = None, None, None, False
    with log.open('xb') as stream:
        child = subprocess.Popen(command, cwd=cwd, env=environment, stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
        try:
            with selectors.DefaultSelector() as selector:
                os.set_blocking(child.stdout.fileno(), False)
                selector.register(child.stdout, selectors.EVENT_READ)
                pipe_open, written = True, 0
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
                        stream.write(chunk[:keep]); written += keep
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
            child.stdout.close()
            if child.poll() is None:
                gone = stop_group(child) or gone
                try:
                    code = child.wait(timeout=5)
                except subprocess.TimeoutExpired as exc:
                    reason, wait_error = reason or 'wait timeout', str(exc)
    return dict(argv=list(map(str, command)), exit=code, reason=reason, waitError=wait_error,
                groupAlreadyGone=gone, reaped=code is not None, pid=child.pid,
                seconds=round(time.monotonic()-start,3), log=str(log), sha256=sha(log))


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
    node_before = sha(node_executable)
    target = args.target.resolve()
    reject_protected_paths((target, args.output), Path(preflight['home']))
    if target.exists() and any(target.iterdir()):
        raise RuntimeError('fresh target required; prior incident target products are not admitted')
    output = args.output.resolve()
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

    def run(name, command, *, deadline=90, environment=None):
        log = output / (name + '.log')
        result = run_bounded(command, root, environment or cargo_env, log, deadline=deadline)
        result['name'] = name
        runs.append(result)
        if result['exit'] != 0 or result['reason'] or not result['reaped']:
            raise RuntimeError(f'{name}: exit={result["exit"]}, reason={result["reason"]}; see {log}')
        return log.read_text()

    success, error = False, None
    try:
        baseline = args.baseline.resolve()
        node = [str(node_executable), '--import', str(root / 'scripts/gateway-parity/watcher.mjs'), '--import',
                str(baseline / 'node_modules/tsx/dist/loader.mjs'), str(recorder)]
        run('record', node + [str(baseline), str(fixture), str(scratch)], environment=node_env)
        oracle = json.loads(fixture.read_text())
        ids = {row['id'] for row in oracle['cases']}
        if len(oracle['cases']) != 10 or len(ids) != 10 or any(row['status'] != 'RUN' for row in oracle['cases']):
            raise RuntimeError('ten unique RUN cases required; NOT_RUN never passes')
        cargo = [preflight['binaries']['cargo']['path'], 'test', '--manifest-path', str(repo / 'Cargo.toml'),
                 '-p', 'pilotdeck-gateway', '--lib', '--locked', '--offline']
        replay = run('replay', cargo + ['metadata_io', '--', '--nocapture'])
        if 'test result: ok. 3 passed; 0 failed; 2 ignored;' not in replay:
            raise RuntimeError('expected three mandatory replay tests')
        cargo_env['PDRS_METADATA_IO_LIVE'] = str(fixture)
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
        if fixture.exists():
            try:
                run('restore', node + ['--restore', str(fixture)], deadline=15, environment=node_env)
                check_restore(fixture)
            except Exception as exc:
                success, error = False, (error or '') + '\ncleanup: ' + str(exc)
        postflight = {}
        try:
            check_postflight(postflight, root, args.toolchain, original, preflight, sources_before,
                             rustup_before, node_executable, node_before, Path(node_env['HOME']))
        except Exception as exc:
            success, error = False, (error or '') + '\npostflight: ' + str(exc)
        manifest = dict(success=success, error=error, runs=runs, preflight=preflight, postflight=postflight,
                        sourceHead=subprocess.check_output([str(git_executable), 'rev-parse', 'HEAD'], cwd=root,
                                                          env=cargo_env, text=True).strip(), sources=sources_before)
        (output / 'verification.json').write_text(json.dumps(manifest, indent=2)+'\n')
    print(json.dumps({'success': success, 'error': error, 'output': str(output)}))
    return 0 if success else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(json.dumps({'success': False, 'preflightError': str(exc)}))
        raise SystemExit(1)
