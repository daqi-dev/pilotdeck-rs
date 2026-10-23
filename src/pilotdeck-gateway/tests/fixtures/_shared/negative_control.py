"""Bounded negative controls: mutate only disposable /tmp copies."""
import contextlib
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[5]
SRC = REPO / 'src'


@contextlib.contextmanager
def scratch(label):
    """SIGTERM/INT/deadline unwind child waits before deleting the directory."""
    def stop(signum, frame):
        raise SystemExit(f'interrupted by signal {signum}')

    signals = (signal.SIGTERM, signal.SIGINT, signal.SIGALRM)
    previous = {sig: signal.signal(sig, stop) for sig in signals}
    signal.alarm(580)
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
    target = Path(os.environ['CARGO_TARGET_DIR']).resolve()
    assert target == SRC / 'target', f'wrong build directory: {target}'
    env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0',
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
    return root / 'src'
