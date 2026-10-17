#!/usr/bin/env python3
"""A5.2b isolated no-listen fixtures. Never downloads, restarts gateways or kills by port."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def registration_status():
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    files = []
    for name in ('preregistration-b.mjs', 'preregistration-b-extra.mjs', 'rules-b.mjs'):
        path = f'scripts/a5-19/{name}'
        result = subprocess.run(['git', 'show', f'HEAD:{path}'], cwd=ROOT, capture_output=True)
        files.append({'path': path, 'committedBytesMatch': result.returncode == 0 and result.stdout == (ROOT / path).read_bytes()})
    return {'status': 'COMMITTED_BEFORE_COLLECTION' if all(f['committedBytesMatch'] for f in files) else 'NOT_COMMITTED',
            'headCommit': head, 'files': files}


def isolated_environment(output):
    isolation = Path(tempfile.mkdtemp(prefix='isolation-', dir=output))
    env = {k: v for k, v in os.environ.items() if k in ('PATH', 'LANG', 'LC_ALL', 'SYSTEMROOT')}
    for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'TMPDIR', 'PILOT_HOME'):
        folder = isolation / key.lower()
        folder.mkdir()
        env[key] = str(folder)
    env.update(PILOTDECK_SKIP_BROWSER_OPEN='1', SKIP_BROWSER_OPEN='1', NODE_DISABLE_COMPILE_CACHE='1',
               A52B_ISOLATION=str(isolation), A52B_PREREGISTRATION_STATUS=json.dumps(registration_status()))
    return env


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, help='baseline checkout (default: PDRS_BASELINE or this checkout resources/PilotDeck)')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--sidecar', action='store_true', help='only stdio sidecar G cross-check; no listen')
    args = parser.parse_args()
    args.baseline = Path(args.baseline or os.environ.get('PDRS_BASELINE') or ROOT / 'resources/PilotDeck').resolve()
    # 按本入口实际用到的产物准入：fixtures 读 TS 源码，--sidecar 起已构建的 sidecar（sidecar-b.mts:57）
    required = ['src/gateway/client/InProcessGateway.ts']
    if args.sidecar:
        required.append('dist/src/cli/pilotdeck-agent-loop-sidecar.js')
    missing = [name for name in required if not (args.baseline / name).is_file()]
    if missing:
        parser.error(f'baseline at {args.baseline} lacks {missing}; pass --baseline or set PDRS_BASELINE')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    node = shutil.which('node')
    if not node:
        parser.error('Node 22 must be installed; no download fallback')
    env = isolated_environment(output)
    env.update(A52B_BASELINE=str(args.baseline.resolve()), A52B_OUTPUT=str(output / 'raw'))
    prereg = ROOT / 'scripts/a5-19/preregistration-b.mjs'
    audit = {'launcherPid': os.getpid(), 'environment': env, 'repeats': 5,
             'preregistrationSha256': hashlib.sha256(prereg.read_bytes()).hexdigest(),
             'preregistrationCommit': json.loads(env['A52B_PREREGISTRATION_STATUS']),
             'gatewayRestart': False, 'networkListeners': [], 'downloads': []}
    command = [node, '--experimental-transform-types', str(ROOT / 'scripts/a5-19' / ('sidecar-b.mts' if args.sidecar else 'fixtures-b.mts'))]
    audit['command'] = command
    (output / 'launch.json').write_text(json.dumps(audit, ensure_ascii=False, indent=2) + '\n')
    with (output / 'run.log').open('w') as log:
        child = subprocess.Popen(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
        audit['childPid'] = child.pid
        try:
            code = child.wait(timeout=900)
        except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
            audit['interruption'] = type(error).__name__
            code = 2
    audit.update(exitCode=code, childReaped=True)
    (output / 'launch.json').write_text(json.dumps(audit, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'exitCode': code, 'evidence': str(output)}, ensure_ascii=False))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
