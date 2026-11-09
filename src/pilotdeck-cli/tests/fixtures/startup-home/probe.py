#!/usr/bin/env python3
"""HOME-unset Node/Rust string-only probe; run on mbp2, no OS-home IO."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('--node', required=True)
parser.add_argument('--test-bin', required=True)
parser.add_argument('--evidence', required=True)
args = parser.parse_args()
node = str(Path(args.node).resolve(strict=True))
binary = str(Path(args.test_bin).resolve(strict=True))
out = Path(args.evidence)
out.mkdir(mode=0o700, parents=False, exist_ok=False)
with tempfile.TemporaryDirectory(prefix='pdrs-home-string-') as scratch:
    env = os.environ.copy()
    env.pop('HOME', None)
    # The child runs only one string helper test. Still override ordinary CLI
    # roots defensively; it never calls config/SDK/skills/setup entrypoints.
    env['PILOT_HOME'] = str(Path(scratch) / 'pilot')
    env['TMPDIR'] = scratch
    node_script = "process.stdout.write(JSON.stringify({node:process.version,icu:process.versions.icu,home:require('node:os').homedir()}))"
    oracle = subprocess.run([node, '-e', node_script], env=env, cwd=scratch,
                            capture_output=True, timeout=10, check=True)
    data = json.loads(oracle.stdout)
    assert data['node'] == 'v22.22.0', data['node']
    assert data['icu'] == '78.2', data['icu']
    (out / 'node.json').write_bytes(oracle.stdout + b'\n')
    env['PDRS_EXPECTED_OS_HOME'] = data['home']
    # Remove PILOT_HOME for pure path default assertion. This invokes only
    # paths::tests; there is no production CLI startup or IO on that path.
    env.pop('PILOT_HOME', None)
    rust = subprocess.run([binary, '--ignored', '--exact',
                           'paths::tests::process_home_unset_string_probe', '--nocapture'],
                          env=env, cwd=scratch, capture_output=True, timeout=15)
    (out / 'rust.stdout').write_bytes(rust.stdout)
    (out / 'rust.stderr').write_bytes(rust.stderr)
    digest = hashlib.sha256()
    with open(binary, 'rb') as executable:
        while True:
            chunk = executable.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
    binary_sha = digest.hexdigest()
    result = {'returncode': rust.returncode, 'node': data['node'], 'icu': data['icu'],
              'binarySha256': binary_sha,
              'homeStringSha256': hashlib.sha256(data['home'].encode()).hexdigest(),
              'scratchEntries': sorted(p.name for p in Path(scratch).iterdir()),
              'scope': 'OS home string comparison only; no filesystem access on returned path'}
    (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    assert rust.returncode == 0, result
    assert b'1 passed' in rust.stdout, rust.stdout
    assert result['scratchEntries'] == [], result
print(str(out))
