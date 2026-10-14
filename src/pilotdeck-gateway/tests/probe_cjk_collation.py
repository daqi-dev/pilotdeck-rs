#!/usr/bin/env python3
"""Record fresh-process Node oracles with mandatory HOME/XDG/TMPDIR isolation.

Usage: python3 tests/probe_cjk_collation.py BASELINE NEW_OUTPUT_DIRECTORY
No listener, network, package installation, or collation data download.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

baseline = Path(sys.argv[1]).resolve()
output = Path(sys.argv[2]).resolve()
output.mkdir(parents=True, exist_ok=False)
for name in ('home', 'xdg', 'tmp'):
    (output / name).mkdir()
env = {k: v for k, v in os.environ.items() if not k.startswith('LC_') and k not in ('LANG', 'PILOT_HOME', 'PILOTDECK_CONFIG_PATH', 'PILOT_AGENT_MODEL')}
env.update(HOME=str(output / 'home'), XDG_CONFIG_HOME=str(output / 'xdg'),
           XDG_CACHE_HOME=str(output / 'xdg'), XDG_DATA_HOME=str(output / 'xdg'),
           TMPDIR=str(output / 'tmp'), SKIP_BROWSER_OPEN='1', PILOTDECK_SKIP_BROWSER_OPEN='1')
matrix = [{'LANG': lang} for lang in ('C', 'en_US.UTF-8', 'zh_CN.UTF-8', 'zh_TW', 'ja_JP')]
matrix += [{}, {'LANG': 'en_US.UTF-8', 'LC_MESSAGES': 'zh_TW'},
           {'LANG': 'zh_CN.UTF-8', 'LC_MESSAGES': 'ja_JP', 'LC_ALL': 'C'},
           {'LANG': 'C', 'LC_ALL': 'zh_CN.UTF-8'}]
rows = []
for variables in matrix:
    result = subprocess.run([shutil.which('node'), '--import', str(baseline / 'node_modules/tsx/dist/loader.mjs'),
                             str(Path(__file__).with_name('record_cjk_collation.mts').resolve()),
                             str(baseline), str(output / 'scratch')],
                            env={**env, **variables}, check=True, capture_output=True, text=True, timeout=60)
    row = json.loads(result.stdout)
    rows.append(row)
    print(json.dumps({'env': variables, 'resolved': row['resolved'], 'minimal': row['minimal']}, ensure_ascii=False))
commit = subprocess.check_output(['git', '-C', str(baseline), 'rev-parse', 'HEAD'], text=True).strip()
(output / 'node.json').write_text(json.dumps({'baselineCommit': commit, 'rows': rows}, ensure_ascii=False, indent=2) + '\n')
