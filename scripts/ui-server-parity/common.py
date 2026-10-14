"""Version binding and capture-clock admission; never certify changed apparatus from cache."""
import hashlib
from pathlib import Path
import threading
import time

ROOT = Path(__file__).resolve().parents[2]


def apparatus_hashes():
    files = sorted((ROOT / 'scripts/ui-server-parity').rglob('*.py'))
    files += sorted((ROOT / 'scripts/ui-server-parity').glob('*.mjs'))
    files += [ROOT / 'scripts/ui-server-parity' / name for name in ('recording.rs', 'scenarios.json', 'coverage.json')]
    files += sorted((ROOT / 'specs/ui-server-parity').glob('*.md'))
    files += [ROOT / 'scripts/exp008' / name for name in ('run.py', 'compare.py', 'enumerate_hook.mjs', 'preload.mjs', 'proxy/src/main.rs', 'proxy/Cargo.toml', 'proxy/Cargo.lock')]
    return {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in files}


def source_hashes(baseline, recorder):
    """Capture actual executable/code bytes, not a git revision or current replay files."""
    import shutil
    baseline = Path(baseline)
    files = []
    for name in ('src', 'dist', 'ui/server'):
        directory = baseline / name
        if not directory.is_dir():
            raise ValueError(f'missing baseline code tree: {directory}')
        files.extend(p for p in directory.rglob('*') if p.is_file())
    for name in ('package.json', 'pnpm-lock.yaml', 'ui/package.json'):
        path = baseline / name
        if not path.is_file():
            raise ValueError(f'missing baseline dependency manifest: {path}')
        files.append(path)
    digest = hashlib.sha256()
    for path in sorted(set(files)):
        digest.update(str(path.relative_to(baseline)).encode() + b'\0')
        digest.update(hashlib.sha256(path.read_bytes()).digest())
    node = shutil.which('node')
    if not node:
        raise ValueError('node executable missing')
    return {'node': hashlib.sha256(Path(node).read_bytes()).hexdigest(),
            'baseline': digest.hexdigest(),
            'recorder_proxy': hashlib.sha256(Path(recorder).read_bytes()).hexdigest()}


# Clock violations describe the environment, not DUT behavior (REFUSED / exit 2).
INTERVAL_NS = 250_000_000
MAX_GAP_NS = 2_000_000_000


class ClockRefused(ValueError):
    pass


def validate_clock_check(meta, *, historical=False):
    check = meta.get('clock_regression_check')
    if check is None:
        if historical:
            return 'absent'
        raise ClockRefused('clock_regression_check absent: new captures require clock monitoring')
    if not isinstance(check, dict) or type(check.get('schema')) is not int or check['schema'] != 1:
        raise ClockRefused('invalid clock_regression_check schema')
    samples = check.get('samples')
    if (check.get('status') != 'PASS' or check.get('interval_ns') != INTERVAL_NS
            or check.get('max_gap_ns') != MAX_GAP_NS
            or not isinstance(samples, list) or len(samples) < 2):
        raise ClockRefused('clock regression detected or incomplete clock monitor')
    previous = None
    lower_bound = None
    for i, sample in enumerate(samples):
        if not isinstance(sample, dict):
            raise ClockRefused('invalid clock sample')
        wall, before, after = (sample.get(k) for k in ('wall_ns', 'mono_before_ns', 'mono_after_ns'))
        phase = 'start' if i == 0 else 'end' if i == len(samples) - 1 else 'periodic'
        if (sample.get('phase') != phase or any(type(v) is not int or v <= 0 for v in (wall, before, after))
                or after < before or after - before > MAX_GAP_NS):
            raise ClockRefused('invalid/bracket-delayed clock sample')
        if previous:
            if before <= previous['mono_after_ns'] or after - previous['mono_before_ns'] > MAX_GAP_NS:
                raise ClockRefused('clock monitor gap or nonmonotonic observer clock')
            if wall < previous['wall_ns'] or wall - before < lower_bound:
                raise ClockRefused('wall clock regression relative to monotonic clock')
        lower_bound = max(lower_bound if lower_bound is not None else wall - after, wall - after)
        previous = sample
    if (samples[0]['wall_ns'] // 1_000_000 != meta.get('wall_start_ms')
            or samples[-1]['wall_ns'] // 1_000_000 != meta.get('wall_end_ms')):
        raise ClockRefused('clock samples do not cover capture start/end')
    return 'present'


class ClockMonitor:
    def __init__(self, wall=time.time_ns, monotonic=time.monotonic_ns):
        self.wall, self.monotonic = wall, monotonic
        self.samples = []
        self.stop_event = threading.Event()
        self.error = None

    def sample(self, phase):
        before = self.monotonic()
        wall = self.wall()
        after = self.monotonic()
        self.samples.append(dict(phase=phase, wall_ns=wall, mono_before_ns=before, mono_after_ns=after))

    def start(self):
        self.sample('start')
        self.thread = threading.Thread(target=self._poll, name='b2-clock-monitor', daemon=True)
        self.thread.start()
        return self.samples[0]['wall_ns'] // 1_000_000

    def _poll(self):
        try:
            while not self.stop_event.wait(INTERVAL_NS / 1_000_000_000):
                self.sample('periodic')
        except Exception as error:
            self.error = str(error)

    def finish(self, meta):
        self.stop_event.set()
        self.thread.join()
        self.sample('end')
        meta['wall_end_ms'] = self.samples[-1]['wall_ns'] // 1_000_000
        check = dict(schema=1, interval_ns=INTERVAL_NS, max_gap_ns=MAX_GAP_NS,
                     status='PASS', samples=self.samples)
        meta['clock_regression_check'] = check
        try:
            if self.error:
                raise ClockRefused('clock monitor failed: ' + self.error)
            validate_clock_check(meta)
        except ClockRefused as error:
            check.update(status='REFUSED', reason=str(error))
        return check
