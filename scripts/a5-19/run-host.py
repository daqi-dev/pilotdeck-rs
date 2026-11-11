#!/usr/bin/env python3
"""Capture one original #1314 scene at k5 with an actual TS host and sidecar."""
import sys
if not sys.flags.isolated:
    raise SystemExit('use python3 -I')
sys.dont_write_bytecode = True

import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import threading

ROOT = Path(__file__).resolve().parents[2]
BASELINE = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
REPEATS = 5


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def ref(path):
    path = Path(path).resolve(strict=True)
    return {'path': str(path), 'sha256': sha(path)}


def save(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def read(path):
    return json.loads(Path(path).read_text())


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    source = Path(path).read_bytes()
    exec(compile(source, str(path), 'exec'), module.__dict__)
    module._apparatus_source = source
    return module


def git(path, *args):
    return subprocess.check_output(['git', '-C', str(path), *args], timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ('baseline', 'node', 'source-manifest', 'binary-manifest', 'out'):
        parser.add_argument('--' + flag, type=Path, required=True)
    parser.add_argument('--sidecar', choices=('ts', 'rust'), required=True)
    parser.add_argument('--case', required=True)
    parser.add_argument('--lease-wait', type=float, default=10)
    args = parser.parse_args()
    root = ROOT
    out, node, baseline = args.out.resolve(), args.node.resolve(strict=True), args.baseline.resolve(strict=True)
    out.mkdir(exist_ok=False, parents=True)
    source_pin, binary_pin = ref(args.source_manifest), ref(args.binary_manifest)
    source = read(source_pin['path'])
    head = git(root, 'rev-parse', 'HEAD').decode().strip()
    assert source['head'] == head and not git(root, 'status', '--porcelain', '--untracked-files=no').strip()
    for row in source['files']:
        relative = Path(row['path'])
        assert not relative.is_absolute() and '..' not in relative.parts and sha(root / relative) == row['sha256']
    programs = read(binary_pin['path'])
    programs = programs if isinstance(programs, list) else programs['binaries']
    assert len(programs) == 4 and {row['path'] for row in programs} == {
        'pilotdeck', 'pilotdeck-control', 'pilotdeck-gateway', 'pilotdeck-agent-loop-sidecar'}
    binary_dir = Path(binary_pin['path']).parent
    for row in programs:
        assert sha(binary_dir / row['path']) == row['sha256']
    assert git(baseline, 'rev-parse', 'HEAD').decode().strip() == BASELINE
    assert not git(baseline, 'status', '--porcelain', '--untracked-files=no').strip()
    isolated = {'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'NODE_DISABLE_COMPILE_CACHE': '1'}
    isolation = out / 'isolation'
    for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'TMPDIR', 'PILOT_HOME'):
        directory = isolation / key.lower()
        directory.mkdir(parents=True)
        isolated[key] = str(directory)
    versions = json.loads(subprocess.check_output([str(node), '-p', 'JSON.stringify(process.versions)'], env=isolated, timeout=5))
    assert [versions[key] for key in ('node', 'icu', 'uv')] == ['22.22.0', '78.2', '1.52.1']
    registration = read(root / 'scripts/gateway-parity/fixtures/session-calibration.json')
    assert len(registration['cases']) == 72 and args.case in registration['cases']
    native = load('host_fixture_native_collector', root / 'scripts/native-witness/collect.py')
    witness = native.w
    session = witness.frozen_auditor()
    suite = session._load('host_suite', session.SCRIPT / 'host_suite.py')
    session._verify_cli_imports()
    paths = {row['path'] for row in source['files']}
    assert len(paths) == len(source['files']) and paths == suite.registered_source_paths(root, head)
    expectation = read(root / 'scripts/gateway-parity/fixtures/session-expectations.json')
    require_provider = args.case in witness.native.required_provider_cases(expectation)
    fixture = root / 'scripts/a5-19'
    adapter, harness = fixture / 'host-sidecar-adapter.mjs', fixture / 'six_cells.mts'
    if args.sidecar == 'rust':
        command, argv = str((binary_dir / 'pilotdeck-agent-loop-sidecar').resolve(strict=True)), []
    else:
        command, argv = str(node), ['--import', str(fixture / 'host-sidecar-source-loader.mjs'),
                                    str(baseline / 'src/cli/pilotdeck-agent-loop-sidecar.ts')]
    env = dict(isolated, A52_ISOLATION=str(isolation), A52_BASELINE=str(baseline),
               A52_OUTPUT=str(out / 'raw'), A52_TMP=str(isolation / 'measurements'),
               PILOTDECK_SKIP_BROWSER_OPEN='1', SKIP_BROWSER_OPEN='1', UV_USE_IO_URING='0',
               A19_HOST_ADAPTER=str(adapter), A19_HOST_BASELINE=str(baseline),
               A19_HOST_SIDECAR_SPEC=str(out / 'spec.json'), A19_HOST_CASE=args.case)
    originals = {name: ref(fixture / name) for name in (
        'six_cells.mts', 'host-sidecar-adapter.mjs', 'host-sidecar-source-loader.mjs', 'host-sidecar-stimulus.mjs')}
    originals['run-host.py'] = ref(__file__)
    for name, original in originals.items():
        (out / name).write_bytes(Path(original['path']).read_bytes())
    git_bin = Path('/usr/bin/git').resolve(strict=True)
    git_sha = sha(git_bin)
    contract = {'host': 'ts', 'sidecarLanguage': args.sidecar, 'project': str(out), 'externalEndpoints': [],
                'fixtureSources': originals,
                'binaries': {'host': witness.identity(node, [str(node), '--experimental-transform-types', str(harness)]),
                             'sidecar': witness.identity(command, [command, *argv]),
                             'baselineGitProbe': {'path': str(git_bin), 'argv': ['git', '-C', str(baseline), 'rev-parse', 'HEAD'], 'sha256': git_sha},
                             'worktreeGitProbe': {'path': str(git_bin), 'argv': ['git', '-C', str(root), 'rev-parse', 'HEAD'], 'sha256': git_sha}}}
    requests, server, worker = [], None, None
    endpoint = None

    def stop_provider():
        nonlocal server
        if server is not None:
            with session.common.deferred_signals():
                if worker is not None and worker.is_alive():
                    server.shutdown()
                    worker.join(timeout=6)
                    assert not worker.is_alive(), 'owned provider did not stop'
                server.server_close()
                server = None
                session.common.LIVE_GROUPS.discard('ts-host-fixture-provider')

    with session.common.verified_parity_lock(timeout=args.lease_wait):
        try:
            if require_provider:
                with session.common.deferred_signals():
                    server = native.BoundedHTTPServer(('127.0.0.1', 0), native.fixture_provider_handler(requests))
                    session.common.LIVE_GROUPS.add('ts-host-fixture-provider')
                    endpoint = f'127.0.0.1:{server.server_port}'
                    worker = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.05}, daemon=True)
                    worker.start()
                contract['externalEndpoints'] = [endpoint]
            save(out / 'spec.json', {'language': args.sidecar, 'command': command, 'args': argv,
                                    'requireProvider': require_provider,
                                    'providerEndpoint': f'http://{endpoint}/v1' if endpoint else None})
            save(out / 'preflight.json', {'head': head, 'sourceManifest': source_pin, 'binaryManifest': binary_pin,
                 'sourceFilesVerified': len(source['files']),
                 'originals': {name: pin for name, pin in originals.items() if name != 'run-host.py'},
                 'producer': originals['run-host.py'],
                 'controlledEnvironment': {'UV_USE_IO_URING': '0',
                     'seccomp': next(line for line in Path('/proc/self/status').read_text().splitlines() if line.startswith('Seccomp:'))},
                 'case': args.case, 'repeats': REPEATS, 'requireProvider': require_provider,
                 'networkNamespace': os.readlink('/proc/self/ns/net')})
            witness.capture(contract, out / 'capture', env, timeout=60,
                            validate=lambda _launch, _evidence: stop_provider(), ownership=session.common.LIVE_GROUPS)
        finally:
            stop_provider()
            raw_folder = out / 'raw'
            raw_folder.mkdir(exist_ok=True)
            save(raw_folder / 'provider.json', {'endpoint': endpoint, 'requests': requests})
        raw, launch, evidence = witness.load_capture(out / 'capture')
        checked = witness.checked_evidence(raw, launch, evidence)
        assert checked['cleanup']['allStopped'] is True
        assert all(checked[key]['complete'] is True for key in ('execWitness', 'networkWitness', 'transcriptWitness'))
        assert Counter(process['role'] for process in checked['processes']) == Counter(
            {'host': 1, 'sidecar': REPEATS, 'baselineGitProbe': 1, 'worktreeGitProbe': 1})
        host_pid = next(process['pid'] for process in checked['processes'] if process['role'] == 'host')
        calls = checked['networkWitness']['calls']
        if require_provider:
            provider_summary = suite.provider_request_summary(witness.native, requests, args.case, REPEATS)
            assert calls and all(call['target'] == endpoint and call['ownerPid'] == host_pid for call in calls)
        else:
            assert not calls and not requests
            provider_summary = {'modelStreams': 0, 'titles': 0, 'totalRequests': 0}
        assert all(ref(original['path']) == original for original in originals.values())
        summary = read(raw_folder / 'manifest.json')
        assert summary['executedCases'] == [args.case] and len(summary['expectedCases']) == 72
        assert summary['totalMeasurements'] == REPEATS and len(summary['missingCases']) == 71
        for source_row in summary['importedSources']:
            original = git(baseline, 'show', BASELINE + ':' + source_row['path'])
            assert hashlib.sha256(original).hexdigest() == source_row['sha256'] == sha(baseline / source_row['path'])
        rows = read(raw_folder / f"{args.case.split('/')[0]}.json")['observations']
        assert [row['id'] for row in rows] == [f'{args.case}/{k}' for k in range(1, REPEATS + 1)]
        transcript_opens = {row['path'] for row in checked['transcriptWitness']['opens']}
        for observation in rows:
            transcript = (root / observation['durableStatus']['source']).resolve()
            assert transcript.is_relative_to(out) and transcript.read_text() == observation['transcriptRawText']
            assert str(transcript) in transcript_opens
            actual = {key: observation[key] for key in ('frames', 'durableStatus', 'registry')}
            assert session._native_observation_structure(actual) is None
        result = {'status': 'HOST_CAPTURE_PARTITION', 'case': args.case, 'repeats': REPEATS,
                  'host': 'ts', 'sidecar': args.sidecar, 'receipt': ref(out / 'capture/receipt.json'),
                  'rawSha256': {path.name: sha(path) for path in raw_folder.glob('*.json')},
                  'preflight': ref(out / 'preflight.json'), 'providerEvidence': provider_summary,
                  'expectedWholeHostCases': 72, 'wholeHost': 'NOT_RUN'}
        save(out / 'host-capture.json', result)
    print(json.dumps(result, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
