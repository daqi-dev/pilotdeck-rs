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
from negative_suite import Case, invalidate_completed, publish_summary, run_cases

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


def probe_slot(script, **_kwargs):
    """The no-op cargo gate injected as negative_suite.wait_for_slot (#1147).

    This runner compiles nothing: every probe is a node run and occupies no
    cargo slot, so run_cases' per-run gate wait would spend up to 18 x 900 s
    of budget inside scratch's 1800 s deadline while holding the tree lock,
    for a gate this process can never contend. The gate itself is untouched;
    only this runner's view of it is a no-op, and run_cases still writes the
    (empty) transcript into <label>.slots.log. The shared #1166 scanner's
    cold build, if missing, still queues on negative_process's own slot gate
    inside mutating_sources; this bypass covers only run_cases' per-run wait.
    """
    return subprocess.CompletedProcess([], 0, '', '')


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
    # #1666/#1734: a missing or blank caller target is "never started" (2),
    # not "ran and did not pass" (1). `SystemExit('...')` would exit 1;
    # count-negative's shared environment() refuses the same way.
    if not (env.get('CARGO_TARGET_DIR') or '').strip() \
            or not (env.get('PDRS_BASELINE') or '').strip():
        print('INVALID: caller must supply CARGO_TARGET_DIR and PDRS_BASELINE',
              file=sys.stderr, flush=True)
        return 2
    out = Path(sys.argv[1]).resolve()
    try:
        out.mkdir(parents=True, exist_ok=False)
    except OSError as error:
        # Same canon: an evidence directory that cannot be created (already
        # taken, EACCES, ENOSPC, a non-directory in the path) means "never
        # started"; nothing was measured.
        print(f'INVALID: cannot create output directory {out}: {error}',
              file=sys.stderr, flush=True)
        return 2

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
    # group kill reaps the session-own cargo/node children. Shielded during
    # restore are SIGINT/SIGTERM/SIGHUP/SIGQUIT only: the SIGALRM deadline is
    # NOT deferred through the restore window (registered as #2360 / M6
    # #1099; changing _SHIELDED_SIGNALS touches the M6-exclusive suite).
    try:
        return _guarded_body(env, out, base_guarded)
    except SystemExit as error:
        # Verdicts reach the caller only through `_guarded_body`'s return
        # value; any SystemExit escaping the guarded window is a cancellation
        # or a refuse, never a verdict. scratch's deadline raises SystemExit
        # with a string code (which would exit 1) and mutating_sources'
        # deferred signals raise SystemExit(128 + signum) (which would exit
        # 143/130): both are "never verified" and must exit 2 (#1076),
        # matching count-negative.py's handler. Print the carried code first
        # so the signal diagnosis survives the normalization (#1147 r2).
        if error.code != 2:
            print(f'INVALID: unwound with SystemExit({error.code!r}); exiting 2',
                  file=sys.stderr, flush=True)
        raise SystemExit(2) from error


def _guarded_body(env, out, base_guarded):
    # No `as root`: the guarded window and run_cases take the repository
    # root (module-level `root`), not scratch's temporary directory.
    # Every summary publication is deferred until each guard window has
    # closed: run_cases publishes inside its own (already restored and
    # re-verified) window, but this runner stacks the closure guards and
    # scratch outside it, and a cancellation or close-out failure there must
    # not leave the verdict summary on disk (#1076; #1147 r2). run_cases
    # resolves publish_summary through its module globals at call time - the
    # same injection point as the factory and judges below - so a local
    # wrapper buffers the call instead of reaching the disk.
    verdict, completed, publications = 1, False, []

    def deferred_publish(out_dir, rows):
        publications.append((out_dir, rows))

    error = None
    try:
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
                    # cancellation tests patch the same attributes). wait_for_slot
                    # gets the no-op probe_slot for the same reason (#1147 r1).
                    saved = (negative_suite.run_owned, negative_suite.passed,
                             negative_suite.classify, negative_suite.wait_for_slot,
                             negative_suite.publish_summary)
                    negative_suite.run_owned = ProbeFactory(out)
                    negative_suite.passed = probe_passed
                    negative_suite.classify = probe_classify
                    negative_suite.wait_for_slot = probe_slot
                    negative_suite.publish_summary = deferred_publish
                    try:
                        # The timeout is the node probe window; a .mts mutant's
                        # compilation proof is the node run itself (a syntax error
                        # exits nonzero and is INVALID), matching count-negative's
                        # #1139 generator rows: cargo would only compile unmutated
                        # Rust here, so no cargo receipt is recorded.
                        verdict = run_cases(root, [Case(*row) for row in mutations],
                                            out, env, mutate, timeout=150, script=__file__)
                        completed = True
                    finally:
                        (negative_suite.run_owned, negative_suite.passed,
                         negative_suite.classify, negative_suite.wait_for_slot,
                         negative_suite.publish_summary) = saved
    except BaseException as caught:
        error = caught
    if error is not None and completed:
        # run_cases finished and buffered its verdict, but a guard's
        # close-out failed afterwards: that verdict is unverified, void it
        # before any publication reaches the disk (#1076; count-negative's
        # main does the same on its unwind).
        for _out_dir, rows in publications:
            invalidate_completed(rows, f'source guard: {type(error).__name__}: {error}')
    for out_dir, rows in publications:
        publish_summary(out_dir, rows)
    if error is not None:
        raise error
    return verdict


if __name__ == '__main__':
    raise SystemExit(main())
