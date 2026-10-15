#!/usr/bin/env python3
"""P2 文档/构建回归与负控；stub 构建命令，不构建、不下载、不 listen。"""
import json
import os
from datetime import date
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def browser_claim_matches_evidence(markdown, evidence):
    section = markdown.split('[当前 coexist.json]', 1)[1].split('### 给 F3', 1)[0]
    return (f"状态 `{evidence['status']}`" in section
            and f"`{evidence['browser']['status']}`" in section
            and 'BROWSER PASS' in section
            and 'PASS_HTTP_WS_SSE_ONLY' not in section
            and 'NOT_RUN' not in section)


def offline_locked(commands):
    return len(commands) == 3 and all('--offline' in row.split() and '--locked' in row.split()
                                      for row in commands)


class P2Cleanup(unittest.TestCase):
    def test_browser_claim_matches_coexist_evidence_and_rejects_stale_claims(self):
        markdown = (ROOT / 'docs/experiments/exp007-frontend-probe/readme.md').read_text()
        evidence = json.loads((ROOT / 'docs/experiments/exp007-frontend-probe/evidence/coexist.json').read_text())
        self.assertTrue(browser_claim_matches_evidence(markdown, evidence))
        for old in ('PASS_HTTP_WS_SSE_ONLY', 'NOT_RUN'):
            self.assertFalse(browser_claim_matches_evidence(markdown.replace(evidence['status'], old), evidence))
        changed = dict(evidence, browser=dict(evidence['browser'], status='NOT_RUN'))
        self.assertFalse(browser_claim_matches_evidence(markdown, changed))

    def test_build_coexist_runs_every_cargo_call_locked_offline_and_rejects_missing_flag(self):
        directory = ROOT / '_tmp' / (date.today().strftime('%Y%m%d') + '-exp007-controls')
        directory.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=directory) as temporary:
            scratch = Path(temporary)
            script = scratch / 'scripts/exp007/build-coexist.sh'
            script.parent.mkdir(parents=True)
            script.write_text((ROOT / 'scripts/exp007/build-coexist.sh').read_text())
            binaries = scratch / 'bin'
            binaries.mkdir()
            for name, body in [('cargo', '#!/bin/sh\nprintf "%s\\n" "$*" >> "$CARGO_LOG"\n'),
                               ('python3', '#!/bin/sh\nexit 0\n')]:
                path = binaries / name
                path.write_text(body)
                path.chmod(0o755)
            log = scratch / 'cargo.log'
            env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ['PATH'], CARGO_LOG=str(log))
            subprocess.run(['/bin/sh', str(script)], env=env, check=True)
            commands = log.read_text().splitlines()
            self.assertTrue(offline_locked(commands), commands)
            self.assertTrue(commands[0].startswith('build '))
            self.assertTrue(commands[1].startswith('build '))
            self.assertTrue(commands[2].startswith('install '))
            for flag in ('--offline', '--locked'):
                for index in range(3):
                    changed = commands.copy()
                    changed[index] = changed[index].replace(flag, '')
                    self.assertFalse(offline_locked(changed), (flag, index))


if __name__ == '__main__':
    unittest.main(verbosity=2)
