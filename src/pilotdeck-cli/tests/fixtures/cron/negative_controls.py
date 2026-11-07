#!/usr/bin/env python3
"""Run three bounded, isolated cron mutants on a clean committed tree.

Caller must reserve 127.0.0.1:18789 and a cargo slot. Each archive has its own
CARGO_TARGET_DIR. A build error, timeout or unrelated panic is not a kill.
"""
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[5]
SOURCE = Path('src/pilotdeck-cli/src/cron.rs')
MUTANTS = {
    'retained-socket': ('            close_socket(&mut ws).await;\n            return Ok(0);',
                        '            std::future::pending::<()>().await;\n            close_socket(&mut ws).await;\n            return Ok(0);',
                        'connection retained after terminal outcome'),
    'no-request': ('    let id = uuid::Uuid::new_v4().to_string();',
                   '    std::future::pending::<()>().await;\n    let id = uuid::Uuid::new_v4().to_string();',
                   'WS frame timed out'),
    'wrong-method': ('Ok(("cron_list", params.into()))',
                     'Ok(("cron_typo", params.into()))',
                     'cron method differs from TS fixture'),
}


def run(cmd, cwd, env, log):
    with log.open('xb') as output:
        proc = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            return proc.wait(timeout=900)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
            raise RuntimeError(f'timed out: {cmd}')


def main():
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative_controls.py <new-output-dir>')
    if subprocess.run(['git', '-C', str(ROOT), 'diff', '--quiet', 'HEAD']).returncode or subprocess.run(['git', '-C', str(ROOT), 'diff', '--cached', '--quiet', 'HEAD']).returncode:
        raise SystemExit('tracked files differ from HEAD; commit first')
    out = Path(sys.argv[1]).resolve()
    out.mkdir()  # exclusive
    sha = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip()
    archive = subprocess.check_output(['git', '-C', str(ROOT), 'archive', '--format=tar', sha])
    results = {}
    for name in ('positive', *MUTANTS):
        tree = out / name
        tree.mkdir()
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(tree, filter='data')
        path = tree / SOURCE
        text = path.read_text()
        if name != 'positive':
            old, new, marker = MUTANTS[name]
            if text.count(old) != 1:
                raise RuntimeError(f'{name}: mutation anchor count is {text.count(old)}, expected 1')
            path.write_text(text.replace(old, new, 1))
        env = os.environ.copy()
        env['CARGO_TARGET_DIR'] = str(tree / '_build' / 'cargo-target')
        command = ['cargo', 'test', '-p', 'pilotdeck-cli', '--test', 'cron_exit']
        if run([*command, '--no-run'], tree / 'src', env, out / f'{name}-build.log') != 0:
            raise RuntimeError(f'{name}: mutant failed to compile; not a kill')
        rc = run([*command, '--', '--nocapture'], tree / 'src', env, out / f'{name}-test.log')
        transcript = (out / f'{name}-test.log').read_text(errors='replace')
        if name == 'positive':
            if rc != 0:
                raise RuntimeError('positive control failed')
        elif rc == 0 or marker not in transcript:
            raise RuntimeError(f'{name}: expected bound assertion not observed (rc={rc})')
        results[name] = {'exit_code': rc, 'assertion': None if name == 'positive' else marker}
    (out / 'result.json').write_text(json.dumps({'head': sha, 'results': results}, indent=2) + '\n')
    print(json.dumps(results))


if __name__ == '__main__':
    main()
