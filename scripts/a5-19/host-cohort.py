"""Collect/resume the four #1314 cells through the registered run-host entry."""
import sys
if not sys.flags.isolated or sys.flags.optimize:
    raise SystemExit('use python3 -I with proof checks enabled')
sys.dont_write_bytecode = True

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import uuid

ENTRY = Path(__file__).resolve().with_name('run-host.py')
ROOT = ENTRY.parents[2]
OUTPUT_ROOT = ROOT / '_tmp'
BASELINE = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
IMAGE = 'sha256:eb58eb269804b8a9b2bb9ceeb9a175cac9b79612d4c8dafe0a987b277bd55683'
UV = Path('/home/clawbox-08/opt/libuv-1.52.1')
ROLES = tuple((host, sidecar) for host in ('ts', 'rust') for sidecar in ('ts', 'rust'))
K = 5
PROGRAMS = {'pilotdeck', 'pilotdeck-control', 'pilotdeck-gateway', 'pilotdeck-agent-loop-sidecar'}


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def pin(path):
    path = Path(path).resolve(strict=True)
    return {'path': str(path), 'sha256': sha(path)}


def checked(ref):
    path = Path(ref['path'])
    assert path.is_absolute() and pin(path) == ref, f'changed artifact: {path}'
    return path


def read(path):
    return json.loads(Path(path).read_text())


def save(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + '.partial')
    with temporary.open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    # Hardlink publication rejects an existing final filename even if a second
    # launcher got here while an interrupted collector still owns the attempt.
    os.link(temporary, path)
    temporary.unlink()


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], timeout=10).decode().strip()


def clean_head(root, expected):
    assert git(root, 'rev-parse', 'HEAD') == expected
    assert not git(root, 'status', '--porcelain', '--untracked-files=no')


def disk_level(root):
    result = subprocess.run(
        ['/bin/bash', '-c', 'source "$1/scripts/lib/disk-budget.sh" && pdrs_disk_level "$1"',
         '_', str(root)], capture_output=True, text=True, timeout=45, check=True)
    level = result.stdout.strip()
    assert level in ('ok', 'warn'), 'repository disk guard refused further evidence writes: ' + level
    return level


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    source = path.read_bytes()
    exec(compile(source, str(path), 'exec'), module.__dict__)
    module._apparatus_source = source
    return module


def load_common(root):
    return load_module('full_host_registered_common', root / 'scripts/gateway-parity/common.py')


def load_scoring_modules(root):
    session = load_module('full_host_registered_session', root / 'scripts/gateway-parity/session_domain.py')
    suite = session._load('host_suite', session.SCRIPT / 'host_suite.py')
    session._verify_cli_imports()
    return session.native_evidence, suite


def baseline_dependencies(common, baseline, *, hash_bytes):
    modules = (baseline / 'node_modules').resolve(strict=True)
    assert modules.is_dir() and modules.is_relative_to(baseline), 'installed modules escaped the fixed TS baseline'
    if hash_bytes:
        return {'path': str(modules), 'sha256': common.tree_digest(modules)}
    return str(modules)


def plan_jobs(cases):
    assert len(cases) == 72 and len(set(cases)) == len(cases)
    jobs = []
    for host, sidecar in ROLES:
        if host == 'ts':
            jobs.extend({'host': host, 'sidecar': sidecar, 'cases': [case], 'rounds': list(range(1, K + 1))}
                        for case in cases)
            continue
        # These bound artifact size/time per Linux trace. They are not scoring
        # thresholds. Measured TS sidecar ~3 MiB/turn; never increase 64 MiB.
        batch = 8 if sidecar == 'ts' else 16
        for k in range(1, K + 1):
            for registry in ('off', 'on'):
                group = [case for case in cases if case.startswith('registry-') == (registry == 'on')]
                for offset in range(0, len(group), batch):
                    jobs.append({'host': host, 'sidecar': sidecar, 'cases': group[offset:offset + batch],
                                 'rounds': [k], 'registry': registry})
    for index, job in enumerate(jobs, 1):
        job['id'] = f"{index:03d}-{job['host']}-{job['sidecar']}"
    return jobs


def identity(plan, *, full, prepared_dependencies=None):
    root = Path(plan['sourceRoot'])
    baseline = Path(plan['baseline'])
    assert root == ROOT, 'the registered entry must own the source checkout'
    clean_head(root, plan['head'])
    clean_head(baseline, BASELINE)
    source = read(checked(plan['sourceManifest']))
    binary_manifest = checked(plan['binaryManifest'])
    build = read(binary_manifest)
    assert source['head'] == plan['head'] and source.get('runtimeLibraries') == plan['runtimeLibraries']
    assert isinstance(build, list) and len(build) == 4 and {row['path'] for row in build} == PROGRAMS
    assert baseline_dependencies(None, baseline, hash_bytes=False) == plan['baselineDependencies']['path']
    if full:
        for row in source['files']:
            path = Path(row['path'])
            assert not path.is_absolute() and '..' not in path.parts and sha(root / path) == row['sha256'], str(path)
        for row in build:
            assert sha(Path(plan['binaryManifest']['path']).parent / row['path']) == row['sha256'], row['path']
        checked(plan['node'])
        common = load_common(root)
        common.verify_native_runtime_libraries(source, binary_manifest)
        dependencies = prepared_dependencies or baseline_dependencies(common, baseline, hash_bytes=True)
        assert dependencies == plan['baselineDependencies'], 'installed TS dependency bytes changed since cohort start'
        assert pin(__file__) == plan['runner'] and pin(ENTRY) == plan['producer'], (
            'resume with the same registered scheduler and producer')
    cases = read(checked(plan['calibration']))['cases']
    assert plan['jobs'] == plan_jobs(cases), 'schedule must cover the original whole matrix'
    for role in ROLES:
        pairs = [(case, k) for job in plan['jobs'] if (job['host'], job['sidecar']) == role
                 for case in job['cases'] for k in job['rounds']]
        assert len(pairs) == len(set(pairs)) == 72 * K
        assert set(pairs) == {(case, k) for case in cases for k in range(1, K + 1)}
    return root, baseline


def prepare(args):
    root, baseline = ROOT, args.baseline.resolve(strict=True)
    cohort = args.cohort.resolve()
    assert OUTPUT_ROOT.resolve() == OUTPUT_ROOT
    assert cohort.is_relative_to(OUTPUT_ROOT) and cohort != OUTPUT_ROOT
    cohort.mkdir(exist_ok=False)
    source = pin(args.source_manifest)
    source_document = read(source['path'])
    calibration = pin(root / 'scripts/gateway-parity/fixtures/session-calibration.json')
    dependencies = baseline_dependencies(load_common(root), baseline, hash_bytes=True)
    plan = {'sourceRoot': str(root), 'head': source_document['head'], 'baseline': str(baseline),
            'node': pin(args.node), 'sourceManifest': source, 'binaryManifest': pin(args.binary_manifest),
            'runtimeLibraries': source_document.get('runtimeLibraries'), 'baselineDependencies': dependencies,
            'runner': pin(__file__), 'producer': pin(ENTRY), 'calibration': calibration, 'image': IMAGE,
            'jobs': plan_jobs(read(calibration['path'])['cases'])}
    identity(plan, full=True, prepared_dependencies=dependencies)
    save(cohort / 'plan.json', plan)
    print(json.dumps({'plan': str(cohort / 'plan.json'), 'jobs': len(plan['jobs']), 'measurementsPerCell': 360}))


def capture_ref(plan, job, attempt):
    launch, cleanup = read(attempt / 'launch.json'), read(attempt / 'cleanup.json')
    assert launch['job'] == job and launch['planSha256'] == sha(attempt.parents[2] / 'plan.json')
    assert launch['runtimeLibraries'] == plan['runtimeLibraries']
    assert launch['baselineDependencies'] == plan['baselineDependencies']
    assert cleanup['exitCode'] == 0 and cleanup['containerRemaining'] == '' and not cleanup['pendingOwners']
    out = attempt / 'inside'
    if job['host'] == 'ts':
        reference = read(out / 'host-capture.json')
        assert reference['case'] == job['cases'][0] and reference['repeats'] == K and reference['sidecar'] == job['sidecar']
        checked(reference['receipt'])
        preflight = read(checked(reference['preflight']))
        assert preflight['head'] == plan['head'] and preflight['sourceManifest'] == plan['sourceManifest'] and preflight['binaryManifest'] == plan['binaryManifest']
        assert reference['rawSha256'] == {path.name: sha(path) for path in (out / 'raw').glob('*.json')}
    else:
        receipt = out / 'capture/receipt.json'
        assert read(receipt)['status'] == 'WITNESS_CAPTURED'
        assert read(out / 'capture/launch.json')['cases'] == job['cases']
        reference = {'receipt': pin(receipt), 'cases': job['cases'], 'repetition': job['rounds'][0],
                     'registry': job['registry']}
        if job['sidecar'] == 'ts':
            reference['tsSidecarProvenance'] = pin(out / 'ts-sidecar-provenance.json')
    if 'ts' in (job['host'], job['sidecar']):
        proof = attempt / 'baseline-runtime-proof.json'
        assert read(proof)['tree'] == plan['baselineDependencies'], 'capture dependency tree differs from cohort'
        reference['baselineRuntimeProof'] = pin(proof)
    return reference


def selected_attempt(cohort, plan, job):
    attempts = sorted((cohort / 'jobs' / job['id']).glob('attempt-*'))
    completed = [attempt for attempt in attempts if (attempt / 'accepted-capture.json').is_file()]
    assert len(completed) <= 1, 'two traces cannot count as one planned run'
    if completed:
        attempt = completed[0]
        reference = capture_ref(plan, job, attempt)
        assert read(attempt / 'accepted-capture.json') == reference, 'checkpoint bytes changed'
        return reference, len(attempts)
    return None, len(attempts)


def start(cohort, plan, job, attempt, common, scoring, wait):
    root, baseline = identity(plan, full=False)
    node = Path(plan['node']['path'])
    binary_dir = Path(plan['binaryManifest']['path']).parent
    name = 'pdrs-host-full-' + uuid.uuid4().hex[:16]
    command = ['docker', 'run', '--rm', '--pull', 'never', '--name', name, '--network', 'none', '--read-only',
               '--user', f'{os.getuid()}:{os.getgid()}', '--tmpfs', '/tmp:rw,nosuid,nodev,mode=1777,size=256m',
               '--workdir', str(root)]
    for path in (Path('/usr'), Path('/lib'), root, baseline, node.parent.parent, UV):
        command.extend(['--mount', f'type=bind,src={path},dst={path},readonly'])
    command.extend(['--mount', f'type=bind,src={cohort},dst={cohort}', IMAGE, '/usr/libexec/docker/docker-init', '--', '/usr/bin/python3', '-I'])
    inside = attempt / 'inside'
    if job['host'] == 'ts':
        command.extend([str(root / 'scripts/a5-19/run-host.py'), '--baseline', str(baseline), '--node', str(node),
                        '--source-manifest', plan['sourceManifest']['path'], '--binary-manifest', plan['binaryManifest']['path'],
                        '--out', str(inside), '--case', job['cases'][0], '--sidecar', job['sidecar'], '--lease-wait', '1'])
        timeout = 90
    else:
        sidecar = node if job['sidecar'] == 'ts' else binary_dir / 'pilotdeck-agent-loop-sidecar'
        command.extend([str(root / 'scripts/native-witness/collect.py'), '--gateway', str(binary_dir / 'pilotdeck-gateway'),
                        '--sidecar', str(sidecar), '--output', str(inside), '--session-domain', '--registry', job['registry']])
        for case in job['cases']:
            command.extend(['--session-case', case])
        if job['sidecar'] == 'ts':
            command.extend(['--ts-sidecar-baseline', str(baseline)])
        timeout = 930
    child = None
    timed_out = False
    with common.verified_parity_lock(timeout=wait):
        clean_head(root, plan['head'])
        clean_head(baseline, BASELINE)
        level = disk_level(root)
        attempt.mkdir(exist_ok=False, parents=True)
        save(attempt / 'launch.json', {'job': job, 'planSha256': sha(cohort / 'plan.json'), 'container': name,
                                       'command': command, 'hostNamespace': os.readlink('/proc/self/ns/net'), 'diskBudget': level,
                                       'runtimeLibraries': plan['runtimeLibraries'],
                                       'baselineDependencies': plan['baselineDependencies']})
        try:
            with (attempt / 'stdout').open('x') as stdout, (attempt / 'stderr').open('x') as stderr:
                with common.deferred_signals():
                    child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True)
                    common.LIVE_GROUPS.add(child.pid)
                try:
                    child.wait(timeout=timeout)
                except subprocess.TimeoutExpired:
                    timed_out = True
        finally:
            with common.deferred_signals():
                if child is not None and child.poll() is None:
                    subprocess.run(['docker', 'stop', '--time', '5', name], capture_output=True, timeout=12)
                    child.wait(timeout=10)
                remaining = subprocess.check_output(['docker', 'ps', '-aq', '--filter', 'name=^/' + name + '$'],
                                                    text=True, timeout=5).strip()
                if child is not None and child.poll() is not None and not remaining:
                    common.LIVE_GROUPS.discard(child.pid)
                save(attempt / 'cleanup.json', {'exitCode': None if child is None else child.returncode,
                                               'containerRemaining': remaining, 'pendingOwners': sorted(common.LIVE_GROUPS)})
                assert not remaining and not common.LIVE_GROUPS, 'owned cleanup incomplete; retain the lease'
        if timed_out:
            save(attempt / 'incomplete.json', {'status': 'CAPTURE_TIMEOUT', 'timeoutSeconds': timeout})
            return False
        if child.returncode == 0:
            if 'ts' in (job['host'], job['sidecar']):
                native, suite = scoring
                raw = native.read_trace(inside / 'capture/trace.log')
                proof = suite.baseline_runtime_proof(native, raw, baseline, plan['baselineDependencies'])
                save(attempt / 'baseline-runtime-proof.json', proof)
            save(attempt / 'accepted-capture.json', capture_ref(plan, job, attempt))
            return True
        return False


def open_plan(path):
    path = path.resolve(strict=True)
    assert OUTPUT_ROOT.resolve() == OUTPUT_ROOT
    assert path.name == 'plan.json' and path.parent.is_relative_to(OUTPUT_ROOT) and path.parent != OUTPUT_ROOT
    plan = read(path)
    assert plan['image'] == IMAGE
    identity(plan, full=True)
    return path.parent, plan


def run(args):
    cohort, plan = open_plan(args.plan)
    root = Path(plan['sourceRoot'])
    common = load_common(root)
    scoring = load_scoring_modules(root)
    attempted = failures = 0
    assert not args.job or set(args.job) <= {row['id'] for row in plan['jobs']}
    for job in plan['jobs']:
        if args.cell and f"{job['host']}/{job['sidecar']}" not in args.cell:
            continue
        if args.job and job['id'] not in args.job:
            continue
        accepted, count = selected_attempt(cohort, plan, job)
        if accepted or count >= 3 or (count and not args.retry_failed):
            continue
        try:
            ok = start(cohort, plan, job, cohort / 'jobs' / job['id'] / f'attempt-{count + 1:02d}', common, scoring, args.lease_wait)
        except TimeoutError:
            print(json.dumps({'status': 'BLOCKED', 'job': job['id'], 'reason': 'shared host parity lease unavailable'}))
            return 2
        attempted += 1
        failures += not ok
        print(json.dumps({'job': job['id'], 'captureStatus': 'CAPTURED' if ok else 'INCOMPLETE'}), flush=True)
        if args.max_captures and attempted >= args.max_captures:
            break
    return 1 if failures else 0


def manifest(args):
    cohort, plan = open_plan(args.plan)
    cells = [{'host': host, 'sidecar': sidecar, 'captures': []} for host, sidecar in ROLES]
    missing = []
    for job in plan['jobs']:
        reference, _ = selected_attempt(cohort, plan, job)
        if reference is None:
            missing.append(job['id'])
        else:
            cell = next(row for row in cells if (row['host'], row['sidecar']) == (job['host'], job['sidecar']))
            cell['captures'].append(reference)
    output = args.output.resolve()
    assert output.is_relative_to(cohort)
    fields = ('sourceManifest', 'binaryManifest', 'node', 'baseline', 'baselineDependencies')
    save(output, {**{field: plan[field] for field in fields}, 'cells': cells})
    print(json.dumps({'manifest': str(output), 'missingJobs': missing, 'scope': 'CAPTURE_MANIFEST_ONLY_NO_HOST_VERDICT'}))
    return 0 if not missing else 3


def main(argv):
    cli = argparse.ArgumentParser(prog='run-host.py cohort', description=__doc__)
    sub = cli.add_subparsers(dest='mode', required=True)
    prep = sub.add_parser('prepare')
    for name in ('baseline', 'node', 'source-manifest', 'binary-manifest', 'cohort'):
        prep.add_argument('--' + name, type=Path, required=True)
    start_parser = sub.add_parser('run')
    start_parser.add_argument('--plan', type=Path, required=True)
    start_parser.add_argument('--max-captures', type=int, choices=range(1, 1000))
    start_parser.add_argument('--cell', choices=[f'{host}/{sidecar}' for host, sidecar in ROLES], action='append')
    start_parser.add_argument('--job', action='append', help='one approved plan job for a short owned window')
    start_parser.add_argument('--lease-wait', type=float, default=10)
    start_parser.add_argument('--retry-failed', action='store_true')
    render = sub.add_parser('manifest')
    render.add_argument('--plan', type=Path, required=True)
    render.add_argument('--output', type=Path, required=True)
    args = cli.parse_args(argv)
    if args.mode == 'prepare':
        prepare(args)
        return 0
    if args.mode == 'manifest':
        return manifest(args)
    def interrupted(signum, _frame):
        raise KeyboardInterrupt(f'owned capture interrupted by signal {signum}')
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, interrupted)
    return run(args)
