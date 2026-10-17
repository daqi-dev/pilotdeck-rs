#!/usr/bin/env python3
"""A5.2a TS-only 六格入口；在 Node 启动前隔离，所有原始证据留在 output。"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, help='baseline checkout (default: PDRS_BASELINE or this checkout resources/PilotDeck)')
    parser.add_argument('--output', type=Path, default=ROOT / '_tmp/a52/fixtures')
    args = parser.parse_args()
    args.baseline = Path(args.baseline or os.environ.get('PDRS_BASELINE') or ROOT / 'resources/PilotDeck').resolve()
    if not (args.baseline / 'src/gateway/client/InProcessGateway.ts').is_file():
        parser.error(f'baseline not found at {args.baseline}; pass --baseline or set PDRS_BASELINE')
    output = args.output.resolve()
    # Each invocation retains its own raw files; never overwrite prior evidence.
    output.mkdir(parents=True, exist_ok=False)
    isolation = Path(tempfile.mkdtemp(prefix='isolation-', dir=output))
    node = shutil.which('node')
    if not node:
        parser.error('Node 22 must already be installed; no download fallback')
    env = {k: v for k, v in os.environ.items() if k in ('PATH', 'LANG', 'LC_ALL', 'SYSTEMROOT')}
    for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'TMPDIR', 'PILOT_HOME'):
        folder = isolation / key.lower()
        folder.mkdir()
        env[key] = str(folder)
    env.update(PILOTDECK_SKIP_BROWSER_OPEN='1', SKIP_BROWSER_OPEN='1', NODE_DISABLE_COMPILE_CACHE='1',
               A52_ISOLATION=str(isolation), A52_BASELINE=str(args.baseline.resolve()),
               A52_OUTPUT=str(output / 'raw'), A52_TMP=str(isolation / 'measurements'))
    command = [node, str(ROOT / 'scripts/a5-19/six_cells.mts')]
    audit = {'command': command, 'cwd': str(ROOT), 'environment': env, 'launcherPid': os.getpid(),
             'networkListeners': [], 'downloads': [], 'repeats': 5, 'e1Integration': 'NOT_IMPLEMENTED'}
    (output / 'launch.json').write_text(json.dumps(audit, ensure_ascii=False, indent=2) + '\n')
    with (output / 'run.log').open('w') as log:
        child = subprocess.Popen(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
        audit['childPid'] = child.pid
        try:
            code = child.wait(timeout=1200)
        except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
            # Only this launcher-owned child; no port lookup, pkill or user processes.
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
            audit['interruption'] = type(error).__name__
            code = 2
    audit['exitCode'] = code
    audit['childReaped'] = True
    (output / 'launch.json').write_text(json.dumps(audit, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'exitCode': code, 'evidence': str(output)}, ensure_ascii=False))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
