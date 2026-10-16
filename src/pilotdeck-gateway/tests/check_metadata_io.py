#!/usr/bin/env python3
"""Re-record default Node metadata errors, verify retained Native Rust, restore."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time
import tomllib


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


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


def environments(original, preflight, scratch, target):
    cargo = dict(original, HOME=preflight['home'], CARGO_HOME=preflight['cargoHome'],
                 RUSTUP_HOME=preflight['rustupHome'], RUSTUP_AUTO_INSTALL='0',
                 RUSTC=preflight['binaries']['rustc']['path'], RUSTDOC=preflight['binaries']['rustdoc']['path'],
                 RUSTC_WRAPPER='', RUSTC_WORKSPACE_WRAPPER='',
                 CARGO_BUILD_RUSTC_WRAPPER='', CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='',
                 CARGO_TARGET_DIR=str(target), CARGO_BUILD_JOBS='2',
                 PATH=str(Path(preflight['toolchain']) / 'bin') + os.pathsep + original.get('PATH', ''))
    # Removing a wrapper variable exposes Cargo config or CARGO_BUILD_* again.
    # Explicit empty values override both sources and disable wrapper execution.
    for key in ('RUSTUP_TOOLCHAIN', 'PDRS_METADATA_IO_LIVE'):
        cargo.pop(key, None)
    node = dict(cargo, HOME=str(scratch / 'isolation/home'), TMPDIR=str(scratch / 'isolation/tmp'),
                PILOT_HOME=str(scratch / 'isolation/home'), PILOTDECK_SKIP_BROWSER_OPEN='1', SKIP_BROWSER_OPEN='1')
    for key in ('XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME'):
        node[key] = str(scratch / 'isolation/xdg')
    return cargo, node


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
    target = args.target.resolve()
    if target.exists() and any(target.iterdir()):
        raise RuntimeError('fresh target required; prior incident target products are not admitted')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    scratch = output / 'recording'
    cargo_env, node_env = environments(original, preflight, scratch, target)
    whitelist = ('HOME', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_AUTO_INSTALL', 'RUSTC', 'RUSTDOC',
                 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER',
                 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER', 'CARGO_TARGET_DIR', 'CARGO_BUILD_JOBS', 'PATH')
    preflight['cargoEnvironment'] = {key: cargo_env[key] for key in whitelist}
    preflight['nodeHome'] = node_env['HOME']
    preflight['cargoArgvPrefix'] = [preflight['binaries']['cargo']['path']]
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
        start = time.monotonic()
        reason = None
        with log.open('wb') as stream:
            child = subprocess.Popen(command, cwd=root, env=environment or cargo_env, stdout=stream,
                                     stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
            try:
                while child.poll() is None:
                    if time.monotonic() - start > deadline or log.stat().st_size > 1024 * 1024:
                        reason = 'timeout' if time.monotonic() - start > deadline else 'output limit'
                        os.killpg(child.pid, signal.SIGKILL)
                        break
                    time.sleep(0.025)
                code = child.wait(timeout=5)
            finally:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=5)
        data = log.read_bytes()
        if len(data) > 1024 * 1024:
            reason = 'output limit'
        runs.append(dict(name=name, argv=list(map(str, command)), exit=code, reason=reason,
                         seconds=round(time.monotonic()-start, 3), log=str(log), sha256=hashlib.sha256(data).hexdigest()))
        if code != 0 or reason:
            raise RuntimeError(f'{name}: exit={code}, reason={reason}; see {log}')
        return data.decode()

    success, error = False, None
    try:
        baseline = args.baseline.resolve()
        node = ['node', '--import', str(root / 'scripts/gateway-parity/watcher.mjs'), '--import',
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
            receipts = re.findall(r'\b' + marker + r' (\S+)', text)
            if len(receipts) != 10 or set(receipts) != ids:
                raise RuntimeError(f'{marker}: expected ten unique actual-case receipts')
        success = True
    except Exception as exc:
        error = str(exc)
    finally:
        if fixture.exists():
            try:
                run('restore', ['node', str(recorder), '--restore', str(fixture)], deadline=15, environment=node_env)
            except Exception as exc:
                success, error = False, (error or '') + '\ncleanup: ' + str(exc)
        sources = ['pilotdeck-gateway/src/metadata.rs', 'pilotdeck-gateway/src/metadata/tests.rs',
                   'pilotdeck-gateway/src/pairing/tests.rs', 'pilotdeck-gateway/tests/record_metadata_io.mts',
                   'pilotdeck-gateway/tests/check_metadata_io.py']
        manifest = dict(success=success, error=error, runs=runs, preflight=preflight,
                        sourceHead=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(),
                        sources={name: sha(repo/name) if (repo/name).is_file() else None for name in sources})
        (output / 'verification.json').write_text(json.dumps(manifest, indent=2)+'\n')
    print(json.dumps({'success': success, 'error': error, 'output': str(output)}))
    return 0 if success else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(json.dumps({'success': False, 'preflightError': str(exc)}))
        raise SystemExit(1)
