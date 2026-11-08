#!/usr/bin/env python3
"""Real fixed CLI consumers with empty/relative homes and zero network."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

p = argparse.ArgumentParser()
p.add_argument('--baseline', required=True, type=Path)
p.add_argument('--node', required=True, type=Path)
p.add_argument('--output', required=True, type=Path)
a = p.parse_args()
base = a.baseline.resolve(strict=True)
node = a.node.resolve(strict=True)
out = a.output.resolve()
out.mkdir(mode=0o700, parents=False, exist_ok=False)
sha = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
assert subprocess.check_output(['git', '-C', str(base), 'rev-parse', 'HEAD'], text=True).strip() == sha
sources = {}
for name in ['src/cli/pilotdeck.ts', 'src/cli/createLocalGateway.ts', 'src/pilot/paths.ts',
             'src/extension/skills/migrateLegacyBundledSkills.ts', 'src/extension/skills/migrateSkills.ts',
             'src/session/search/NodeSessionSearchPort.ts']:
    original = subprocess.check_output(['git', '-C', str(base), 'show', sha + ':' + name])
    assert original == (base / name).read_bytes(), name
    sources[name] = hashlib.sha256(original).hexdigest()
version = json.loads(subprocess.check_output([str(node), '-p', 'JSON.stringify({node:process.version,icu:process.versions.icu})']))
assert version == {'node': 'v22.22.0', 'icu': '78.2'}
valid = '---\nname: sample\ndescription: A sample skill for testing local migration.\n---\nBody.\n'
legacy = '---\nname: docx\ndescription: Fixture bundled skill.\n---\nBody.\n'
chat = json.dumps({'type': 'accepted_input', 'sessionId': 'session', 'turnId': 'turn', 'sequence': 1,
                   'createdAt': '2026-01-02T13:04:00.000Z', 'messages': [
                       {'role': 'user', 'content': [{'type': 'text', 'text': 'fixture needle text'}]}]}) + '\n'
rows = []
for kind in ['legacy', 'migrate', 'chat']:
    for home in ['', 'relative-home']:
        case = {'id': kind + ('-empty' if home == '' else '-relative'), 'kind': kind, 'HOME': home, 'PILOT_HOME': '~'}
        root = out / case['id']
        root.mkdir(mode=0o700)
        io_home = root / home
        inputs = {'blocked.yaml': 'gateway: [\n', 'bundled/docx/SKILL.md': legacy}
        def home_path(name):
            return str(Path(home) / name)
        if kind == 'legacy':
            inputs[home_path('skills/docx/SKILL.md')] = legacy
            case['argv'] = []
        elif kind == 'migrate':
            inputs[home_path('.claude/skills/sample/SKILL.md')] = valid
            case['argv'] = ['skills', 'migrate', '--from', 'claude-code', '--execute', '--json']
        else:
            inputs[home_path('projects/p/chats/session.jsonl')] = chat
            case['argv'] = ['chat', 'search', 'needle', '--all-projects', '--json']
        for name, text in inputs.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        (root / 'tmp').mkdir()
        data = root / 'case.json'
        data.write_text(json.dumps(case))
        env = {'PATH': str(node.parent) + ':/usr/bin:/bin', 'HOME': home, 'PILOT_HOME': '~',
               'PILOTDECK_CONFIG_PATH': str(root / 'blocked.yaml'),
               'PILOTDECK_BUNDLED_SKILLS_DIR': str(root / 'bundled'),
               'LANG': 'C', 'LC_ALL': 'C', 'TZ': 'UTC', 'NO_COLOR': '1',
               'TSX_DISABLE_CACHE': '1', 'TMPDIR': str(root / 'tmp'),
               'FIXTURE_NETWORK_EVENTS': str(root / 'network-events.json')}
        command = [str(node), '--import', str(base / 'node_modules/tsx/dist/loader.mjs'),
                   str(Path(__file__).with_name('consumers-driver.mjs').resolve()), str(data),
                   str(base / 'src/cli/pilotdeck.ts')]
        child = subprocess.Popen(command, cwd=root, env=env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        timeout = False
        try:
            stdout, stderr = child.communicate(timeout=15)
        except subprocess.TimeoutExpired:
            timeout = True
            child.kill()
            stdout, stderr = child.communicate(timeout=3)
        network = json.loads((root / 'network-events.json').read_text()) if (root / 'network-events.json').exists() else None
        after = {}
        for file in root.rglob('*'):
            relative = file.relative_to(root)
            if relative.parts[0] == 'tmp' or str(relative) in ['case.json', 'network-events.json', 'raw.json']:
                continue
            if file.is_file():
                after[str(relative)] = file.read_text()
        row = dict(case, inputs=inputs, after=after, stdout=stdout.decode(), stderr=stderr.decode(),
                   code=child.returncode, timeout=timeout, networkAttempts=network)
        (root / 'raw.json').write_text(json.dumps(row, ensure_ascii=False, indent=2) + '\n')
        assert not timeout and network == [], case['id']
        assert child.returncode == (1 if kind == 'legacy' else 0), case['id']
        if kind == 'chat':
            assert len(json.loads(stdout)['matches']) == 1, case['id']
        elif kind == 'migrate':
            assert json.loads(stdout)['summary']['migrated'] == 1, case['id']
        else:
            assert b'Activated bundled skills directly; moved 1 unchanged legacy copy to ' in stdout, case['id']
            assert home_path('skills/docx/SKILL.md') not in after, case['id']
            assert after[home_path('.legacy-bundled-skills-migrated-v1')] == 'completed\n', case['id']
        rows.append(json.loads(json.dumps(row, ensure_ascii=False).replace(str(root), '<CASE>')))
(out / 'consumers-oracle.json').write_text(json.dumps(dict(baseline=sha, sources=sources, **version, cases=rows), ensure_ascii=False, indent=2) + '\n')
print('recorded', len(rows), 'real CLI consumer cases')
