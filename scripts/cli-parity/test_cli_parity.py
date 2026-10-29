"""Contract tests use clearly synthetic recordings, never calibration evidence."""
import sys
if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit(f'#701: os must be preloaded from CPython 3.11+ frozen modules; run with -I: python3 -I {__file__}')
import os

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  Without it, exit before importing anything: a same-named stdlib
# file beside this entry (json.py, pathlib.py, tempfile.py) would otherwise
# execute before the guard. Discover can add the directory back even with
# -I, so reject that path too. ``sys`` is builtin; ``os`` is imported only
# after the preflight above proved it is a preloaded frozen module, so neither
# can be shadowed from the reinserted directory.
# The directory is identified by (device, inode), not by its spelling: a
# case-insensitive alias or a mixed ``//``/symlink form names this very
# directory while the raw string differs (fix-r2 GLM sibling P1).
_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_reinserted = any(
    os.path.isdir(entry or os.curdir)
    and os.path.samefile(entry or os.curdir, _boot_dir)
    for entry in sys.path)
if not sys.flags.isolated or _boot_reinserted:
    raise SystemExit(f'#701: unsafe import path; run with -I by file: python3 -I {__file__}')

import importlib.util
from pathlib import Path

# The bootstrap itself is also loaded from source by its exact sibling path.
_bootstrap_path = Path(__file__).resolve().with_name('_bootstrap.py')
_bootstrap_spec = importlib.util.spec_from_file_location('_cli_parity_bootstrap', _bootstrap_path)
_bootstrap = importlib.util.module_from_spec(_bootstrap_spec)
exec(compile(_bootstrap_path.read_bytes(), str(_bootstrap_path), 'exec'), _bootstrap.__dict__)
_load = _bootstrap.load

import ast
import base64
import hashlib
from contextlib import contextmanager, nullcontext, redirect_stdout
import io
from itertools import combinations
import json
import os
import re
import shlex
import shutil
import signal
import socket
import stat
import subprocess
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

_here = Path(__file__).resolve().parent
_common = _load('common', _here / 'common.py')
LIVE_GROUPS = _common.LIVE_GROUPS
Blocked = _common.Blocked
CleanupBlocked = _common.CleanupBlocked
HEAD = _common.HEAD
MOCK_PORT = _common.MOCK_PORT
OPENSSL_CONFIG = _common.OPENSSL_CONFIG
ROOT = _common.ROOT
SCRIPT = _common.SCRIPT
SCENARIOS = _common.SCENARIOS
TOKEN = _common.TOKEN
baseline_path = _common.baseline_path
environment = _common.environment
env_summary = _common.env_summary
fixture = _common.fixture
digest_plain = _common.digest_plain
read_plain = _common.read_plain
hashes = _common.hashes
initial_tree = _common.initial_tree
mock_gateway_scenarios = _common.mock_gateway_scenarios
output_path = _common.output_path
parity_lock = _common.parity_lock
per_stream_scenarios = _common.per_stream_scenarios
save = _common.save
snapshot = _common.snapshot
staged_skills_identity = _common.staged_skills_identity
_compare = _load('compare', _here / 'compare.py')
coalesce = _compare.coalesce
compare = _compare.compare
comparison_view = _compare.comparison_view
expected_stdout = _compare.expected_stdout
load_round = _compare.load_round
GATEWAY_PROBE_CHECKS = _compare.GATEWAY_PROBE_CHECKS
PROBE_CHECKS = _compare.PROBE_CHECKS
run_controls = _load('controls', _here / 'controls.py').run_controls
_host_controls = _load('host_negative_controls', _here / 'host_negative_controls.py')
check_exec_control = _host_controls.check_exec_control
host_control_main = _host_controls.main
MOCK_GATEWAY_ALLOW = _load('policy', _here / 'policy.py').MOCK_GATEWAY_ALLOW
_run_round = _load('run_round', _here / 'run_round.py')
baseline_identity = _run_round.baseline_identity
capture = _run_round.capture
check_budget = _run_round.check_budget
main = _run_round.main
mock_gateway_preflight = _run_round.mock_gateway_preflight
preflight = _run_round.preflight
mock_listening_line = _run_round.mock_listening_line
run_round = _run_round.run_round
rust_isolation = _run_round.rust_isolation
sandbox_profile = _run_round.sandbox_profile
stage_rust = _run_round.stage_rust
stop_group = _run_round.stop_group

# #933-2 (fix-r4): the profile guard pins sandbox_profile() output for fixed
# placeholder paths — policy.SCRIPT included, since the probe literal is the
# only checkout-derived byte any profile carries. Nothing is normalized, so
# no normalization can mask a rule change: a relaxed rule, a changed port or
# a reordered deny flips a digest in every checkout, while moving the checkout
# (real, non-ASCII or nested under a system path) cannot. The Rust binary must
# keep the staged <stage>/bin layout sandbox_profile() demands, so its parent
# directory is literally named "bin".
# fix-r5: every placeholder input sits under its own root, disjoint from the
# other inputs and from the policy's fixed read paths (see the disjointness
# assertion in test_mock_profile_appends_only_the_loopback_allow). Sharing one
# root (the previous single-/PDRS shape) let a mutant grant an input's parent
# metadata the profile already carried, and the digests could not see it.
PIN_RUNTIME = Path('/PDRS-RUNTIME/runtime')
PIN_BASELINE = Path('/PDRS-BASELINE/checkout')
PIN_NODE = Path('/PDRS-NODE/bin/node')
PIN_RUST_BIN = Path('/PDRS-RUSTBIN/rust-stage/bin/pilotdeck')
PIN_SCRIPT = Path('/PDRS-SCRIPT/scripts/cli-parity')


def pinned_profile(side, mock_gateway=False):
    """sandbox_profile() text for the placeholder paths; see PIN_* above."""
    with patch('policy.SCRIPT', PIN_SCRIPT):
        return sandbox_profile(PIN_RUNTIME, PIN_BASELINE,
                               PIN_NODE if side == 'ts' else PIN_RUST_BIN,
                               side, mock_gateway=mock_gateway)


def pinned_trees():
    """(name, directory objects) each pinned input can contribute to the
    metadata ancestors: itself (ts reads baseline/dist and friends, so the
    baseline itself is an ancestor) and its parents, `/` excluded. The sets
    must stay pairwise disjoint for the digest pin; the assertion lives in
    test_mock_profile_appends_only_the_loopback_allow."""
    pinned = (('runtime', PIN_RUNTIME), ('baseline', PIN_BASELINE), ('node', PIN_NODE),
              ('rust-bin', PIN_RUST_BIN), ('script', PIN_SCRIPT))
    return [(name, ({path} | set(path.parents)) - {Path('/')}) for name, path in pinned]


def bind_policy(run):
    p = run['provenance']
    iso = run['isolation']
    executed = p['stagedExecutable'] if run['side'] == 'rust' else p['executable']
    iso['profileText'] = sandbox_profile(Path(run['runtime']), Path(p['baseline']['path']), executed['path'], run['side'])
    iso['profileSha256'] = hashlib.sha256(iso['profileText'].encode()).hexdigest()
    iso['profilePath'] = '/synthetic/' + run['side'] + '.sb'
    iso['probeProfileSha256'] = hashlib.sha256(sandbox_profile(
        Path(run['runtime']), Path(p['baseline']['path']), p['node']['path'], 'ts').encode()).hexdigest()


def bind_mock_policy(run):
    """#933: bind the relaxed mock profile the same way bind_policy binds the base."""
    p = run['provenance']
    iso = run['mockIsolation']
    executed = p['stagedExecutable'] if run['side'] == 'rust' else p['executable']
    iso['profileText'] = sandbox_profile(Path(run['runtime']), Path(p['baseline']['path']), executed['path'], run['side'],
                                         mock_gateway=True)
    iso['profileSha256'] = hashlib.sha256(iso['profileText'].encode()).hexdigest()
    iso['profilePath'] = '/synthetic/' + run['side'] + '-mock.sb'
    iso['probeProfileSha256'] = hashlib.sha256(sandbox_profile(
        Path(run['runtime']), Path(p['baseline']['path']), p['node']['path'], 'ts', mock_gateway=True).encode()).hexdigest()


# These legacy contract fixtures always target the default registry, even when
# this test process is launched with the chat-search suite selected.
REGISTERED_SCENARIOS = json.loads((SCRIPT / 'scenarios.json').read_text())
CHAT_SEARCH_SCENARIOS = json.loads((SCRIPT / 'chat-search.json').read_text())
CALIBRATED_SCENARIOS = [s for s in REGISTERED_SCENARIOS if s.get('oracle') != 'PENDING-TS-CALIBRATION']
CORE_SCENARIOS = REGISTERED_SCENARIOS[:13]
SKILLS_SCENARIOS = REGISTERED_SCENARIOS[13:29]
MOCK_GATEWAY_SCENARIOS = [s for s in REGISTERED_SCENARIOS if s.get('mockGateway') is True]
CALIBRATED_SKILLS_SCENARIOS = [s for s in SKILLS_SCENARIOS if s in CALIBRATED_SCENARIOS]
PENDING_SCENARIOS = [s for s in REGISTERED_SCENARIOS if s.get('oracle') == 'PENDING-TS-CALIBRATION']

SYNTHETIC_RUST = b'synthetic rust candidate\n'


def as_rust(path):
    run = json.loads((path / 'round.json').read_text())
    staged = Path(run['runtime']).parent / 'rust-stage/bin/pilotdeck'
    # compare reads the staged copy's bytes, so the synthetic copy must exist on disk.
    staged.parent.mkdir(parents=True, exist_ok=True)
    staged.write_bytes(SYNTHETIC_RUST)
    staged = str(staged)
    expected = {'side': 'rust', 'executable': {'path': '/synthetic/rust-candidate',
                                               'sha256': hashlib.sha256(SYNTHETIC_RUST).hexdigest()},
                'sourceHead': 'a' * 40, 'sourcePath': '/synthetic/rust-source'}
    run['side'] = 'rust'
    run['provenance'].update(expected)
    run['provenance']['stagedExecutable'] = {'path': staged, 'sha256': expected['executable']['sha256']}
    skills = Path(staged).parent.parent / 'skills'
    (skills / 'docx').mkdir(parents=True, exist_ok=True)
    (skills / 'docx/SKILL.md').write_text('synthetic skill\n')
    run['provenance']['stagedSkills'] = staged_skills_identity(skills)
    run['isolation']['nodeExec'] = {'denied': True, 'status': 'DONE', 'timedOut': False, 'exitCode': 71, 'stdoutHex': '',
        'stderrHex': b'sandbox-exec: execvp(): Operation not permitted'.hex(),
        'audit': {'allStopped': True, 'remaining': [], 'errors': []}}
    bind_policy(run)
    if 'mockIsolation' in run:
        # The rust-mock profile must deny Node exec exactly like the rust base.
        run['mockIsolation']['nodeExec'] = {**run['isolation']['nodeExec']}
        bind_mock_policy(run)
    save(path / 'round.json', run)
    mock_ids = {s['id'] for s in SCENARIOS if s.get('mockGateway') is True}
    for scenario in SCENARIOS:
        p = path / 'cases' / scenario['id'] / 'record.json'
        row = json.loads(p.read_text())
        row['executedArgv'] = [staged] + scenario['argv']
        binding = run['mockIsolation'] if scenario['id'] in mock_ids else run['isolation']
        row['sandboxArgv'] = ['/usr/bin/sandbox-exec', '-f', binding['profilePath'], *row['executedArgv']]
        row['profileSha256'] = binding['profileSha256']
        save(p, row)
    return expected


def write_stream(case, stream, data):
    events = [r for r in json.loads((case / 'events.json').read_text()) if r['stream'] != stream]
    if data:
        events.append({'stream': stream, 'hex': data.hex()})
    (case / f'{stream}.bin').write_bytes(data)
    save(case / 'events.json', events)


def synthetic_round(path, runtime, scenario_fixtures=False):
    path.mkdir()
    (path / 'cases').mkdir()
    run = {'schema': 1, 'kind': 'cli-d0-recording', 'origin': 'synthetic-unit-fixture', 'side': 'ts', 'status': 'DONE',
           'runtime': str(runtime), 'scenarios': [s['id'] for s in SCENARIOS], 'apparatus': hashes(),
           'isolation': {'status': 'PASS', 'errors': [], 'exitCode': 0, 'profileSha256': '0' * 64,
                         'audit': {'allStopped': True, 'remaining': [], 'errors': []},
                         'driverProcess': {'pid': 1},
                         'probe': {'status': 'PASS', 'homeMatches': True, 'outsideReadDenied': True,
                                   'outsideWriteDenied': True, 'listenDenied': True, 'connectDenied': True,
                                   'childDenied': True, 'runtimeWritable': True, 'stdinIsTTY': False,
                                   'osHomedir': str(runtime / 'home')}},
           'provenance': {'side': 'ts', 'sourcePath': '/synthetic/baseline', 'opensslConfig': OPENSSL_CONFIG,
                          'node': {'path': '/synthetic/unit-test-only', 'sha256': '0' * 64}, 'executable': {'path': '/synthetic/unit-test-only', 'sha256': '0' * 64},
                          'sourceHead': HEAD, 'baseline': {'sourceHead': HEAD, 'entrySha256': '0' * 64,
                              'path': '/synthetic/baseline', 'files': {'src/fixture.ts': '0' * 64},
                              'trees': {name: '0' * 64 for name in ('src', 'dist', 'packages', 'node_modules', 'skills')}}}}
    run['isolation']['probe'].update({key: True for key in PROBE_CHECKS})
    if any(s.get('mockGateway') is True for s in SCENARIOS):
        run['mockIsolation'] = {'status': 'PASS', 'errors': [], 'exitCode': 0,
                                 'audit': {'allStopped': True, 'remaining': [], 'errors': []},
                                 'probeCleanup': {'unixSocketRemoved': True},
                                 'gatewayProbe': {'connectAllowed': True, **dict.fromkeys(GATEWAY_PROBE_CHECKS[1:], 'EPERM')},
                                 'mockProcess': {'pid': 456,
                                                  'argv': ['/synthetic/unit-test-only', str(SCRIPT / 'mock_gateway.mjs'), str(MOCK_PORT)],
                                                  'token': {'redacted': True, 'length': len(TOKEN),
                                                            'sha256': hashlib.sha256(TOKEN.encode()).hexdigest()}}}
        bind_mock_policy(run)
    bind_policy(run)
    save(path / 'round.json', run)
    mock_ids = {s['id'] for s in SCENARIOS if s.get('mockGateway') is True}
    for scenario in SCENARIOS:
        folder = path / 'cases' / scenario['id']
        folder.mkdir()
        fixture(folder / 'tree', scenario if scenario_fixtures else None)
        save(folder / 'initial.json', initial_tree(scenario if scenario_fixtures else None))
        if 'pilot/.legacy-bundled-skills-migrated-v1' in scenario.get('diskAdditions', {}):
            marker = folder / 'tree/pilot/.legacy-bundled-skills-migrated-v1'
            marker.write_bytes(b'completed\n')
            marker.chmod(0o600)
        save(folder / 'final.json', snapshot(folder / 'tree'))
        events = []
        for stream in ('stdout', 'stderr'):
            data = (expected_stdout(scenario, runtime) if stream == 'stdout' and 'stdout' in scenario
                    else scenario.get(stream, scenario.get(stream + 'Prefix', '') + 'synthetic\n').encode())
            if stream == 'stdout' and scenario['id'].startswith('help-'):
                data = (SCRIPT / 'help.txt').read_bytes()
            (folder / f'{stream}.bin').write_bytes(data)
            if data:
                events.append({'stream': stream, 'hex': data.hex()})
        save(folder / 'events.json', events)
        binding = run['mockIsolation'] if scenario['id'] in mock_ids else run['isolation']
        save(folder / 'record.json', {'status': 'DONE', 'timedOut': False, 'exitCode': scenario.get('exit', 1),
             'argv': scenario['argv'], 'stdinHex': scenario['stdinHex'], 'cwd': str(runtime / 'project'),
             'executedArgv': ['/synthetic/unit-test-only', '/synthetic/baseline/dist/src/cli/pilotdeck.js'] + scenario['argv'],
             'sandboxArgv': ['/usr/bin/sandbox-exec', '-f', binding['profilePath'], '/synthetic/unit-test-only',
                             '/synthetic/baseline/dist/src/cli/pilotdeck.js'] + scenario['argv'],
             'profileSha256': binding['profileSha256'],
             'env': env_summary(environment(runtime, scenario)), 'pid': 123,
             'audit': {'allStopped': True, 'remaining': [], 'errors': []},
             'processTree': [{'pid': 123, 'ppid': 1, 'pgid': 123, 'state': 'Z', 'command': 'synthetic'}]})


APPARATUS_MODULES = _bootstrap.APPARATUS_MODULES
APPARATUS_FILES = _bootstrap.APPARATUS_FILES
ENTRY_FILES = _bootstrap.ENTRY_FILES
MIRROR_FILES = _bootstrap.MIRROR_FILES


def module_statements(source):
    body = ast.parse(source).body
    if (body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant)
            and isinstance(body[0].value.value, str)):
        body = body[1:]
    return body


BOOTSTRAP_PREFIX = """\
import importlib.util
from pathlib import Path
_bootstrap_path = Path(__file__).resolve().with_name('_bootstrap.py')
_bootstrap_spec = importlib.util.spec_from_file_location('_cli_parity_bootstrap', _bootstrap_path)
_bootstrap = importlib.util.module_from_spec(_bootstrap_spec)
exec(compile(_bootstrap_path.read_bytes(), str(_bootstrap_path), 'exec'), _bootstrap.__dict__)
_load = _bootstrap.load
"""


def documented_invocations(source):
    """Yield shell argv and physical start line, including continued commands."""
    python = r'python(?:\d+(?:\.\d+)*[a-z]?)?'
    variable = r'\$(?:\{[A-Za-z_]\w*\}|[A-Za-z_]\w*)'
    invocation = re.compile(
        rf'''(?<![\w/.-])(?:"(?:[^"`\n;&|]*/)?{python}"|'''
        rf"'(?:[^'`\n;&|]*/)?{python}'|"
        rf'''(?:[^\s`"';&|]*/)?{python}|["']?{variable}["']?)'''
        r'''[ \t]+(?P<args>[^`\n;&|]+)''')
    logical, start = '', 1
    for number, line in enumerate(source.splitlines(), 1):
        if not logical:
            start = number
        logical += line
        if logical.endswith('\\'):
            logical = logical[:-1] + ' '
            continue
        for match in invocation.finditer(logical):
            if 'scripts/cli-parity' in match.group('args'):
                yield start, shlex.split(match.group('args'), comments=True)
        logical = ''


def isolated_file_options(options):
    """Consume Python options; -I used as a -W/-X value is not isolation."""
    isolated = False
    remaining = iter(options)
    for option in remaining:
        if option in ('-W', '-X'):
            if next(remaining, None) is None:
                return isolated, False
        elif option.startswith(('-W', '-X')):
            continue
        elif re.fullmatch(r'-[bBdEIOPqRsSuvx]+', option):
            isolated |= 'I' in option[1:]
        else:
            return isolated, False  # -m/-c/--, early script, unknown or exit flag
    return isolated, True


class BootstrapTests(unittest.TestCase):
    """#701 item 3: entries reject same-directory imposters before loading one."""

    def test_apparatus_inventory_matches_directory(self):
        registered = {name for name in ENTRY_FILES + APPARATUS_FILES + MIRROR_FILES
                      if name.endswith('.py')}
        actual = {path.name for path in SCRIPT.glob('*.py') if path.is_file()}
        self.assertEqual(actual, registered,
                         '#701: Python apparatus inventory must match actual directory files')

    def test_bootstrap_cache_is_singleton_and_cleaned_on_exit(self):
        for ending, expected_exit in (('', 0), ('raise SystemExit(7)', 7),
                                      ('raise RuntimeError("#701 exit probe")', 1)):
            with self.subTest(ending=ending), tempfile.TemporaryDirectory(prefix='d0-cache-') as temp:
                holder = Path(temp).resolve()
                bootstrap = holder / '_bootstrap.py'
                shutil.copy2(SCRIPT / '_bootstrap.py', bootstrap)
                probe = holder / 'probe.py'
                probe.write_text('''\
import json
from pathlib import Path
import sys
import tempfile

tempfile.tempdir = str(Path(__file__).parent)
path = Path(__file__).with_name('_bootstrap.py')
prefixes = []
for _ in range(3):
    exec(compile(path.read_bytes(), str(path), 'exec'), {'__file__': str(path)})
    prefixes.append(sys.pycache_prefix)
print(json.dumps({'prefixes': prefixes, 'dontWrite': sys.dont_write_bytecode,
                  'directories': [str(p) for p in path.parent.glob('pdrs-cli-parity-pyc-*')]}),
      flush=True)
''' + ending + '\n')
                result = subprocess.run([sys.executable, '-I', str(probe)],
                                        capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, expected_exit,
                                 '#701: bootstrap cache probe must reach its intended exit: ' + result.stderr)
                report = json.loads(result.stdout)
                self.assertTrue(report['dontWrite'], '#701: bootstrap must disable bytecode writes')
                self.assertEqual(len(report['prefixes']), 3,
                                 '#701: bootstrap cache probe must execute three loads')
                prefix = report['prefixes'][0]
                self.assertIsInstance(prefix, str, '#701: bootstrap must use a private cache prefix')
                self.assertEqual(report['prefixes'], [prefix] * 3,
                                 '#701: repeated bootstrap loads must reuse one cache directory')
                self.assertEqual(report['directories'], [prefix],
                                 '#701: bootstrap must allocate exactly one private cache directory')
                self.assertFalse(Path(prefix).exists(),
                                 '#701: bootstrap cache directory must be removed on process exit')

    def test_isolated_guard_is_the_first_statement(self):
        for name in ENTRY_FILES:
            with self.subTest(entry=name):
                body = module_statements((SCRIPT / name).read_text())
                self.assertGreaterEqual(len(body), 6, f'#701: {name}: missing isolated guard')
                self.assertEqual(ast.unparse(body[0]), 'import sys',
                                 f'#701: {name}: only boot-essential imports may load before the guard')
                preflight = body[1]
                self.assertIsInstance(
                    preflight, ast.If,
                    f'#701: {name}: the os-preload preflight must precede import os')
                self.assertEqual(
                    ast.unparse(preflight.test),
                    "sys.version_info < (3, 11) or 'os' not in sys.modules",
                    f'#701: {name}: the preflight must reject unfrozen or unloaded os')
                self.assertTrue(any(isinstance(node, ast.Raise) for node in preflight.body),
                                f'#701: {name}: the preflight must fail closed before importing os')
                self.assertEqual(ast.unparse(body[2]), 'import os',
                                 f'#701: {name}: os may only load after the preflight')
                self.assertEqual(
                    ast.unparse(body[3]),
                    '_boot_dir = os.path.dirname(os.path.realpath(__file__))',
                    f'#701: {name}: the guard must resolve this directory once')
                identity = ast.unparse(body[4])
                self.assertTrue(identity.startswith('_boot_reinserted = any('),
                                f'#701: {name}: the guard must build one reinsertion flag')
                self.assertIn('os.path.samefile(entry or os.curdir, _boot_dir)', identity,
                              f'#701: {name}: the guard must compare directory identity, not spelling')
                guard = body[5]
                self.assertIsInstance(guard, ast.If,
                                      f'#701: {name}: the isolated guard must follow the boot prelude')
                self.assertEqual(ast.unparse(guard.test),
                                 'not sys.flags.isolated or _boot_reinserted',
                                 f'#701: {name}: the guard must reject non-isolated or reinserted paths')
                self.assertTrue(any(isinstance(node, ast.Raise) for node in guard.body),
                                f'#701: {name}: the guard must exit before loading anything')

    def test_entry_guard_rejects_unfrozen_or_unloaded_os(self):
        """fix-r4 GLM P1: import os may only run after its preload is proved."""
        marker = ('#701: an interpreter without a preloaded frozen os must be '
                  'rejected before importing os')
        preload = 'os must be preloaded from CPython 3.11+ frozen modules'
        forged = ('import sys\n'
                  'with open(sys.argv[2], "w") as stream:\n'
                  '    stream.write("forged os executed")\n'
                  'raise RuntimeError("forged os module executed")\n')
        frozen_probe = ('import runpy, sys\n'
                        'sys.path.insert(0, sys.argv[1])\n'
                        'runpy.run_path(sys.argv[2], run_name="__main__")\n')
        version_probe = ('import sys\n'
                         'entry, marker, staged = sys.argv[1], sys.argv[2], sys.argv[3]\n'
                         'source = open(entry, "rb").read()\n'
                         'sys.version_info = (3, 10, 0, "final", 0)\n'
                         'sys.path.insert(0, staged)\n'
                         'exec(compile(source, entry, "exec"), '
                         '{"__file__": entry, "__name__": "__main__"})\n')
        unloaded_probe = ('import sys\n'
                          'entry, marker, staged = sys.argv[1], sys.argv[2], sys.argv[3]\n'
                          'source = open(entry, "rb").read()\n'
                          'del sys.modules["os"]\n'
                          'sys.path.insert(0, staged)\n'
                          'exec(compile(source, entry, "exec"), '
                          '{"__file__": entry, "__name__": "__main__"})\n')
        with tempfile.TemporaryDirectory(prefix='d0-osguard-') as temp:
            holder = Path(temp)
            for name in ENTRY_FILES:
                staged = holder / name
                # copy2 so the negative-control harness can substitute the
                # mutant bytes into the temporary staging directory.
                shutil.copy2(SCRIPT / name, staged)
                forge = holder / ('forge-' + name)
                forge.mkdir()
                (forge / 'os.py').write_text(forged)
                marker_path = holder / (name + '.marker')
                cases = (
                    ('frozen-modules-off',
                     [sys.executable, '-I', '-X', 'frozen_modules=off', '-c',
                      frozen_probe, str(staged.parent), str(staged)]),
                    ('version-below-3-11',
                     [sys.executable, '-I', '-c', version_probe, str(staged),
                      str(marker_path), str(forge)]),
                    ('os-not-preloaded',
                     [sys.executable, '-I', '-c', unloaded_probe, str(staged),
                      str(marker_path), str(forge)]),
                )
                for label, command in cases:
                    with self.subTest(entry=name, case=label):
                        marker_path.unlink(missing_ok=True)
                        result = subprocess.run(command, capture_output=True,
                                                text=True, timeout=60)
                        output = result.stdout + result.stderr
                        self.assertEqual(
                            result.returncode, 1,
                            marker + f' [{name}/{label}] rc={result.returncode}: '
                            + output[-300:])
                        self.assertTrue(
                            preload in output or 'unsafe import path' in output,
                            marker + f' [{name}/{label}] rejected later than the guard: '
                            + output[-300:])
                        self.assertFalse(
                            marker_path.exists(),
                            marker + f' [{name}/{label}] the forged os.py executed')

    def test_entries_reject_a_mixed_spelling_reinsertion(self):
        """#701 fix-r2: the same directory under another spelling is still that directory."""
        marker = '#701: a reinserted entry directory must be rejected whatever its spelling'
        probe = ('import runpy, sys\n'
                 'sys.path.insert(0, sys.argv[1])\n'
                 'sys.argv = [sys.argv[2], "--help"]\n'
                 'runpy.run_path(sys.argv[0], run_name="__main__")\n')
        with tempfile.TemporaryDirectory(prefix='d0-alias-') as temp:
            # Stage an on-disk copy: the negative-control harness substitutes
            # read_text/copy2 for the mutated entry, and a subprocess must see
            # those bytes while still starting in a fresh interpreter.
            staged = Path(temp) / 'apparatus'
            staged.mkdir()
            for fname in MIRROR_FILES:
                shutil.copy2(SCRIPT / fname, staged / fname)
            link = Path(temp) / 'linked-apparatus'
            link.symlink_to(staged, target_is_directory=True)
            forms = [(staged, link), (link, staged)]
            case = staged.with_name(staged.name.upper())
            if case.is_dir():
                try:
                    same_case = os.path.samefile(case, staged)
                except OSError:
                    same_case = False
                if same_case:
                    forms += [(staged, case), (case, staged)]
            for name in ENTRY_FILES:
                for on_path, entry_dir in forms:
                    with self.subTest(entry=name, spelling=str(on_path)):
                        result = subprocess.run(
                            [sys.executable, '-I', '-c', probe, str(on_path),
                             str(entry_dir / name)],
                            capture_output=True, text=True, timeout=60)
                        self.assertEqual(result.returncode, 1,
                                         f'{marker}: {name}: rc={result.returncode}')
                        self.assertTrue(
                            result.stderr.startswith('#701: unsafe import path'),
                            f'{marker}: {name}: the guard itself must reject, got '
                            + result.stderr[:200])
                        self.assertIn(str(entry_dir / name), result.stderr,
                                      f'{marker}: {name}: the entry itself must reject, got '
                                      + result.stderr[:200])
            # The checkout's own bytes reject too (no staging involved).
            real = subprocess.run(
                [sys.executable, '-I', '-c', probe, str(SCRIPT),
                 str(SCRIPT / 'run_round.py')],
                capture_output=True, text=True, timeout=60)
            self.assertEqual(real.returncode, 1, marker + ': checkout entry')
            self.assertIn(str(SCRIPT / 'run_round.py'), real.stderr,
                          marker + ': checkout entry must reject itself')
            # Paired control: the same staged bytes with just the raise
            # disabled must let the mixed spelling reach the entry body.
            text = (staged / 'run_round.py').read_text()
            head = text[:text.index('\nimport importlib.util\n')]
            raise_line = ("    raise SystemExit(f'#701: unsafe import path; "
                          "run with -I by file: python3 -I {__file__}')")
            self.assertEqual(head.count(raise_line), 1,
                             '#701 control: the guard raise anchor is stale')
            (staged / 'probe.py').write_text(
                head.replace(raise_line, '    pass  # control: guard disabled', 1)
                + "\nprint('REACHED')\n")
            admitted = subprocess.run(
                [sys.executable, '-I', '-c', probe, str(staged), str(link / 'probe.py')],
                capture_output=True, text=True, timeout=60)
            self.assertEqual(admitted.returncode, 0,
                             '#701 control: ' + admitted.stderr[-300:])
            self.assertIn('REACHED', admitted.stdout,
                          '#701 control: the mixed spelling must reach the file')

    def test_apparatus_modules_load_by_explicit_path_only(self):
        for name in APPARATUS_FILES:
            tree = ast.parse((SCRIPT / name).read_text())
            with self.subTest(entry=name):
                for node in ast.walk(tree):
                    if isinstance(node, ast.ImportFrom):
                        self.assertNotIn(node.module, APPARATUS_MODULES,
                                         f'#701: {name}:{node.lineno}: bare apparatus import')
                    elif isinstance(node, ast.Import):
                        for alias in node.names:
                            self.assertNotIn(alias.name.split('.')[0], APPARATUS_MODULES,
                                             f'#701: {name}:{node.lineno}: bare apparatus import')
                    elif isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                        target = node.func.value
                        if (node.func.attr in ('insert', 'append', 'extend')
                                and isinstance(target, ast.Attribute) and target.attr == 'path'
                                and isinstance(target.value, ast.Name) and target.value.id == 'sys'):
                            self.fail(f'#701: {name}:{node.lineno}: sys.path.{node.func.attr} is forbidden')

    def test_entries_use_shared_bootstrap(self):
        expected = ast.parse(BOOTSTRAP_PREFIX).body
        for name in ENTRY_FILES:
            with self.subTest(entry=name):
                source = (SCRIPT / name).read_text()
                tree = ast.parse(source)
                body = module_statements(source)
                self.assertEqual(
                    [ast.dump(node) for node in body[6:6 + len(expected)]],
                    [ast.dump(node) for node in expected],
                    f'#701: {name}: must load the shared bootstrap by explicit source path')
                for node in ast.walk(tree):
                    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                        self.assertNotEqual(node.name, '_load',
                                            f'#701: {name}: entry must not define its own _load')
                    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
                        self.assertNotEqual(node.func.id, '__import__',
                                            f'#701: {name}: dynamic bare import is forbidden')
                for binding in ('_load', '_bootstrap', '_bootstrap_path', '_bootstrap_spec'):
                    stores = [node for node in ast.walk(tree)
                              if isinstance(node, ast.Name) and node.id == binding
                              and isinstance(node.ctx, (ast.Store, ast.Del))]
                    self.assertEqual(len(stores), 1,
                                     f'#701: {name}: shared bootstrap binding {binding} must not change')

    def test_documented_commands_use_isolated_mode(self):
        # Search tracked docs, shell and CI, without treating source literals
        # in mutation fixtures as runnable documentation.
        result = subprocess.run(
            ['git', 'grep', '-l', '-F', 'scripts/cli-parity', '--',
             '*.md', '*.sh', '*.yml', '*.yaml'],
            cwd=ROOT, capture_output=True, text=True, timeout=30)
        self.assertIn(result.returncode, (0, 1), '#701: command inventory must complete')
        matched = set()
        for relative in result.stdout.splitlines():
            try:
                commands = list(documented_invocations((ROOT / relative).read_text()))
            except ValueError as error:
                self.fail(f'#701: {relative}: CLI parity command cannot be parsed: {error}')
            for number, argv in commands:
                targets = [index for index, token in enumerate(argv) if 'scripts/cli-parity' in token]
                self.assertTrue(targets, f'#701: {relative}:{number}: CLI parity command needs a path')
                matched.add(relative)
                isolated, explicit_file = isolated_file_options(argv[:targets[0]])
                self.assertTrue(isolated, f'#701: {relative}:{number}: CLI parity command requires -I')
                self.assertTrue(explicit_file,
                                f'#701: {relative}:{number}: CLI parity command requires an explicit file')
        self.assertTrue(matched, '#701: documented command inventory must not be empty')
        self.assertIn('scripts/cli-parity/README.md', matched,
                      '#701: documented command inventory must include scripts/cli-parity/README.md')

    def test_discover_rejects_reinserted_path_before_stdlib(self):
        with tempfile.TemporaryDirectory(prefix='d0-discover-') as temp:
            mirror = Path(temp).resolve()
            for name in MIRROR_FILES:
                shutil.copy2(SCRIPT / name, mirror / name)
            (mirror / 'json.py').write_text('raise RuntimeError("#701 JSON IMPOSTER EXECUTED")\n')
            result = subprocess.run(
                [sys.executable, '-I', '-B', '-m', 'unittest', 'discover',
                 '-s', str(mirror), '-p', 'test_cli_parity.py', '-v'],
                capture_output=True, text=True, timeout=60)
            message = '#701: discover must reject the reinserted path before importing json'
            self.assertEqual(result.returncode, 1, message)
            self.assertIn('SystemExit: #701: unsafe import path', result.stderr, message)
            self.assertNotIn('JSON IMPOSTER EXECUTED', result.stdout + result.stderr, message)

    def test_each_entry_starts_isolated_with_imposter_packages(self):
        with tempfile.TemporaryDirectory(prefix='d0-bootstrap-') as temp:
            mirror = Path(temp)
            for name in MIRROR_FILES:
                shutil.copy2(SCRIPT / name, mirror / name)
            for name in ('_bootstrap', 'common', 'policy', 'compare', 'test_cli_parity'):
                (mirror / name).mkdir()
                (mirror / name / '__init__.py').write_text(
                    'raise RuntimeError("imposter package loaded")\n')
            for name in ENTRY_FILES:
                with self.subTest(entry=name):
                    result = subprocess.run([sys.executable, '-I', str(mirror / name), '-h'],
                                            capture_output=True, text=True, timeout=60)
                    self.assertEqual(result.returncode, 0,
                                     f'#701: {name}: isolated startup must ignore packages: {result.stderr}')
                    self.assertEqual(result.stderr, '', f'#701: {name}: imposter must never execute')


class ContractTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='d0-test-')
        self.root = Path(self.temp.name).resolve()
        # Production has no fixture acceptance switch, including in either CLI.
        self.origin_patch = patch('compare.RECORDING_ORIGIN', 'synthetic-unit-fixture')
        self.origin_patch.start()
        # Legacy synthetic fixtures cover the original 13 cases; they do not
        # recreate D2's real Git blobs. Production always uses every registered case.
        for module in ('compare', 'run_round', __name__):
            patcher = patch.object(sys.modules[module], 'SCENARIOS', CORE_SCENARIOS)
            patcher.start()
            self.addCleanup(patcher.stop)

    def tearDown(self):
        self.origin_patch.stop()
        self.temp.cleanup()

    def source(self):
        source = self.root / 'source'
        synthetic_round(source, self.root / 'fixed-runtime')
        return source

    @contextmanager
    def mixed_stream_rounds(self, side, declared=True):
        cases = [{'id': name, 'argv': [], 'stdinHex': '', 'token': False, 'exit': 1,
                  'stdout': 'migration summary\n', 'stderr': 'configuration error\n'}
                 for name in ('mixed', 'strict')]
        if declared:
            cases[0].update(streamOrder='per-stream', streamOrderReason='Synthetic unit fixture only.')
        with patch('compare.SCENARIOS', cases), patch.object(sys.modules[__name__], 'SCENARIOS', cases):
            source, target = self.root / (side + '-source'), self.root / (side + '-target')
            synthetic_round(source, self.root / 'fixed-runtime')
            shutil.copytree(source, target)
            expected = as_rust(target) if side == 'rust' else None
            yield source, target, expected

    def test_per_stream_allows_only_declared_cross_stream_reordering(self):
        for side in ('ts', 'rust'):
            with self.subTest(side=side), self.mixed_stream_rounds(side) as (source, target, expected):
                events = target / 'cases/mixed/events.json'
                save(events, list(reversed(json.loads(events.read_text()))))
                raw = events.read_bytes()
                self.assertEqual(compare(source, target, expected)['status'], 'PASS')
                self.assertEqual(events.read_bytes(), raw)
                # The exception is local to mixed, not a switch for the round.
                strict = target / 'cases/strict/events.json'
                save(strict, list(reversed(json.loads(strict.read_text()))))
                result = compare(source, target, expected)
                self.assertEqual(result['status'], 'FAIL')
                self.assertEqual({r['rule'] for r in result['failures']}, {'strict.order'})

    def test_undeclared_cross_stream_reordering_fails_comparison(self):
        for side in ('ts', 'rust'):
            with self.subTest(side=side), self.mixed_stream_rounds(side, declared=False) as (source, target, expected):
                events = target / 'cases/mixed/events.json'
                save(events, list(reversed(json.loads(events.read_text()))))
                result = compare(source, target, expected)
                self.assertEqual(result['status'], 'FAIL')
                self.assertEqual({r['rule'] for r in result['failures']}, {'mixed.order'})

    def test_per_stream_keeps_exact_bytes_and_independent_ts_oracle(self):
        for side in ('ts', 'rust'):
            with self.mixed_stream_rounds(side) as (source, target, expected):
                left, right = source / 'cases/mixed', target / 'cases/mixed'
                for channel in ('stdout', 'stderr'):
                    with self.subTest(side=side, channel=channel):
                        original = (left / (channel + '.bin')).read_bytes()
                        changed = original + b'\x00\xff'
                        # Keep events coherent so the semantic byte guard must fail.
                        write_stream(right, channel, changed)
                        result = compare(source, target, expected)
                        self.assertEqual(result['status'], 'FAIL')
                        rules = {r['rule'] for r in result['failures']}
                        self.assertIn('mixed.' + channel, rules)
                        if side == 'ts':
                            self.assertIn('mixed.oracle.' + channel, rules)
                        # Identical drift on both sides still fails TS vs its oracle.
                        write_stream(left, channel, changed)
                        result = compare(source, target, expected)
                        self.assertEqual(result['status'], 'FAIL')
                        self.assertEqual({r['rule'] for r in result['failures']}, {'mixed.oracle.' + channel})
                        write_stream(left, channel, original)
                        write_stream(right, channel, original)

    def test_per_stream_keeps_raw_event_integrity_check(self):
        for side in ('ts', 'rust'):
            with self.mixed_stream_rounds(side) as (source, target, expected):
                for channel in ('stdout', 'stderr'):
                    with self.subTest(side=side, channel=channel):
                        path = target / 'cases/mixed' / (channel + '.bin')
                        original = path.read_bytes()
                        path.write_bytes(b'')
                        result = compare(source, target, expected)
                        self.assertEqual(result['status'], 'FAIL')
                        self.assertIn('mixed.' + channel + '.events', {r['rule'] for r in result['failures']})
                        path.write_bytes(original)

    def test_invalid_stream_order_declarations_are_apparatus_errors(self):
        invalid = [{'streamOrder': 'per-stream'}, {'streamOrderReason': 'orphan reason'}]
        invalid += [{'streamOrder': value, 'streamOrderReason': 'synthetic evidence'}
                    for value in ('', 'strict', 'ignore', None, True, 1, [], {})]
        invalid += [{'streamOrder': 'per-stream', 'streamOrderReason': value}
                    for value in ('', ' \n\t', None, True, 1, [], {})]
        for declaration in invalid:
            with self.subTest(declaration=declaration), patch('compare.SCENARIOS', [{'id': 'bad', **declaration}]), \
                 patch('compare.load_round') as load:
                result = compare(self.root / 'left', self.root / 'right')
                self.assertEqual(result['status'], 'BLOCKED')
                self.assertEqual(result['failures'][0]['rule'], 'apparatus.stream-order')
                load.assert_not_called()

    def test_runner_rejects_invalid_stream_order_before_launch(self):
        output = self.root / 'invalid-registry'
        with patch('run_round.SCENARIOS', [{'id': 'bad', 'streamOrder': 'per-stream'}]), \
             patch('run_round.output_path', return_value=output), \
             patch('run_round.baseline_path') as baseline, \
             patch('sys.argv', ['run_round.py']), redirect_stdout(io.StringIO()):
            self.assertEqual(main(), 2)
        baseline.assert_not_called()
        summary = json.loads((output / 'summary.json').read_text())
        self.assertEqual(summary['status'], 'BLOCKED')
        self.assertIn('apparatus.stream-order', summary['error'])

    def test_empty_scenario_registry_cannot_pass(self):
        compare_module = _compare
        source = self.root / 'empty-reg'
        with patch.object(compare_module, 'SCENARIOS', []):
            synthetic_round(source, self.root / 'empty-runtime')
            errors = compare_module.load_round(source)[2]
        self.assertIn('round.empty-registry', errors)

    def test_all_five_mutations_detected_on_synthetic_fixture(self):
        result = run_controls(self.source(), self.root / 'mutants')
        self.assertEqual(result['status'], 'PASS', result)
        self.assertEqual(len(result['controls']), 5)
        for row in result['controls']:
            self.assertEqual(row['status'], 'PASS')
            self.assertEqual(row['scenario'], 'flag-missing' if row['name'] == 'change-stderr' else 'help-long')
            self.assertIn(row['expectedPairRule'], row['observedRules'])
        self.assertEqual(result['notApplicable'], [])

    def test_chat_search_controls_select_eligible_cases_and_detect_mutations(self):
        with patch('compare.SCENARIOS', CHAT_SEARCH_SCENARIOS), \
             patch.object(sys.modules[__name__], 'SCENARIOS', CHAT_SEARCH_SCENARIOS):
            source = self.root / 'chat-source'
            synthetic_round(source, self.root / 'chat-runtime', scenario_fixtures=True)
            result = run_controls(source, self.root / 'chat-controls')
        self.assertEqual(result['status'], 'PASS', result)
        self.assertEqual(result['notApplicable'], [])
        self.assertEqual(len(result['controls']), 5)
        for row in result['controls']:
            self.assertEqual(row['status'], 'PASS', row)
            self.assertEqual(row['scenario'], 'chat-text' if row['name'] == 'drop-stdout' else 'chat-usage')
            self.assertIn(row['expectedRule'], row['observedRules'])
            self.assertIn(row['expectedPairRule'], row['observedRules'])

    def test_controls_report_inapplicable_surfaces_explicitly(self):
        cases = [{'id': 'empty', 'argv': [], 'stdinHex': '', 'token': False,
                  'exit': 73, 'stdout': '', 'stderr': '',
                  'diskRemovals': ['pilot/pilotdeck.yaml', 'project/keep.txt', 'openssl.cnf']}]
        with patch('compare.SCENARIOS', cases), patch.object(sys.modules[__name__], 'SCENARIOS', cases):
            source = self.root / 'empty-source'
            synthetic_round(source, self.root / 'empty-runtime')
            case = source / 'cases/empty'
            for name in cases[0]['diskRemovals']:
                (case / 'tree' / name).unlink()
            save(case / 'final.json', snapshot(case / 'tree'))
            result = run_controls(source, self.root / 'empty-controls')
        self.assertEqual(result['status'], 'PASS', result)
        self.assertEqual(result['notApplicable'], ['drop-stdout', 'drop-disk', 'extra-file'])
        self.assertEqual(json.loads((self.root / 'empty-controls/controls.json').read_text()), result)
        for row in result['controls']:
            if row['name'] in result['notApplicable']:
                self.assertEqual(row['status'], 'NOT_APPLICABLE')
                self.assertIsNone(row['scenario'])
                self.assertIn('no eligible case', row['reason'])
            else:
                self.assertEqual(row['status'], 'PASS', row)

    def test_controls_block_zero_applicable_cases(self):
        with patch('controls.compare', return_value={'status': 'PASS'}), \
                patch('controls.load_round', return_value=({}, {}, [])):
            result = run_controls(self.root / 'zero-source', self.root / 'zero-controls')
        self.assertEqual(result['status'], 'BLOCKED', result)
        self.assertEqual(len(result['notApplicable']), 5)
        self.assertFalse(any(row['status'] == 'PASS' for row in result['controls']))
        self.assertEqual(json.loads((self.root / 'zero-controls/controls.json').read_text()), result)

    def test_disk_controls_find_artifacts_without_keep_txt(self):
        cases = [{'id': 'other-file', 'argv': [], 'stdinHex': '', 'token': False,
                  'exit': 0, 'stdout': 'ok\n', 'stderr': '',
                  'fixtureFiles': {'project/unexpected.bin': 'existing\n'},
                  'diskRemovals': ['pilot/pilotdeck.yaml', 'project/keep.txt', 'openssl.cnf']}]
        with patch('compare.SCENARIOS', cases), patch.object(sys.modules[__name__], 'SCENARIOS', cases):
            source = self.root / 'other-source'
            synthetic_round(source, self.root / 'other-runtime', scenario_fixtures=True)
            case = source / 'cases/other-file'
            for name in cases[0]['diskRemovals']:
                (case / 'tree' / name).unlink()
            save(case / 'final.json', snapshot(case / 'tree'))
            result = run_controls(source, self.root / 'other-controls')
        self.assertEqual(result['status'], 'PASS', result)
        self.assertEqual(result['notApplicable'], [])
        tree = self.root / 'other-controls/extra-file/cases/other-file/tree/project'
        self.assertEqual((tree / 'unexpected.bin').read_bytes(), b'existing\n')
        self.assertEqual((tree / 'unexpected.bin.extra').read_bytes(), b'INJECTED\x00\xff')

    def test_host_control_uses_current_suite_and_keeps_default_choice(self):
        summary = {'status': 'FAIL', 'rustComparison': {'status': 'FAIL'}}
        for scenarios, expected in ((REGISTERED_SCENARIOS, 'help-long'),
                                    (CHAT_SEARCH_SCENARIOS, 'chat-usage')):
            with self.subTest(scenario=expected), patch('host_negative_controls.SCENARIOS', scenarios):
                recording = self.root / expected
                case = recording / 'rust/cases' / expected
                case.mkdir(parents=True)
                (case / 'stderr.bin').write_bytes(b'D0 exec-node denied\n')
                save(case / 'record.json', {'exitCode': 126})
                result = check_exec_control(recording, summary, 1)
                self.assertEqual(result['status'], 'PASS', result)
                self.assertEqual(result['scenario'], expected)
                for exit_code, stderr, runner_exit in ((125, b'D0 exec-node denied\n', 1),
                                                      (126, b'other error\n', 1),
                                                      (126, b'D0 exec-node denied\n', 0)):
                    save(case / 'record.json', {'exitCode': exit_code})
                    (case / 'stderr.bin').write_bytes(stderr)
                    self.assertEqual(check_exec_control(recording, summary, runner_exit)['status'], 'FAIL')

    def test_host_control_blocks_empty_suite(self):
        with patch('host_negative_controls.SCENARIOS', []):
            result = check_exec_control(self.root / 'absent', {}, 0)
        self.assertEqual(result['status'], 'BLOCKED')
        self.assertIsNone(result['scenario'])
        self.assertIn('no case', result['reason'])

    def test_host_control_empty_suite_exits_nonzero(self):
        output = self.root / 'empty-host-control'
        argv = ['host_negative_controls.py', '--baseline', str(self.root),
                '--node', str(self.root / 'node'), '--output', str(output)]
        completed = subprocess.CompletedProcess([], 0, b'', b'')
        with patch('sys.argv', argv), patch('host_negative_controls.SCENARIOS', []), \
                patch('host_negative_controls.output_path', return_value=output), \
                patch('host_negative_controls.baseline_path', return_value=self.root), \
                patch('host_negative_controls.subprocess.run', return_value=completed), \
                patch('host_negative_controls.read_plain', return_value=b'{"status":"BLOCKED"}'), \
                redirect_stdout(io.StringIO()):
            self.assertEqual(host_control_main(), 2)
        report = json.loads((output / 'control.json').read_text())
        self.assertEqual(report['status'], 'BLOCKED')
        self.assertIsNone(report['scenario'])

    def test_exact_ts_warning_prefix_only_once_without_mutating_raw_evidence(self):
        source = self.source()
        original = self.root / 'original'
        shutil.copytree(source, original)
        warning = (b'(node:123) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n'
                   b'(Use `node --trace-warnings ...` to show where the warning was created)\n')
        case = source / 'cases/flag-missing'
        data = warning + (case / 'stderr.bin').read_bytes()
        write_stream(case, 'stderr', data)
        # Split both lines across pipe reads, with stdout interspersed. Removal
        # must preserve all remaining stream order and coalesce newly adjacent reads.
        events = [('stderr', warning[:30]), ('stdout', b'a'), ('stderr', warning[30:]),
                  ('stdout', b'b'), ('stderr', b'product')]
        stripped, view = comparison_view('ts', warning + b'product', events)
        self.assertEqual(stripped, b'product')
        self.assertEqual(view, [('stdout', b'ab'), ('stderr', b'product')])
        self.assertEqual(compare(original, source)['status'], 'PASS')
        self.assertEqual((case / 'stderr.bin').read_bytes(), data)
        self.assertEqual(comparison_view('ts', warning * 2, [('stderr', warning * 2)])[0], warning)
        self.assertEqual(comparison_view('rust', warning, [('stderr', warning)])[0], warning)

    def test_product_experimental_warning_and_nonprefix_sqlite_are_compared(self):
        source = self.source()
        for name, extra in (
            ('product-name', b'(node:12) ExperimentalWarning: product-canary\n'),
            ('product-code', b'(node:12) [ExperimentalWarning] ProductWarning: product-canary\n'),
            ('nonprefix-sqlite', b'product\n(node:12) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n(Use `node --trace-warnings ...` to show where the warning was created)\n'),
            ('almost-sqlite', b'(node:12) ExperimentalWarning: SQLite is an experimental feature and might change at any time\r\n(Use `node --trace-warnings ...` to show where the warning was created)\n')):
            with self.subTest(name=name):
                target = self.root / name
                shutil.copytree(source, target)
                write_stream(target / 'cases/help-long', 'stderr', extra)
                result = compare(source, target)
                self.assertIn('help-long.stderr', [r['rule'] for r in result['failures']])
                self.assertEqual(comparison_view('ts', extra, [('stderr', extra)])[0], extra)

    def test_rust_sqlite_warning_is_a_difference(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        warning = (b'(node:1) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n'
                   b'(Use `node --trace-warnings ...` to show where the warning was created)\n')
        write_stream(source / 'cases/help-long', 'stderr', warning)
        self.assertEqual(compare(source, target, expected)['status'], 'PASS')
        write_stream(target / 'cases/help-long', 'stderr', warning)
        result = compare(source, target, expected)
        self.assertIn('help-long.stderr', [r['rule'] for r in result['failures']])

    def test_help_middle_drift_rejected_even_when_both_sides_agree(self):
        source = self.source()
        case = source / 'cases/help-long'
        write_stream(case, 'stdout', (case / 'stdout.bin').read_bytes() + b'undocumented command\n')
        result = compare(source, source)
        self.assertIn('help-long.oracle.stdout', [r['rule'] for r in result['failures']])

    def test_rust_evidence_requires_independent_binary_and_source_binding(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        self.assertEqual(compare(source, target, expected)['status'], 'PASS')
        for field, bad in (('executable', {'path': '/another/candidate', 'sha256': '2' * 64}),
                           ('sourceHead', 'b' * 40), ('sourcePath', '/another/source'), ('side', 'ts')):
            with self.subTest(field=field):
                result = compare(source, target, {**expected, field: bad})
                self.assertIn('round.rust-binding', [r['rule'] for r in result['failures']])
        self.assertIn('round.rust-binding', [r['rule'] for r in compare(source, target)['failures']])
        self.assertIn('round.expected-rust-side', [r['rule'] for r in compare(source, source, expected)['failures']])

    def test_side_relabel_and_ts_provenance_in_rust_cannot_pass(self):
        source = self.source()
        target = self.root / 'relabel'
        shutil.copytree(source, target)
        path = target / 'round.json'
        run = json.loads(path.read_text())
        run['side'] = 'rust'
        save(path, run)
        result = compare(source, target)
        self.assertIn('round.provenance-side', [r['rule'] for r in result['failures']])
        # Even changing the tag cannot make the Node executable a Rust source.
        expected = as_rust(target)
        run = json.loads(path.read_text())
        run['provenance']['executable'] = run['provenance']['node']
        save(path, run)
        self.assertIn('round.rust-source', [r['rule'] for r in compare(source, target, expected)['failures']])

    def test_baseline_dependency_and_skills_identity_must_match(self):
        source = self.source()
        for field in ('node_modules', 'skills'):
            target = self.root / field
            shutil.copytree(source, target)
            path = target / 'round.json'
            run = json.loads(path.read_text())
            run['provenance']['baseline']['trees'][field] = '1' * 64
            save(path, run)
            self.assertIn('round.baseline', [r['rule'] for r in compare(source, target)['failures']])
            del run['provenance']['baseline']['trees'][field]
            save(path, run)
            self.assertIn('round.baseline-trees', [r['rule'] for r in compare(source, target)['failures']])

    def test_installed_manifest_covers_hidden_dependencies_content_skills_and_links(self):
        baseline = self.root / 'baseline'
        for name in ('src', 'dist/src/cli', 'packages', 'node_modules/.pnpm/pkg', 'skills'):
            (baseline / name).mkdir(parents=True)
        (baseline / 'package.json').write_text('{}')
        (baseline / 'dist/src/cli/pilotdeck.js').write_text('entry')
        dependency = baseline / 'node_modules/.pnpm/pkg/index.js'
        dependency.write_text('original')
        with patch('run_round.git_head', return_value=HEAD):
            first = baseline_identity(baseline)
            dependency.write_text('modified')
            second = baseline_identity(baseline)
            self.assertNotEqual(first['trees']['node_modules'], second['trees']['node_modules'])
            (baseline / 'skills/new.md').write_text('skill')
            third = baseline_identity(baseline)
            self.assertNotEqual(second['trees']['skills'], third['trees']['skills'])
            # pnpm hardlinks are legal; their contents still participate.
            os.link(dependency, baseline / 'node_modules/hardlink.js')
            link = baseline / 'node_modules/pkg'
            link.symlink_to('.pnpm/pkg', target_is_directory=True)
            fourth = baseline_identity(baseline)
            self.assertNotEqual(third['trees']['node_modules'], fourth['trees']['node_modules'])
            (baseline / 'apps/desktop').mkdir(parents=True)
            (baseline / 'node_modules/desktop').symlink_to('../apps/desktop')
            fifth = baseline_identity(baseline)
            self.assertNotEqual(fourth['trees']['node_modules'], fifth['trees']['node_modules'])
            policy = sandbox_profile(self.root / 'runtime', baseline, '/installed/node', 'ts')
            self.assertNotIn(str(baseline / 'apps'), policy)
            link.unlink()
            link.symlink_to(self.root / 'outside')
            with self.assertRaises(Blocked):
                baseline_identity(baseline)

    def test_openssl_is_controlled_and_profiles_separate_exec_and_reads(self):
        runtime, baseline = self.root / 'runtime', self.root / 'baseline'
        fixture(runtime)
        self.assertEqual((runtime / 'openssl.cnf').read_bytes(), b'')
        self.assertEqual(environment(runtime, SCENARIOS[0])['OPENSSL_CONF'], str(runtime / 'openssl.cnf'))
        ts = sandbox_profile(runtime, baseline, '/opt/homebrew/bin/node', 'ts')
        rust = sandbox_profile(runtime, baseline, '/candidate/rust-stage/bin/pilotdeck', 'rust')
        self.assertIn('(allow process-exec (literal "/candidate/rust-stage/bin/pilotdeck"))', rust)
        self.assertEqual(rust.count('(allow process-exec'), 1)
        self.assertIn('(subpath "/candidate/rust-stage/skills")', rust)
        self.assertNotIn('(subpath "/candidate/rust-stage")', rust)
        self.assertNotIn('(subpath "/candidate/rust-stage/bin")', rust)
        with self.assertRaises(ValueError):
            sandbox_profile(runtime, baseline, '/candidate/pilotdeck', 'rust')
        for policy in (ts, rust):
            self.assertNotIn('/opt/homebrew/etc/openssl@3', policy)
            self.assertNotIn('(allow file-read-metadata)', policy)
            self.assertNotIn('opendirectoryd', policy)
        self.assertIn(str(baseline / 'node_modules'), ts)
        self.assertNotIn(str(baseline), rust)
        self.assertNotIn('homebrew', rust)
        self.assertNotIn('/opt/homebrew/bin/node', rust)

    def test_rust_stage_copies_binary_and_skills_and_keeps_baseline_unreadable(self):
        baseline, output = self.root / 'baseline', self.root / 'out'
        (baseline / 'skills/docx').mkdir(parents=True)
        (baseline / 'skills/docx/SKILL.md').write_text('skill')
        output.mkdir()
        original = self.root / 'pilotdeck-built'
        original.write_bytes(b'synthetic binary')
        original.chmod(0o755)
        expected = {'executable': {'path': str(original), 'sha256': hashlib.sha256(b'synthetic binary').hexdigest()}}
        staged = stage_rust(output, expected, baseline)
        self.assertEqual(staged, {'path': str(output / 'rust-stage/bin/pilotdeck'),
                                  'sha256': expected['executable']['sha256']})
        self.assertEqual((output / 'rust-stage/skills/docx/SKILL.md').read_text(), 'skill')
        policy = sandbox_profile(output / 'runtime', baseline, staged['path'], 'rust')
        self.assertIn(f'(subpath "{output / "rust-stage/skills"}")', policy)
        self.assertIn(f'(allow process-exec (literal "{staged["path"]}"))', policy)
        self.assertNotIn(str(original), policy)
        self.assertNotIn(str(baseline), policy)
        # A copy that does not hash like --rust-bin is never run.
        tampered = {'executable': {**expected['executable'], 'sha256': '1' * 64}}
        (self.root / 'out2').mkdir()
        with self.assertRaises(Blocked):
            stage_rust(self.root / 'out2', tampered, baseline)
        (baseline / 'skills/link').symlink_to('docx', target_is_directory=True)
        (self.root / 'out3').mkdir()
        with self.assertRaises(Blocked):
            stage_rust(self.root / 'out3', expected, baseline)
        # A root-level skills/ link is materialized by copytree, so it is refused up front.
        (baseline / 'skills/link').unlink()
        (baseline / 'skills').rename(baseline / 'real-skills')
        (baseline / 'skills').symlink_to('real-skills', target_is_directory=True)
        (self.root / 'out4').mkdir()
        with self.assertRaisesRegex(Blocked, 'is a symlink'):
            stage_rust(self.root / 'out4', expected, baseline)

    def test_rust_round_must_run_the_registered_stage_copy(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        self.assertEqual(compare(source, target, expected)['status'], 'PASS')
        path = target / 'round.json'
        run = json.loads(path.read_text())
        for staged in ({**run['provenance']['stagedExecutable'], 'sha256': '2' * 64},
                       {**run['provenance']['stagedExecutable'], 'path': '/synthetic/rust-stage/bin/pilotdeck'}):
            with self.subTest(staged=staged):
                mutated = json.loads(json.dumps(run))
                mutated['provenance']['stagedExecutable'] = staged
                save(path, mutated)
                self.assertIn('round.rust-stage', [r['rule'] for r in compare(source, target, expected)['failures']])
        del run['provenance']['stagedExecutable']
        save(path, run)
        self.assertEqual(compare(source, target, expected)['status'], 'FAIL')

    def test_offline_compare_reads_staged_copy_bytes(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        self.assertEqual(compare(source, target, expected)['status'], 'PASS')
        staged = self.root / 'rust-stage/bin/pilotdeck'
        elsewhere = self.root / 'elsewhere'
        elsewhere.write_bytes(SYNTHETIC_RUST)
        for name in ('content', 'missing', 'symlink'):
            with self.subTest(mutation=name):
                staged.unlink(missing_ok=True)
                if name == 'content':
                    staged.write_bytes(b'tampered rust candidate\n')
                elif name == 'symlink':
                    staged.symlink_to(elsewhere)  # Same bytes behind a link still fail.
                rules = [r['rule'] for r in compare(source, target, expected)['failures']]
                self.assertIn('round.rust-stage-bytes', rules)

    def test_plain_fd_reads_reject_leaf_replacement_and_hardlinks(self):
        target = self.root / 'target'
        other = self.root / 'other'
        other.write_bytes(b'same bytes')
        real_open = os.open
        for reader in (digest_plain, read_plain):
            for mutation in ('symlink', 'regular', 'hardlink', 'added-link', 'fifo'):
                with self.subTest(reader=reader.__name__, mutation=mutation):
                    target.unlink(missing_ok=True)
                    target.write_bytes(b'same bytes')
                    saved = self.root / 'saved'
                    saved.unlink(missing_ok=True)
                    def swap(path, flags, *args, **kwargs):
                        self.assertEqual(Path(path), target)
                        # Keep the old inode alive so the replacement cannot
                        # accidentally reuse it on filesystems with fast reuse.
                        if mutation == 'added-link':
                            os.link(target, saved)
                            return real_open(path, flags, *args, **kwargs)
                        target.rename(saved)
                        if mutation == 'symlink':
                            target.symlink_to(other)
                        elif mutation == 'regular':
                            target.write_bytes(b'same bytes')
                        elif mutation == 'hardlink':
                            os.link(other, target)
                        else:
                            os.mkfifo(target)
                        return real_open(path, flags, *args, **kwargs)
                    with patch('common.os.open', side_effect=swap) as opened:
                        with self.assertRaises((OSError, ValueError)):
                            reader(target)
                        self.assertEqual(opened.call_count, 1)
        target.unlink()
        target.write_bytes(b'plain file')
        self.assertEqual(read_plain(target), b'plain file')
        self.assertEqual(digest_plain(target), hashlib.sha256(b'plain file').hexdigest())

    def test_snapshot_rejects_symlink_swapped_between_lstat_and_open(self):
        tree = self.root / 'tree'
        tree.mkdir()
        target = tree / 'file'
        target.write_bytes(b'identical')
        elsewhere = self.root / 'elsewhere'
        elsewhere.write_bytes(b'identical')
        real_open = os.open
        def swap(path, flags, *args, **kwargs):
            if Path(path) == target:
                target.unlink()
                target.symlink_to(elsewhere)
            return real_open(path, flags, *args, **kwargs)
        with patch('common.os.open', side_effect=swap) as opened:
            with self.assertRaises((OSError, ValueError)):
                snapshot(tree)
            self.assertEqual(opened.call_count, 1)

    def test_compare_rejects_symlink_swapped_during_each_evidence_read(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        case = target / 'cases/help-long'
        real_open = os.open
        # Derive victims from the actual reader, including snapshot(tree) and
        # staged artifacts. New read points automatically join this regression.
        with patch('common.os.open', wraps=real_open) as opened:
            _, _, errors = load_round(target)
        self.assertEqual(errors, [])
        paths = list(dict.fromkeys(Path(call.args[0]) for call in opened.call_args_list))
        self.assertTrue(paths)
        self.assertTrue(any(path.is_relative_to(case / 'tree') for path in paths))
        for victim in paths:
            with self.subTest(victim=victim):
                original = victim.read_bytes()
                mode = stat.S_IMODE(victim.lstat().st_mode)
                elsewhere = self.root / 'same-bytes'
                elsewhere.write_bytes(original)
                swapped = False
                try:
                    def swap(path, flags, *args, **kwargs):
                        nonlocal swapped
                        if Path(path) == victim and not swapped:
                            victim.unlink()
                            victim.symlink_to(elsewhere)
                            swapped = True
                        return real_open(path, flags, *args, **kwargs)
                    with patch('common.os.open', side_effect=swap):
                        result = compare(source, target, expected)
                    self.assertTrue(swapped)
                    self.assertEqual(result['status'], 'FAIL', result)
                finally:
                    # #245: restore content AND mode inside try/finally, so a
                    # subTest failure cannot leave damage that weakens the
                    # refusal criteria of every later victim.
                    victim.unlink(missing_ok=True)
                    victim.write_bytes(original)
                    os.chmod(victim, mode)

    def test_offline_compare_revalidates_staged_skills_manifest_and_disk(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        manifest = target / 'round.json'
        original = json.loads(manifest.read_text())
        skills = self.root / 'rust-stage/skills'
        pristine = self.root / 'pristine-skills'
        shutil.copytree(skills, pristine)
        self.assertEqual(compare(source, target, expected)['status'], 'PASS')
        for mutation in ('content', 'addition', 'missing-file', 'mode', 'hardlink', 'symlink',
                         'missing-root', 'root-symlink', 'missing-provenance', 'wrong-path', 'wrong-sha'):
            with self.subTest(mutation=mutation):
                if skills.is_symlink():
                    skills.unlink()
                elif skills.exists():
                    shutil.rmtree(skills)
                shutil.copytree(pristine, skills)
                run = json.loads(json.dumps(original))
                skill = skills / 'docx/SKILL.md'
                if mutation == 'content':
                    skill.write_text('changed skill')
                elif mutation == 'addition':
                    (skills / '.hidden').write_text('extra')
                elif mutation == 'missing-file':
                    skill.unlink()
                elif mutation == 'mode':
                    skill.chmod(0o700)
                elif mutation == 'hardlink':
                    os.link(skill, skills / 'linked')
                elif mutation == 'symlink':
                    skill.unlink()
                    skill.symlink_to(pristine / 'docx/SKILL.md')
                elif mutation in ('missing-root', 'root-symlink'):
                    shutil.rmtree(skills)
                    if mutation == 'root-symlink':
                        skills.symlink_to(pristine, target_is_directory=True)
                elif mutation == 'missing-provenance':
                    del run['provenance']['stagedSkills']
                elif mutation == 'wrong-path':
                    run['provenance']['stagedSkills']['path'] = str(pristine)
                else:
                    run['provenance']['stagedSkills']['sha256'] = 'f' * 64
                save(manifest, run)
                rules = [r['rule'] for r in compare(source, target, expected)['failures']]
                self.assertIn('round.rust-stage-skills-bytes', rules)

    def test_pending_oracle_cannot_pass(self):
        compare_module = _compare
        pending = [{'id': 'pending', 'argv': [], 'stdinHex': '', 'token': False,
                    'oracle': compare_module.PENDING}]
        source = self.root / 'pending'
        with patch.object(compare_module, 'SCENARIOS', pending), \
             patch.object(sys.modules[__name__], 'SCENARIOS', pending):
            synthetic_round(source, self.root / 'pending-runtime')
            result = compare_module.compare(source, source)
        self.assertEqual(result['status'], 'FAIL')
        self.assertIn('pending.oracle.pending', [r['rule'] for r in result['failures']])

    def test_registry_has_calibrated_core_and_skills_batch(self):
        self.assertEqual(len(CALIBRATED_SCENARIOS), 30)
        self.assertEqual(len(REGISTERED_SCENARIOS), 30)
        # #933: the mock-gateway batch is calibrated from two real TS rounds
        # (run A, ts-1 == ts-2); no scenario may stay PENDING, and the mock
        # scenario's oracle never comes from Rust output.
        self.assertEqual(PENDING_SCENARIOS, [])
        self.assertEqual([s['id'] for s in MOCK_GATEWAY_SCENARIOS], ['sessions-list-mock'])
        mock_scenario = MOCK_GATEWAY_SCENARIOS[0]
        self.assertEqual(mock_scenario['argv'], ['sessions', 'list'])
        self.assertTrue(mock_scenario['token'])
        self.assertEqual(mock_scenario['exit'], 0)
        self.assertIn('"d0-mock-session-1"', mock_scenario['stdout'])
        self.assertEqual(mock_scenario['stderr'], '')
        self.assertNotIn('oracle', mock_scenario)
        for scenario in CALIBRATED_SCENARIOS:
            self.assertIn('exit', scenario)
            self.assertTrue('stderr' in scenario or 'stderrSha256' in scenario)
        self.assertEqual(len(SKILLS_SCENARIOS), 16)
        self.assertEqual(len(CALIBRATED_SKILLS_SCENARIOS), 16)
        for scenario in CALIBRATED_SKILLS_SCENARIOS:
            self.assertNotIn('oracle', scenario)
            self.assertIn('stdout', scenario)
            before = initial_tree(scenario)
            removals = scenario.get('diskRemovals', [])
            additions = scenario.get('diskAdditions', {})
            self.assertEqual(len(removals), len(set(removals)))
            self.assertLessEqual(set(removals), before.keys())
            self.assertNotIn('.', removals)
            self.assertFalse((before.keys() - set(removals)) & additions.keys())
        self.assertEqual(sum('stdoutRuntime' in s for s in SKILLS_SCENARIOS), 13)
        self.assertEqual(per_stream_scenarios(REGISTERED_SCENARIOS), {'legacy-skills-mixed', 'legacy-skills-current'})
        for scenario in CALIBRATED_SKILLS_SCENARIOS:
            if scenario['id'] == 'legacy-skills-mixed':
                self.assertIn('d0-final/ts-1/cases/legacy-skills-mixed/events.json', scenario['streamOrderReason'])
                self.assertIn('d0-final/ts-2/cases/legacy-skills-mixed/events.json', scenario['streamOrderReason'])
            elif scenario['id'] == 'legacy-skills-current':
                # #933 batch: the same recorder race as legacy-skills-mixed
                # flipped this scenario's observed interleave under load.
                self.assertIn('20261026-issue-933/final-round/ts-1/cases/legacy-skills-current/events.json',
                              scenario['streamOrderReason'])
                self.assertIn('20261026-issue-933/final-round/ts-2/cases/legacy-skills-current/events.json',
                              scenario['streamOrderReason'])
            else:
                self.assertNotIn('streamOrder', scenario)
                self.assertNotIn('streamOrderReason', scenario)
                expected_streams = 2 if scenario['id'] == 'legacy-skills-current' else 1
                self.assertEqual(sum(bool(scenario[channel]) for channel in ('stdout', 'stderr')), expected_streams)

    # #933 D0-pr2: mock gateway relaxation — marker, profile, evidence, lifecycle.

    @contextmanager
    def mock_gateway_rounds(self, side):
        cases = [{'id': 'plain', 'argv': [], 'stdinHex': '', 'token': False, 'exit': 1,
                  'stdout': 'migration summary\n', 'stderr': 'configuration error\n'},
                 {'id': 'marked', 'argv': ['sessions', 'list'], 'stdinHex': '', 'token': True,
                  'mockGateway': True, 'exit': 0, 'stdout': '[\n  "sessions"\n]\n', 'stderr': ''}]
        with patch('compare.SCENARIOS', cases), patch.object(sys.modules[__name__], 'SCENARIOS', cases):
            source, target = self.root / (side + '-mock-source'), self.root / (side + '-mock-target')
            synthetic_round(source, self.root / 'mock-runtime')
            shutil.copytree(source, target)
            expected = as_rust(target) if side == 'rust' else None
            yield source, target, expected

    def test_mock_gateway_marker_is_strict_boolean_true(self):
        # #933-1: a truthy-but-not-True marker must not widen the relaxation.
        for bad in ('true', 'True', 'yes', 1, ['true'], {'marked': True}):
            with self.subTest(bad=bad):
                # Message-carrying refusal: the #933 negative controls bind to
                # this assertion, so it must not fail message-less.
                try:
                    mock_gateway_scenarios([{'id': 'marked', 'mockGateway': bad}])
                except Blocked as error:
                    self.assertIn('apparatus.mock-gateway', str(error),
                                  f'#933-1: marker {bad!r} must be refused as an apparatus error')
                else:
                    self.fail(f'#933-1: marker {bad!r} must be refused, not accepted')
        # Message-carrying assertions throughout: the #933 negative controls
        # bind to this test, so no failure point may be message-less.
        self.assertEqual(mock_gateway_scenarios([{'id': 'a'}, {'id': 'b', 'mockGateway': True}]), {'b'},
                        '#933-1: a strict True marker must register the scenario')
        self.assertEqual(mock_gateway_scenarios([{'id': 'a'}]), set(),
                        '#933-1: an unmarked registry must register nothing')
        with patch('compare.SCENARIOS', [{'id': 'marked', 'mockGateway': 'true'}]), \
             patch('compare.load_round') as load:
            result = compare(self.root / 'left', self.root / 'right')
        self.assertEqual(result['status'], 'BLOCKED',
                        '#933-1: a truthy-but-not-True registry must block compare')
        self.assertEqual(result['failures'][0]['rule'], 'apparatus.mock-gateway',
                        '#933-1: the block must name the mock-gateway apparatus rule')
        load.assert_not_called()

    def test_mock_profile_appends_only_the_loopback_allow(self):
        # #933-2 (fix-r4 P1-1): every profile the recorder can emit is pinned
        # by digest over the fixed placeholder paths — no normalization step
        # exists that could mask a permission change. The four combinations
        # cover both sides for the 29 unmarked scenarios (base) and for
        # sessions-list-mock (mock); the ts base doubles as the probe profile
        # both sides record (bind_policy always probes with the node binary on
        # the ts side).
        # fix-r5 P1: the pinned inputs sit under pairwise-disjoint roots, and
        # away from the policy's fixed read paths, so every ancestor of every
        # input is authorized by that input alone: a mutant granting an
        # input's parent the profile does not already carry (the review's
        # ancestors.add(SCRIPT.parent) widening the rust metadata allow to the
        # checkout's scripts dir) cannot keep the pinned bytes, and a digest
        # flips. A shared root — the pre-fix single-/PDRS shape, where exactly
        # that mutant survived — is refused here, not rediscovered by a
        # negative control.
        trees = pinned_trees()
        for (name_a, tree_a), (name_b, tree_b) in combinations(trees, 2):
            with self.subTest(pair=f'{name_a}/{name_b}'):
                shared = tree_a & tree_b
                self.assertFalse(shared,
                                 f'#933-2: pinned inputs {name_a} and {name_b} must sit '
                                 f'under disjoint roots; they share {sorted(shared)}')
        fixed = {parent for path in (Path('/System'), Path('/usr/lib'), Path('/Library/Apple'),
                                     Path('/dev/null'), Path('/dev/urandom'), Path('/dev/random'),
                                     Path('/opt/homebrew/Cellar'), Path('/opt/homebrew/opt'))
                 for parent in path.parents} - {Path('/')}
        for name, tree in trees:
            with self.subTest(overlap=name):
                shared = tree & fixed
                self.assertFalse(shared,
                                 f'#933-2: pinned input {name} must not sit under a fixed '
                                 f'policy path; it shares {sorted(shared)}')
        pinned = {('ts', False): 'c5eb273a8170174ede7343d69efd4701b13cc34136eb918a639586c23979ea16',
                  ('rust', False): '635cf7ad7e36311de52ccf4501114ada51ae0ab0d5154c5e7b72aa4e1b93648c',
                  ('ts', True): 'fac4f3aaeb764a89f76d8bcc58086dd16a44d89ec873d62e1d5ff60e5af704c3',
                  ('rust', True): '7ae751c55e079928960f1fde95152eed39a1f7694c6d7d62586936853e949c29'}
        for (side, mock), digest in pinned.items():
            with self.subTest(side=side, mock=mock):
                self.assertEqual(
                    hashlib.sha256(pinned_profile(side, mock).encode()).hexdigest(), digest,
                    f'#933-2: the {"mock " if mock else "base "}{side.upper()} profile changed')
        # A different real checkout — non-ASCII, or nested under a system path —
        # cannot flip the guard: every path input is pinned by the guard itself.
        for other_root in ('/another-checkout', '/tmp/中文树根', '/usr/local/pilotdeck-checkout'):
            with self.subTest(root=other_root):
                with patch('policy.SCRIPT', Path(other_root) / 'scripts/cli-parity'):
                    self.assertEqual(
                        hashlib.sha256(pinned_profile('ts').encode()).hexdigest(),
                        pinned[('ts', False)],
                        f'#933-2: the pinned digest must hold in a checkout rooted at {other_root}')
        runtime, baseline = Path('/synthetic/runtime'), Path('/synthetic/baseline')
        ts = sandbox_profile(runtime, baseline, '/synthetic/unit-test-only', 'ts')
        rust = sandbox_profile(runtime, baseline, '/synthetic/rust-stage/bin/pilotdeck', 'rust')
        relax = f'(allow network-outbound (remote ip "localhost:{MOCK_PORT}"))\n'
        for side, base in (('ts', ts), ('rust', rust)):
            binary = '/synthetic/unit-test-only' if side == 'ts' else '/synthetic/rust-stage/bin/pilotdeck'
            with self.subTest(side=side):
                mock = sandbox_profile(runtime, baseline, binary, side, mock_gateway=True)
                self.assertEqual(mock, base.replace('(deny network*)\n', '(deny network*)\n' + relax),
                                 '#933-2: exactly one allow rule, directly after the network deny')
                self.assertEqual(mock.count('network-outbound'), 1,
                                 '#933-2: the mock profile must add exactly one network-outbound rule')
                self.assertNotIn('18789', mock,
                                 '#933-2: the mock profile must not open the user port 18789')
        self.assertEqual(sandbox_profile(runtime, baseline, '/synthetic/unit-test-only', 'ts', mock_gateway=False), ts,
                         '#933-2: the default profile must not carry the allow rule')
        with self.assertRaisesRegex(ValueError, 'boolean',
                                    msg='#933-2: a non-boolean mock_gateway must be refused'):
            sandbox_profile(runtime, baseline, '/synthetic/unit-test-only', 'ts', mock_gateway='true')
        # The allow rule and every gateway coordinate derive from one constant,
        # so the sandbox surface and the CLI's gateway URL cannot drift apart.
        self.assertEqual(MOCK_GATEWAY_ALLOW, f'(allow network-outbound (remote ip "localhost:{MOCK_PORT}"))',
                         '#933-2: the allow rule must be derived from MOCK_PORT')
        env = environment(runtime, {'token': False})
        for key, expected in (('PILOTDECK_GATEWAY_PORT', str(MOCK_PORT)),
                              ('PILOTDECK_GATEWAY_URL', f'ws://127.0.0.1:{MOCK_PORT}/ws'),
                              ('PILOTDECK_SDK_GATEWAY_URL', f'ws://127.0.0.1:{MOCK_PORT}/ws')):
            self.assertEqual(env[key], expected,
                             f'#933-2: {key} must be exactly the value derived from MOCK_PORT')

    def test_mock_scenario_binds_mock_profile_and_others_base(self):
        # #933-3: each scenario must be bound to its own profile evidence.
        for side in ('ts', 'rust'):
            with self.subTest(side=side), self.mock_gateway_rounds(side) as (source, target, expected):
                # Sanity gate: must not fail message-less — the #933-3 negative
                # control binds this test, and a broken binding check rejects
                # valid evidence before any tampering happens.
                self.assertEqual(compare(source, target, expected)['status'], 'PASS',
                                '#933-3: the untampered round must compare PASS; '
                                'a broken profile binding rejects valid evidence')
                run = json.loads((target / 'round.json').read_text())
                for name, wrong in (('marked', run['isolation']), ('plain', run['mockIsolation'])):
                    with self.subTest(case=name):
                        path = target / 'cases' / name / 'record.json'
                        original = path.read_bytes()
                        record = json.loads(original)
                        record['sandboxArgv'] = ['/usr/bin/sandbox-exec', '-f', wrong['profilePath'],
                                                 *record['executedArgv']]
                        record['profileSha256'] = wrong['profileSha256']
                        save(path, record)
                        self.assertIn(f'{name}.sandbox-binding',
                                     [r['rule'] for r in compare(source, target, expected)['failures']],
                                     f'#933-3: {name} must run under its registered profile')
                        path.write_bytes(original)

    def test_mock_gateway_isolation_evidence_required_and_tamper_detected(self):
        # #933-4: every mock-gateway evidence field is checked, both sides.
        for side in ('ts', 'rust'):
            with self.subTest(side=side), self.mock_gateway_rounds(side) as (source, target, expected):
                self.assertEqual(compare(source, target, expected)['status'], 'PASS',
                                '#933-4: the untampered round must compare PASS before tampering')
                path = target / 'round.json'
                original = json.loads(path.read_text())
                def mutate(change):
                    run = json.loads(json.dumps(original))
                    change(run['mockIsolation'])
                    save(path, run)
                    return [r['rule'] for r in compare(source, target, expected)['failures']]
                mutations = (
                    ('probe-allowed', lambda iso: iso['gatewayProbe'].update(connectAllowed=False), 'round.gateway-probe'),
                    ('probe-default-gateway', lambda iso: iso['gatewayProbe'].update(connectDefaultGatewayDenied=False), 'round.gateway-probe'),
                    ('probe-other-port', lambda iso: iso['gatewayProbe'].update(connectOtherDenied=False), 'round.gateway-probe'),
                    ('probe-unix-socket', lambda iso: iso['gatewayProbe'].update(connectUnixDenied=False), 'round.gateway-probe'),
                    ('probe-listen', lambda iso: iso['gatewayProbe'].update(listenDenied=False), 'round.gateway-probe'),
                    ('profile-text', lambda iso: iso.update(profileText=iso['profileText'] + '(allow default)\n'), 'round.mock-profile-binding'),
                    ('profile-sha', lambda iso: iso.update(profileSha256='0' * 64), 'round.mock-profile-binding'),
                    ('probe-profile-sha', lambda iso: iso.update(probeProfileSha256='0' * 64), 'round.mock-probe-profile'),
                    ('process-pid', lambda iso: iso['mockProcess'].update(pid=0), 'round.mock-process'),
                    ('process-argv', lambda iso: iso['mockProcess'].update(argv=['/node', 'mock_gateway.mjs', '29000']), 'round.mock-process'),
                    ('process-token', lambda iso: iso['mockProcess'].update(token={'redacted': True, 'length': 3, 'sha256': '0' * 64}), 'round.mock-process'),
                    ('probe-cleanup', lambda iso: iso['audit'].update(remaining=[99]), 'round.mock-probe-cleanup'),
                    ('unix-socket-left', lambda iso: iso.update(probeCleanup={'unixSocketRemoved': False}), 'round.mock-probe-cleanup'),
                    ('status', lambda iso: iso.update(status='BLOCKED'), 'round.mock-isolation'),
                )
                for label, change, rule in mutations:
                    with self.subTest(side=side, mutation=label):
                        self.assertIn(rule, mutate(change), f'#933-4: {label} tamper not detected')
                if side == 'rust':
                    with self.subTest(side=side, mutation='rust-node-exec'):
                        self.assertIn('round.mock-node-exec', mutate(lambda iso: iso['nodeExec'].update(denied=False)),
                                      '#933-4: rust mock Node-exec denial not checked')
                # Removing the evidence entirely fails presence instead of comparing equal.
                run = json.loads(json.dumps(original))
                del run['mockIsolation']
                save(path, run)
                self.assertIn('round.mock-gateway-presence',
                              [r['rule'] for r in compare(source, target, expected)['failures']],
                              '#933-4: removed mock evidence must fail presence')
                save(path, original)
                self.assertEqual(compare(source, target, expected)['status'], 'PASS',
                                '#933-4: the restored round must compare PASS again')
        # Evidence attached without any marked scenario is equally rejected.
        source = self.source()
        target = self.root / 'extra-mock-evidence'
        shutil.copytree(source, target)
        path = target / 'round.json'
        run = json.loads(path.read_text())
        run['mockIsolation'] = {'status': 'PASS', 'errors': [], 'exitCode': 0, 'profileText': '',
                                'profileSha256': '0' * 64, 'probeProfileSha256': '0' * 64,
                                'gatewayProbe': {}, 'audit': {}, 'probeCleanup': {}, 'mockProcess': {}}
        save(path, run)
        self.assertIn('round.mock-gateway-presence', [r['rule'] for r in compare(source, target)['failures']],
                      '#933-4: mock evidence without a marked scenario must fail presence')

    def test_occupied_mock_port_blocks_round(self):
        # #933-5: the mock owns 28999 by binding it itself (no check-then-use
        # preflight); an occupied port — or any other early exit — must Block
        # the whole round, and the refusal must fire before the probe runs.
        class FakeMock:
            def __init__(self, exitcode):
                self.pid, self.returncode = 999_000_001, exitcode

            def poll(self):
                return self.returncode

            def wait(self, timeout=None):
                return self.returncode

        profile = self.root / 'occupied.sb'
        profile.write_text('synthetic policy')
        (self.root / 'tmp').mkdir()
        # Case 1: a real mock against a genuinely occupied 28999 — the owned
        # process cannot bind and exits; the round must refuse to continue.
        node = shutil.which('node')
        if node is not None:
            listener = socket.socket()
            listener.bind(('127.0.0.1', MOCK_PORT))
            listener.listen(1)
            self.addCleanup(listener.close)
            output = self.root / 'occupied-real'
            output.mkdir()
            try:
                mock_gateway_preflight(output, self.root, node, profile)
            except Blocked as error:
                self.assertIn('exited before listening', str(error),
                              '#933-5: an occupied 28999 must make the owned mock exit and Block the round')
            else:
                self.fail('#933-5: an occupied 28999 must Block the round; there is no fallback port')
        # Case 2: the mock process is already dead — the refusal precedes any probe.
        output = self.root / 'occupied-dead'
        output.mkdir()
        with patch('run_round.spawn', return_value=FakeMock(1)), \
             patch('run_round.capture') as captured, patch('run_round.stop_group') as stopped:
            stopped.return_value = {'allStopped': True, 'remaining': [], 'errors': []}
            try:
                mock_gateway_preflight(output, self.root, '/synthetic/node', profile)
            except Blocked as error:
                self.assertIn('exited before listening', str(error),
                              '#933-5: an early-exiting mock must Block the round before any probe')
            else:
                self.fail('#933-5: an early-exiting mock must refuse the round, not pass the preflight')
            captured.assert_not_called()
            stopped.assert_called_once()
        # Case 3: the mock confirmed listening, then died — the aliveness
        # re-check after the probe must still Block the round.
        output = self.root / 'occupied-after'
        output.mkdir()

        def dying_spawn(argv, **kwargs):
            kwargs['stdout'].write((mock_listening_line(MOCK_PORT) + '\n').encode())
            kwargs['stdout'].flush()
            return FakeMock(1)

        record = {'status': 'DONE', 'timedOut': False, 'exitCode': 0,
                  'audit': {'allStopped': True, 'remaining': [], 'errors': []}}
        with patch('run_round.spawn', side_effect=dying_spawn), \
             patch('run_round.capture', return_value=(record, {'stdout': b'{}', 'stderr': b''}, [])), \
             patch('run_round.stop_group') as stopped:
            stopped.return_value = {'allStopped': True, 'remaining': [], 'errors': []}
            try:
                mock_gateway_preflight(output, self.root, '/synthetic/node', profile)
            except Blocked as error:
                self.assertIn('exited after readiness', str(error),
                              '#933-5: a mock that dies after readiness must Block the round')
            else:
                self.fail('#933-5: a dead mock after readiness must refuse the round, not pass the preflight')
            stopped.assert_called_once()

    def test_mock_gateway_answers_ping_with_pong(self):
        # #933-7 (review P1-2): a valid RFC 6455 ping must be answered with a
        # pong carrying the same payload, and must not close the connection.
        node = shutil.which('node')
        if node is None:
            self.skipTest('node is required to exercise the mock gateway')
        probe = socket.socket()
        probe.bind(('127.0.0.1', 0))
        port = probe.getsockname()[1]
        probe.close()
        logs = self.root / 'mock-ping'
        logs.mkdir()
        with (logs / 'stdout.log').open('wb') as out, (logs / 'stderr.log').open('wb') as err:
            proc = subprocess.Popen([node, str(SCRIPT / 'mock_gateway.mjs'), str(port)],
                                    env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C',
                                         'PILOTDECK_MOCK_TOKEN': TOKEN},
                                    stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                    start_new_session=True)
            self.addCleanup(stop_group, proc)
            deadline = time.monotonic() + 10
            while True:
                try:
                    connection = socket.create_connection(('127.0.0.1', port), timeout=10)
                    break
                except OSError:
                    if time.monotonic() >= deadline or proc.poll() is not None:
                        self.fail('#933-7: the mock gateway did not start listening')
                    time.sleep(0.05)
            self.addCleanup(connection.close)
            connection.settimeout(10)

            def read_exact(count):
                data = b''
                while len(data) < count:
                    chunk = connection.recv(count - len(data))
                    if not chunk:
                        raise AssertionError(
                            '#933-7: the mock closed the connection during the ping exchange')
                    data += chunk
                return data

            def send_frame(opcode, payload):
                mask = os.urandom(4)
                masked = bytes(byte ^ mask[index & 3] for index, byte in enumerate(payload))
                connection.sendall(bytes([0x80 | opcode, 0x80 | len(masked)]) + mask + masked)

            key = base64.b64encode(os.urandom(16)).decode()
            connection.sendall(('GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n'
                                'Connection: Upgrade\r\nSec-WebSocket-Key: ' + key +
                                '\r\nSec-WebSocket-Version: 13\r\n\r\n').encode())
            response = b''
            while b'\r\n\r\n' not in response:
                chunk = connection.recv(4096)
                if not chunk:
                    self.fail('#933-7: the mock closed the connection before the handshake')
                response += chunk
            self.assertTrue(response.startswith(b'HTTP/1.1 101 '),
                            '#933-7: the mock must complete the websocket upgrade')
            # Two pings: the second proves the connection survived the first
            # exchange instead of being closed right after one pong.
            for payload in (b'ping', b'second-ping'):
                with self.subTest(payload=payload):
                    send_frame(0x9, payload)
                    header = read_exact(2)
                    body = read_exact(header[1] & 0x7f)
                    self.assertEqual((header[0], header[1], body), (0x8a, len(payload), payload),
                                     '#933-7: a ping must be answered with a pong carrying the same '
                                     'payload, not with a close or a dropped connection')
            send_frame(0x8, b'')
            header = read_exact(2)
            self.assertEqual(header[0] & 0x0f, 0x8,
                             '#933-7: the mock must echo the client close')
        # The readiness line the preflight waits on is part of the mock's
        # contract: if the two ever drift apart, every round Blocks.
        self.assertIn(mock_listening_line(port), (logs / 'stdout.log').read_text(),
                      '#933-5: the real mock must print the listening line the preflight waits for')

    def test_preflight_failure_after_spawn_stops_the_mock(self):
        # #933-8 (review P1-3): any failure after spawn() — here an unreadable
        # profile — must stop the registered mock before the error propagates.
        node = shutil.which('node')
        if node is None:
            self.skipTest('node is required to exercise the mock gateway')
        output = self.root / 'cleanup-window'
        output.mkdir()
        procs = []
        real_spawn = sys.modules['run_round'].spawn

        def recording_spawn(*args, **kwargs):
            proc = real_spawn(*args, **kwargs)
            procs.append(proc)
            return proc

        def stop_any_leak():
            # The assertion below may fire on a leaked mock (negative-control
            # mutant); the test itself must never leave it behind.
            for proc in procs:
                if proc.poll() is None:
                    stop_group(proc)
        self.addCleanup(stop_any_leak)
        with patch('run_round.spawn', side_effect=recording_spawn):
            with self.assertRaises(OSError,
                                   msg='#933-8: the unreadable profile must still surface its failure'):
                mock_gateway_preflight(output, self.root, node, self.root / 'missing.sb')
        self.assertIsNotNone(procs[0].poll(),
                             '#933-8: a preflight failure after spawn must stop the mock before propagating')
        self.assertNotIn(procs[0].pid, LIVE_GROUPS,
                         '#933-8: the stopped mock must leave the live-group registry')

    def test_spawn_stops_its_group_when_a_queued_signal_fires(self):
        # #933-8 (fix-r3 P1-1, layer A): a SIGINT arriving while Popen runs is
        # queued, and is delivered the instant deferred_signals lifts the
        # block — before spawn() returns. The group is already registered but
        # the caller holds no handle, so spawn must stop it itself.
        def deliver_then_return(*_args, **_kwargs):
            os.kill(os.getpid(), signal.SIGINT)  # queued: DEFERRED is blocked
            return SimpleNamespace(pid=999_000_002)

        stops = []

        def recorded_stop(proc):
            LIVE_GROUPS.discard(proc.pid)
            stops.append(proc.pid)
            return {'allStopped': True, 'remaining': [], 'errors': []}

        self.addCleanup(LIVE_GROUPS.discard, 999_000_002)
        with patch('run_round.subprocess.Popen', side_effect=deliver_then_return), \
             patch('run_round.stop_group', side_effect=recorded_stop):
            with self.assertRaises(KeyboardInterrupt,
                                   msg='#933-8: the deferred signal must still surface, not vanish'):
                sys.modules['run_round'].spawn(['/synthetic/cmd'])
        self.assertEqual(stops, [999_000_002],
                         '#933-8: a queued signal delivered before spawn() returns '
                         'must stop the group it registered')
        self.assertNotIn(999_000_002, LIVE_GROUPS,
                         '#933-8: the stopped group must leave the live-group registry')

    def test_preflight_signal_before_spawn_return_still_stops_the_mock(self):
        # #933-8 (fix-r3 P1-1, layer B — the review's P1 repro): the queued
        # signal fires when the block lifts, which can sit between spawn()
        # returning and the handle being stored. The review showed the
        # preflight then skipping stop_group with proc still None, leaving a
        # registered-but-unowned mock behind.
        def deliver_then_return(*_args, **_kwargs):
            os.kill(os.getpid(), signal.SIGINT)
            return SimpleNamespace(pid=999_000_001)

        stops = []

        def recorded_stop(proc):
            LIVE_GROUPS.discard(proc.pid)
            stops.append(proc.pid)
            return {'allStopped': True, 'remaining': [], 'errors': []}

        self.addCleanup(LIVE_GROUPS.discard, 999_000_001)
        with patch('run_round.subprocess.Popen', side_effect=deliver_then_return), \
             patch('run_round.stop_group', side_effect=recorded_stop):
            with self.assertRaises(KeyboardInterrupt,
                                   msg='#933-8: the deferred signal must still surface, not vanish'):
                mock_gateway_preflight(MagicMock(), MagicMock(), '/synthetic/node', MagicMock())
        self.assertEqual(stops, [999_000_001],
                         '#933-8: a queued signal delivered before the spawn handle is '
                         'stored must still stop the registered mock')
        self.assertNotIn(999_000_001, LIVE_GROUPS,
                         '#933-8: the stopped mock must leave the live-group registry')

    def test_preflight_passes_the_mock_port_to_the_isolation_probe(self):
        # #933-2 (fix-r3 P2): the probe's connect-denial check must test the
        # port the apparatus derives from MOCK_PORT — passed in on argv, never
        # a literal the probe could let drift away from the allow rule.
        output = self.root / 'probe-argv'
        output.mkdir()
        profile = self.root / 'probe.sb'
        profile.write_text('synthetic policy\n')
        argv_seen = []

        class FakeProc:
            pid = 999_000_003
            returncode = 0
            stdin = stdout = stderr = MagicMock()

            def communicate(self, *_args, timeout=None):
                return (b'{"status": "PASS"}', b'')

        def recording_spawn(argv, **_kwargs):
            argv_seen.append(list(argv))
            return FakeProc()

        with patch('run_round.spawn', side_effect=recording_spawn), \
             patch('run_round.stop_group') as stopped:
            stopped.return_value = {'allStopped': True, 'remaining': [], 'errors': []}
            evidence = preflight(output, self.root, profile, '/synthetic/node')
        self.assertEqual(evidence['status'], 'PASS',
                         '#933-2: the synthetic probe run must pass for the argv check to mean anything')
        self.assertEqual(argv_seen, [['/usr/bin/sandbox-exec', '-f', str(profile), '/synthetic/node',
                                      str(SCRIPT / 'isolation_probe.mjs'), str(output / 'outside-sentinel.txt'),
                                      str(MOCK_PORT)]],
                         '#933-2: the isolation probe must be passed the port derived from MOCK_PORT')
        self.assertNotIn('28999', (SCRIPT / 'isolation_probe.mjs').read_text(),
                         '#933-2: the probe must not hardcode the port; it reads argv[3]')

    def test_run_round_selects_the_mock_profile_per_scenario(self):
        # #933-6: run_round must run marked scenarios under the mock profile.
        cases = [{'id': 'plain', 'argv': [], 'stdinHex': '', 'token': False},
                 {'id': 'marked', 'argv': ['sessions', 'list'], 'stdinHex': '', 'token': True, 'mockGateway': True}]
        output = self.root / 'selection'
        output.mkdir()
        base, mock = output / 'ts.sb', output / 'ts-mock.sb'
        base.write_text('base profile\n')
        mock.write_text(f'base profile\n(allow network-outbound (remote ip "localhost:{MOCK_PORT}"))\n')
        provenance = {'baseline': {'path': '/synthetic/baseline'}, 'side': 'ts',
                      'executable': {'path': '/synthetic/node'}}
        seen = []
        def fake_capture(argv, env, cwd, stdin, timeout):
            seen.append(list(argv))
            return ({'status': 'DONE', 'timedOut': False, 'exitCode': 0,
                     'audit': {'allStopped': True, 'remaining': [], 'errors': []}}, {}, [])
        with patch('run_round.SCENARIOS', cases), \
             patch.object(sys.modules[__name__], 'SCENARIOS', cases), \
             patch('run_round.baseline_identity', return_value=provenance['baseline']), \
             patch('run_round.capture', side_effect=fake_capture):
            isolation = {'profileSha256': hashlib.sha256(base.read_bytes()).hexdigest()}
            mock_isolation = {'profileSha256': hashlib.sha256(mock.read_bytes()).hexdigest()}
            try:
                run_round(output, output / 'runtime', base, isolation, 'ts-1', provenance, 15,
                          mock_profile=mock, mock_isolation=mock_isolation)
            except Blocked as error:
                # The #933-6 negative control binds this test: a run_round that
                # mis-binds the marked scenario refuses the round, and that
                # refusal must surface here as the bound assertion failing.
                self.fail(f'#933-6: the marked scenario must run under the mock profile; '
                          f'run_round refused the round instead: {error}')
        executed = ['/synthetic/node', '/synthetic/baseline/dist/src/cli/pilotdeck.js']
        self.assertEqual(seen[0], ['/usr/bin/sandbox-exec', '-f', str(base), *executed],
                         '#933-6: the unmarked scenario must run under the base profile')
        self.assertEqual(seen[1], ['/usr/bin/sandbox-exec', '-f', str(mock), *executed, 'sessions', 'list'],
                         '#933-6: the marked scenario must run under the mock profile')
        for case_id, profile in (('plain', base), ('marked', mock)):
            row = json.loads((output / 'ts-1/cases' / case_id / 'record.json').read_text())
            self.assertEqual(row['profileSha256'], hashlib.sha256(profile.read_bytes()).hexdigest(),
                             f'#933-6: {case_id} recorded the wrong profile digest')
        # A marked registry without mock evidence is refused before any case runs.
        with patch('run_round.SCENARIOS', cases), \
             patch('run_round.baseline_identity', return_value=provenance['baseline']):
            with self.assertRaisesRegex(Blocked, 'mock profile',
                                        msg='#933-6: a marked registry without mock evidence must be refused'):
                run_round(output / 'refused', output / 'runtime-2', base,
                          {'profileSha256': hashlib.sha256(base.read_bytes()).hexdigest()},
                          'ts-1', provenance, 15)
        self.assertFalse((output / 'refused').exists(),
                         '#933-6: the refusal must happen before any case runs')

    def test_current_builtin_copy_pins_full_docx_tree_and_calibrated_migration(self):
        scenario = next(s for s in REGISTERED_SCENARIOS if s['id'] == 'legacy-skills-current')
        self.assertEqual(set(scenario), {'id', 'argv', 'stdinHex', 'token', 'fixtureBlobs',
                                        'exit', 'stdout', 'stderr', 'stdoutRuntime',
                                        'diskRemovals', 'diskAdditions',
                                        'streamOrder', 'streamOrderReason'})
        self.assertEqual(scenario['exit'], 1)
        self.assertEqual(scenario['stdoutRuntime'], 'text')
        self.assertEqual(scenario['stderr'], 'Config must contain an agent section.\n')
        self.assertEqual(scenario['argv'], [])
        self.assertEqual(scenario['stdinHex'], '')
        self.assertFalse(scenario['token'])
        paths = {
            'LICENSE', 'SKILL.md', 'requirements.txt',
            'references/optional-tools.md', 'references/word-specifics.md',
            'scripts/docx.sh', 'scripts/docx_cli.py',
            'scripts/docxlib/__init__.py', 'scripts/docxlib/annotations.py',
            'scripts/docxlib/common.py', 'scripts/docxlib/core.py',
            'scripts/docxlib/delivery.py', 'scripts/docxlib/render.py', 'scripts/docxlib/smoke.py',
        }
        blobs = scenario['fixtureBlobs']
        self.assertEqual(set(blobs), {'pilot/skills/docx/' + name for name in paths})
        for dest, blob in blobs.items():
            self.assertEqual(blob['revision'], HEAD)
            self.assertEqual('pilot/' + blob['path'], dest)
            self.assertGreater(blob['size'], 0)
            self.assertRegex(blob['sha256'], r'^[0-9a-f]{64}$')
        tree = initial_tree(scenario)
        for name in blobs:
            self.assertEqual(tree[name]['sha256'], blobs[name]['sha256'])
        moved = {name for name in tree if name == 'pilot/skills/docx' or name.startswith('pilot/skills/docx/')}
        self.assertEqual(len(moved), 18)
        self.assertEqual(set(scenario['diskRemovals']), moved)
        additions = scenario['diskAdditions']
        self.assertEqual(len(additions), 21)
        for name in moved:
            target = name.replace('pilot/skills/docx', 'pilot/skill-backups/legacy-bundled-v1/docx', 1)
            self.assertEqual(additions[target], tree[name])
        self.assertEqual(additions['pilot/.legacy-bundled-skills-migrated-v1'], {
            'type': 'file', 'mode': 0o600, 'size': 10,
            'sha256': hashlib.sha256(b'completed\n').hexdigest()})

    def test_scenario_files_have_independent_initial_inventory(self):
        scenario = next(s for s in REGISTERED_SCENARIOS if s['id'] == 'skills-custom-execute')
        runtime = self.root / 'skill-fixture'
        fixture(runtime, scenario)
        self.assertEqual(snapshot(runtime), initial_tree(scenario))
        (runtime / 'imports/sample/assets/info.txt').write_text('mutated')
        self.assertNotEqual(snapshot(runtime), initial_tree(scenario))
        for invalid in ('../escape', '/absolute', 'pilot/pilotdeck.yaml', 'home'):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                fixture(self.root / 'invalid', {'fixtureFiles': {invalid: 'bad'}})
        self.assertFalse((self.root / 'invalid').exists())

    def test_historical_blob_requires_registered_bytes(self):
        scenario = {'fixtureBlobs': {'pilot/skills/a/SKILL.md': {
            'revision': 'a' * 40, 'path': 'skills/a/SKILL.md', 'size': 7,
            'sha256': hashlib.sha256(b'fixture').hexdigest()}}}
        with patch('common.subprocess.check_output', return_value=b'fixture'):
            fixture(self.root / 'blob', scenario, Path('/synthetic/baseline'))
        self.assertEqual(snapshot(self.root / 'blob'), initial_tree(scenario))
        with patch('common.subprocess.check_output', return_value=b'changed'), self.assertRaises(ValueError):
            fixture(self.root / 'bad-blob', scenario, Path('/synthetic/baseline'))

    def test_registered_pending_scenarios_cannot_self_compare_to_pass(self):
        compare_module = _compare
        # Reintroduce missing calibration on the actual batch IDs/argv. This
        # guard must survive filling the registry, rather than become vacuous.
        cases = [{**{k: s[k] for k in ('id', 'argv', 'stdinHex', 'token')},
                  'oracle': compare_module.PENDING} for s in SKILLS_SCENARIOS]
        self.assertEqual(len(cases), 16)
        with patch.object(compare_module, 'SCENARIOS', cases), patch.object(sys.modules[__name__], 'SCENARIOS', cases):
            source = self.root / 'pending-batch'
            synthetic_round(source, self.root / 'pending-runtime')
            result = compare_module.compare(source, source)
        self.assertEqual(result['status'], 'FAIL')
        rules = {row['rule'] for row in result['failures']}
        for scenario in cases:
            self.assertIn(scenario['id'] + '.oracle.pending', rules)

    def test_runtime_bound_oracle_preserves_exact_output_and_paths(self):
        compare_module = _compare
        runtime = self.root / 'runtime "quoted" \\ 中文'
        cases = [
            {'id': 'text', 'argv': [], 'stdinHex': '', 'token': False, 'exit': 0,
             'stdout': 'Target: {runtime}/pilot/skills\n', 'stdoutRuntime': 'text', 'stderr': ''},
            {'id': 'json', 'argv': [], 'stdinHex': '', 'token': False, 'exit': 0,
             'stdout': '{\n  "path": "{runtime}/pilot/skills"\n}\n', 'stdoutRuntime': 'json', 'stderr': ''}]
        actual = {'text': f'Target: {runtime}/pilot/skills\n'.encode(),
                  'json': (json.dumps({'path': str(runtime) + '/pilot/skills'}, ensure_ascii=False, indent=2) + '\n').encode()}
        with patch.object(compare_module, 'SCENARIOS', cases), patch.object(sys.modules[__name__], 'SCENARIOS', cases):
            source = self.root / 'runtime-binding'
            synthetic_round(source, runtime)
            for scenario in cases:
                name = scenario['id']
                self.assertEqual(expected_stdout(scenario, runtime), actual[name])
                write_stream(source / 'cases' / name, 'stdout', actual[name])
            self.assertEqual(compare(source, source)['status'], 'PASS')
            for name in actual:
                case = source / 'cases' / name
                for mutation in (actual[name].replace(b'pilot/skills', b'pilot/other'),
                                 actual[name].replace(b'runtime', b'wrong-root'),
                                 cases[0 if name == 'text' else 1]['stdout'].encode(),
                                 actual[name] + b' ', actual[name].replace(b'\n', b'\r\n')):
                    with self.subTest(name=name, mutation=mutation):
                        write_stream(case, 'stdout', mutation)
                        self.assertEqual({r['rule'] for r in compare(source, source)['failures']},
                                         {f'{name}.oracle.stdout'})
                write_stream(case, 'stdout', actual[name])
            # Even two independently valid oracles must not hide cross-run paths.
            other = self.root / 'other-runtime-binding'
            synthetic_round(other, self.root / 'other-runtime')
            rules = {r['rule'] for r in compare(source, other)['failures']}
            self.assertTrue({'round.runtime', 'text.stdout', 'json.stdout'} <= rules)

    def test_invalid_runtime_binding_is_rejected(self):
        for scenario in ({'stdout': '{runtime}', 'stdoutRuntime': 'anything'},
                         {'stdout': 'no placeholder', 'stdoutRuntime': 'text'},
                         {'stdout': '{runtime}', 'stdoutRuntime': 'json', 'stdoutSha256': '0' * 64},
                         {'stdout': '{runtime}', 'stdoutRuntime': 'text', 'stdoutPrefix': ''}):
            with self.subTest(scenario=scenario), self.assertRaises(ValueError):
                expected_stdout(scenario, self.root)

    def test_bundled_skills_missing_scenario_expects_no_marker(self):
        scenario = next(s for s in SCENARIOS if s['id'] == 'bundled-skills-missing')
        self.assertNotIn('diskAdditions', scenario)
        env = environment(self.root, scenario)
        self.assertEqual(env['PILOTDECK_BUNDLED_SKILLS_DIR'], str(self.root / 'absent-skills'))
        self.assertNotIn('absent-skills', initial_tree())
        self.assertIn('Bundled skills root is missing', scenario.get('stderr', ''))

    def test_env_paths_cannot_override_isolation_or_escape_runtime(self):
        for paths in ({'HOME': 'elsewhere'}, {'PILOTDECK_BUNDLED_SKILLS_DIR': '../outside'},
                      {'PILOTDECK_BUNDLED_SKILLS_DIR': '/abs'}, {'PILOTDECK_BUNDLED_SKILLS_DIR': ''},
                      # token:false leaves these unset; they still must not be injectable.
                      {'PILOTDECK_TOKEN': 'x'}, {'PILOTDECK_SDK_AUTH_TOKEN': 'x'}):
            with self.subTest(paths=paths):
                with self.assertRaises(ValueError):
                    environment(self.root, {'token': False, 'envPaths': paths})

    def test_probe_private_read_and_metadata_denials_required(self):
        source = self.source()
        path = source / 'round.json'
        run = json.loads(path.read_text())
        for key in ('privateReadDenied', 'outsideMetadataDenied'):
            run['isolation']['probe'][key] = False
            save(path, run)
            self.assertIn('round.probe', [r['rule'] for r in compare(source, source)['failures']])
            run['isolation']['probe'][key] = True

    def test_rust_node_denial_probe_rejects_success_incomplete_and_timeout(self):
        profile = self.root / 'rust.sb'
        profile.write_text('synthetic policy')
        good = {'status': 'DONE', 'timedOut': False, 'exitCode': 71,
                'audit': {'allStopped': True, 'remaining': [], 'errors': []}}
        denied = {'stdout': b'', 'stderr': b'sandbox-exec: execvp(): Operation not permitted\n'}
        with patch('run_round.capture', return_value=(good, denied, [])):
            result = rust_isolation(self.root, self.root, profile, '/node', {})
            self.assertTrue(result['nodeExec']['denied'])
        for changes, streams in (({'status': 'ERROR'}, denied), ({'timedOut': True}, denied),
                                 ({'exitCode': 0}, {'stdout': b'v22\n', 'stderr': b''}),
                                 ({}, {'stdout': b'', 'stderr': b'sandbox_apply: Operation not permitted'})):
            with self.subTest(changes=changes, streams=streams):
                with patch('run_round.capture', return_value=({**good, **changes}, streams, [])):
                    with self.assertRaises(Blocked):
                        rust_isolation(self.root, self.root, profile, '/node', {})

    def test_rust_incomplete_node_denial_evidence_is_rejected(self):
        source = self.source()
        target = self.root / 'rust'
        shutil.copytree(source, target)
        expected = as_rust(target)
        path = target / 'round.json'
        run = json.loads(path.read_text())
        run['isolation']['nodeExec']['status'] = 'ERROR'
        save(path, run)
        self.assertIn('round.rust-node-exec', [r['rule'] for r in compare(source, target, expected)['failures']])

    def test_sandbox_wrapper_and_policy_mutations_rejected(self):
        source = self.source()
        path = source / 'cases/help-long/record.json'
        row = json.loads(path.read_text())
        row['sandboxArgv'] = row['executedArgv']
        save(path, row)
        self.assertIn('help-long.sandbox-binding', [r['rule'] for r in compare(source, source)['failures']])
        path = source / 'round.json'
        run = json.loads(path.read_text())
        run['isolation']['profileText'] += '(allow default)\n'
        run['isolation']['profileSha256'] = hashlib.sha256(run['isolation']['profileText'].encode()).hexdigest()
        save(path, run)
        self.assertIn('round.profile-binding', [r['rule'] for r in compare(source, source)['failures']])

    def test_batch_budget_includes_three_rounds_and_rejects_overflow(self):
        check_budget(15, True)  # Default per-scenario timeout with the registered cases.
        with self.assertRaises(Blocked):
            check_budget(60, True)  # 13 cases x 60s x 3 rounds + reserve exceeds the lease.
        with patch('run_round.SCENARIOS', [None] * 30):
            with self.assertRaises(Blocked):
                check_budget(60, True)
        with patch('run_round.SCENARIOS', [None] * 31):
            with self.assertRaises(Blocked):
                check_budget(1, False)

    def test_ts_executed_argv_rejects_warning_suppression(self):
        source = self.source()
        self.assertEqual(compare(source, source)['status'], 'PASS')
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario['id']):
                path = source / 'cases' / scenario['id'] / 'record.json'
                record = json.loads(path.read_text())
                original = record['executedArgv'][:]
                record['executedArgv'].insert(1, '--disable-warning=ExperimentalWarning')
                save(path, record)
                result = compare(source, source)
                self.assertEqual(result['status'], 'FAIL')
                self.assertEqual({row['rule'] for row in result['failures']},
                                 {f'{scenario["id"]}.executed-argv'})
                record['executedArgv'] = original
                save(path, record)

    def test_expected_disk_additions_require_exact_complete_tree(self):
        source = self.source()
        self.assertEqual(compare(source, source)['status'], 'PASS')
        marked = [s['id'] for s in SCENARIOS
                  if 'pilot/.legacy-bundled-skills-migrated-v1' in s.get('diskAdditions', {})]
        self.assertTrue(marked)
        for name in marked:
            for mutation in ('extra', 'missing', 'content', 'size', 'mode', 'type',
                             'missing-fixture', 'changed-fixture'):
                with self.subTest(scenario=name, mutation=mutation):
                    target = self.root / f'{name}-{mutation}'
                    shutil.copytree(source, target)
                    case = target / 'cases' / name
                    marker = case / 'tree/pilot/.legacy-bundled-skills-migrated-v1'
                    if mutation == 'extra':
                        (case / 'tree/pilot/.unexpected').write_bytes(b'extra')
                    elif mutation == 'missing':
                        marker.unlink()
                    elif mutation == 'content':
                        marker.write_bytes(b'Completed\n')  # Same length; SHA-256 must differ.
                    elif mutation == 'size':
                        marker.write_bytes(b'completed\n\n')
                    elif mutation == 'mode':
                        marker.chmod(0o644)
                    elif mutation == 'type':
                        marker.unlink()
                        marker.mkdir(mode=0o600)
                    elif mutation == 'missing-fixture':
                        (case / 'tree/project/keep.txt').unlink()
                    else:
                        (case / 'tree/project/keep.txt').write_bytes(b'changed')
                    # Keep manifests valid: the independent oracle must reject even
                    # two equally wrong recordings, not just stale cached metadata.
                    save(case / 'final.json', snapshot(case / 'tree'))
                    result = compare(target, target)
                    self.assertEqual(result['status'], 'FAIL')
                    self.assertEqual({row['rule'] for row in result['failures']},
                                     {f'{name}.oracle.disk'})

    def test_expected_disk_removals_require_exact_complete_tree(self):
        compare_module = _compare
        # A synthetic oracle only, never written to the production registry.
        scenario = {'id': 'remove', 'argv': [], 'stdinHex': '', 'token': False,
                    'exit': 0, 'stdout': '', 'stderr': '', 'diskRemovals': ['project/keep.txt']}
        with patch.object(compare_module, 'SCENARIOS', [scenario]), patch.object(sys.modules[__name__], 'SCENARIOS', [scenario]):
            source = self.root / 'removal'
            synthetic_round(source, self.root / 'removal-runtime')
            self.assertEqual(compare(source, source)['status'], 'FAIL')
            case = source / 'cases/remove'
            (case / 'tree/project/keep.txt').unlink()
            save(case / 'final.json', snapshot(case / 'tree'))
            self.assertEqual(compare(source, source)['status'], 'PASS')
            for invalid in (['missing'], ['.'], ['project/keep.txt', 'project/keep.txt']):
                with self.subTest(invalid=invalid), patch.dict(scenario, diskRemovals=invalid):
                    rules = {r['rule'] for r in compare(source, source)['failures']}
                    self.assertIn('remove.oracle.disk', rules)
            (case / 'tree/openssl.cnf').unlink()
            save(case / 'final.json', snapshot(case / 'tree'))
            self.assertIn('remove.oracle.disk', {r['rule'] for r in compare(source, source)['failures']})

    def test_missing_both_rounds_cannot_pass(self):
        self.assertEqual(compare(self.root, self.root)['status'], 'FAIL')

    def test_production_rejects_synthetic_calibration(self):
        source = self.source()
        with patch('compare.RECORDING_ORIGIN', 'live-os-sandbox'):
            self.assertEqual(compare(source, source)['status'], 'FAIL')
            self.assertEqual(run_controls(source, self.root / 'controls')['status'], 'BLOCKED')

    def test_unknown_side_cannot_bypass_oracles(self):
        source = self.source()
        path = source / 'round.json'
        run = json.loads(path.read_text())
        run['side'] = 'anything'
        save(path, run)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_stub_isolation_is_rejected(self):
        source = self.source()
        path = source / 'round.json'
        run = json.loads(path.read_text())
        run['isolation'] = {'status': 'PASS'}
        save(path, run)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_symlink_cannot_mutate_original_evidence(self):
        source = self.source()
        original = source / 'cases/help-long/stdout.bin'
        retained = self.root / 'original.bin'
        original.rename(retained)
        before = retained.read_bytes()
        original.symlink_to(retained)
        self.assertEqual(run_controls(source, self.root / 'controls')['status'], 'BLOCKED')
        self.assertEqual(retained.read_bytes(), before)
        self.assertFalse((self.root / 'controls').exists())

    def test_case_directory_symlink_rejected(self):
        source = self.source()
        folder = source / 'cases/help-long'
        retained = self.root / 'original-case'
        folder.rename(retained)
        folder.symlink_to(retained, target_is_directory=True)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_extra_root_symlink_cannot_be_used_as_control_report(self):
        source = self.source()
        original = self.root / 'original.json'
        original.write_text('retain this evidence\n')
        before = original.read_bytes()
        (source / 'comparison.json').symlink_to(original)
        self.assertEqual(run_controls(source, self.root / 'controls')['status'], 'BLOCKED')
        self.assertEqual(original.read_bytes(), before)
        self.assertFalse((self.root / 'controls').exists())

    def test_cyclic_symlink_is_a_failed_recording(self):
        source = self.source()
        path = source / 'cases/help-long/tree/project/cycle'
        path.symlink_to('cycle')
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_controls_cannot_copy_inside_source(self):
        source = self.source()
        with self.assertRaises(ValueError):
            run_controls(source, source / 'controls')
        self.assertFalse((source / 'controls').exists())

    def test_rust_pair_comparison_catches_all_five_mutations(self):
        source = self.source()
        for name in ('stdout', 'stderr', 'exitCode', 'drop-disk', 'extra-file'):
            with self.subTest(name=name):
                target = self.root / name
                shutil.copytree(source, target)
                expected = as_rust(target)
                self.assertEqual(compare(source, target, expected)['status'], 'PASS')
                case = target / 'cases/help-long'
                if name in ('stdout', 'stderr'):
                    (case / f'{name}.bin').write_bytes(b'changed\x00\xff')
                    events = [r for r in json.loads((case / 'events.json').read_text()) if r['stream'] != name]
                    events.append({'stream': name, 'hex': b'changed\x00\xff'.hex()})
                    save(case / 'events.json', events)
                elif name == 'exitCode':
                    row = json.loads((case / 'record.json').read_text())
                    row[name] = 73
                    save(case / 'record.json', row)
                else:
                    if name == 'drop-disk':
                        (case / 'tree/project/keep.txt').unlink()
                    else:
                        (case / 'tree/project/extra').write_bytes(b'extra')
                    save(case / 'final.json', snapshot(case / 'tree'))
                result = compare(source, target, expected)
                self.assertEqual(result['status'], 'FAIL')
                field = 'disk' if name in ('drop-disk', 'extra-file') else name
                self.assertIn(f'help-long.{field}', [r['rule'] for r in result['failures']])

    def test_missing_scenario_on_both_sides_cannot_pass(self):
        source = self.source()
        (source / 'cases/empty-nontty').rename(source / 'cases/renamed')
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_stale_disk_manifest_and_hidden_extra_file(self):
        source = self.source()
        (source / 'cases/help-long/tree/.hidden').write_bytes(b'\x00\xff')
        result = compare(source, source)
        self.assertTrue(any('disk.manifest' in row['rule'] for row in result['failures']))

    def test_changed_fixture_rejected_even_if_both_sides_agree(self):
        source = self.source()
        path = source / 'cases/help-long/initial.json'
        save(path, {})
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_exit_and_cleanup_are_not_truthy_checks(self):
        source = self.source()
        path = source / 'cases/help-long/record.json'
        record = json.loads(path.read_text())
        record['exitCode'] = False
        record['audit']['allStopped'] = 'true'
        save(path, record)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_unknown_env_and_injected_parent_secrets_rejected(self):
        with patch.dict(os.environ, {'NODE_OPTIONS': '--require /evil', 'API_KEY': 'secret',
                                      'HTTP_PROXY': 'http://secret', 'SSH_AUTH_SOCK': '/socket'}):
            env = environment(self.root, SCENARIOS[0])
        self.assertTrue({'NODE_OPTIONS', 'API_KEY', 'HTTP_PROXY', 'SSH_AUTH_SOCK'}.isdisjoint(env))
        source = self.source()
        path = source / 'cases/help-long/record.json'
        record = json.loads(path.read_text())
        record['env']['API_KEY'] = 'secret'
        save(path, record)
        self.assertEqual(compare(source, source)['status'], 'FAIL')

    def test_token_summary_does_not_disclose_value(self):
        env = environment(self.root, next(s for s in SCENARIOS if s['token']))
        summary = env_summary(env)
        self.assertNotIn(env['PILOTDECK_SDK_AUTH_TOKEN'], json.dumps(summary))
        self.assertEqual(summary['PILOTDECK_SDK_AUTH_TOKEN']['sha256'], hashlib.sha256(env['PILOTDECK_SDK_AUTH_TOKEN'].encode()).hexdigest())

    def test_baseline_precedence_and_no_fallback_on_missing_explicit(self):
        for name in ('explicit', 'env'):
            entry = self.root / name / 'dist/src/cli/pilotdeck.js'
            entry.parent.mkdir(parents=True)
            entry.write_text('fixture')
        env = {'PDRS_BASELINE': str(self.root / 'env')}
        self.assertEqual(baseline_path(self.root / 'explicit', env), self.root / 'explicit')
        self.assertEqual(baseline_path(None, env), self.root / 'env')
        with self.assertRaises(Blocked):
            baseline_path(self.root / 'missing', env)
        with patch('common.ROOT', self.root):
            with self.assertRaises(Blocked):
                baseline_path(None, {})
            default = self.root / 'resources/PilotDeck'
            entry = default / 'dist/src/cli/pilotdeck.js'
            entry.parent.mkdir(parents=True)
            entry.write_text('fixture')
            self.assertEqual(baseline_path(None, {'PDRS_BASELINE': ''}), default)

    def test_lock_is_exclusive_and_existing_owner_is_retained(self):
        lock = self.root / 'lock'
        with parity_lock(lock):
            original = (lock / 'owner.json').read_bytes()
            with self.assertRaises(Blocked):
                with parity_lock(lock):
                    self.fail('second owner acquired lock')
            self.assertEqual((lock / 'owner.json').read_bytes(), original)
        self.assertFalse(lock.exists())

    def test_cleanup_failure_retains_owned_lock(self):
        lock = self.root / 'lock'
        with self.assertRaises(CleanupBlocked):
            with parity_lock(lock):
                LIVE_GROUPS.add(999_999_001)  # 一个未被证实停止的进程组
                raise CleanupBlocked('synthetic failure')
        LIVE_GROUPS.discard(999_999_001)
        self.assertTrue((lock / 'owner.json').is_file())

    def test_lock_retained_whenever_a_group_is_unverified_regardless_of_exception(self):
        # 释放与否只看 LIVE_GROUPS，不看异常类型：OSError 发生在清理之前也必须保留
        for error in (KeyboardInterrupt(), SystemExit(3), LookupError('unknown'), PermissionError('before cleanup'), Blocked('x')):
            lock = self.root / f'lock-live-{type(error).__name__}'
            with self.assertRaises(type(error)):
                with parity_lock(lock):
                    LIVE_GROUPS.add(999_999_002)
                    raise error
            LIVE_GROUPS.discard(999_999_002)
            self.assertTrue((lock / 'owner.json').is_file(), type(error).__name__)

    def test_lock_released_when_all_groups_verified_even_on_interrupt(self):
        for error in (KeyboardInterrupt(), Blocked('controlled'), OSError('io')):
            lock = self.root / f'lock-clean-{type(error).__name__}'
            with self.assertRaises(type(error)):
                with parity_lock(lock):
                    raise error
            self.assertFalse(lock.exists(), type(error).__name__)

    def test_stop_group_unregisters_only_when_all_stopped(self):
        run_round = _run_round
        proc = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True)
        LIVE_GROUPS.add(proc.pid)
        original = run_round._stop_group
        try:
            run_round._stop_group = lambda p: {'allStopped': False, 'remaining': [p.pid], 'errors': ['synthetic EPERM']}
            run_round.stop_group(proc)
            self.assertIn(proc.pid, LIVE_GROUPS)
            run_round._stop_group = original
            self.assertTrue(run_round.stop_group(proc)['allStopped'])
            self.assertNotIn(proc.pid, LIVE_GROUPS)
        finally:
            run_round._stop_group = original
            LIVE_GROUPS.discard(proc.pid)
            if proc.poll() is None:
                proc.kill()

    def test_stop_group_defers_interrupts_and_delivers_after_cleanup(self):
        run_round = _run_round
        proc = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True)
        seen = []
        def handler(signum, frame):
            seen.append((signum, proc.poll() is not None))  # 投递时清理是否已完成
        previous = signal.signal(signal.SIGINT, handler)
        original = run_round._stop_group
        try:
            def interrupted(p):
                os.kill(os.getpid(), signal.SIGINT)  # 清理进行中收到中断
                self.assertEqual(seen, [])  # 清理期间不投递
                return original(p)
            run_round._stop_group = interrupted
            audit = run_round.stop_group(proc)
            self.assertTrue(audit['allStopped'])
            self.assertEqual(seen, [(signal.SIGINT, True)])  # 不丢：清理完成后投递一次
        finally:
            run_round._stop_group = original
            signal.signal(signal.SIGINT, previous)
            if proc.poll() is None:
                proc.kill()

    def test_spawned_child_runs_with_normal_signal_mask(self):
        run_round = _run_round
        proc = run_round.spawn([sys.executable, '-c', 'import signal;print(sorted(int(s) for s in signal.pthread_sigmask(signal.SIG_BLOCK, set())))'],
                               stdout=subprocess.PIPE, text=True, start_new_session=True)
        out = proc.communicate(timeout=10)[0].strip()
        run_round.stop_group(proc)
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGALRM):
            self.assertNotIn(str(int(sig)), out.strip('[]').replace(' ', '').split(','), out)

    def test_lease_alarm_during_cleanup_is_not_lost(self):
        run_round = _run_round
        proc = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True)
        fired = []
        previous = signal.signal(signal.SIGALRM, lambda *_: fired.append(True))
        try:
            original = run_round._stop_group
            def slow(p):
                signal.setitimer(signal.ITIMER_REAL, 0.05)  # 租约闹钟在清理窗口内到期
                time.sleep(0.2)
                self.assertEqual(fired, [])  # 窗口内挂起，不投递
                return original(p)
            run_round._stop_group = slow
            try:
                run_round.stop_group(proc)
            finally:
                run_round._stop_group = original
            self.assertEqual(fired, [True])  # 解除后投递，不丢
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)
            if proc.poll() is None:
                proc.kill()

    def test_spawn_registers_group_atomically_with_respect_to_interrupts(self):
        run_round = _run_round
        seen = []
        previous = signal.signal(signal.SIGINT, lambda *_: seen.append(set(LIVE_GROUPS)))
        real_popen = subprocess.Popen
        def popen_then_interrupt(*a, **k):
            p = real_popen(*a, **k)
            os.kill(os.getpid(), signal.SIGINT)  # Popen 刚返回、登记之前收到中断
            return p
        try:
            with patch.object(run_round.subprocess, 'Popen', popen_then_interrupt):
                proc = run_round.spawn([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True)
            self.assertEqual(len(seen), 1)
            self.assertIn(proc.pid, seen[0])  # 中断投递时进程组已登记
        finally:
            signal.signal(signal.SIGINT, previous)
            run_round.stop_group(proc)
            self.assertNotIn(proc.pid, LIVE_GROUPS)

    def test_lock_guard_and_directory_symlinks_do_not_redirect_writes(self):
        lock = self.root / 'lock'
        original = self.root / 'original'
        original.write_text('preserve')
        lock.with_name('lock.guard').symlink_to(original)
        with self.assertRaises(OSError):
            with parity_lock(lock):
                self.fail('symlink guard accepted')
        self.assertEqual(original.read_text(), 'preserve')
        lock.with_name('lock.guard').unlink()
        original_dir = self.root / 'original-dir'
        original_dir.mkdir()
        lock.symlink_to(original_dir)
        with self.assertRaises(Blocked):
            with parity_lock(lock):
                self.fail('symlink lock accepted')
        self.assertEqual(list(original_dir.iterdir()), [])

    def test_new_lock_permissions_are_private(self):
        lock = self.root / 'lock'
        with parity_lock(lock):
            self.assertEqual(lock.stat().st_mode & 0o777, 0o700)
            self.assertEqual((lock / 'owner.json').stat().st_mode & 0o777, 0o600)

    def test_failed_preflight_cannot_start_any_product_round(self):
        output = self.root / 'blocked-run'
        with patch('sys.argv', ['run_round.py', '--node', sys.executable]), \
             patch('run_round.output_path', return_value=output), \
             patch('run_round.baseline_path', return_value=self.root / 'baseline'), \
             patch('run_round.baseline_identity', return_value={'sourceHead': HEAD}), \
             patch('run_round.sandbox_profile', return_value='synthetic policy'), \
             patch('run_round.parity_lock', return_value=nullcontext({'pid': 1})), \
             patch('run_round.preflight', side_effect=Blocked('synthetic EPERM')), \
             patch('run_round.run_round') as product, redirect_stdout(io.StringIO()):
            self.assertEqual(main(), 2)
            product.assert_not_called()
        self.assertEqual(json.loads((output / 'summary.json').read_text())['status'], 'BLOCKED')

    def test_snapshot_does_not_follow_links_or_open_fifo(self):
        root = self.root / 'tree'
        fixture(root)
        (root / 'project/link').symlink_to('keep.txt')
        self.assertEqual(snapshot(root)['project/link']['type'], 'symlink')
        (root / 'project/escape').symlink_to('../../../outside')
        with self.assertRaises(ValueError):
            snapshot(root)
        (root / 'project/escape').unlink()
        os.mkfifo(root / 'fifo')
        with self.assertRaises(ValueError):
            snapshot(root)

    def test_snapshot_detects_mode_and_empty_directory(self):
        root = self.root / 'tree'
        fixture(root)
        self.assertEqual(snapshot(root), initial_tree())
        (root / 'project/empty').mkdir()
        (root / 'project/keep.txt').chmod(0o400)
        tree = snapshot(root)
        self.assertIn('project/empty', tree)
        self.assertEqual(tree['project/keep.txt']['mode'], 0o400)
        root.chmod(0o755)
        self.assertEqual(snapshot(root)['.']['mode'], 0o755)

    def test_chunk_boundaries_ignored_but_interstream_order_preserved(self):
        row = lambda stream, data: {'stream': stream, 'hex': data.hex()}
        a = [row('stdout', b'\x00'), row('stdout', b'\xff\n'), row('stderr', b'e')]
        b = [row('stdout', b'\x00\xff\n'), row('stderr', b'e')]
        self.assertEqual(coalesce(a), coalesce(b))
        self.assertNotEqual(coalesce(a), coalesce(list(reversed(b))))

    def test_stream_truncation_rejected(self):
        source = self.source()
        (source / 'cases/help-long/stdout.bin').write_bytes(b'')
        result = compare(source, source)
        self.assertTrue(any('stdout.events' in row['rule'] for row in result['failures']))

    def test_output_path_rejects_external_path(self):
        with self.assertRaises(Blocked):
            output_path(self.root / 'outside')

    def test_profile_only_runtime_is_writable(self):
        with patch('run_round.sys.platform', 'darwin'):
            profile = sandbox_profile(self.root / 'runtime', self.root / 'baseline', '/bin/true', 'ts')
        self.assertIn('(deny default)', profile)
        self.assertIn('(deny network*)', profile)
        self.assertIn('(deny process-fork)', profile)
        self.assertIn(f'(allow file-write* (subpath "{self.root}/runtime"))', profile)

    def test_capture_timeout_kills_process_group(self):
        # A trusted Python fixture tests cleanup only; not an unconfined product run.
        with patch('run_round.process_table', return_value=[]):
            record, streams, _ = capture([sys.executable, '-I', '-c', 'import time; time.sleep(30)'],
                                         {'PATH': '/usr/bin:/bin'}, self.root, b'', .1)
        self.assertTrue(record['timedOut'])
        self.assertTrue(record['audit']['allStopped'])
        self.assertEqual(record['status'], 'ERROR')
        with self.assertRaises(ProcessLookupError):
            os.killpg(record['pid'], 0)

    def test_capture_interrupt_still_records_cleanup(self):
        with patch('run_round.process_table', side_effect=KeyboardInterrupt('synthetic cancellation')):
            record, _, _ = capture([sys.executable, '-I', '-c', 'import time; time.sleep(30)'],
                                   {'PATH': '/usr/bin:/bin'}, self.root, b'', 1)
        self.assertEqual(record['status'], 'ERROR')
        self.assertTrue(record['audit']['allStopped'])
        with self.assertRaises(ProcessLookupError):
            os.killpg(record['pid'], 0)


if __name__ == '__main__':
    unittest.main()
