#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative checker rewrites for #977; no declarative whole-file product anchors
"""#977 marker mutants: every weakened fail-closed rule must fail a named test.

Each rewrite weakens one ``anchor_selfcheck.py`` rule added by #977 (marker
ignored / empty reason / late marker / malformed prefix / duplicate marker /
stale signals / missing marker source). The bound oracle is
``test_anchor_selfcheck.py``; a mutant is KILLED only when
``test_unsupported_marker_is_explicit_and_fails_closed`` fails on an assertion
whose message carries ``#977``. Anything else (suite green, or the bound test
passing while others fail) is SURVIVED / INVALID, never silently accepted.
No cargo is launched, so ``wait_for_slot`` is deliberately not called: a
python-only control must not block on an unrelated cargo slot.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
CHECKER = FIXTURES / 'anchor_selfcheck.py'
TESTS = FIXTURES / 'test_anchor_selfcheck.py'
TEST = 'test_unsupported_marker_is_explicit_and_fails_closed'

# (name, old fragment, new fragment, #977 label the killed failure must show).
# Every fragment occurs exactly once in the pristine checker; a stale anchor
# is recorded as INVALID and fails the run instead of counting as SURVIVED.
REWRITES = [
    ('marker-ignored', '        if reason is not None:\n            markers[relative] = reason',
     '        if False and reason is not None:\n            markers[relative] = reason',
     '#977 marker-present'),
    ('empty-reason-accepted',
     "        if not text:\n            raise Unsupported(f'unsupported marker on line {number} needs a nonempty reason')",
     "        if not text and False:\n            raise Unsupported(f'unsupported marker on line {number} needs a nonempty reason')",
     '#977 marker-empty-reason'),
    ('marker-limit-removed', 'MARKER_LINE_LIMIT = 20', 'MARKER_LINE_LIMIT = 9999',
     '#977 marker-after-limit'),
    ('stale-signals-ignored', '    for name in UNSUPPORTED_SIGNALS:\n        if name not in markers:',
     '    for name in UNSUPPORTED_SIGNALS:\n        if False and name not in markers:',
     '#977 stale-signal-missing-script'),
    ('malformed-marker-ignored',
     "            if re.match(r'^#\\s*anchor-selfcheck:', stripped):",
     "            if False and re.match(r'^#\\s*anchor-selfcheck:', stripped):",
     '#977 malformed-marker'),
    ('duplicate-marker-accepted',
     "        if reason is not None:\n            raise Unsupported(f'duplicate unsupported marker on line {number}')",
     "        if False and reason is not None:\n            raise Unsupported(f'duplicate unsupported marker on line {number}')",
     '#977 duplicate-marker'),
    ('missing-marker-source-ignored',
     "    if not script.is_file():\n        raise Unsupported(f'unsupported marker source does not exist: {script}')",
     "    if not script.is_file():\n        return None",
     '#977 missing marker script must fail closed'),
]


def run_suite(env):
    # -I keeps the test from importing the mutated checker through a stray
    # sys.path entry; the suite itself copies fixtures into temp directories.
    return run_owned([sys.executable, '-I', str(TESTS)], env=env, cwd=ROOT, timeout=900)


def failure_block(log, test):
    """Return the unittest failure block of ``test`` or None when it passed."""
    head = f'FAIL: {test} ('
    start = log.find(head)
    if start < 0:
        return None
    end = log.find('\n====', start)
    return log[start:] if end < 0 else log[start:end]


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        # No cargo is launched here, but the caller contract of every negative
        # control still requires the shared target directory.
        print('CARGO_TARGET_DIR must be set by the caller', file=sys.stderr)
        return 2
    env = dict(os.environ)
    out = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path('anchor-marker-evidence')
    out.mkdir(parents=True, exist_ok=False)
    records = []
    with mutating_sources(ROOT, [CHECKER, TESTS]) as originals:
        baseline = run_suite(env)
        (out / 'baseline.log').write_text(baseline.stdout + baseline.stderr, encoding='utf-8')
        if baseline.returncode != 0:
            print(f'baseline suite already failing (exit {baseline.returncode}); aborting',
                  file=sys.stderr)
            return 2
        original = originals[CHECKER].decode('utf-8')
        for name, old, new, label in REWRITES:
            record = {'name': name, 'label': label}
            records.append(record)
            occurrences = original.count(old)
            if occurrences != 1:
                record['verdict'] = 'INVALID'
                record['reason'] = f'anchor occurs {occurrences} times, expected 1'
                continue
            mutant = original.replace(old, new, 1)
            try:
                compile(mutant, str(CHECKER), 'exec')
            except SyntaxError as error:
                record['verdict'] = 'INVALID'
                record['reason'] = f'mutant does not compile: {error}'
                continue
            try:
                CHECKER.write_text(mutant, encoding='utf-8')
                result = run_suite(env)
                log = result.stdout + result.stderr
            finally:
                CHECKER.write_bytes(originals[CHECKER])
            (out / f'{name}.log').write_text(log, encoding='utf-8')
            block = failure_block(log, TEST)
            if block is not None and label in block:
                record['verdict'] = 'KILLED'
                record['reason'] = block.strip().splitlines()[-1]
            elif result.returncode == 0:
                record['verdict'] = 'SURVIVED'
                record['reason'] = 'bound test still passes with the mutant applied'
            elif block is not None:
                record['verdict'] = 'INVALID'
                record['reason'] = f'bound test failed without its {label} assertion'
            else:
                record['verdict'] = 'INVALID'
                record['reason'] = 'suite failed without failing the bound test'
            print(f"{record['verdict']} {name}: {record['reason']}")
        restored = run_suite(env)
        (out / 'restored.log').write_text(restored.stdout + restored.stderr, encoding='utf-8')
        if restored.returncode != 0:
            print('restored checker does not pass the suite', file=sys.stderr)
            return 2
    summary = {verdict: sum(1 for record in records if record['verdict'] == verdict)
               for verdict in ('KILLED', 'SURVIVED', 'INVALID')}
    (out / 'summary.json').write_text(
        json.dumps({'summary': summary, 'records': records}, indent=2) + '\n', encoding='utf-8')
    print(f"SUMMARY killed={summary['KILLED']} survived={summary['SURVIVED']} "
          f"invalid={summary['INVALID']}")
    return 0 if summary['SURVIVED'] == 0 and summary['INVALID'] == 0 else 1


if __name__ == '__main__':
    sys.exit(main())
