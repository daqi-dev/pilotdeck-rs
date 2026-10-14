"""Version binding and capture-clock admission; never certify changed apparatus from cache."""
# Isolated bootstrap: verify the source loader before executing local code.
import os as _boot_os
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
import hashlib as _boot_hashlib
import subprocess as _boot_subprocess
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_os.path.isfile(_boot_manifest):
        with open(_boot_manifest) as _boot_file:
            _boot_pins = [line.split()[0] for line in _boot_file
                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]
        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:
            raise ValueError('apparatus bootstrap hash mismatch')
    else:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import hashlib
import os
from pathlib import Path
import threading
import time
from apparatus import scan, SPEC_FILES, EXP008_FILES, toolchain_files, ApparatusUnsafe

ROOT = Path(__file__).resolve().parents[2]


def apparatus_hashes():
    files = [Path(p) for p in scan(ROOT / 'scripts/ui-server-parity')]
    files += [Path(p) for p in scan(ROOT / 'scripts/exp008')]
    # The approval/hash manifest is not apparatus: filling its pending review
    # fields must not invalidate the captures used to approve this very ruler.
    files += [ROOT / 'specs/ui-server-parity' / name for name in SPEC_FILES]
    files += toolchain_files(ROOT)
    return {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in files}


def dependency_roots(baseline):
    """Inventory physical trees once; never recurse through directory symlinks."""
    import fnmatch
    import json
    baseline = Path(baseline).resolve()
    node_ancestor_admission(baseline)
    modules, packages, links, directories = set(), set(), [], []

    def discover(directory):
        with os.scandir(directory) as entries:
            for entry in entries:
                path = Path(entry.path)
                if entry.name == 'node_modules':
                    modules.add(path)
                if entry.is_symlink():
                    links.append(path)
                elif entry.is_dir(follow_symlinks=False):
                    directories.append(path)
                    discover(path)

    discover(baseline)
    top = baseline / 'node_modules'
    if not top.is_dir() or top.is_symlink():
        raise ApparatusUnsafe('missing/nonregular baseline node_modules tree: ' + str(top))
    manifest = baseline / 'package.json'
    if manifest.exists():
        workspaces = json.loads(manifest.read_text()).get('workspaces', [])
        if isinstance(workspaces, dict):
            workspaces = workspaces.get('packages', [])
        if not isinstance(workspaces, list) or any(not isinstance(p, str) for p in workspaces):
            raise ApparatusUnsafe('invalid baseline workspaces declaration')
        for pattern in workspaces:
            if Path(pattern.lstrip('!')).is_absolute() or '..' in Path(pattern).parts:
                raise ApparatusUnsafe('escaping workspace pattern: ' + pattern)
        def workspace_match(parts, pattern):
            # '*' matches one segment; '**' matches zero or more segments.
            if not pattern:
                return not parts
            if pattern[0] == '**':
                return (workspace_match(parts, pattern[1:]) or
                        bool(parts) and workspace_match(parts[1:], pattern))
            return (bool(parts) and fnmatch.fnmatchcase(parts[0], pattern[0])
                    and workspace_match(parts[1:], pattern[1:]))

        for path in directories:
            relative = path.relative_to(baseline).parts
            if (any(workspace_match(relative, Path(p).parts) for p in workspaces if not p.startswith('!'))
                    and not any(workspace_match(relative, Path(p[1:]).parts) for p in workspaces if p.startswith('!'))
                    and (path / 'package.json').is_file()):
                packages.add(path)
    # Actual links cover undeclared local/file: packages too. pnpm store targets
    # already live in node_modules; outside that tree include the entire package.
    for link in links:
        target = link.resolve()
        if (target != baseline and target.is_relative_to(baseline) and target.is_dir()
                and 'node_modules' not in target.relative_to(baseline).parts
                and (target / 'package.json').is_file()):
            packages.add(target)
    roots = modules | packages
    effective = [p for p in sorted(roots) if not any(parent in roots for parent in p.parents)]
    return {'node_modules': sorted(str(p.relative_to(baseline)) for p in modules),
            'workspace_packages': sorted(str(p.relative_to(baseline)) for p in packages),
            'covered_roots': [str(p.relative_to(baseline)) for p in effective]}


def node_ancestor_admission(baseline):
    """Optional Node dependencies can otherwise resolve outside the baseline."""
    ancestors = [p / 'node_modules' for p in Path(baseline).resolve().parents
                 if os.path.lexists(p / 'node_modules')]
    if ancestors:
        warning = 'WARNING: Node ancestor node_modules forbidden: ' + ', '.join(map(str, ancestors))
        print(warning, file=_boot_sys.stderr)
        raise ApparatusUnsafe(warning)


def node_modules_hash(baseline, *, coverage=None):
    """One framed digest for all dependency roots, types, bytes and link targets.

    Inventory all depths, then hash the union once (nested roots are not reread).
    No metadata cache: every invocation rereads every covered ordinary file.
    """
    import json
    import stat
    baseline = Path(baseline).resolve()
    digest = hashlib.sha256()

    def frame(value):
        digest.update(len(value).to_bytes(8, 'big'))
        digest.update(value)

    def visit(path):
        mode = path.lstat().st_mode
        frame(os.fsencode(path.relative_to(baseline)))
        if stat.S_ISLNK(mode):
            frame(b'symlink')
            frame(os.fsencode(os.readlink(path)))
            target = path.resolve()
            frame(os.fsencode(target.relative_to(baseline) if target.is_relative_to(baseline) else target))
        elif stat.S_ISDIR(mode):
            frame(b'directory')
            with os.scandir(path) as entries:
                for entry in sorted(entries, key=lambda e: os.fsencode(e.name)):
                    visit(Path(entry.path))
        elif stat.S_ISREG(mode):
            frame(b'file')
            with path.open('rb') as source:
                frame(hashlib.file_digest(source, 'sha256').digest())
        else:
            raise ApparatusUnsafe('nonregular node_modules entry: ' + str(path))

    try:
        inventory = dependency_roots(baseline)
        frame(b'baseline-dependencies-v2')
        frame(json.dumps(inventory, sort_keys=True, separators=(',', ':')).encode())
        for relative in inventory['covered_roots']:
            visit(baseline / relative)
        if coverage is not None:
            coverage.update(inventory)
    except (OSError, RuntimeError, ValueError) as error:
        raise ApparatusUnsafe('node_modules tree unreadable/changed: ' + str(error)) from error
    return digest.hexdigest()


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
            'node_modules': node_modules_hash(baseline),
            'recorder_proxy': hashlib.sha256(Path(recorder).read_bytes()).hexdigest()}


# Clock violations describe the environment, not DUT behavior (REFUSED / exit 2).
INTERVAL_NS = 250_000_000
MAX_GAP_NS = 2_000_000_000
OFFSET_DROP_TOLERANCE_NS = 1_000_000
MAX_BRACKET_NS = 1_000_000
MAX_BRACKET_ATTEMPTS = 8


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
    if check.get('status') == 'REFUSED':
        raise ClockRefused('clock monitor REFUSED: ' + str(check.get('reason', 'unspecified clock failure')))
    if (check.get('status') != 'PASS' or check.get('interval_ns') != INTERVAL_NS
            or check.get('max_gap_ns') != MAX_GAP_NS
            or not isinstance(samples, list) or len(samples) < 2):
        raise ClockRefused('clock regression detected or incomplete clock monitor')
    previous = None
    for i, sample in enumerate(samples):
        if not isinstance(sample, dict):
            raise ClockRefused('invalid clock sample')
        wall, before, after = (sample.get(k) for k in ('wall_ns', 'mono_before_ns', 'mono_after_ns'))
        phase = 'start' if i == 0 else 'end' if i == len(samples) - 1 else 'periodic'
        if (sample.get('phase') != phase or any(type(v) is not int or v <= 0 for v in (wall, before, after))
                or after < before or after - before > MAX_BRACKET_NS):
            raise ClockRefused('invalid/bracket-delayed clock sample')
        if previous:
            if after - before + previous['mono_after_ns'] - previous['mono_before_ns'] > MAX_BRACKET_NS:
                raise ClockRefused('adjacent clock bracket uncertainty exceeds 1 ms')
            if before <= previous['mono_after_ns'] or after - previous['mono_before_ns'] > MAX_GAP_NS:
                raise ClockRefused('clock monitor gap or nonmonotonic observer clock')
            if wall < previous['wall_ns']:
                raise ClockRefused('wall clock regression between adjacent samples')
            # Compare adjacent brackets, never a running maximum: a normal
            # negative slew may accumulate far more than 1 ms over a capture.
            previous_lower = previous['wall_ns'] - previous['mono_after_ns']
            if previous_lower - (wall - before) > OFFSET_DROP_TOLERANCE_NS:
                raise ClockRefused('wall clock regression relative to monotonic clock')
        previous = sample
    if (samples[0]['wall_ns'] // 1_000_000 != meta.get('wall_start_ms')
            or samples[-1]['wall_ns'] // 1_000_000 != meta.get('wall_end_ms')):
        raise ClockRefused('clock samples do not cover capture start/end')
    rejected = check.get('rejected_samples', [])
    if not isinstance(rejected, list):
        raise ClockRefused('invalid rejected clock samples')
    for sample in rejected:
        if (not isinstance(sample, dict)
                or any(type(sample.get(k)) is not int or sample[k] <= 0
                       for k in ('wall_ns', 'mono_before_ns', 'mono_after_ns'))
                or sample['mono_after_ns'] < sample['mono_before_ns']):
            raise ClockRefused('invalid rejected clock sample')
    # Broad brackets cannot support the offset test, but their wall reads still
    # prove a rollback. Recompute that fact even if archived status says PASS.
    observed = sorted(samples + rejected, key=lambda s: s['mono_before_ns'])
    if any(after['wall_ns'] < before['wall_ns'] for before, after in zip(observed, observed[1:])):
        raise ClockRefused('wall clock regression including rejected brackets')
    return 'present'


class ClockMonitor:
    def __init__(self, wall=time.time_ns, monotonic=time.monotonic_ns):
        self.wall, self.monotonic = wall, monotonic
        self.samples = []
        self.rejected_samples = []
        self.stop_event = threading.Event()
        self.error = None

    def sample(self, phase):
        previous_width = (self.samples[-1]['mono_after_ns'] - self.samples[-1]['mono_before_ns']
                          if self.samples else 0)
        previous_wall = max((s['wall_ns'] for s in self.samples[-1:] + self.rejected_samples[-1:]), default=None)
        for _ in range(MAX_BRACKET_ATTEMPTS):
            before = self.monotonic()
            wall = self.wall()
            after = self.monotonic()
            sample = dict(phase=phase, wall_ns=wall, mono_before_ns=before, mono_after_ns=after)
            if any(type(v) is not int or v <= 0 for v in (wall, before, after)) or after < before:
                raise ClockRefused('invalid/nonmonotonic clock bracket')
            if previous_wall is not None and wall < previous_wall:
                self.rejected_samples.append({**sample, 'reason': 'wall clock regression between adjacent samples'})
                raise ClockRefused('wall clock regression between adjacent samples (including rejected brackets)')
            previous_wall = wall
            # Keep the LAST ACCEPTED sample as the coverage anchor across retries.
            if self.samples and (before <= self.samples[-1]['mono_after_ns']
                                 or after - self.samples[-1]['mono_before_ns'] > MAX_GAP_NS):
                raise ClockRefused('clock monitor gap or nonmonotonic observer clock during resampling')
            if after - before + previous_width <= MAX_BRACKET_NS:
                self.samples.append(sample)
                return
            self.rejected_samples.append({**sample, 'reason': 'bracket uncertainty exceeds 1 ms'})
            # No sleep and no return to the 250 ms poll loop: retry this sample.
        raise ClockRefused(f'clock bracket exceeds 1 ms budget for {MAX_BRACKET_ATTEMPTS} consecutive attempts')

    def start(self):
        try:
            self.sample('start')
        except Exception as error:
            self.error = str(error)
            raise
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
        if hasattr(self, 'thread'):
            self.thread.join()
        if self.samples:
            try:
                self.sample('end')
                meta['wall_end_ms'] = self.samples[-1]['wall_ns'] // 1_000_000
            except Exception as error:
                self.error = self.error or str(error)
        check = dict(schema=1, interval_ns=INTERVAL_NS, max_gap_ns=MAX_GAP_NS,
                     max_bracket_ns=MAX_BRACKET_NS, max_bracket_attempts=MAX_BRACKET_ATTEMPTS,
                     status='PASS', samples=self.samples, rejected_samples=self.rejected_samples)
        meta['clock_regression_check'] = check
        try:
            if self.error:
                raise ClockRefused('clock monitor failed: ' + self.error)
            validate_clock_check(meta)
        except ClockRefused as error:
            check.update(status='REFUSED', reason=str(error))
        return check
