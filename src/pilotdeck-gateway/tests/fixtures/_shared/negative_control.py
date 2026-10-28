# anchor-selfcheck: unsupported shared mutation runner, no mutation definitions
"""Bounded negative controls: mutate only disposable /tmp copies."""
import contextlib
import difflib
import json
import math
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'


@contextlib.contextmanager
def scratch(label, *, deadline=580):
    """SIGTERM/INT/deadline unwind child waits before deleting the directory."""
    def stop(signum, frame):
        raise SystemExit(f'interrupted by signal {signum}')

    signals = (signal.SIGTERM, signal.SIGINT, signal.SIGALRM)
    previous = {sig: signal.signal(sig, stop) for sig in signals}
    signal.alarm(deadline)
    root = None
    status = subprocess.check_output(['git', 'status', '--short'], cwd=REPO)
    try:
        with tempfile.TemporaryDirectory(prefix=f'pdrs222-{label}-', dir='/tmp') as name:
            root = Path(name)
            print(f'temporary directory: {root}', flush=True)
            yield root
    finally:
        signal.alarm(0)
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        if root is not None:
            assert not root.exists(), f'temporary directory leaked: {root}'
            print(f'temporary directory removed: {root}', flush=True)
        after = subprocess.check_output(['git', 'status', '--short'], cwd=REPO)
        assert after == status, 'negative control changed git status'
        print('git status --short unchanged (including pending repair edits)', flush=True)


def environment(root):
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG=os.environ.get('CARGO_PROFILE_DEV_DEBUG', 'line-tables-only'),
               CARGO_PROFILE_TEST_DEBUG=os.environ.get('CARGO_PROFILE_TEST_DEBUG', 'line-tables-only'),
               CARGO_INCREMENTAL='0', RUSTC_WRAPPER='', TMPDIR=str(root))
    for key in ('PDRS_RECORDER_ORACLE', 'PDRS_SESSION_DOMAIN_ORACLE',
                'PDRS_TRANSCRIPT_ADAPTER_ORACLE'):
        env.pop(key, None)
    return env


def run(name, command, env, cwd=SRC, diagnostic=None):
    if command[0] == 'cargo':
        deadline = time.monotonic() + 120
        while True:
            slot = subprocess.run([str(REPO / 'scripts/cargo-slots.sh')],
                                  capture_output=True, text=True, timeout=10)
            print(slot.stdout + slot.stderr, end='', flush=True)
            if slot.returncode == 0:
                break
            assert slot.returncode == 1 and time.monotonic() < deadline, 'cargo slots full'
            time.sleep(10)
    print(f'--- {name} ---', flush=True)
    # A file avoids pipe backpressure; the complete raw output is printed before
    # cleanup, so the caller can retain evidence outside the disposable directory.
    log_path = Path(env['TMPDIR']) / 'child.log'
    with log_path.open('w') as log:
        child = subprocess.Popen(command, cwd=cwd, env=env, stdout=log,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = child.wait(timeout=240)
        except BaseException:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            raise
    output = log_path.read_text()
    print(output, end='', flush=True)
    print(f'{name}: exit={code}; pid={child.pid}; reaped={child.poll() is not None}', flush=True)
    if command[0] == 'cargo':
        assert 'running 1 test' in output, 'compilation or zero-test failure is not evidence'
        expected = 'test result: FAILED' if diagnostic else 'test result: ok. 1 passed'
        assert expected in output, name
    if diagnostic:
        assert code != 0 and diagnostic in output, name
        print(f'{name}: KILLED', flush=True)
    else:
        assert code == 0, name
        print(f'{name}: PASS', flush=True)


def source_copy(root):
    """Copy tracked workspace inputs, including pending fixes, never target/.env."""
    files = subprocess.check_output(['git', 'ls-files', '-z', 'src'], cwd=REPO)
    for entry in files.split(b'\0'):
        if not entry:
            continue
        path = Path(os.fsdecode(entry))
        destination = root / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(REPO / path, destination)
    shutil.copyfile(REPO / 'LICENSE', root / 'LICENSE')
    # rustup must keep the repository toolchain when Cargo runs in /tmp.
    shutil.copyfile(REPO / 'rust-toolchain.toml', root / 'rust-toolchain.toml')
    return root / 'src'


# Opt-in #994 API. Legacy run/environment/scratch defaults above remain intact.
COMPILE_IDLE_SECONDS = 900
BASELINE_RUN_SECONDS = 900
RUN_FACTOR = 5
MIN_RUN_SECONDS = 20


def adaptive_timeout(seconds):
    if not math.isfinite(seconds) or seconds < 0:
        raise ValueError('#994 invalid baseline duration')
    return max(MIN_RUN_SECONDS, seconds * RUN_FACTOR)


def persistent_environment(directory):
    """Use the caller's persistent negative target, never the product target."""
    env = environment(directory)
    target = Path(env['CARGO_TARGET_DIR']).expanduser().resolve()
    forbidden = (REPO / '_build/cargo-target', SRC / 'target')
    if any(target == path.resolve() or path.resolve() in target.parents
           for path in forbidden):
        raise ValueError('#994 supply a separate persistent CARGO_TARGET_DIR, e.g. '
                         '<tree>/_build/negctl-target')
    if target == Path(directory).resolve() or Path(directory).resolve() in target.parents:
        raise ValueError('#994 negative target must outlive the scratch directory')
    env.update(CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL='1')
    return env


class CargoTests:
    """Build once per invocation, then time only the selected libtest binary.

    Commands deliberately name exactly one target; selection/libtest options
    are separate, so none can leak into --no-run or accidentally select zero
    tests. All phases, including restored, rebuild using the same target.
    """

    def __init__(self, source, output, env):
        self.source, self.output, self.env = Path(source), Path(output), env
        self.baselines = {}
        self.compiled = {}

    def save(self, name, value):
        with (self.output / name).open('x') as stream:
            stream.write(value)

    def phase(self, label, command, *, timeout, idle_timeout=None, cwd=None):
        started = time.monotonic()
        result = None
        try:
            result = run_owned(command, cwd=cwd or self.source, env=self.env,
                               timeout=timeout, idle_timeout=idle_timeout)
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                raw = ''.join(value.decode(errors='replace') if isinstance(value, bytes)
                              else value or '' for value in (error.stdout, error.stderr))
            self.save(label + '.log', raw + f'\n{type(error).__name__}: {error}\n')
            raise
        else:
            self.save(label + '.log', result.stdout + result.stderr)
            return result, time.monotonic() - started
        finally:
            self.save(label + '.json', json.dumps({
                'command': command, 'cwd': str(cwd or self.source),
                'target': self.env['CARGO_TARGET_DIR'], 'timeout': timeout,
                'idleTimeout': idle_timeout, 'elapsedSeconds': time.monotonic() - started,
                'returncode': result.returncode if result is not None else None,
            }, indent=2) + '\n')

    def run(self, label, target, arguments, *, positive=False, restored=False):
        """target is ('--lib',) or ('--test', name); arguments go to libtest."""
        if target != ('--lib',) and not (len(target) == 2 and target[0] == '--test'):
            raise ValueError('#994 expected exactly one Cargo test target')
        key = (tuple(target), tuple(arguments))
        self.compiled[label] = False
        if not positive and not restored and key not in self.baselines:
            raise ValueError('#994 no passing unmutated baseline for this selection')
        slot = wait_for_slot(REPO / 'scripts/cargo-slots.sh', timeout=900)
        self.save(label + '.slots.log', slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', *target,
                   '--no-run', '--message-format=json']
        compiled, _ = self.phase(label + '-compile', command, timeout=None,
                                 idle_timeout=COMPILE_IDLE_SECONDS)
        if compiled.returncode:
            raise RuntimeError('#994 compilation failed; not a killed mutation')
        expected = 'pilotdeck_gateway' if target == ('--lib',) else target[1]
        artifacts = set()
        finished = False
        for line in compiled.stdout.splitlines():
            row = json.loads(line)
            if row.get('reason') == 'build-finished':
                finished = row.get('success') is True
            if (row.get('reason') == 'compiler-artifact'
                    and row.get('target', {}).get('name') == expected
                    and row.get('profile', {}).get('test') is True
                    and row.get('executable')):
                artifacts.add(row['executable'])
        if not finished or len(artifacts) != 1:
            raise RuntimeError('#994 missing/ambiguous current Cargo test artifact')
        executable = Path(artifacts.pop()).resolve()
        target_dir = Path(self.env['CARGO_TARGET_DIR']).resolve()
        if target_dir not in executable.parents or not executable.is_file():
            raise RuntimeError('#994 test artifact missing or outside caller target')
        self.compiled[label] = True
        limit = (BASELINE_RUN_SECONDS if key not in self.baselines
                 else adaptive_timeout(self.baselines[key]))
        result, elapsed = self.phase(label + '-run', [str(executable), *arguments],
                                     cwd=self.source / 'pilotdeck-gateway', timeout=limit)
        # The caller also checks the fixture's exact positive cardinality.
        if positive and result.returncode == 0 and 'test result: ok.' in result.stdout:
            self.baselines[key] = elapsed
        return result


def mutation_suite(label, rows, output, selection, passed, classify, *, guarded=()):
    """HEAD-guarded scratch mutations; account for every row, including errors.

    rows: (name, real source path, old, new, selections). Source writes are only
    to the disposable copy. The existing source guard owns the original bytes;
    no synthetic Git repository or alternate baseline is involved.
    """
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    records = []
    interrupted = None

    def publish():
        counts = {state: sum(row['status'] == state for row in records)
                  for state in ('KILLED', 'SURVIVED', 'INVALID')}
        # Atomic replacement also makes interrupted summaries readable.
        pending = output / 'summary.json.tmp'
        pending.write_text(json.dumps({'controls': records, 'counts': counts}, indent=2) + '\n')
        pending.replace(output / 'summary.json')
        return counts

    try:
        paths = list(dict.fromkeys([*(row[1] for row in rows), *guarded]))
        with mutating_sources(REPO, paths) as snapshots:
            with scratch(label, deadline=0) as directory:
                env = persistent_environment(directory)
                env['PDRS_553_NEGATIVE_SPILL_PATH'] = str(directory / 'unconfigured.log')
                source = source_copy(directory)
                runner = CargoTests(source, output, env)
                print(f'persistent target: {env["CARGO_TARGET_DIR"]}', flush=True)
                for name, path, old, new, tests in rows:
                    item = dict(name=name, status='INVALID', reason='not run',
                                compiled=False, restoredPass=False, selections=[])
                    original = snapshots[path]
                    copied = directory / path.relative_to(REPO)
                    try:
                        text = original.decode()
                        if text.count(old) != 1 or old == new or not tests:
                            raise ValueError('#994 stale/invalid mutation anchor or empty selections')
                        changed = text.replace(old, new, 1)
                        runner.save(name + '.patch', ''.join(difflib.unified_diff(
                            text.splitlines(True), changed.splitlines(True),
                            fromfile=str(path.relative_to(REPO)), tofile=str(path.relative_to(REPO)))))
                        for index, test in enumerate(tests):
                            result = runner.run(f'{name}-{index}-positive', *selection(test), positive=True)
                            if not passed(result, test):
                                raise RuntimeError(f'#994 positive selection did not pass: {test}')
                        copied.write_text(changed)
                        for index, test in enumerate(tests):
                            try:
                                result = runner.run(f'{name}-{index}-negative', *selection(test))
                                status, reason = classify(name, result, test)
                            except Exception as error:
                                status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                            item['selections'].append(dict(test=test, status=status, reason=reason,
                                compiled=runner.compiled.get(f'{name}-{index}-negative', False)))
                        item['compiled'] = all(row['compiled'] for row in item['selections'])
                        states = [row['status'] for row in item['selections']]
                        item['status'] = ('INVALID' if 'INVALID' in states else
                                          'SURVIVED' if 'SURVIVED' in states else 'KILLED')
                        item['reason'] = '; '.join(row['reason'] for row in item['selections'])
                    except Exception as error:
                        item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                    finally:
                        copied.write_bytes(original)
                        if copied.read_bytes() != original:
                            raise RuntimeError('#994 scratch restoration failed')
                    # Every mutation, including INVALID, recompiles pristine bytes
                    # in the same target before the successor can use it.
                    restored = True
                    for index, test in enumerate(tests):
                        try:
                            result = runner.run(f'{name}-{index}-restored', *selection(test),
                                                restored=True)
                            if not passed(result, test):
                                raise RuntimeError(f'#994 restored selection did not pass: {test}')
                        except Exception as error:
                            restored = False
                            item['reason'] += f'; restored: {type(error).__name__}: {error}'
                    item['restoredPass'] = restored
                    if not restored:
                        item['status'] = 'INVALID'
                    records.append(item)
                    publish()
                    print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except BaseException as error:
        reason = f'suite/source guard: {type(error).__name__}: {error}'
        for row in records:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for row in rows[len(records):]:
            records.append(dict(name=row[0], status='INVALID', reason=reason,
                                compiled=False, restoredPass=False, selections=[]))
        if not isinstance(error, Exception):
            interrupted = error
    counts = publish()
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()), flush=True)
    if interrupted is not None:
        raise interrupted
    return int(not records or any(row['status'] != 'KILLED' for row in records))
