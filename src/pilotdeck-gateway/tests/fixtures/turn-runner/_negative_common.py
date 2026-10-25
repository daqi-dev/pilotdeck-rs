"""Shared mutation anchors and bounded cargo execution for turn-runner controls."""
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, wait_for_slot

def mutate(original, scope, replacements, *, expected_count=1):
    if scope is None:
        body = original
        for old, new in replacements:
            assert body.count(old) == expected_count, (old, body.count(old))
            body = body.replace(old, new, 1)
        return body
    methods = list(re.finditer(r'^    (?:pub(?:\([^)]*\))? )?(?:async )?fn (\w+)', original, re.M))
    matches = [i for i, method in enumerate(methods) if method[1] == scope]
    assert len(matches) == 1, (scope, matches)
    i = matches[0]
    start = methods[i].start()
    end = methods[i + 1].start() if i + 1 < len(methods) else len(original)
    body = original[start:end]
    for old, new in replacements:
        assert old == old.lstrip() and '\n' not in old, old
        assert body.count(old) == 1, (scope, old, body.count(old))
        body = body.replace(old, new)
    return original[:start] + body + original[end:]

def run(name, *, root, out, command, env):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)
    result = run_owned(command, cwd=root, env=env, timeout=900)
    (out / f'{name}.log').write_text(result.stdout + result.stderr)
    return result
