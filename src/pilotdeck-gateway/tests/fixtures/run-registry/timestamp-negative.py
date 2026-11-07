#!/usr/bin/env python3
"""#1129: bind clock and exclusive-write mutations to real TS probe assertions.

#1147: the guarded window reuses negative_suite.run_cases' lifecycle (positive,
mutate, negative, restored per case; #1076 exit contract; atomic summary
publication) with this script's own node command factory and probe judges
injected at call time, sits inside negative_control's scratch signal shield,
and guards the generator's full local import closure discovered with
run-registry/count-negative.py's fail-closed scanner.

Run after committing, with caller-owned CARGO_TARGET_DIR and PDRS_BASELINE.
Run exclusively in this worktree: no concurrent builds, tests or source writers.
The source guard intentionally refuses uncommitted sources and new files.
"""
import importlib.util
import itertools
import os
from pathlib import Path
import subprocess
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / '_shared'))
import negative_suite
from negative_control import scratch
from negative_process import mutating_sources, run_owned
from negative_suite import Case, run_cases

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/tests/fixtures/run-registry/generate.mts'
probe = root / 'src/pilotdeck-gateway/tests/fixtures/run-registry/verify-timestamps.mts'
mutations = [
    ('remove-snapshot-clock', source,
     'globalThis.Date = SnapshotDate;', 'globalThis.Date = NativeDate;',
     'run-registry/verify-timestamps.mts::verifyTimestamps',
     '#1129 same-millisecond recovery masks only the new timestamp'),
    ('remove-clock-restoration', source,
     '  globalThis.Date = NativeDate;', '  // mutant: leave the snapshot clock installed',
     'run-registry/verify-timestamps.mts::verifyTimestamps',
     '#1129 generator restores its caller clock'),
    ('ignore-explicit-date-arguments', source,
     'args.length ? args : [1760745600000 + tick++]', '[1760745600000 + tick++]',
     'run-registry/verify-timestamps.mts::verifyTimestamps',
     '#1129 generator explicit Date arguments stay native'),
    ('overwrite-initial-fixture', source,
     'await copyFile(path, join(output, initialFile), constants.COPYFILE_EXCL);',
     'await copyFile(path, join(output, initialFile), 0);',
     'run-registry/verify-timestamps.mts::verifyExclusiveInitial',
     '#1129 initial copy refuses to overwrite existing fixture'),
    ('overwrite-after-fixture', source,
     'await copyFile(path, join(output, after), constants.COPYFILE_EXCL);',
     'await copyFile(path, join(output, after), 0);',
     'run-registry/verify-timestamps.mts::verifyExclusiveAfter',
     '#1129 after copy refuses to overwrite existing fixture'),
    ('overwrite-oracle-fixture', source,
     "await writeFile(join(output, 'oracle.json'), oracle, { flag: 'wx' });",
     "await writeFile(join(output, 'oracle.json'), oracle, { flag: 'w' });",
     'run-registry/verify-timestamps.mts::verifyExclusiveOracle',
     '#1129 oracle publication refuses to overwrite existing fixture'),
]

# run_cases hardcodes its shared cargo libtest command; the factory below
# receives exactly that shape plus one test name and refuses anything else,
# so a future change in the M6-exclusive suite fails closed here instead of
# being misread as probe output (#1147).
CARGO_PREFIX = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                '-p', 'pilotdeck-gateway', '--lib']
LIBTEST_TAIL = ['--', '--exact', '--include-ignored']


def count_runner():
    """Load run-registry/count-negative.py for its fail-closed closure scanner.

    The dashed module name cannot be imported, and registering it in
    sys.modules is rejected by the anchor selfcheck's dynamic-access rules;
    executing only its module-level definitions needs neither. Its closure
    helpers are byte-synced with recorder/fix3-negative.py by
    test_import_closure.py, so reusing them here cannot drift from
    count-negative (#1147 P2-2).
    """
    path = Path(__file__).resolve().parent / 'count-negative.py'
    spec = importlib.util.spec_from_file_location('run_registry_count_negative', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _lines(text):
    """Record lines by '\n' only; splitlines() also splits U+2028/U+2029 (#1140)."""
    lines = text.split('\n')
    return lines[:-1] if lines and lines[-1] == '' else lines


def probe_passed(result, test):
    return (result.returncode == 0
            and _lines(result.stdout).count(f'TEST: {test}') == 1
            and _lines(result.stdout).count(f'PASS: {test}') == 1)


def probe_classify(result, case):
    if probe_passed(result, case.test):
        return 'SURVIVED', 'bound probe still passed'
    out_lines, err_lines = _lines(result.stdout), _lines(result.stderr)
    if (result.returncode == 1
            and out_lines.count(f'TEST: {case.test}') == 1
            and f'PASS: {case.test}' not in out_lines
            and f'AssertionError [ERR_ASSERTION]: {case.assertion}' in err_lines):
        return 'KILLED', 'bound real TS probe assertion failed'
    return 'INVALID', 'missing bound runtime assertion; never a kill'


def mutate(text, scope, edits):
    for old, new in edits:
        if text.count(old) != 1 or old == new:
            raise ValueError('#1147 stale timestamp mutation anchor')
        text = text.replace(old, new, 1)
    return text


class ProbeFactory:
    """The node command factory injected as negative_suite.run_owned (#1147).

    run_cases resolves run_owned through its module globals at call time and
    hands the factory its own hardcoded cargo libtest command plus one test
    name. Anything but that exact shape means the M6-exclusive shared suite
    drifted, and guessing what to run with it would misjudge every control,
    so the factory refuses instead (fail-closed).
    """

    def __init__(self, out):
        self.out = out
        self.serial = itertools.count()

    def __call__(self, command, *, cwd, env, timeout):
        prefix, tail = len(CARGO_PREFIX), len(LIBTEST_TAIL)
        test = command[prefix] if len(command) == prefix + 1 + tail else None
        if (test is None or command[:prefix] != CARGO_PREFIX
                or command[prefix + 1:] != LIBTEST_TAIL
                or not isinstance(test, str)):
            raise RuntimeError(f'#1147 run_cases command shape drifted: {command}')
        directory = self.out / f'probe-{next(self.serial):02d}'
        directory.mkdir(parents=True, exist_ok=False)
        node = ['node', '--import',
                str(Path(env['PDRS_BASELINE']) / 'node_modules/tsx/dist/loader.mjs'),
                str(probe), str(directory), test.rsplit('::', 1)[1]]
        result = run_owned(node, cwd=cwd, env=env, timeout=timeout)
        (directory / 'node.stdout').write_text(result.stdout)
        (directory / 'node.stderr').write_text(result.stderr)
        # The fresh probe directory carries this run's generated bytes; append
        # its path so run_cases' evidence log records where they went.
        return subprocess.CompletedProcess(
            node, result.returncode,
            result.stdout + f'probe-output: {directory}\n', result.stderr)


def main():
    if sys.argv[1:] == ['--check-anchors']:
        for name, path, old, new, _test, _marker in mutations:
            if path.read_text().count(old) != 1 or old == new:
                raise SystemExit(f'{name}: expired or invalid anchor')
        print(f'{len(mutations)} unique anchors checked')
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: timestamp-negative.py NEW_OUTPUT_DIR | --check-anchors')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
        raise SystemExit('caller must supply CARGO_TARGET_DIR and PDRS_BASELINE')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)

    fixture_root = Path(__file__).resolve().parents[1]
    base_guarded = [source, probe, Path(__file__).resolve(),
                    Path(__file__).resolve().parent / 'count-negative.py',
                    fixture_root / '_shared/negative_control.py',
                    *(fixture_root / name for name in (
                        'negative_process.py', 'negative_suite.py',
                        'negative_panic.py', 'pending_reanchor.py',
                    ))]
    # scratch installs the SIGTERM/SIGINT/SIGALRM -> SystemExit handlers and
    # the deadline before the guarded window opens (#1147 P2-1): a signal then
    # unwinds through run_cases' #1076 handling and every mutating_sources'
    # shielded restore, the tree is verified against HEAD, and run_owned's
    # group kill reaps the session-own cargo/node children.
    with scratch('run-registry-timestamp', deadline=1800):
        # Discover imports only after the entry, probe, runner and this
        # script's judge inputs are checked under the tree lock, then keep
        # that reentrant lock through the closure guard (#1147 P2-2, mirroring
        # count-negative.guarded_inputs). The probe executes the generator in
        # place, so the probe's closure contains the generator's whole local
        # import closure as well.
        with mutating_sources(root, base_guarded):
            closure = count_runner().local_import_closure(probe, root)
            with mutating_sources(root, closure):
                # run_cases resolves run_owned/passed/classify through its
                # module globals at call time, so the node command factory and
                # probe judges are injected here without touching the
                # M6-exclusive shared suite (#1147 P2-3; test_negative_suite's
                # cancellation tests patch the same attributes).
                saved = (negative_suite.run_owned, negative_suite.passed,
                         negative_suite.classify)
                negative_suite.run_owned = ProbeFactory(out)
                negative_suite.passed = probe_passed
                negative_suite.classify = probe_classify
                try:
                    # The timeout is the node probe window; a .mts mutant's
                    # compilation proof is the node run itself (a syntax error
                    # exits nonzero and is INVALID), matching count-negative's
                    # #1139 generator rows: cargo would only compile unmutated
                    # Rust here, so no cargo receipt is recorded.
                    return run_cases(root, [Case(*row) for row in mutations],
                                     out, env, mutate, timeout=150, script=__file__)
                finally:
                    negative_suite.run_owned, negative_suite.passed, \
                        negative_suite.classify = saved


if __name__ == '__main__':
    raise SystemExit(main())
