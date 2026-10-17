#!/usr/bin/env python3
"""Round 2 no-listen launcher; reuse the established isolation before startup."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('a52b_launcher', ROOT / 'scripts/a5-19/run-b.py')
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, help='baseline checkout (default: PDRS_BASELINE or this checkout resources/PilotDeck)')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.baseline = Path(args.baseline or os.environ.get('PDRS_BASELINE') or ROOT / 'resources/PilotDeck').resolve()
    if not (args.baseline / 'src/gateway/client/InProcessGateway.ts').is_file():
        parser.error(f'baseline not found at {args.baseline}; pass --baseline or set PDRS_BASELINE')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    node = shutil.which('node')
    if not node:
        parser.error('Node 22 must exist; no download fallback')
    env = launcher.isolated_environment(output)
    path = 'scripts/a5-19/preregistration-b-production.mjs'
    registration = ROOT / path
    committed = subprocess.run(['git', 'show', f'HEAD:{path}'], cwd=ROOT, capture_output=True)
    matching = committed.returncode == 0 and committed.stdout == registration.read_bytes()
    status = {'status': 'COMMITTED_BEFORE_COLLECTION' if matching else 'WORKTREE_REGISTERED_NOT_COMMITTED',
              'headCommit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'path': path, 'sha256': hashlib.sha256(registration.read_bytes()).hexdigest()}
    env.update(A52B_BASELINE=str(args.baseline.resolve()), A52B_OUTPUT=str(output / 'raw'),
               A52B_PRODUCTION_REGISTRATION=json.dumps(status))
    command = [node, '--experimental-transform-types', str(ROOT / 'scripts/a5-19/production-close-b.mts')]
    audit = {'launcherPid': os.getpid(), 'environment': env, 'command': command,
             'registration': status, 'networkListeners': [], 'downloads': [], 'gatewayRestart': False}
    with (output / 'run.log').open('w') as log:
        child = subprocess.Popen(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
        audit['childPid'] = child.pid
        try:
            code = child.wait(timeout=300)
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
