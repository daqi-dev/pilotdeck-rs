#!/usr/bin/env python3
"""#2398 Python-only source mutants; run only after the coordinator commit.

Guards under test (issue #2398, from the #2372 fix-r1 dual-lineage review and
the #2758 r1 dual-lineage review):
  - scripts/parity/check_frozen.py git(), scripts/parity/g03_compare.py
    baseline_path() and scripts/check-frozen-delivery.py git()/git_bytes()
    must never inherit GIT_* from the caller, never read a global gitconfig,
    and always time out.
  - scripts/parity's five result files (score.py, g03_compare.py,
    check_ruler.py, self_compare.py, controls/run_all.py) must be created
    exclusively through G.write_result_exclusive; a later run may never
    overwrite an earlier run's results, a crashed run may never leave a
    0-byte/half-written result at the published path, and self_compare's
    json+markdown pair must be published (or rolled back) as one unit.

Execution environment (r1): this fixture is run by the coordinator on a host
where the PilotDeck baseline is discoverable (resources/ is not checked in,
so a CI checkout cannot run the g03/self_compare controls); it is not a CI
step. mutating_sources may build the workspace scanner with cargo on a cold
tree (negative_process._ensure_workspace_scanner); every cargo launch goes
through the shared slot gate.

Every mutated source equals HEAD before its window opens (mutating_sources);
each mutant compiles before its control runs; stale anchors abort instead of
being recorded as SURVIVED. No shadow repository or HEAD exemption is used.
Positive-only controls (no mutation: the hostile-env equal-output proofs, the
bounded-exit proof, and the five exclusive-write first/second-run cycles) live
in `positives` and are judged PASS/FAIL. Both tables are pinned to the static
rosters EXPECTED_MUTATIONS / EXPECTED_POSITIVES (registered()), so a dropped
or renamed row is a startup refusal, not a smaller green run.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, tree_lock, write_source_atomic
from negative_selection import claim_dir, refuse, registered

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
     'os.link(temporary, path)',
     'os.replace(temporary, path)',
     'control_g03_exclusive_write'),
    ('g03-atomic-publish', root / 'scripts/parity/g03_compare.py',
     '    temporary = _write_result_temporary(path, data)\n'
     '    try:\n'
     '        _publish_exclusive(temporary, path)\n'
     '    finally:\n'
     '        temporary.unlink(missing_ok=True)',
     '    try:\n'
     "        with open(path, 'x', encoding='utf-8') as stream:\n"
     '            stream.write(data)\n'
     '    except FileExistsError:\n'
     "        raise SystemExit(f'#2398 result file already exists: {path} '\n"
     "                         '(an earlier run produced it; remove it or pass a different --json path)')",
     'control_g03_atomic_publish'),
    ('g03-group-rollback', root / 'scripts/parity/g03_compare.py',
     '        for path in published:\n'
     '            path.unlink(missing_ok=True)',
     '        for path in published:\n'
     '            pass',
     'control_g03_group_rollback'),
    ('cfd-git-env-strip', root / 'scripts/check-frozen-delivery.py',
     "env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}",
     'env = dict(os.environ)',
     'control_cfd_hostile_env'),
    ('cfd-git-config-global', root / 'scripts/check-frozen-delivery.py',
     "env['GIT_CONFIG_GLOBAL'] = os.devnull",
     "env['GIT_CONFIG_GLOBAL'] = os.environ.get('GIT_CONFIG_GLOBAL') or os.devnull",
     'control_cfd_hostile_config'),
]

positives = [
    ('check-frozen-hostile-env', 'control_check_frozen_hostile_env'),
    ('check-frozen-hostile-config', 'control_check_frozen_hostile_config'),
    ('g03-hostile-env', 'control_g03_hostile_env'),
    ('g03-hostile-config', 'control_g03_hostile_config'),
    ('g03-git-timeout-bound', 'control_g03_git_timeout'),
    ('score-exclusive-write', 'control_score_exclusive_write'),
    ('g03-exclusive-write-cycle', 'control_g03_exclusive_write'),
    ('g03-atomic-publish', 'control_g03_atomic_publish'),
    ('g03-group-rollback', 'control_g03_group_rollback'),
    ('check-ruler-exclusive-write', 'control_check_ruler_exclusive_write'),
    ('run-all-exclusive-write', 'control_run_all_exclusive_write'),
    ('self-compare-exclusive-write', 'control_self_compare_exclusive_write'),
    ('cfd-hostile-env', 'control_cfd_hostile_env'),
    ('cfd-hostile-config', 'control_cfd_hostile_config'),
]

# Static rosters: the denominators of this run are literals in the source, not
# len(table) — a dropped or renamed row is a startup refusal, never a smaller
# green run (#1666; negative_selection.registered).
EXPECTED_MUTATIONS = [
    'parity-git-env-strip',
    'parity-git-config-global',
    'g03-git-env-strip',
    'g03-git-config-global',
    'g03-git-timeout',
    'g03-exclusive-write',
    'g03-atomic-publish',
    'g03-group-rollback',
    'cfd-git-env-strip',
    'cfd-git-config-global',
]
EXPECTED_POSITIVES = [
    'check-frozen-hostile-env',
    'check-frozen-hostile-config',
    'g03-hostile-env',
    'g03-hostile-config',
    'g03-git-timeout-bound',
    'score-exclusive-write',
    'g03-exclusive-write-cycle',
    'g03-atomic-publish',
    'g03-group-rollback',
    'check-ruler-exclusive-write',
    'run-all-exclusive-write',
    'self-compare-exclusive-write',
    'cfd-hostile-env',
    'cfd-hostile-config',
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
        (scratch / f'{label}.{suffix}').write_text(value, encoding='utf-8')
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
    if 'cannot verify' in clean.stdout + clean.stderr:
        # A clean run that could not reach git proves nothing: the equal-output
        # comparison below would be vacuously green.
        raise AssertionError('#2398 the clean check_frozen run could not reach git; '
                             'the hostile comparison would be vacuous')
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
    if not out.is_file():
        raise AssertionError('#2398 g03_compare did not run to its result write under a '
                             'hostile GIT_CONFIG_GLOBAL')


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
    synthetic self_compare corpus (the catalog sids come from the baseline).
    The rev-parse leg uses the same isolation as the code under test (#2398):
    a hostile caller GIT_DIR must not redirect corpus discovery, and the call
    must be bounded."""
    configured = os.environ.get('PDRS_BASELINE')
    if configured and Path(configured).is_dir():
        return Path(configured).resolve()
    local = root / 'resources/PilotDeck'
    if local.is_dir():
        return local.resolve()
    env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    env['GIT_CONFIG_GLOBAL'] = os.devnull
    common = subprocess.run(['git', '--no-optional-locks', '-c', 'core.fsmonitor=false',
                             '-c', 'core.hooksPath=/dev/null', '-c', 'safe.directory=*',
                             '-C', str(root), 'rev-parse', '--path-format=absolute',
                             '--git-common-dir'], capture_output=True, text=True,
                            env=env, timeout=30)
    if common.returncode == 0 and common.stdout.strip():
        candidate = Path(common.stdout.strip()).parent / 'resources/PilotDeck'
        if candidate.is_dir():
            return candidate.resolve()
    raise RuntimeError('#2398 PilotDeck baseline not discoverable for the self_compare corpus')


def build_self_compare_corpus(scratch):
    scenarios = json.loads((baseline_dir() / 'tools/agent-loop-parity/scenarios.json')
                           .read_text(encoding='utf-8'))['scenarios']
    sids = [s['scenarioId'] for s in scenarios if 'pilotdeck' in s.get('pairs', [])]
    if not sids:
        raise RuntimeError('#2398 no pilotdeck scenarios in the baseline catalog')
    count = len(sids)
    corpus = scratch / 'corpus'
    corpus.mkdir()
    (corpus / 'progress.txt').write_text('r1 r2 r3 r4 r5 DONE\n', encoding='utf-8')
    for number in range(1, 6):
        directory = corpus / f'r{number}'
        directory.mkdir()
        (directory / 'summary.json').write_text(
            json.dumps({'scenarios': count, 'blocked': [], 'oracleFailures': [],
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


# Drivers for the publish-semantics controls: they load the live g03_compare
# module (mutant bytes during a window, pristine bytes otherwise) and exercise
# write_result_exclusive / write_results_exclusive directly, because a CLI run
# that crashes mid-write cannot be staged from the outside.
ATOMIC_DRIVER_SOURCE = '''\
import builtins
import importlib.util
import sys
from pathlib import Path

g03_path, target, mode = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
spec = importlib.util.spec_from_file_location('g03_driver_load', g03_path)
G = importlib.util.module_from_spec(spec)
spec.loader.exec_module(G)

if mode == 'retry':
    # A crashed process is gone; the retry is a fresh process publishing the
    # same path again.
    G.write_result_exclusive(target, '#2398 retry after crash\\n')
    if target.read_text(encoding='utf-8') != '#2398 retry after crash\\n':
        raise AssertionError('#2398 the retry after a crashed write produced wrong bytes')
    print('#2398 atomic retry ok')
    raise SystemExit(0)

real_open = builtins.open


class HalfWriter:
    """#2398: write half the payload, then die — a mid-write crash."""

    def __init__(self, stream):
        self.stream = stream

    def write(self, data):
        self.stream.write(data[:max(1, len(data) // 2)])
        self.stream.flush()
        raise OSError('#2398 simulated mid-write crash')

    def flush(self):
        self.stream.flush()

    def fileno(self):
        return self.stream.fileno()

    def close(self):
        self.stream.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.stream.close()
        return False


def crashing_open(file, mode='r', *args, **kwargs):
    stream = real_open(file, mode, *args, **kwargs)
    return HalfWriter(stream) if 'x' in mode else stream


builtins.open = crashing_open
crashed = False
try:
    G.write_result_exclusive(target, '#2398 atomic publish probe\\n' * 64)
except OSError as error:
    crashed = True
    print(f'crash fired: {error}')
finally:
    builtins.open = real_open
if not crashed:
    raise AssertionError('#2398 the simulated mid-write crash never fired')
if target.exists():
    raise AssertionError(f'#2398 a crashed write left a result file at the published path '
                         f'{target}; the temp-then-link publish is not in force')
print('#2398 atomic crash ok')
'''

GROUP_DRIVER_SOURCE = '''\
import importlib.util
import sys
from pathlib import Path

g03_path, json_target, md_target = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
spec = importlib.util.spec_from_file_location('g03_driver_load', g03_path)
G = importlib.util.module_from_spec(spec)
spec.loader.exec_module(G)

md_target.write_text('stale markdown from an earlier run\\n', encoding='utf-8')
try:
    G.write_results_exclusive([(json_target, 'fresh json\\n'), (md_target, 'fresh md\\n')])
except SystemExit as error:
    if '#2398 result file already exists' not in str(error):
        raise
else:
    raise AssertionError('#2398 the group publish must refuse an existing target loudly')
if json_target.exists():
    raise AssertionError('#2398 the group publish left a half-published pair behind '
                         '(json published, markdown stale); the rollback is not in force')
if md_target.read_text(encoding='utf-8') != 'stale markdown from an earlier run\\n':
    raise AssertionError('#2398 the refused group publish overwrote the stale markdown target')
md_target.unlink()
G.write_results_exclusive([(json_target, 'fresh json\\n'), (md_target, 'fresh md\\n')])
if json_target.read_text(encoding='utf-8') != 'fresh json\\n' or \\
        md_target.read_text(encoding='utf-8') != 'fresh md\\n':
    raise AssertionError('#2398 the retry after a refused group publish produced wrong bytes')
print('#2398 group rollback ok')
'''


def run_driver(scratch, label, source, *arguments):
    driver = scratch / f'{label}-driver.py'
    driver.write_text(source, encoding='utf-8')
    return run_cli(scratch, label, [str(driver), str(root / 'scripts/parity/g03_compare.py'),
                                     *map(str, arguments)])


def control_g03_atomic_publish(scratch):
    """A run that dies mid-write must leave no result file at the published
    path (temp-then-link, never a direct write), and a fresh process retrying
    the same path must succeed. Killed by the g03-atomic-publish mutant."""
    target = scratch / 'g03-crash.json'
    crash = run_driver(scratch, 'atomic-crash', ATOMIC_DRIVER_SOURCE, target, 'crash')
    crash_log = crash.stdout + crash.stderr
    if crash.returncode != 0 or '#2398 atomic crash ok' not in crash_log:
        raise AssertionError(f'#2398 atomic crash driver failed (exit={crash.returncode}): '
                             f'{crash_log.strip()}')
    retry = run_driver(scratch, 'atomic-retry', ATOMIC_DRIVER_SOURCE, target, 'retry')
    retry_log = retry.stdout + retry.stderr
    if retry.returncode != 0 or '#2398 atomic retry ok' not in retry_log:
        raise AssertionError(f'#2398 atomic retry driver failed (exit={retry.returncode}): '
                             f'{retry_log.strip()}')


def control_g03_group_rollback(scratch):
    """A group publish refused on its second member must roll back the first,
    and the refused group must not overwrite the stale target. Killed by the
    g03-group-rollback and g03-exclusive-write mutants."""
    json_target = scratch / 'self-compare.json'
    md_target = scratch / 'self-compare.md'
    result = run_driver(scratch, 'group', GROUP_DRIVER_SOURCE, json_target, md_target)
    log = result.stdout + result.stderr
    if result.returncode != 0 or '#2398 group rollback ok' not in log:
        raise AssertionError(f'#2398 group rollback driver failed (exit={result.returncode}): '
                             f'{log.strip()}')


# The delivery precheck spawns the three domain checkers as children; the
# gateway/ui children reach their own apparatus git calls (apparatus.py,
# _guard.py), which are separate registered devices outside this round's
# scope. The cfd controls therefore compare only the tool's OWN output: the
# root/base header lines, the head/discovery/base sections its hardened
# git()/git_bytes() produce, and the event-files listing. The final
# "BLOCKED n / READY m" verdict is excluded as well: it aggregates the child
# domain verdicts, which legitimately differ under a hostile environment.
CFD_CHILD_SECTIONS = ('gateway-parity', 'parity', 'ui-server-parity')


def cfd_own_sections(text):
    kept, skip = [], False
    for line in text.splitlines():
        if line.startswith('=== '):
            skip = line[len('=== '):].split(' ===')[0] in CFD_CHILD_SECTIONS
        if line.startswith('BLOCKED ') and ' / READY ' in line:
            continue
        if not skip:
            kept.append(line)
    return '\n'.join(kept)


def compare_cfd(scratch, extra):
    clean = run_cli(scratch, 'clean', ['scripts/check-frozen-delivery.py', '--root', '.'])
    hostile = run_cli(scratch, 'hostile', ['scripts/check-frozen-delivery.py', '--root', '.'],
                      env=child_env(extra), timeout=300)
    if '=== event files added vs merge-base ===' not in clean.stdout:
        # The tool never got past its own head/discovery/base git sections (a
        # healthy tree stopping earlier is a fixture problem, not a pass).
        raise AssertionError('#2398 the clean delivery-precheck run never reached its own '
                             'git sections; the hostile comparison would be vacuous')
    if (cfd_own_sections(clean.stdout), clean.stderr) != (cfd_own_sections(hostile.stdout),
                                                          hostile.stderr):
        raise AssertionError('#2398 hostile git environment changed the delivery precheck\'s '
                             'own sections (head/discovery/base/event listing); GIT_* '
                             'isolation or GIT_CONFIG_GLOBAL pinning is not in force')


def control_cfd_hostile_env(scratch):
    fake_repo = scratch / 'fake-repo'  # deliberately not a repository
    compare_cfd(scratch, {'GIT_DIR': str(fake_repo), 'GIT_WORK_TREE': str(scratch),
                          'GIT_INDEX_FILE': str(scratch / 'fake-index')})


def control_cfd_hostile_config(scratch):
    bad = scratch / 'bad-gitconfig'
    bad.write_text(BAD_GITCONFIG, encoding='utf-8')
    compare_cfd(scratch, {'GIT_CONFIG_GLOBAL': str(bad)})


RUNNERS = {
    'control_check_frozen_hostile_env': control_check_frozen_hostile_env,
    'control_check_frozen_hostile_config': control_check_frozen_hostile_config,
    'control_g03_hostile_env': control_g03_hostile_env,
    'control_g03_hostile_config': control_g03_hostile_config,
    'control_g03_git_timeout': control_g03_git_timeout,
    'control_g03_exclusive_write': control_g03_exclusive_write,
    'control_g03_atomic_publish': control_g03_atomic_publish,
    'control_g03_group_rollback': control_g03_group_rollback,
    'control_score_exclusive_write': control_score_exclusive_write,
    'control_check_ruler_exclusive_write': control_check_ruler_exclusive_write,
    'control_run_all_exclusive_write': control_run_all_exclusive_write,
    'control_self_compare_exclusive_write': control_self_compare_exclusive_write,
    'control_cfd_hostile_env': control_cfd_hostile_env,
    'control_cfd_hostile_config': control_cfd_hostile_config,
}


def scratch_dir(out, name):
    directory = out / name
    directory.mkdir(parents=True)
    return directory


def main():
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py <fresh output directory> [mutation names...]')
    try:
        registered(mutations, EXPECTED_MUTATIONS)
        registered(positives, EXPECTED_POSITIVES)
    except ValueError as error:
        refuse(f'control roster drifted: {error}')
    out = claim_dir(Path(sys.argv[1]).resolve())
    selected = sys.argv[2:]
    if len(set(selected)) != len(selected):
        refuse(f'repeated mutation name in the selection: {selected}')
    chosen = [item for item in mutations if not selected or item[0] in selected]
    if selected and set(selected) != {item[0] for item in chosen}:
        refuse('#2398 unknown mutation name')
    results = []
    # The whole run (mutation windows, restored checks, positive checks) holds
    # the same-tree lock: every control reads or executes the live tree, and a
    # concurrent runner between windows would judge moving bytes.
    with tree_lock(root):
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
                    write_source_atomic(root, path, mutated.encode())
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
