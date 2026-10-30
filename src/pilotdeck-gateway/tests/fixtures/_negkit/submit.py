# anchor-selfcheck: unsupported M6 submit adapter, tables live in the two fixture runners
"""Shared validation and guarded execution for the first M6 migrations."""
from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from _negkit import contract as C

BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'


@dataclass(frozen=True)
class Mutation:
    binding: C.Case
    path: Path | None = None
    before: str | None = None
    after: str | None = None
    scenario: str | None = None
    capability: str | None = None


def baseline_environment(environ):
    env = C.environment(environ)
    if 'PDRS_NEGATIVE_BASELINE' in env:
        raise ValueError('#782 PDRS_NEGATIVE_BASELINE is forbidden; Rust source must equal HEAD')
    root = Path(env['PDRS_BASELINE'])
    result = run_owned(['git', '-C', str(root), 'rev-parse', 'HEAD'], timeout=10)
    if result.returncode != 0 or result.stdout.strip() != BASELINE_COMMIT:
        raise ValueError('#782 PDRS_BASELINE must be the fixed TS baseline commit')
    env['CARGO_NET_OFFLINE'] = 'true'
    for key in ('PDRS_SUBMIT_CASE', 'PDRS_SUBMIT_CAPABILITY'):
        env.pop(key, None)
    return env


def submit_marker(oracle, case, index, step):
    rows = [row for row in oracle if row.get('name') == case]
    if (len(rows) != 1 or type(index) is not int or index < 0
            or index >= len(rows[0]['steps']) or rows[0]['steps'][index] != step):
        raise ValueError('#935 submit marker must name an existing oracle case/index/step')
    return f'#813 {case}: step {index} "{step}": submit scenario must match'


def validate_submit_marker(oracle, marker):
    match = re.fullmatch(r'#813 (.+): step ([0-9]+) "([^"]+)": submit scenario must match', marker)
    if match is None or submit_marker(oracle, match[1], int(match[2]), match[3]) != marker:
        raise ValueError('#935 malformed or unbound submit marker')


def unwrap_site(workspace, path, before, after, original):
    """The replaced unwrap begins at the same byte/line/column (#835)."""
    if (not isinstance(before, str) or not before.startswith('unwrap_or_else(')
            or not isinstance(after, str) or not after.startswith('unwrap()')):
        raise ValueError('#835 unwrap must start at both replacement spans')
    text = original.decode()
    if text.count(before) != 1:
        raise ValueError('#835 unwrap anchor must occur exactly once')
    relative = path.resolve().relative_to(workspace.resolve())
    head = text[:text.index(before)]
    return f'{relative}:{head.count(chr(10)) + 1}:{len(head.rsplit(chr(10), 1)[-1]) + 1}'


def legacy_payload(marker):
    """Recorded complete first-line shapes; never a free substring fallback."""
    equality = ('changed SDK config must dirty the cached handle',
                'UTF-16 busy status message', 'a prepared lease must be released exactly once')
    if marker in equality or marker == 'failed: message':
        message = 'message' if marker == 'failed: message' else marker
        return re.escape('assertion `left == right` failed: ' + message)
    if marker in tuple(name + ': explicit capability refusal' for name in (
            'snapshot-recorder-unavailable', 'snapshot-cwd-unavailable', 'cwd-host-unavailable')):
        return re.escape('assertion failed: ' + marker)
    suffixes = {
        'lease_release must precede after_turn': r' \(lease@[0-9]+ after@[0-9]+\): ',
        'attachments must record lease_release and after_turn': r' \(lease@(?:None|Some\([0-9]+\)) after@(?:None|Some\([0-9]+\))\): ',
        'lease_release recorded without an attachment lease': r' \(lease@[0-9]+\): ',
    }
    if marker in suffixes:
        prefix = 'lease-absent: ' if marker.startswith('lease_release recorded') else r'lease-(?:normal|pump-failed|timeout): '
        return prefix + re.escape(marker) + suffixes[marker] + r'\[(?:"[^"\r\n]*"(?:, )?)*\]'
    raise ValueError('#935 unregistered legacy assertion payload')


def test_inventory(path, prefix):
    """Positive controls include every current non-ignored test in the module."""
    text = path.read_text()
    names = re.findall(r'#\[(?:tokio::)?test(?:\([^\n]*\))?\]\s*(?:async )?fn (\w+)\(', text)
    if not names:
        raise ValueError('#1110 positive test inventory is empty')
    return tuple(prefix + '::' + name for name in names)


def check(entries, oracle):
    C.select_cases([entry.binding for entry in entries], None, oracle)
    for entry in entries:
        if entry.path is not None:
            if not entry.before or entry.path.read_text().count(entry.before) != 1:
                raise ValueError(f'{entry.binding.mutation}: stale/nonunique mutation anchor')
            if entry.before == entry.after:
                raise ValueError(f'{entry.binding.mutation}: mutation is a no-op')
    return len(entries)


def run(entries, oracle, repo, output, selected, environ):
    """Compile every mutation, run exact positives/red/restored positives.

    Only the existing HEAD guard supplies restore bytes. Ordinary failures
    remain per-case INVALID; the contract owns cancellation and publication.
    """
    check(entries, oracle)
    env = baseline_environment(environ)
    positives = [entry.binding.mutation for entry in entries if entry.binding.kind == 'positive']
    if selected:
        if len(set(selected)) != len(selected):
            raise ValueError('duplicate selection')
        selected = positives + [name for name in selected if name not in positives]
    else:
        selected = None
    by_name = {entry.binding.mutation: entry for entry in entries}
    paths = sorted({entry.path for entry in entries if entry.path is not None})
    output = Path(output).resolve()
    snapshots = {}
    failed_baselines = set()

    @contextmanager
    def guard():
        with mutating_sources(repo, paths) as saved:
            snapshots.update(saved)
            hashes = {str(path.relative_to(repo)): hashlib.sha256(data).hexdigest()
                      for path, data in saved.items()}
            (output / 'original-sha256.json').write_text(json.dumps(hashes, indent=2) + '\n')
            yield
        (output / 'restored-sha256.json').write_text(json.dumps(hashes, indent=2) + '\n')

    def cargo(name, phase, test, run_env, no_run=False):
        slots = wait_for_slot(repo / 'scripts/cargo-slots.sh', timeout=900)
        command = ['cargo', 'test', '--offline', '--manifest-path', str(repo / 'src/Cargo.toml'),
                   '-p', 'pilotdeck-gateway', '--lib', test]
        command += ['--no-run'] if no_run else ['--', '--exact', '--show-output']
        log = output / f'{name}-{phase}.log'
        with log.open('x') as stream:
            stream.write('COMMAND ' + ' '.join(command) + '\n' + slots.stdout + slots.stderr)
            try:
                result = run_owned(command, cwd=repo, env=run_env, timeout=240)
            except subprocess.TimeoutExpired as error:
                stream.write(str(error) + '\n' + str(error.stdout or '') + str(error.stderr or ''))
                raise
            stream.write(result.stdout + result.stderr + f'\nexit={result.returncode}\n')
        return result

    def require_positive(result, binding):
        positive = C.Case(binding.mutation, binding.test, '#1110 positive', kind='positive',
                          positive_tests=(binding.test,))
        if C.classify(result, positive)[0] != 'PASS':
            raise ValueError('named positive control failed')

    def execute(binding, run_env):
        entry = by_name[binding.mutation]
        if any(path.read_bytes() != data for path, data in snapshots.items()):
            raise RuntimeError('source changed outside this run')
        run_env = dict(run_env)
        if entry.scenario:
            run_env['PDRS_SUBMIT_CASE'] = entry.scenario
        if entry.capability:
            run_env['PDRS_SUBMIT_CAPABILITY'] = entry.capability
        if binding.kind == 'positive':
            try:
                results = [cargo(binding.mutation, f'positive-{i}', test, run_env)
                           for i, test in enumerate(binding.positive_tests)]
            except BaseException:
                failed_baselines.update(binding.positive_tests)
                raise
            result = subprocess.CompletedProcess([], max(abs(r.returncode) for r in results),
                                                 '\n'.join(r.stdout for r in results),
                                                 '\n'.join(r.stderr for r in results))
            if C.classify(result, binding)[0] != 'PASS':
                failed_baselines.update(binding.positive_tests)
            return result
        original = snapshots[entry.path]
        modified = original.decode().replace(entry.before, entry.after, 1).encode()
        positive = cargo(binding.mutation, 'positive', binding.test, run_env)
        failure = None
        try:
            entry.path.write_bytes(modified)
            compiled = cargo(binding.mutation, 'compile', binding.test, run_env, no_run=True)
            if compiled.returncode != 0:
                raise ValueError('mutant did not compile')
            result = cargo(binding.mutation, 'mutant', binding.test, run_env)
        except Exception as error:
            failure = error
        finally:
            if entry.path.read_bytes() != modified:
                raise RuntimeError('concurrent source edit; refusing per-case overwrite')
            entry.path.write_bytes(original)
        restored = cargo(binding.mutation, 'restored', binding.test, run_env)
        require_positive(restored, binding)
        if failure is not None:
            raise failure
        require_positive(positive, binding)
        if binding.test in failed_baselines:
            raise ValueError('baseline failed; mutant cannot be KILLED')
        return result

    result = C.run([entry.binding for entry in entries], output, guard=guard, execute=execute,
                   oracle=oracle, selected=selected, environ=env)
    print(json.dumps(result.document['counts']), flush=True)
    if result.error:
        print(result.error, file=sys.stderr)
    return result.code
