#!/usr/bin/env python3
"""Coordinator-only cold restart / high loopback WS; terminate/reap only owned children."""
import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('a52b_launcher', Path(__file__).with_name('run-b.py'))
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)


def stop_owned(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)
    return child.returncode


def ready_barrier(child, log, marker, timeout=30):
    selector = selectors.DefaultSelector()
    selector.register(child.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + timeout
    pending = ''
    try:
        while time.monotonic() < deadline:
            if not selector.select(timeout=min(1, max(0, deadline-time.monotonic()))):
                if child.poll() is not None:
                    raise RuntimeError(f'child exited before {marker}: {child.returncode}')
                continue
            chunk = os.read(child.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError(f'child EOF before {marker}')
            text = chunk.decode('utf8', errors='replace')
            log.write(text)
            log.flush()
            pending += text
            if marker in pending.splitlines():
                return
        raise TimeoutError(f'barrier not reached: {marker}')
    finally:
        selector.close()


def check_rule(ok, rule):
    if not ok:
        raise RuntimeError(f'{rule}: coordinator evidence validation failed')


def check_restart(before, after, phases):
    check_rule(before['pid'] != after['pid'] and before['pilotHome'] == after['pilotHome']
               and all(p['reaped'] for p in phases), 'RESTART_IDENTITY')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--coordinator', action='store_true', required=True, help='required explicit external-run acknowledgement')
    parser.add_argument('--baseline', type=Path, default=Path('/Users/da/ws/pilotdeck-rs/resources/PilotDeck'))
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--suite', choices=('cold', 'ws'), default='cold')
    parser.add_argument('--port', type=int, default=48789, help='WS only: high loopback port, never scan/kill an owner')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    node = shutil.which('node')
    if not node:
        parser.error('Node 22 required; no download fallback')
    if not 40000 <= args.port <= 59999:
        parser.error('port must be 40000..59999')
    if args.suite == 'ws':
        env = launcher.isolated_environment(output)
        env.update(A52B_COORDINATOR='1', A52B_WS='1', A52B_PORT=str(args.port),
                   A52B_BASELINE=str(args.baseline.resolve()), A52B_OUTPUT=str(output / 'raw'))
        command = [node, '--experimental-transform-types', str(ROOT / 'scripts/a5-19/ws-b.mts')]
        audit = {'command': command, 'environment': env, 'host': '127.0.0.1', 'port': args.port,
                 'launcherPid': os.getpid(), 'ownsOnlySpawnedChild': True}
        with (output / 'run.log').open('w') as log:
            child = subprocess.Popen(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
            audit['pid'] = child.pid
            try:
                code = child.wait(timeout=900)
            finally:
                audit.update(exitCode=stop_owned(child), reaped=True)
                (output / 'launch.json').write_text(json.dumps(audit, ensure_ascii=False, indent=2) + '\n')
        return code
    command = [node, '--experimental-transform-types', str(ROOT / 'scripts/a5-19/cold-child-b.mts')]
    audit = {'launcherPid': os.getpid(), 'command': command, 'ownedChildren': [], 'listeners': [], 'repeats': 5,
             'preregistrationSha256': hashlib.sha256((ROOT / 'scripts/a5-19/preregistration-b.mjs').read_bytes()).hexdigest()}
    summaries = []
    try:
        for state in ('running', 'unknown'):
            for repetition in range(1, 6):
                directory = output / state / str(repetition)
                directory.mkdir(parents=True)
                env = launcher.isolated_environment(directory)
                env.update(A52B_COORDINATOR='1', A52B_BASELINE=str(args.baseline.resolve()),
                           A52B_OUTPUT=str(directory), A52B_COLD_STATE=state)
                phases = []
                for phase, marker in (('prepare', 'A52B_READY'), ('read', 'A52B_READ_DONE')):
                    env['A52B_COLD_PHASE'] = phase
                    with (directory / f'{phase}.log').open('w') as log:
                        child = subprocess.Popen(command, cwd=ROOT, env=env, stdin=subprocess.PIPE,
                                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
                        child_audit = {'pid': child.pid, 'phase': phase, 'state': state, 'repetition': repetition,
                                       'pilotHome': env['PILOT_HOME'], 'environment': env}
                        audit['ownedChildren'].append(child_audit)
                        try:
                            ready_barrier(child, log, marker)
                            child_audit['barrierReached'] = marker
                            if phase == 'read':
                                code = child.wait(timeout=20)
                                check_rule(code == 0, 'RESTART_IDENTITY')
                        finally:
                            child_audit['exitCode'] = stop_owned(child)
                            child_audit['reaped'] = True
                            for pipe in (child.stdin, child.stdout):
                                if pipe:
                                    pipe.close()
                        phases.append(child_audit)
                before = json.loads((directory / 'before.json').read_text())
                after = json.loads((directory / 'after.json').read_text())
                check_restart(before, after, phases)
                disk = json.loads(after['registryRawText'])
                record = disk['records'][0]
                results = {r['id']: r for r in after['responses']}
                check_rule(all(results[m]['ok'] for m in ('run_get', 'run_events', 'run_reattach')), 'REGISTRY_RAW')
                # These compare read APIs to raw bytes, never assume interrupted
                # or a product terminal in advance. All actual values are kept.
                for method in ('run_get', 'run_reattach'):
                    api = results[method]['result']
                    check_rule(all(api.get(k) == record.get(k) for k in ('state', 'revision', 'lastSeq', 'result')), 'REGISTRY_RAW')
                check_rule(results['run_events']['result']['events'] == disk['events']['\0session\0run'], 'REGISTRY_RAW')
                check_rule(before['transcriptRawText'] == after['transcriptRawText'], 'STATUS_RAW')
                check_rule(before['preregistrationSha256'] == after['preregistrationSha256'] == audit['preregistrationSha256'], 'COVERAGE')
                summaries.append({'caseId': f'cold/{state}', 'repetition': repetition, 'beforeState': before['queryRecord']['state'],
                                  'afterRecord': record, 'responses': after['responses'], 'evidence': str(directory)})
        check_rule(len(summaries) == 10, 'COVERAGE')
        # Explicitly invalid PID/home evidence must hit the named rule too.
        for name in ('same-pid', 'different-home', 'unreaped-old-pid'):
            bad_after, bad_phases = copy.deepcopy(after), copy.deepcopy(phases)
            if name == 'same-pid':
                bad_after['pid'] = before['pid']
            elif name == 'different-home':
                bad_after['pilotHome'] += '/different'
            else:
                bad_phases[0]['reaped'] = False
            try:
                check_restart(before, bad_after, bad_phases)
            except RuntimeError as error:
                audit.setdefault('controls', []).append({'name': name, 'hitRule': str(error).split(':')[0]})
            else:
                raise RuntimeError(f'{name}: restart identity control escaped')
        (output / 'manifest.json').write_text(json.dumps({'summaries': summaries, 'audit': audit}, ensure_ascii=False, indent=2) + '\n')
        return 0
    finally:
        (output / 'launch.json').write_text(json.dumps(audit, ensure_ascii=False, indent=2) + '\n')


if __name__ == '__main__':
    raise SystemExit(main())
