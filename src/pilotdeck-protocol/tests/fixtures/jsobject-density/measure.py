#!/usr/bin/env python3
"""#1128 object-density measurement: boxed HEAD vs unboxed pre-boxing vs Node.

Usage:
  CARGO_TARGET_DIR=... python3 measure.py NEW_OUTPUT_DIR [workload ...]

Workloads: small-1 small-8 small-32 wide-1 wide-8 wide-32 deep-1 deep-8
deep-32 empty-1 empty-8 empty-32 transcript baseline (MiB units). Without a
selection every workload runs. The Rust harness is
`src/pilotdeck-protocol/tests/object_density.rs` (`#[ignore]`d, never a CI
judgment); this script builds it in release mode, measures decode / compact /
pretty / drop wall times over 7 reps plus a single-rep process for peak RSS
(wait4 rusage, bytes on macOS), then repeats everything with the boxing of
`JsObject.indices` reverted in place (the #983 memory fix stays committed;
only the measurement build is unboxed), and finally runs the same workload
files through Node v22.22.0 (JSON.parse + JSON.stringify(x, null, 2)).

The pre-boxing variant is a local, in-place reverse of the exact boxing diff
(`git diff c46e3803d^ c46e3803d -- src/pilotdeck-protocol/src/lib.rs`), per
the coordinator's instruction: the old implementation is never committed.
The script refuses to start on a dirty tracked tree, verifies the boxing
commit is reachable, and runs the reverse-patch window inside the shared
`mutating_sources` guard: the tree lock serializes concurrent in-place
runners, lib.rs is verified byte-for-byte against HEAD before the window
opens, and the snapshot restore is verified against both the snapshot and
the git object on the way out — never a blanket `git checkout`. It fails
closed if the diff anchor no longer applies, rebuilds after restoring, and
re-runs one spot-check workload so the post numbers reproduce. A
`pre-build-in-progress` marker file inside the output directory marks the
mutation window.
"""
from pathlib import Path
import hashlib
import json
import os
import statistics
import subprocess
import sys
import time

sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[5]
shared = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path.insert(0, str(shared))
from negative_process import (mutating_sources, new_output_directory,  # noqa: E402
                              run_owned, wait_for_slot)

# The commit that boxed JsObject.indices (#983 fix-r1); its parent is the
# unboxed baseline. Override only if the anchor is known to have moved.
BOXING_COMMIT = os.environ.get('PDRS_BOXING_COMMIT', 'c46e3803d')
LIB_RS = 'src/pilotdeck-protocol/src/lib.rs'
TRANSCRIPT = 'src/pilotdeck-gateway/tests/fixtures/compaction-orchestrator/oracle.json'
MIB = 1024 * 1024
# Pretty outputs that are dumped and sha256-compared across all three
# runners (boxed, unboxed, Node): one per class plus the transcript. A
# selected label of this set whose post or pre dump is missing aborts the
# run; a missing Node dump stays `tsMatchesRust: null` because the
# indented-stringify RangeError on deep spines is an expected baseline
# property, not a harness failure.
DUMP_CLASSES = {'small-1', 'wide-1', 'deep-1', 'empty-1', 'transcript'}
REPS = 7
NODE = os.environ.get('NODE22', '/opt/homebrew/bin/node')
RUN_TIMEOUT = 900

WORKLOADS = [
    ('small-1', 'small', MIB), ('small-8', 'small', 8 * MIB), ('small-32', 'small', 32 * MIB),
    ('wide-1', 'wide', MIB), ('wide-8', 'wide', 8 * MIB), ('wide-32', 'wide', 32 * MIB),
    ('deep-1', 'deep', MIB), ('deep-8', 'deep', 8 * MIB), ('deep-32', 'deep', 32 * MIB),
    ('empty-1', 'empty', MIB), ('empty-8', 'empty', 8 * MIB), ('empty-32', 'empty', 32 * MIB),
    ('transcript', 'file', None), ('baseline', 'small', 10),
]


def git(*args, check=True):
    result = subprocess.run(['git', '-C', str(root), *args], capture_output=True)
    if check and result.returncode:
        raise SystemExit(f'#1128 git {args[0]} failed: {result.stderr.decode().strip()}')
    return result


def display(path):
    """Repo-relative when possible; measurement outputs live outside the tree."""
    try:
        return str(Path(path).relative_to(root))
    except ValueError:
        return str(path)


def bench_line(output, marker):
    for line in output.splitlines():
        if line.startswith(marker + ' '):
            return json.loads(line[len(marker) + 1:])
    raise SystemExit(f'#1128 no {marker} line in runner output:\n{output[-2000:]}')


def build_release_test(env, log_name, out):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)
    (out / log_name).write_text(slot.stdout + slot.stderr)
    result = run_owned(
        ['cargo', 'test', '--release', '--offline', '--manifest-path', 'src/Cargo.toml',
         '-p', 'pilotdeck-protocol', '--test', 'object_density', '--no-run',
         '--message-format=json'],
        cwd=root, env=env, timeout=1800, idle_timeout=600)
    if result.returncode:
        raise SystemExit(f'#1128 release build of the harness failed:\n{result.stdout[-4000:]}')
    executable = None
    for line in result.stdout.splitlines():
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        target = entry.get('target') or {}
        if (entry.get('reason') == 'compiler-artifact' and target.get('name') == 'object_density'
                and target.get('kind') == ['test'] and entry.get('executable')):
            executable = entry['executable']
    if not executable:
        raise SystemExit('#1128 could not locate the built harness executable')
    return executable


def run_wrapped(command, run_env, label, out):
    """Run a measurement command under the wait4 wrapper; return (stdout line dict, rss).

    `run_owned` starts the wrapper in its own session and kills the whole
    group on timeout, so an over-running harness process cannot survive the
    run as an orphan and pollute later timings.
    """
    pid_file = out / f'{label}.pid'
    wrapped = [sys.executable, str(Path(__file__).resolve()), '--exec', str(pid_file), *command]
    child = run_owned(wrapped, env=run_env, timeout=RUN_TIMEOUT)
    usage = json.loads(pid_file.with_suffix('.usage.json').read_text())
    if child.returncode or usage['exit']:
        raise SystemExit(f'#1128 run {label} failed (exit {usage["exit"]}):\n'
                         f'{child.stdout[-2000:]}\n{child.stderr[-3000:]}')
    return usage, child.stdout


def harness_env(env, *, kind, label, reps, warmup, target_bytes=None, input_path=None,
                emit=None, dump=None):
    run_env = dict(env)
    run_env.update({
        'OBJECT_DENSITY_KIND': kind, 'OBJECT_DENSITY_LABEL': label,
        'OBJECT_DENSITY_REPS': str(reps), 'OBJECT_DENSITY_WARMUP': str(warmup),
    })
    if target_bytes is not None:
        run_env['OBJECT_DENSITY_TARGET_BYTES'] = str(target_bytes)
    if input_path is not None:
        run_env['OBJECT_DENSITY_INPUT'] = str(input_path)
    if emit is not None:
        run_env['OBJECT_DENSITY_EMIT'] = str(emit)
    if dump is not None:
        run_env['OBJECT_DENSITY_DUMP_PRETTY'] = str(dump)
    return run_env


def run_rust(exe, env, out, *, label, kind, reps, warmup, target_bytes=None, input_path=None,
             emit=None, dump=None):
    run_env = harness_env(env, kind=kind, label=label, reps=reps, warmup=warmup,
                          target_bytes=target_bytes, input_path=input_path, emit=emit, dump=dump)
    usage, stdout = run_wrapped([exe, 'object_density_bench', '--exact', '--ignored', '--nocapture'],
                                run_env, label, out)
    return bench_line(stdout, '#1128-BENCH'), usage['rssWait4Bytes']


def run_ts(env, out, *, label, input_path, reps, warmup, dump=None):
    run_env = harness_env(env, kind='file', label=label, reps=reps, warmup=warmup,
                          input_path=input_path, dump=dump)
    usage, stdout = run_wrapped([NODE, str(Path(__file__).resolve().parent / 'bench_ts.mjs')],
                                run_env, label, out)
    return bench_line(stdout, '#1128-TS'), usage['rssWait4Bytes']


def measure_rust(name, exe, env, out, workloads, inputs):
    rows = {}
    for label, _kind, _target in workloads:
        if label not in inputs:
            continue
        timed, _ = run_rust(exe, env, out, label=f'{name}-{label}-timed', kind='file',
                            reps=REPS, warmup=1, input_path=inputs[label])
        dump = out / 'dumps' / f'{name}-{label}.pretty' if label in DUMP_CLASSES else None
        rss_line, rss = run_rust(exe, env, out, label=f'{name}-{label}-rss', kind='file',
                                reps=1, warmup=0, input_path=inputs[label], dump=dump)
        rows[label] = {'timed': timed, 'rssLine': rss_line, 'rssBytes': rss}
        print(f'#1128 {name} {label}: valueBytes={timed["valueBytes"]} '
              f'decode={statistics.median(timed["decodeMs"]):.1f}ms '
              f'compact={statistics.median(timed["compactMs"]):.1f}ms '
              f'pretty={statistics.median(timed["prettyMs"]):.1f}ms '
              f'drop={statistics.median(timed["dropMs"]):.1f}ms rss={rss / MIB:.0f}MiB', flush=True)
    return rows


def main():
    args = sys.argv[1:]
    if args and args[0] == '--exec':
        pid_file = Path(args[1])
        pid = os.fork()
        if pid == 0:
            temporary = pid_file.with_suffix('.tmp')
            temporary.write_text(str(os.getpid()))
            temporary.replace(pid_file)
            os.execvpe(args[2], args[2:], os.environ)
        _, status, usage = os.wait4(pid, 0)
        code = os.waitstatus_to_exitcode(status)
        temporary = pid_file.with_suffix('.usage.tmp')
        temporary.write_text(json.dumps({'exit': code, 'rssWait4Bytes': usage.ru_maxrss}))
        temporary.replace(pid_file.with_suffix('.usage.json'))
        return code if code >= 0 else 128 - code
    if not args:
        raise SystemExit(__doc__)
    out = new_output_directory(args[0])
    selected = args[1:] or [row[0] for row in WORKLOADS]
    workloads = [row for row in WORKLOADS if row[0] in selected]
    if len(workloads) != len(selected):
        raise SystemExit(f'#1128 unknown workload in {selected}')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1128 CARGO_TARGET_DIR must be supplied by the caller')
    version = subprocess.run([NODE, '--version'], capture_output=True, text=True).stdout.strip()
    if version != 'v22.22.0':
        raise SystemExit(f'#1128 actual Node v22.22.0 required, found {version}')
    dirty = git('status', '--porcelain', '--untracked-files=no', check=False).stdout.decode()
    if dirty:
        raise SystemExit('#1128 tracked tree differs from HEAD before measuring:\n' + dirty)

    (out / 'workloads').mkdir()
    (out / 'dumps').mkdir()
    exe = build_release_test(env, 'build-post.slots.log', out)

    inputs = {}
    for label, kind, target in workloads:
        if label == 'transcript':
            inputs[label] = root / TRANSCRIPT
            continue
        emitted = out / 'workloads' / f'{label}.json'
        run_rust(exe, env, out, label=f'emit-{label}', kind=kind, reps=0, warmup=0,
                 target_bytes=target, emit=emitted)
        inputs[label] = emitted
    for label, path in inputs.items():
        digest = hashlib.sha256()
        with path.open('rb') as stream:
            for chunk in iter(lambda: stream.read(1 << 20), b''):
                digest.update(chunk)
        (out / 'workloads' / f'{label}.sha256').write_text(digest.hexdigest() + '\n')

    result = {'meta': {
        'started': time.strftime('%Y-%m-%dT%H:%M:%S%z'), 'node': version, 'reps': REPS,
        'head': git('rev-parse', 'HEAD').stdout.decode().strip(), 'boxingCommit': BOXING_COMMIT,
        'uname': subprocess.run(['uname', '-a'], capture_output=True, text=True).stdout.strip(),
        'inputs': {label: {'bytes': path.stat().st_size, 'path': display(path)}
                   for label, path in inputs.items()},
    }, 'workloads': {}}
    result['workloads']['post'] = measure_rust('post', exe, env, out, workloads, inputs)

    marker = out / 'pre-build-in-progress'
    marker.write_text('unboxed measurement build active; lib.rs is reverted below HEAD\n')
    # The whole reverse-patch window runs inside the shared `mutating_sources`
    # guard (#1128 fix-r1, review P0): the tree lock serializes any other
    # in-place runner of this tree, lib.rs is verified byte-for-byte against
    # HEAD before the window opens, and the exit restore writes the HEAD
    # snapshot back — a plain `git checkout --` here would also revert
    # changes this process never made.
    if git('cat-file', '-e', f'{BOXING_COMMIT}^{{commit}}', check=False).returncode:
        raise SystemExit(f'#1128 boxing commit {BOXING_COMMIT} is not reachable from this '
                         'history; set PDRS_BOXING_COMMIT to the moved anchor')
    try:
        with mutating_sources(root, [root / LIB_RS]):
            diff = git('diff', f'{BOXING_COMMIT}^', BOXING_COMMIT, '--', LIB_RS).stdout
            if b'Box<HashMap<JsString, usize>>' not in diff:
                raise SystemExit('#1128 boxing anchor vanished from the diff; refusing to '
                                 'measure a different change')
            applied = subprocess.run(['git', 'apply', '-R', '-'], input=diff, cwd=str(root),
                                     capture_output=True)
            if applied.returncode:
                raise SystemExit('#1128 reverse boxing patch no longer applies: '
                                 + applied.stderr.decode().strip())
            unboxed = (root / LIB_RS).read_text()
            if ('indices: HashMap<JsString, usize>,' not in unboxed
                    or 'indices: Box<HashMap<JsString, usize>>,' in unboxed):
                raise SystemExit('#1128 unboxed verification failed after patching')
            (out / 'pre.patch').write_text(diff.decode())
            exe_pre = build_release_test(env, 'build-pre.slots.log', out)
            result['workloads']['pre'] = measure_rust('pre', exe_pre, env, out, workloads, inputs)
    finally:
        marker.unlink()
    for label, row in result['workloads']['pre'].items():
        if row['timed']['inputFnv64'] != result['workloads']['post'][label]['timed']['inputFnv64']:
            raise SystemExit(f'#1128 variants disagree on the input digest of {label}')

    exe_restored = build_release_test(env, 'build-restored.slots.log', out)
    spot = None
    if 'small-8' in inputs:
        spot, _ = run_rust(exe_restored, env, out, label='restored-spot-small-8', kind='file',
                          reps=REPS, warmup=1, input_path=inputs['small-8'])
        post_line = result['workloads']['post']['small-8']['timed']
        if spot['valueBytes'] != post_line['valueBytes'] or \
                spot['prettyFnv64'] != post_line['prettyFnv64']:
            raise SystemExit('#1128 restored spot-check disagrees with the post measurement')
    # An unrun check must not read as a passed one: `null` and a passed line
    # are different states (#1128 fix-r1, review opus #3).
    result['restoredSpotCheck'] = spot if spot is not None else 'not-run (small-8 not selected)'

    ts_rows = {}
    for label, _kind, _target in workloads:
        if label not in inputs:
            continue
        timed, _ = run_ts(env, out, label=f'{label}-timed', input_path=inputs[label],
                          reps=REPS, warmup=1)
        dump = out / 'dumps' / f'ts-{label}.pretty' if label in DUMP_CLASSES else None
        _, rss = run_ts(env, out, label=f'{label}-rss', input_path=inputs[label],
                        reps=1, warmup=0, dump=dump)
        ts_rows[label] = {'timed': timed, 'rssBytes': rss}
        stringify = (f'{statistics.median(timed["stringifyMs"]):.1f}ms'
                     if timed['stringifyMs'] else timed['stringifyError'])
        print(f'#1128 ts {label}: parse={statistics.median(timed["parseMs"]):.1f}ms '
              f'stringify={stringify} rss={rss / MIB:.0f}MiB', flush=True)
    result['workloads']['ts'] = ts_rows
    usage, _ = run_wrapped([NODE, '-e', 'process.stdout.write("ok")'], env, 'ts-baseline', out)
    result['tsBaselineRssBytes'] = usage['rssWait4Bytes']

    digests = {}
    for label in sorted(DUMP_CLASSES):
        if label not in inputs:
            continue  # not selected in this run
        entry = {}
        for name in ('post', 'pre'):
            path = out / 'dumps' / f'{name}-{label}.pretty'
            # A selected dump class whose Rust-side dump is missing must fail
            # the run, not silently skip the comparison (#1128 fix-r1,
            # review opus #2): that would turn the hard byte check into a
            # vacuous pass.
            if not path.is_file():
                raise SystemExit(f'#1128 {name} pretty dump missing for {label}')
            entry[name] = hashlib.sha256(path.read_bytes()).hexdigest()
        ts_path = out / 'dumps' / f'ts-{label}.pretty'
        if ts_path.is_file():
            entry['ts'] = hashlib.sha256(ts_path.read_bytes()).hexdigest()
            entry['tsMatchesRust'] = entry['ts'] == entry['post']
        else:
            entry['tsMatchesRust'] = None  # Node could not stringify this workload.
        if entry['post'] != entry['pre']:
            raise SystemExit(f'#1128 pretty output differs between boxed and unboxed for {label}')
        digests[label] = entry
    result['prettySha256'] = digests

    (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'out': str(out), 'prettySha256': digests}, indent=2), flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
