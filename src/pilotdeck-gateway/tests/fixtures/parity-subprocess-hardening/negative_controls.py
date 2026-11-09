#!/usr/bin/env python3
"""#2398 Python-only source mutants; run only after the coordinator commit.

Guards under test (issue #2398, from the #2372 fix-r1 dual-lineage review):
  - scripts/parity/check_frozen.py git() and scripts/parity/g03_compare.py
    baseline_path() must never inherit GIT_* from the caller, never read a
    global gitconfig, and always time out.
  - scripts/parity's five result files (score.py, g03_compare.py,
    check_ruler.py, self_compare.py, controls/run_all.py) must be created
    exclusively through G.write_result_exclusive; a later run may never
    overwrite an earlier run's results.

Every mutated source equals HEAD before its window opens (mutating_sources);
each mutant compiles before its control runs; stale anchors abort instead of
being recorded as SURVIVED. No Cargo invocation, shadow repository or HEAD
exemption is used. Positive-only controls (no mutation: the hostile-env
equal-output proofs, the bounded-exit proof, and the five exclusive-write
first/second-run cycles) live in `positives` and are judged PASS/FAIL.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

root = Path(__file__).resolve().parents[5]
mutations = [
    ('parity-git-env-strip', root / 'scripts/parity/check_frozen.py',
     "env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}",
     'env = dict(os.environ)',
     'control_check_frozen_hostile_env'),
    ('parity-git-config-global', root / 'scripts/parity/check_frozen.py',
     "env['GIT_CONFIG_GLOBAL'] = os.devnull",
     "env['GIT_CONFIG_GLOBAL'] = os.environ.get('GIT_CONFIG_GLOBAL') or os.devnull",
     'control_check_frozen_hostile_config'),
    ('g03-git-env-strip', root / 'scripts/parity/g03_compare.py',
     'env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}',
     'env = dict(os.environ)',
     'control_g03_hostile_env'),
    ('g03-git-config-global', root / 'scripts/parity/g03_compare.py',
     'env["GIT_CONFIG_GLOBAL"] = os.devnull',
     'env["GIT_CONFIG_GLOBAL"] = os.environ.get("GIT_CONFIG_GLOBAL", os.devnull)',
     'control_g03_hostile_config'),
    ('g03-git-timeout', root / 'scripts/parity/g03_compare.py',
     'env=env, timeout=GIT_TIMEOUT_SECONDS)',
     'env=env, timeout=None)',
     'control_g03_git_timeout'),
    ('g03-exclusive-write', root / 'scripts/parity/g03_compare.py',
     "with open(path, 'x', encoding='utf-8') as stream:",
     "with open(path, 'w', encoding='utf-8') as stream:",
     'control_g03_exclusive_write'),
]

positives = [
    ('check-frozen-hostile-env', 'control_check_frozen_hostile_env'),
    ('check-frozen-hostile-config', 'control_check_frozen_hostile_config'),
    ('g03-hostile-env', 'control_g03_hostile_env'),
    ('g03-hostile-config', 'control_g03_hostile_config'),
    ('g03-git-timeout-bound', 'control_g03_git_timeout'),
    ('score-exclusive-write', 'control_score_exclusive_write'),
    ('g03-exclusive-write-cycle', 'control_g03_exclusive_write'),
    ('check-ruler-exclusive-write', 'control_check_ruler_exclusive_write'),
    ('run-all-exclusive-write', 'control_run_all_exclusive_write'),
    ('self-compare-exclusive-write', 'control_self_compare_exclusive_write'),
]

# A git config whose boolean value is invalid: any git invocation that reads it
# fails, so honoring GIT_CONFIG_GLOBAL is observable in the checker's output.
BAD_GITCONFIG = '[core]\n\tfileMode = not-a-boolean\n'


def child_env(extra):
    """Child environment for the CLI under test. PDRS_BASELINE is always
    removed: the g03 controls must exercise the rev-parse discovery path, not
    short-circuit past it (a set PDRS_BASELINE would make the isolation
    mutants survive trivially)."""
    env = dict(os.environ)
    env.pop('PDRS_BASELINE', None)
    env.update(extra)
    return env


def run_cli(scratch, label, arguments, *, env=None, timeout=180):
    command = [sys.executable, '-I', *arguments]
    result = run_owned(command, env=env, timeout=timeout, cwd=str(root))
    for suffix, value in (('stdout', result.stdout), ('stderr', result.stderr),
                          ('exit', f'{result.returncode}\n')):
        (scratch / f'{label}.{suffix}').write_text(value)
    return result


def g03_rev_parse_is_decisive():
    """The g03 controls need the rev-parse candidate to be the deciding one:
    a worktree-local resources/PilotDeck would resolve before git runs."""
    if (root / 'resources/PilotDeck').is_dir():
        raise RuntimeError('#2398 precondition failed: worktree resources/PilotDeck exists, '
                           'so baseline discovery never reaches the rev-parse call')


def compare_check_frozen(scratch, extra):
    clean = run_cli(scratch, 'clean', ['scripts/parity/check_frozen.py', '--base', 'origin/main'])
    hostile = run_cli(scratch, 'hostile', ['scripts/parity/check_frozen.py', '--base', 'origin/main'],
                      env=child_env(extra))
    if (clean.returncode, clean.stdout, clean.stderr) != (hostile.returncode, hostile.stdout, hostile.stderr):
        raise AssertionError('#2398 hostile git environment changed check_frozen output '
                             f'(clean exit={clean.returncode}, hostile exit={hostile.returncode}); '
                             'GIT_* isolation or GIT_CONFIG_GLOBAL pinning is not in force')


def control_check_frozen_hostile_env(scratch):
    fake_repo = scratch / 'fake-repo'  # deliberately not a repository
    compare_check_frozen(scratch, {'GIT_DIR': str(fake_repo), 'GIT_WORK_TREE': str(scratch),
                                   'GIT_INDEX_FILE': str(scratch / 'fake-index')})


def control_check_frozen_hostile_config(scratch):
    bad = scratch / 'bad-gitconfig'
    bad.write_text(BAD_GITCONFIG)
    compare_check_frozen(scratch, {'GIT_CONFIG_GLOBAL': str(bad)})


def run_g03(scratch, label, left, right, out, *, env=None, timeout=180):
    return run_cli(scratch, label,
                   ['scripts/parity/g03_compare.py', str(left), str(right), '--json', str(out)],
                   env=env, timeout=timeout)


def g03_pair_dirs(scratch, *names):
    directories = []
    for name in names:
        directory = scratch / name
        directory.mkdir()
        directories.append(directory)
    return directories


def control_g03_hostile_env(scratch):
    g03_rev_parse_is_decisive()
    left, right = g03_pair_dirs(scratch, 'left', 'right')
    out = scratch / 'g03.json'
    result = run_g03(scratch, 'hostile', left, right, out,
                     env=child_env({'GIT_DIR': str(scratch / 'fake-repo')}))
    log = result.stdout + result.stderr
    if 'PilotDeck baseline not found' in log or 'set PDRS_BASELINE' in log:
        raise AssertionError('#2398 hostile GIT_DIR hijacked the g03 baseline discovery '
                             '(rev-parse honored the caller GIT_DIR); GIT_* isolation is not in force')
    if not out.is_file():
        raise AssertionError('#2398 g03_compare did not run to its result write under a hostile GIT_DIR')


def control_g03_hostile_config(scratch):
    g03_rev_parse_is_decisive()
    bad = scratch / 'bad-gitconfig'
    bad.write_text(BAD_GITCONFIG)
    left, right = g03_pair_dirs(scratch, 'left', 'right')
    out = scratch / 'g03.json'
    result = run_g03(scratch, 'hostile', left, right, out,
                     env=child_env({'GIT_CONFIG_GLOBAL': str(bad)}))
    log = result.stdout + result.stderr
    if 'PilotDeck baseline not found' in log or 'set PDRS_BASELINE' in log:
        raise AssertionError('#2398 a hostile GIT_CONFIG_GLOBAL broke the g03 baseline discovery; '
                             'the global-config pin is not in force')


def control_g03_git_timeout(scratch):
    g03_rev_parse_is_decisive()
    fake_bin = scratch / 'bin'
    fake_bin.mkdir()
    git = fake_bin / 'git'
    git.write_text('#!/bin/sh\nexec sleep 600\n')
    git.chmod(0o755)
    env = child_env({})
    env['PATH'] = str(fake_bin) + os.pathsep + env['PATH']
    left, right = g03_pair_dirs(scratch, 'left', 'right')
    out = scratch / 'g03.json'
    started = time.monotonic()
    try:
        result = run_g03(scratch, 'sleeping-git', left, right, out, env=env, timeout=90)
    except subprocess.TimeoutExpired:
        raise AssertionError('#2398 the g03 git subprocess outlived the control bound; '
                             'the GIT_TIMEOUT_SECONDS timeout is not in force')
    elapsed = time.monotonic() - started
    log = result.stdout + result.stderr
    if elapsed > 60:
        raise AssertionError(f'#2398 the g03 git subprocess took {elapsed:.0f}s; the '
                             'GIT_TIMEOUT_SECONDS timeout is not in force')
    if 'PilotDeck baseline not found' not in log:
        raise AssertionError('#2398 a timed-out rev-parse must fall back to skipping the candidate, '
                             f'got exit={result.returncode}')


def exclusive_write_cycle(scratch, label, command, outputs):
    """First run creates every output; a second run must refuse loudly and
    leave the first run's bytes untouched."""
    first = run_cli(scratch, 'first', command)
    for out in outputs:
        if not out.is_file():
            raise AssertionError(f'#2398 the first {label} run did not write {out.name} '
                                 f'(exit={first.returncode})')
    before = {out: out.read_bytes() for out in outputs}
    second = run_cli(scratch, 'second', command)
    log = second.stdout + second.stderr
    if second.returncode == 0 or '#2398 result file already exists' not in log:
        raise AssertionError(f'#2398 a second {label} run must fail loudly instead of '
                             f'silently producing results (exit={second.returncode})')
    for out, data in before.items():
        if out.read_bytes() != data:
            raise AssertionError(f'#2398 {out.name} was overwritten by a later {label} run')


def exclusive_write_overwrite(scratch, label, command_factory, output):
    """Two-run kill check for the 'x' flag. command_factory(run_index) must
    produce commands whose result bytes differ between runs (distinct input
    directories), so a truncating overwrite is observable as changed bytes:
    the clean code refuses and preserves them, a 'w' mutant neither refuses
    nor preserves."""
    run_cli(scratch, 'first', command_factory(1))
    if not output.is_file():
        raise AssertionError(f'#2398 the first {label} run did not write {output.name}')
    before = output.read_bytes()
    second = run_cli(scratch, 'second', command_factory(2))
    log = second.stdout + second.stderr
    if '#2398 result file already exists' not in log:
        raise AssertionError(f'#2398 the second {label} run did not refuse the existing '
                             f'result file (overwrote={output.read_bytes() != before})')
    if output.read_bytes() != before:
        raise AssertionError(f'#2398 {output.name} refused the second {label} run '
                             'but its bytes changed anyway')


def control_score_exclusive_write(scratch):
    ts, rust = g03_pair_dirs(scratch, 'ts', 'rust')
    out = scratch / 'score.json'
    exclusive_write_cycle(scratch, 'score.py',
                           ['scripts/parity/score.py', '--ts', str(ts), '--rust', str(rust),
                            '--json', str(out)], [out])


def control_g03_exclusive_write(scratch):
    out = scratch / 'g03.json'

    def command_factory(run):
        left, right = g03_pair_dirs(scratch, f'left-{run}', f'right-{run}')
        return ['scripts/parity/g03_compare.py', str(left), str(right), '--json', str(out)]

    exclusive_write_overwrite(scratch, 'g03_compare.py', command_factory, out)


def control_check_ruler_exclusive_write(scratch):
    source = scratch / 'source'
    source.mkdir()
    out = scratch / 'ruler.json'
    exclusive_write_cycle(scratch, 'check_ruler.py',
                           ['scripts/parity/check_ruler.py', '--self-compare', str(scratch / 'absent.json'),
                            '--control-source', str(source), '--json', str(out)], [out])


def control_run_all_exclusive_write(scratch):
    source = scratch / 'source'
    source.mkdir()
    out = scratch / 'controls.json'
    exclusive_write_cycle(scratch, 'controls/run_all.py',
                           ['scripts/parity/controls/run_all.py', '--source', str(source),
                            '--json', str(out)], [out])


def baseline_dir():
    """Same candidate order as g03_compare.baseline_path, for building the
    synthetic self_compare corpus (the catalog sids come from the baseline)."""
    configured = os.environ.get('PDRS_BASELINE')
    if configured and Path(configured).is_dir():
        return Path(configured).resolve()
    local = root / 'resources/PilotDeck'
    if local.is_dir():
        return local.resolve()
    common = subprocess.run(['git', '-C', str(root), 'rev-parse', '--path-format=absolute',
                             '--git-common-dir'], capture_output=True, text=True)
    if common.returncode == 0 and common.stdout.strip():
        candidate = Path(common.stdout.strip()).parent / 'resources/PilotDeck'
        if candidate.is_dir():
            return candidate.resolve()
    raise RuntimeError('#2399 PilotDeck baseline not discoverable for the self_compare corpus')


def build_self_compare_corpus(scratch):
    scenarios = json.loads((baseline_dir() / 'tools/agent-loop-parity/scenarios.json')
                           .read_text(encoding='utf-8'))['scenarios']
    sids = [s['scenarioId'] for s in scenarios if 'pilotdeck' in s.get('pairs', [])]
    if len(sids) != 53:
        raise RuntimeError(f'#2398 expected 53 pilotdeck scenarios, found {len(sids)}')
    corpus = scratch / 'corpus'
    corpus.mkdir()
    (corpus / 'progress.txt').write_text('r1 r2 r3 r4 r5 DONE\n', encoding='utf-8')
    for number in range(1, 6):
        directory = corpus / f'r{number}'
        directory.mkdir()
        (directory / 'summary.json').write_text(
            json.dumps({'scenarios': 53, 'blocked': [], 'oracleFailures': [],
                        'provenance': 'synthetic-2398-negative-control'}) + '\n', encoding='utf-8')
        for sid in sids:
            (directory / f'{sid}.pilotdeck-current-sidecar.jsonl').write_text(
                json.dumps({'kind': 'terminal', 'scenarioId': sid, 'q': 1, 'sequence': 0}) + '\n',
                encoding='utf-8')
    return corpus


def control_self_compare_exclusive_write(scratch):
    corpus = build_self_compare_corpus(scratch)
    out = scratch / 'self-compare.json'
    markdown = scratch / 'self-compare.md'
    exclusive_write_cycle(scratch, 'self_compare.py',
                           ['scripts/parity/self_compare.py', '--source', str(corpus),
                            '--json', str(out), '--markdown', str(markdown)], [out, markdown])


RUNNERS = {
    'control_check_frozen_hostile_env': control_check_frozen_hostile_env,
    'control_check_frozen_hostile_config': control_check_frozen_hostile_config,
    'control_g03_hostile_env': control_g03_hostile_env,
    'control_g03_hostile_config': control_g03_hostile_config,
    'control_g03_git_timeout': control_g03_git_timeout,
    'control_g03_exclusive_write': control_g03_exclusive_write,
    'control_score_exclusive_write': control_score_exclusive_write,
    'control_check_ruler_exclusive_write': control_check_ruler_exclusive_write,
    'control_run_all_exclusive_write': control_run_all_exclusive_write,
    'control_self_compare_exclusive_write': control_self_compare_exclusive_write,
}


def scratch_dir(out, name):
    directory = out / name
    directory.mkdir(parents=True)
    return directory


def main():
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py <fresh output directory> [mutation names...]')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = sys.argv[2:]
    chosen = [item for item in mutations if not selected or item[0] in selected]
    if selected and set(selected) != {item[0] for item in chosen}:
        raise SystemExit('#2398 unknown mutation name')
    results = []
    for name, path, before, after, control_name in chosen:
        control = RUNNERS[control_name]
        status, reason, compiled = 'INVALID', 'not run', False
        try:
            with mutating_sources(root, [path]) as originals:
                text = originals[path].decode()
                if text.count(before) != 1:
                    raise RuntimeError(f'#2398 stale anchor {name}: expected 1, found {text.count(before)}')
                control(scratch_dir(out, name + '-positive'))  # must pass on the unmutated source
                mutated = text.replace(before, after, 1)
                compile(mutated, str(path), 'exec')
                compiled = True
                path.write_text(mutated)
                try:
                    control(scratch_dir(out, name + '-negative'))
                except AssertionError as error:
                    message = str(error)
                    if '#2398' not in message:
                        raise RuntimeError(f'control asserted without its marker: {message}')
                    status, reason = 'KILLED', message
                else:
                    status, reason = 'SURVIVED', 'control passed on the mutant'
            control(scratch_dir(out, name + '-restored'))  # restoration verified on the live tree
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
        results.append(dict(name=name, status=status, reason=reason, compiled=compiled))
        print(f'{name}: {status}: {reason}', flush=True)
    for name, control_name in positives:
        control = RUNNERS[control_name]
        status, reason = 'FAIL', 'not run'
        try:
            control(scratch_dir(out, name))
            status, reason = 'PASS', 'hardening observably in force on the committed tree'
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        results.append(dict(name=name, status=status, reason=reason, compiled=False))
        print(f'{name}: {status}: {reason}', flush=True)
    counts = {}
    for row in results:
        counts[row['status']] = counts.get(row['status'], 0) + 1
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps({'controls': results, 'counts': counts},
                                    ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    temporary.replace(out / 'summary.json')
    print(json.dumps(counts), flush=True)
    bad = [row for row in results
           if (row['status'] not in ('KILLED', 'PASS'))]
    return int(bool(bad) or not results)


if __name__ == '__main__':
    raise SystemExit(main())
