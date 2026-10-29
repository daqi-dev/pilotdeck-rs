#!/usr/bin/env python3
# anchor-selfcheck: unsupported B2 private-mirror mutation rows, no in-place source replacement table
"""#1023 negative controls for the B2 entry guards.

Each row copies the frozen B2 apparatus into a private temporary mirror,
applies one mutation there (never in the checkout), refreshes that file's
FROZEN pin so the preflight admits the copy, and runs the bound test in
``scripts/ui-server-parity/test_frozen.py``.  KILLED means the named test
failed at its own ``#1023-N`` assertion.  SURVIVED means the mutation left the
test green; INVALID means the row could not run or reached another failure.
The discover row builds a probe from the real guard bytes: with the guard the
start directory is rejected, without it unittest runs the probe.  The alias row
replays the shipped guard bytes under case/symlink spellings (disabling the
guard admits the probe as its paired positive control) and then restores the
pre-fix realpath comparison so ``#1023-6`` must fail on admission.

No checkout source is written and no cargo runs.  Run all rows, print a
summary, and exit nonzero unless every row is KILLED.
"""
import sys as _boot_sys
if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:
    raise SystemExit(f'#1023: os must be preloaded from CPython 3.11+ frozen modules; run with -I: python3 -I {__file__}')
import os as _boot_os

# Directory identity is compared by (device, inode), never by spelling.
# realpath normalises ``//`` and symlinks but keeps the caller's case, so on a
# case-insensitive volume a differently cased sys.path entry still pointed at
# this directory and was admitted (fix-r2 P1-1).
_boot_dir = _boot_os.path.dirname(_boot_os.path.realpath(__file__))
_boot_roots = (_boot_dir, _boot_os.path.dirname(_boot_dir))
if not _boot_sys.flags.isolated or any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in _boot_roots)
        for entry in _boot_sys.path):
    raise SystemExit(f'#1023: unsafe import path; run with -I by file: python3 -I {__file__}')

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[5]
APPARATUS = 'scripts/ui-server-parity'
SPEC = 'specs/ui-server-parity'
SEPARATOR = '-' * 70
HEADER = re.compile(r'^(FAIL|ERROR): (\S+) \(([^)]+)\)')
# Shipped guard anchor (#1023 fix-r3): identity comparison over (device, inode).
IDENTITY_GUARD = (
    'if any(\n'
    '        _boot_os.path.isdir(entry or _boot_os.curdir)\n'
    '        and any(_boot_os.path.isdir(root)\n'
    '                and _boot_os.path.samefile(entry or _boot_os.curdir, root)\n'
    "                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))\n"
    '        for entry in _boot_sys.path):')
# Prefix only, so disabling keeps the rest of the same condition syntactically valid.
GUARD = ('if any(\n'
         '        _boot_os.path.isdir(entry or _boot_os.curdir)\n')
GUARD_OFF = ('if False and any(\n'
             '        _boot_os.path.isdir(entry or _boot_os.curdir)\n')
# fix-r4 GLM P1: the preflight that proves os is a preloaded frozen module.
# Removing it lets a forged os.py beside a reinserted directory execute on an
# interpreter without frozen os; the shipped guard must reject first.
PREFLIGHT = (
    "if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:\n"
    "    print('{\"verdict\":\"FAIL\",\"errors\":[\"os must be preloaded from "
    "CPython 3.11+ frozen modules\"]}')\n"
    "    raise SystemExit(1)\n")
# The pre-fix-r3 spelling comparison this row restores to prove #1023-6 kills it.
REALPATH_GUARD = (
    'if any(_boot_os.path.realpath(entry or _boot_os.curdir) in {\n'
    '        _boot_os.path.realpath(_boot_dir),\n'
    "        _boot_os.path.realpath(_boot_os.path.join(_boot_dir, 'controls'))}\n"
    '       for entry in _boot_sys.path):')


def copy_apparatus(destination):
    for name in (APPARATUS, 'scripts/exp008', SPEC):
        shutil.copytree(ROOT / name, destination / name)
    shutil.copy2(ROOT / 'rust-toolchain.toml', destination / 'rust-toolchain.toml')
    # Preflight calls toolchain_files(), which reads the index; stage the same
    # tracked set a real checkout would have. No history is needed.
    subprocess.run(['git', 'init', '-q', str(destination)], check=True)
    subprocess.run(['git', '-C', str(destination), 'add', 'scripts', 'specs',
                    'rust-toolchain.toml'], check=True)


def refresh_pin(root, relative):
    manifest = root / SPEC / 'FROZEN.md'
    lines = manifest.read_text().splitlines(keepends=True)
    suffix = '  ../../' + relative
    hits = [index for index, line in enumerate(lines) if line.rstrip('\n').endswith(suffix)]
    if len(hits) != 1:
        raise AssertionError(f'{relative}: expected one FROZEN pin, found {len(hits)}')
    digest = hashlib.sha256((root / relative).read_bytes()).hexdigest()
    lines[hits[0]] = f'{digest}{suffix}\n'
    manifest.write_text(''.join(lines))


def rewrite_source(root, relative, old, new):
    path = root / relative
    text = path.read_text()
    if text.count(old) != 1:
        raise AssertionError(f'{relative}: anchor occurs {text.count(old)} times, expected 1')
    mutated = text.replace(old, new, 1)
    # A mutant that cannot compile is INVALID, never a killed test: verify here
    # before any run, in-process so no __pycache__ lands in the apparatus copy.
    compile(mutated, str(path), 'exec')
    path.write_text(mutated)
    refresh_pin(root, relative)


def run_bound(root, test, timeout=300):
    return subprocess.run(
        [sys.executable, '-I', str(root / APPARATUS / 'test_frozen.py'), test],
        cwd=root, capture_output=True, text=True, timeout=timeout)


def failure_bodies(log):
    lines = log.splitlines()
    bodies = []
    for index, line in enumerate(lines):
        match = HEADER.match(line)
        if not match:
            continue
        start = index + 1
        if start < len(lines) and lines[start] == SEPARATOR:
            start += 1
        body = []
        for later in lines[start:]:
            if later == SEPARATOR:
                break
            body.append(later)
        bodies.append((match.group(1), match.group(2), match.group(3), body))
    return bodies


def classify(result, test, marker):
    log = (result.stdout or '') + (result.stderr or '')
    if result.returncode == 0:
        return 'SURVIVED', 'the bound test still passed'
    if result.returncode != 1:
        return 'INVALID', f'unexpected exit code {result.returncode}'
    if 'Ran 1 test' not in log or not re.search(r'^FAILED', log, re.MULTILINE):
        return 'INVALID', 'no single-test unittest summary'
    names = {'__main__.' + test, 'test_frozen.' + test}
    for kind, name, qualname, body in failure_bodies(log):
        if kind != 'FAIL' or (qualname not in names and name != test.split('.', 1)[1]):
            continue
        text = '\n'.join(body)
        if 'AssertionError:' in text and marker in text:
            return 'KILLED', f'bound test failed at its {marker} assertion'
    return 'INVALID', 'the bound test did not fail at its own assertion'


def row_readme(root):
    relative = APPARATUS + '/README.md'
    path = root / relative
    text = path.read_text()
    start = text.index('python3 -I scripts/ui-server-parity/test_calibrate.py')
    end = text.index('python3 -I scripts/ui-server-parity/controls/run_all.py')
    command = "python3 -I -B -m unittest discover -s scripts/ui-server-parity -p 'test_*.py'\n"
    path.write_text(text[:start] + command + text[end:])
    refresh_pin(root, relative)
    return run_bound(root, 'FrozenControls.test_documented_tests_use_direct_isolated_files'), \
        'FrozenControls.test_documented_tests_use_direct_isolated_files', '#1023-1:'


def row_guard(root):
    relative = APPARATUS + '/check_frozen.py'
    rewrite_source(root, relative, GUARD, GUARD_OFF)
    return run_bound(root, 'FrozenControls.test_entrypoints_reject_reinserted_apparatus_directory'), \
        'FrozenControls.test_entrypoints_reject_reinserted_apparatus_directory', '#1023-2:'


def row_main(root):
    relative = APPARATUS + '/test_calibrate.py'
    rewrite_source(root, relative, "\n\nif __name__ == '__main__':\n    unittest.main()\n", '\n')
    return run_bound(root, 'FrozenControls.test_every_test_file_runs_by_direct_isolated_entry'), \
        'FrozenControls.test_every_test_file_runs_by_direct_isolated_entry', '#1023-4:'


def discover_probe(root, guarded):
    source = (root / APPARATUS / 'test_frozen.py').read_text()
    marker = 'apparatus directory on sys.path'
    end = source.index('raise SystemExit(1)', source.index(marker)) + len('raise SystemExit(1)')
    prefix = source[:end]
    if not guarded:
        prefix = prefix.replace(GUARD, GUARD_OFF, 1)
    probe = (prefix + '\n\nimport unittest\n\n\n'
             'class Probe(unittest.TestCase):\n'
             '    def test_runs(self):\n'
             '        pass\n\n\n'
             "if __name__ == '__main__':\n"
             '    unittest.main()\n')
    path = root / APPARATUS / 'test_zz_probe.py'
    path.write_text(probe)
    return subprocess.run(
        [sys.executable, '-I', '-B', '-m', 'unittest', 'discover',
         '-s', str(root / APPARATUS), '-p', 'test_zz_probe.py'],
        cwd=root, capture_output=True, text=True, timeout=60)


def row_discover(root):
    closed = discover_probe(root, guarded=True)
    if closed.returncode != 1 or 'apparatus directory on sys.path' not in (closed.stdout + closed.stderr):
        raise AssertionError('the guard did not reject discover before importing the probe')
    open_run = discover_probe(root, guarded=False)
    if open_run.returncode != 0 or 'Ran 1 test' not in (open_run.stdout + open_run.stderr):
        raise AssertionError('removing the guard did not let discover run the probe')
    return closed, 'discover-guard', '#1023-3:'


def guard_head(relative):
    """Shipped guard bytes up to and including its exit; no apparatus preflight."""
    text = (ROOT / relative).read_text()
    end = text.index('raise SystemExit(1)', text.index('apparatus directory on sys.path'))
    return text[:end + len('raise SystemExit(1)')]


def alias_reinsertion(root):
    """#1023 fix-r3 P1: identity guards reject case/symlink alias spellings.

    The probes use the shipped guard bytes of a root entry and a controls
    entry, so this row regresses the real guards, not a paraphrase. The
    disabled copies are the paired positive control: without the guard the
    probe prints REACHED. The returned #1023-6 run must fail at its own
    assertion after the guard is restored to the pre-fix realpath comparison.
    """
    probe = ('import runpy, sys\n'
             'sys.path.insert(0, sys.argv[1])\n'
             'runpy.run_path(sys.argv[2], run_name="probe")\n')

    def run(path_dir, entry):
        return subprocess.run(
            [sys.executable, '-I', '-c', probe, str(path_dir), str(entry)],
            cwd=root, capture_output=True, text=True, timeout=60)

    base = root / 'alias' / 'apparatus'
    (base / 'controls').mkdir(parents=True)
    link = root / 'alias' / 'linked'
    link.symlink_to(base, target_is_directory=True)
    case = base.with_name(base.name.upper())
    if not (case.is_dir() and _boot_os.path.samefile(case, base)):
        raise AssertionError('case-insensitive volume required for the #1023 alias regression')
    placements = (
        (APPARATUS + '/check_frozen.py', base / 'entry.py',
         [(case, base / 'entry.py'), (link, base / 'entry.py'),
          (base / 'controls', base / 'entry.py'),
          (base, case / 'entry.py'), (base, link / 'entry.py')]),
        (APPARATUS + '/controls/run_all.py', base / 'controls' / 'entry.py',
         [(case, base / 'controls' / 'entry.py'),
          (case / 'controls', base / 'controls' / 'entry.py'),
          (base, case / 'controls' / 'entry.py')]),
    )
    for relative, entry, forms in placements:
        head = guard_head(relative)
        entry.write_text(head + "\nprint('REACHED')\n")
        for path_dir, guarded_entry in forms:
            closed = run(path_dir, guarded_entry)
            if (closed.returncode != 1
                    or 'apparatus directory on sys.path' not in (closed.stdout + closed.stderr)):
                raise AssertionError(
                    f'guarded alias {path_dir} / {guarded_entry} was admitted: '
                    f'rc={closed.returncode}')
        entry.write_text(head.replace(GUARD, GUARD_OFF, 1) + "\nprint('REACHED')\n")
        opened = run(forms[0][0], forms[0][1])
        if opened.returncode != 0 or 'REACHED' not in opened.stdout:
            raise AssertionError(
                f'disabling the guard for {relative} did not admit the alias probe')
    rewrite_source(root, APPARATUS + '/check_frozen.py', IDENTITY_GUARD, REALPATH_GUARD)
    return run_bound(root, 'FrozenControls.test_entry_guards_reject_alias_spellings'), \
        'FrozenControls.test_entry_guards_reject_alias_spellings', '#1023-6:'


def os_preflight(root):
    """fix-r4 GLM P1: the guard must reject before importing an unproved os."""
    rewrite_source(root, APPARATUS + '/check_frozen.py', PREFLIGHT, 'if False:\n    pass\n')
    return run_bound(root, 'FrozenControls.test_entry_guards_require_preloaded_os'), \
        'FrozenControls.test_entry_guards_require_preloaded_os', '#1023-11:'


ROWS = {
    'readme-discover': row_readme,
    'entry-guard-removed': row_guard,
    'main-removed': row_main,
    'discover-guard-removed': row_discover,
    'alias-reinsertion': alias_reinsertion,
    'os-preflight-removed': os_preflight,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    rows = []
    try:
        with tempfile.TemporaryDirectory(prefix='pdrs-1023-b2-') as temp:
            for name, action in ROWS.items():
                root = Path(temp) / name
                root.mkdir()
                copy_apparatus(root)
                try:
                    result, test, marker = action(root)
                    if name == 'discover-guard-removed':
                        # The row already asserts both arms (guarded rejection and
                        # unguarded discovery); there is no bound unit test.
                        status, reason = ('KILLED', 'guard rejected the probe and the unguarded probe ran')
                    else:
                        status, reason = classify(result, test, marker)
                    log = (result.stdout or '') + (result.stderr or '')
                    (output / f'{name}.log').write_text(log)
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                rows.append({'row': name, 'status': status, 'reason': reason})
                print(f'{name}: {status}: {reason}', flush=True)
    finally:
        counts = {state: sum(row['status'] == state for row in rows)
                  for state in ('KILLED', 'SURVIVED', 'INVALID')}
        (output / 'summary.json').write_text(
            json.dumps({'rows': rows, 'counts': counts}, indent=2) + '\n')
    counts = {state: sum(row['status'] == state for row in rows)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    return 0 if rows and all(row['status'] == 'KILLED' for row in rows) else 1


if __name__ == '__main__':
    raise SystemExit(main())
