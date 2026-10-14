#!/usr/bin/env -S python3 -I
"""20 TS references -> 5 disjoint TS scoring runs -> TS gates -> 1 proxy.

Use calibrate.sh to own /tmp/pdrs-parity.lock. REFUSED attempts are immutable;
--batch B --retry ref7 replaces ONLY that refused slot, then --batch B resumes.
The UTC day, apparatus, source and fixture path are pinned for the entire batch.
"""
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
import argparse
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
import uuid
import time

import hashlib
from apparatus import (git_admission, ApparatusUnsafe, prepare_cargo, cargo_process,
                       cargo_identity, cargo_config_admission)
from common import ROOT, apparatus_hashes, source_hashes, node_modules_hash, validate_clock_check
from multimodal import capture_identity

NAMES = [f'ref{i}' for i in range(1, 21)] + [f'ts{i}' for i in range(1, 6)] + ['proxy']
CAP_BYTES = 30_000_000_000  # 30 GB, conservatively below the legacy 30 GiB check.
BUILD_RESERVE_BYTES = 1_000_000_000
REPORT_RESERVE_BYTES = 1_000_000_000


def read(path):
    return json.loads(path.read_text())


def write(path, value):
    temporary = path.with_suffix(path.suffix + '.new')
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(path)


def utc_day():
    return datetime.now(timezone.utc).date().isoformat()


def size(paths):
    paths = [str(p) for p in paths if p.exists()]
    if not paths:
        return 0
    return sum(int(line.split()[0]) * 1024 for line in
               subprocess.check_output(['du', '-sk', *paths], text=True).splitlines())


def resource_root():
    common = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse',
                                     '--path-format=absolute', '--git-common-dir'], text=True).strip()
    return Path(common).resolve().parent


def space_check(resource, reference, remaining=26, *, build=True):
    if not reference.is_dir() or not (reference / 'direct/recording.jsonl').is_file():
        raise ValueError('--size-reference must be a whole live3 run directory (with direct/recording.jsonl)')
    reference_bytes = size([reference])
    # 50% above measured run size; logs, rejected runs remain on disk.
    per_run = math.ceil(reference_bytes * 1.5)
    used = size([resource / '_build', resource / '_tmp'])
    needed = per_run * remaining + REPORT_RESERVE_BYTES + (BUILD_RESERVE_BYTES if build else 0)
    free = shutil.disk_usage(resource).free
    result = {'resource_root': str(resource), 'cap_bytes': CAP_BYTES, 'used_bytes': used,
              'size_reference': str(reference), 'reference_bytes': reference_bytes,
              'per_run_estimate_bytes': per_run, 'remaining_runs': remaining,
              'estimated_additional_bytes': needed, 'projected_bytes': used + needed,
              'filesystem_free_bytes': free}
    if used + needed > CAP_BYTES or needed > free:
        raise ValueError('space budget exceeded; preserve evidence: ' + json.dumps(result))
    return result


def default_reference(resource):
    # Include archived worker evidence; never use these captures in the TS pool.
    candidates = [p.parent.parent for p in (resource / '_tmp').rglob('run-meta.json')
                  if 'live3' in p.parent.parent.name and p.parent.name == 'direct'
                  and (p.parent / 'recording.jsonl').is_file()]
    if not candidates:
        raise ValueError('live3 size reference absent; supply --size-reference RUN_DIR')
    return max(candidates, key=lambda p: size([p]))


def gate_report(report, *, proxy=False):
    if (report.get('verdict') != 'PENDING_REVIEW' or report.get('calibration_eligible') is not True
            or report.get('comparison_mode') != 'calibration' or report.get('self_compare') is not True
            or len(report.get('pairs', [])) != 10
            or any(p.get('verdict') != 'PASS' for p in report['pairs'])
            or report.get('per_rule_negative') is not True or report.get('same_bad_rejected') is not True
            or report.get('errors') or report.get('independent_review') is not None
            or (proxy and report.get('proxy_compare', {}).get('verdict') != 'PASS')):
        raise ValueError('ruler machine gates did not pass: ' + str(report.get('errors', report.get('verdict'))))


class Batch:
    def __init__(self, path, state):
        self.path, self.state = path, state
        self.slot = path / 'slot'
        self.target = path / 'cargo-target'
        self.cargo_home = path / 'cargo-home'
        self.binary = self.target / 'debug/pdrs-exp008-proxy'

    def save(self):
        write(self.path / 'batch.json', self.state)

    def build(self):
        # Refuse even an empty pre-existing target: every batch starts from nothing.
        try:
            prepare_cargo(self.path, self.cargo_home, self.target, ROOT / '_build/cargo-home')
        except FileExistsError as error:
            raise ApparatusUnsafe('batch target already exists or Cargo home reused; never reuse a build') from error
        # Record the selected toolchain before executing any build input.
        toolchain = cargo_identity(self.path, self.cargo_home, self.target)
        self.state['toolchain'] = toolchain
        self.save()
        for mode in ('fetch', 'build'):
            with (self.path / (mode + '.log')).open('w') as output:
                command = ['cargo', mode, '--locked', '--offline', '--manifest-path',
                           str(ROOT / 'scripts/exp008/proxy/Cargo.toml')]
                try:
                    cargo_process(command, self.path, self.cargo_home, self.target,
                                  stdout=output, stderr=subprocess.STDOUT)
                except subprocess.CalledProcessError as error:
                    raise ApparatusUnsafe(f'batch Cargo {mode} failed; see {self.path / (mode + ".log")}') from error
            space_check(Path(self.state['resource_root']), Path(self.state['size_reference']), build=False)
        self.state['build'] = {'target_dir': str(self.target), 'binary': str(self.binary),
                               'cargo_home': str(self.cargo_home), 'toolchain': toolchain,
                               'sha256': hashlib.sha256(self.binary.read_bytes()).hexdigest()}
        self.save()

    def verify_build(self):
        build = self.state.get('build', {})
        cargo_config_admission(self.path, self.cargo_home)
        if (build.get('target_dir') != str(self.target) or build.get('binary') != str(self.binary)
                or build.get('cargo_home') != str(self.cargo_home) or not self.cargo_home.is_dir()
                or self.target.is_symlink() or self.binary.is_symlink() or not self.binary.is_file()
                or not self.binary.resolve().is_relative_to(self.target.resolve())
                or hashlib.sha256(self.binary.read_bytes()).hexdigest() != build.get('sha256')):
            raise ApparatusUnsafe('batch build missing/changed; never use old _build artifacts')
        if cargo_identity(self.path, self.cargo_home, self.target) != build.get('toolchain'):
            raise ApparatusUnsafe('batch toolchain changed; start a NEW batch')

    def pin(self):
        try:
            source = source_hashes(Path(self.state['baseline']), self.binary)
        except (OSError, ValueError) as error:
            raise ApparatusUnsafe('batch source unavailable/changed: ' + str(error)) from error
        if source['node_modules'] != self.state.get('node_modules', {}).get('sha256'):
            raise ApparatusUnsafe('FAIL: baseline node_modules changed/absent batch binding')
        return {'apparatus': apparatus_hashes(), 'source': source}

    def guard(self, remaining=0):
        if not self.state.get('head'):
            raise ApparatusUnsafe('batch HEAD absent; start a NEW batch')
        git_admission(ROOT, self.state['head'])
        self.verify_build()
        if utc_day() != self.state['utc_day']:
            raise ValueError('REFUSED: batch crossed UTC midnight; start a NEW batch without reusing these runs')
        if self.pin() != self.state['pinned']:
            raise ApparatusUnsafe('FAIL: apparatus/source changed; start a NEW batch, never edit capture metadata')
        return space_check(Path(self.state['resource_root']), Path(self.state['size_reference']), remaining, build=False)

    def admit(self, name):
        side = 'proxy' if name == 'proxy' else 'direct'
        directory = self.path / name / 'direct'
        meta = read(directory / 'run-meta.json')
        validate_clock_check(meta)
        identity = capture_identity(directory, side, self.state['pinned']['apparatus'])
        if (identity['source_sha256'] != self.state['pinned']['source']
                or identity['capture_utc_day'] != self.state['utc_day']
                or identity['fixture_path'] != str(self.slot / 'direct')
                or meta.get('baseline_path') != self.state['baseline']):
            raise ValueError('capture differs from pinned source/day/path: ' + name)
        for other, entry in self.state['runs'].items():
            if other == name or entry['status'] != 'PASS':
                continue
            old = entry['identity']
            if (any(identity[k] == old[k] for k in ('run_id', 'recorder_pid', 'recording_sha256'))
                    or not (identity['wall_start_ms'] > old['wall_end_ms']
                            or identity['wall_end_ms'] < old['wall_start_ms'])):
                raise ValueError('captures overlap or repeat identities: ' + other + '/' + name)
        return identity

    def capture(self, name, *, retry=False):
        entry = self.state['runs'].get(name)
        if retry:
            if not entry or entry['status'] != 'REFUSED':
                raise ValueError('--retry requires that exact run to have recorded REFUSED')
            if name.startswith('ref') and any(n.startswith('ts') or n == 'proxy' for n in self.state['runs']):
                raise ValueError('reference pool already consumed; cannot replace it after scoring starts')
        elif entry:
            if entry['status'] != 'PASS':
                raise ValueError(f'{name} is {entry["status"]}; use --batch {self.path} --retry {name} for REFUSED')
            if self.admit(name) != entry['identity']:
                raise ValueError('accepted archive changed: ' + name)
            return
        index = NAMES.index(name)
        if any(self.state['runs'].get(n, {}).get('status') != 'PASS' for n in NAMES[:index]):
            raise ValueError('capture order requires all earlier slots to PASS')
        if self.slot.exists() or (self.path / name).exists():
            raise ValueError('unarchived slot/target already exists; preserve it and inspect, do not overwrite')
        self.guard(sum(self.state['runs'].get(n, {}).get('status') != 'PASS' for n in NAMES))
        attempt_id = f'{name}-{uuid.uuid4()}'
        log = self.path / 'attempts' / (attempt_id + '.log')
        print('capture ' + name + ' -> ' + str(log), flush=True)
        with log.open('w') as output:
            code = subprocess.run([sys.executable, '-I', str(ROOT / 'scripts/ui-server-parity/run.py'),
                                   '--build-batch', str(self.path),
                                   '--baseline', self.state['baseline'], '--out', str(self.slot),
                                   '--side', 'proxy' if name == 'proxy' else 'direct', '--slot', 'direct'],
                                  cwd=ROOT, stdout=output, stderr=subprocess.STDOUT).returncode
        archive = self.path / 'attempts' / attempt_id
        if not self.slot.exists():
            raise ValueError('runner produced no slot; inspect ' + str(log))
        self.slot.rename(archive)  # Evidence survives nonzero exit as well as REFUSED.
        result = read(archive / 'result.json')
        status = result.get('status')
        self.state['runs'][name] = {'status': status, 'archive': str(archive), 'log': str(log), 'exit_code': code}
        self.state.setdefault('attempts', []).append({'name': name, **self.state['runs'][name]})
        self.save()
        if code != 0 or status != 'PASS':
            raise ValueError(f'{name}: {status}/{code}; preserved {archive}; ' +
                             (f'retry ONLY this run: --batch {self.path} --retry {name}' if status == 'REFUSED' else 'inspect runner failure'))
        archive.rename(self.path / name)
        self.state['runs'][name]['archive'] = str(self.path / name)
        self.state['attempts'][-1]['archive'] = str(self.path / name)
        try:
            self.guard()
            self.state['runs'][name]['identity'] = self.admit(name)
        except Exception:
            self.state['runs'][name]['status'] = 'FAIL'
            self.save()
            raise
        self.save()

    def ruler(self, proxy=False):
        self.guard()
        for name in NAMES if proxy else NAMES[:-1]:
            if self.state['runs'].get(name, {}).get('status') != 'PASS' or self.admit(name) != self.state['runs'][name]['identity']:
                raise ValueError('missing/changed valid capture: ' + name)
        # Immutable report attempts: rerunning a gate never overwrites earlier evidence.
        tag = ('final-ruler' if proxy else 'ts-ruler') + '-' + str(uuid.uuid4())
        report_path = self.path / (tag + '.json')
        command = [sys.executable, '-I', str(ROOT / 'scripts/ui-server-parity/check_ruler.py')]
        for i in range(1, 6):
            command.extend(['--round', str(self.path / f'ts{i}/direct')])
        command += ['--enumeration', str(self.path / 'ts1/enumeration/router-enumeration.json'),
                    '--reference-pool', *[str(self.path / f'ref{i}/direct') for i in range(1, 21)],
                    '--out', str(report_path)]
        if proxy:
            command += ['--proxy', str(self.path / 'proxy/direct')]
        with (self.path / (tag + '.log')).open('w') as output:
            code = subprocess.run(command, cwd=ROOT, stdout=output, stderr=subprocess.STDOUT).returncode
        report = read(report_path)
        self.state['final_ruler' if proxy else 'ts_ruler'] = str(report_path)
        self.save()
        self.guard()
        if code != 2:
            raise ValueError(f'check_ruler exit {code}; see {report_path}')
        gate_report(report, proxy=proxy)
        return report

    def summary(self, error=None, verdict=None):
        checks = {}
        for name, entry in self.state['runs'].items():
            path = Path(entry['archive']) / 'direct/run-meta.json'
            checks[name] = read(path).get('clock_regression_check', {}).get('status', 'absent') if path.exists() else 'absent'
        report = read(Path(self.state['final_ruler'])) if self.state.get('final_ruler') else {}
        result = {'verdict': verdict or ('INCOMPLETE' if error or not report else report['verdict']), 'error': error,
                  'batch': str(self.path), 'utc_day': self.state['utc_day'],
                  'ts_pairs_pass': sum(p['verdict'] == 'PASS' for p in report.get('pairs', [])),
                  'ts_pairs_expected': 10, 'proxy': report.get('proxy_compare', {}).get('verdict'),
                  'clock_regression_check': checks,
                  'all_26_clocks_pass': len(checks) == 26 and all(v == 'PASS' for v in checks.values()),
                  'independent_review': None, 'frozen': False,
                  'head': self.state.get('head'), 'build': self.state.get('build'),
                  'reports': {k: self.state[k] for k in ('ts_ruler', 'final_ruler') if k in self.state}}
        write(self.path / 'summary.json', result)
        print(json.dumps(result, ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--resource-root', type=Path)
    parser.add_argument('--size-reference', type=Path)
    parser.add_argument('--batch', type=Path, help='existing batch.json to resume; no metadata edits')
    parser.add_argument('--retry', choices=NAMES, help='retry only this recorded REFUSED run, then exit')
    parser.add_argument('--preflight', action='store_true', help='read-only space estimate; no build/listen')
    args = parser.parse_args()
    batch = None
    try:
        head = git_admission(ROOT)
        resource = (args.resource_root or resource_root()).resolve()
        reference = (args.size_reference or default_reference(resource)).resolve()
        if args.preflight:
            print(json.dumps(space_check(resource, reference), ensure_ascii=False, indent=2))
            return 0
        if not Path('/tmp/pdrs-parity.lock').is_dir():
            raise ValueError('use calibrate.sh to take the parity lock')
        if args.retry and not args.batch:
            raise ValueError('--retry requires --batch')
        baseline = (args.baseline or resource / 'resources/PilotDeck').resolve()
        if args.batch:
            path = args.batch.resolve()
            state = read(path / 'batch.json')
            if state.get('root') != str(ROOT) or state.get('schema') != 1:
                raise ValueError('batch belongs to another root/schema')
            for key, arg in (('baseline', args.baseline), ('resource_root', args.resource_root), ('size_reference', args.size_reference)):
                if arg and str(arg.resolve()) != state[key]:
                    raise ValueError('cannot change pinned ' + key + ' on resume')
            batch = Batch(path, state)
            batch.guard()
        else:
            estimate = space_check(resource, reference)
            print(json.dumps({'preflight': estimate}, ensure_ascii=False), flush=True)
            if not subprocess.check_output(['node', '--version'], text=True).startswith('v22.'):
                raise ValueError('requires Node v22')
            path = ROOT / '_tmp' / ('b2-calibration-' + utc_day() + '-' + str(uuid.uuid4()))
            (path / 'attempts').mkdir(parents=True)
            state = {'schema': 1, 'root': str(ROOT), 'head': head, 'utc_day': utc_day(), 'baseline': str(baseline),
                     'resource_root': str(resource), 'size_reference': str(reference), 'space_estimate': estimate, 'runs': {}}
            batch = Batch(path, state)
            started = time.monotonic()
            state['node_modules'] = {'sha256': node_modules_hash(baseline),
                                     'elapsed_seconds': time.monotonic() - started}
            batch.save()
            print('batch ' + str(path), flush=True)
            batch.build()
            state['pinned'] = batch.pin()
            batch.save()
            batch.guard()
        if args.retry:
            if args.retry == 'proxy':
                batch.ruler()
            batch.capture(args.retry, retry=True)
        else:
            for name in NAMES[:-1]:
                batch.capture(name)
            batch.ruler()  # Complete TS-only gates before recording any DUT.
            batch.capture('proxy')
            batch.ruler(proxy=True)
        batch.summary()
        return 0
    except ApparatusUnsafe as error:
        if batch:
            batch.summary(str(error), verdict='FAIL')
        else:
            print(json.dumps({'verdict': 'FAIL', 'error': str(error)}, ensure_ascii=False), file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        if batch:
            batch.summary(str(error))
        else:
            print(json.dumps({'verdict': 'REFUSED', 'error': str(error)}, ensure_ascii=False), file=sys.stderr)
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
