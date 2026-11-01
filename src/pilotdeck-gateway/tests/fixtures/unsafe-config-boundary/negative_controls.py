#!/usr/bin/env python3
# anchor-selfcheck: unsupported environment/invocation-condition injection for one #448 guard test; no source replacement table
"""#448 negative controls: the unsafe guard must reject unseen cargo configuration.

``compiler_enforces_gateway_boundary`` now rejects cargo configuration that the
test process cwd cannot see: an out-of-tree ``.cargo/config.toml`` on the
invocation directory chain, ``--config`` overrides in the runner argv, the
toolchain-selection variables (RUSTDOC / RUSTC_BOOTSTRAP / a RUSTUP_TOOLCHAIN
that does not come from this repository's pinned rust-toolchain.toml), and a
CARGO executable that matches no cargo process in the run's ancestry (the
out-of-tree ``[env] force = true`` impersonation). Those conditions live in the
process environment and the invocation context, not in product source, so this
runner injects each condition and requires the guard to fail at its own
dedicated ``#448`` assertion; no source is replaced and the tree stays
untouched throughout.

The out-of-tree attack directory carries this checkout's pinned
rust-toolchain.toml (so the toolchain-file boundary legitimately passes and the
attack reaches its target) plus ``.cargo/config.toml`` with
``rustflags = ["--cfg", "pdrs_448_negative_control"]`` — the required
rustflags-class injection; cargo applies that configuration when invoked from
that directory, which is exactly why the guard must read the runner's context
through /proc instead of the test cwd.

Two positive controls frame the attacks (cargo-mode and direct execution of
the compiled guard binary, the mode the guard's own remedy messages direct a
human to), and a restored cargo-mode baseline at the end proves the out-of-tree
attack left no lasting effect on the shared target directory. Every scenario
runs before the summary; a scenario that cannot start, times out, fails to
compile, or fails at any other assertion is INVALID, a scenario the guard
accepts is SURVIVED, and any row outside its expected state makes the exit
code non-zero.

Usage (Linux only; the /proc invocation boundary is Linux-specific):
    CARGO_TARGET_DIR=... python3 negative_controls.py NEW_OUTPUT_DIR [scenario ...]
"""
import json
import os
import shlex
import shutil
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_process import new_output_directory, run_owned
from negative_selection import baseline_passed

if sys.platform != 'linux':
    raise SystemExit('#448 this control exercises the Linux /proc invocation boundary')

TEST = 'compiler_enforces_gateway_boundary'
root = Path(__file__).resolve().parents[5]
package = root / 'src/pilotdeck-gateway'
out = new_output_directory(Path(sys.argv[1]))
selected = sys.argv[2:]

env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
cargo = shutil.which('cargo')
if cargo is None:
    raise SystemExit('#448 cargo must be on PATH')

manifest = (root / 'rust-toolchain.toml').read_text()
channel = next(line.split('=', 1)[1].strip().strip('"')
               for line in manifest.splitlines()
               if line.strip().startswith('channel'))

# Direct execution must not inherit a toolchain-selection environment: in the
# supported direct mode there is no rustup shim in play, so the guard's
# toolchain boundary sees exactly the caller's exported variables.
TOOLCHAIN_ENV_KEYS = ('RUSTUP_TOOLCHAIN', 'RUSTUP_TOOLCHAIN_SOURCE', 'RUSTDOC', 'RUSTC_BOOTSTRAP')

# The helper process keeps the marker token in its own argv (what the guard
# scans) while spawning the guard with its proper arguments.
HELPER = 'import subprocess, sys; sys.exit(subprocess.call(sys.argv[2:]))'


def publish(path, text):
    pending = path.with_name(path.name + '.pending')
    pending.write_text(text)
    pending.replace(path)


def slots_note(label):
    """Record cargo slot availability; a full pool is advisory, not a wait.

    The host runbook says the slot script counts the host chain too and the
    PATH gate wrapper queues builds itself, so exit 1 is recorded and the run
    proceeds; every other failure is fatal.
    """
    result = run_owned(['bash', str(root / 'scripts/cargo-slots.sh')], timeout=60)
    (out / f'{label}.slots.log').write_text(result.stdout + result.stderr)
    if result.returncode not in (0, 1):
        result.check_returncode()
    return result.returncode


def build_guard():
    slots_note('build')
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--test', 'unsafe_boundary', '--no-run',
               '--message-format=json']
    result = run_owned(command, cwd=root, env=env, timeout=2400, idle_timeout=600)
    (out / 'build.log').write_text(result.stdout + result.stderr)
    if result.returncode != 0:
        raise RuntimeError('guard binary build failed')
    executables = set()
    for line in result.stdout.splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if (row.get('reason') == 'compiler-artifact'
                and row.get('target', {}).get('name') == 'unsafe_boundary'
                and row['target'].get('kind') == ['test'] and row.get('executable')):
            executables.add(row['executable'])
    if len(executables) != 1:
        raise RuntimeError('expected exactly one guard test binary')
    # The out-of-tree attack later rebuilds this target under injected
    # rustflags; run every direct scenario from this snapshot copy.
    binary = out / 'guard-binary'
    shutil.copyfile(executables.pop(), binary)
    binary.chmod(0o755)
    return binary


guard = build_guard()


def direct_environment(extra=None):
    child = {key: value for key, value in os.environ.items()
             if key not in TOOLCHAIN_ENV_KEYS}
    child['CARGO'] = cargo
    if extra:
        child.update(extra)
    return child


def run_direct(label, extra=None, launcher=None, timeout=900):
    command = launcher or [str(guard), '--exact', TEST]
    result = run_owned(command, cwd=package, env=direct_environment(extra), timeout=timeout)
    (out / f'{label}.log').write_text(result.stdout + result.stderr)
    return result


def run_cargo(label, *, cwd=None, timeout=3600):
    slots_note(label)
    command = ['cargo', 'test', '--offline', '--manifest-path', str(root / 'src/Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--test', 'unsafe_boundary', TEST,
               '--', '--exact']
    result = run_owned(command, cwd=cwd or root, env=env, timeout=timeout, idle_timeout=900)
    (out / f'{label}.log').write_text(result.stdout + result.stderr)
    return result


def verdict(result, marker, extra=None):
    """KILLED only at the dedicated #448 assertion; anything else is a failure."""
    log = result.stdout + result.stderr
    if result.returncode == 0 and baseline_passed(log, TEST):
        return 'SURVIVED', 'guard accepted the injected condition'
    if 'could not compile' in log:
        return 'INVALID', 'scenario did not compile'
    if result.returncode != 101 or not named_test_failed(log, TEST):
        return 'INVALID', 'the named guard test did not fail'
    evidence = panic_evidence(log, TEST)
    if marker not in evidence:
        return 'INVALID', 'failure was not at the dedicated #448 assertion'
    if extra is not None and not extra(evidence):
        return 'INVALID', 'dedicated assertion did not name the injected condition'
    return 'KILLED', 'guard failed at its dedicated #448 assertion'


def pass_verdict(result, description):
    log = result.stdout + result.stderr
    if result.returncode == 0 and baseline_passed(log, TEST):
        return 'PASS', description
    return 'INVALID', f'{description} was not reproduced: ' + next(
        (line.strip() for line in log.splitlines() if 'panicked at' in line), 'no guard failure line')


def scenario_baseline_cargo():
    return pass_verdict(run_cargo('baseline-cargo'),
                        'guard passed under a clean cargo invocation')


def scenario_baseline_direct():
    return pass_verdict(run_direct('baseline-direct'),
                        'guard passed when the compiled binary runs directly')


def scenario_env_rustdoc():
    result = run_direct('env-rustdoc', {'RUSTDOC': '/nonexistent-pdrs448-rustdoc'})
    return verdict(result, '#448 RUSTDOC can swap')


def scenario_env_rustc_bootstrap():
    result = run_direct('env-rustc-bootstrap', {'RUSTC_BOOTSTRAP': '1'})
    return verdict(result, '#448 RUSTC_BOOTSTRAP can swap')


def scenario_env_rustup_exported():
    # Even the right channel must be rejected without the toolchain-file source.
    result = run_direct('env-rustup-toolchain-exported', {'RUSTUP_TOOLCHAIN': channel})
    return verdict(result, 'does not come from this repository\'s pinned rust-toolchain.toml')


def scenario_env_rustup_foreign():
    result = run_direct('env-rustup-toolchain-foreign',
                        {'RUSTUP_TOOLCHAIN': 'stable', 'RUSTUP_TOOLCHAIN_SOURCE': 'environment'})
    return verdict(result, 'does not come from this repository\'s pinned rust-toolchain.toml')


# Rust's Debug format for the runner argv quotes each token with double quotes.
QUOTED = lambda token: f'"{token}"'


def scenario_cli_config_token():
    launcher = [sys.executable, '-c', HELPER, '--config', str(guard), '--exact', TEST]
    result = run_direct('cli-config-token', launcher=launcher)
    return verdict(result, '#448 cargo --config overrides',
                   extra=lambda evidence: QUOTED('--config') in evidence)


def scenario_cli_config_joined():
    launcher = [sys.executable, '-c', HELPER, '--config=build.rustflags=[]',
                str(guard), '--exact', TEST]
    result = run_direct('cli-config-joined', launcher=launcher)
    return verdict(result, '#448 cargo --config overrides',
                   extra=lambda evidence: QUOTED('--config=build.rustflags=[]') in evidence)


def scenario_cargo_identity_fake():
    bash = shutil.which('bash')
    if bash is None:
        return 'INVALID', 'bash is unavailable for the impersonation runner'
    directory = out / 'fake-runner'
    directory.mkdir()
    fake = directory / 'cargo'
    shutil.copyfile(bash, fake)
    fake.chmod(0o755)
    # A trailing statement so bash forks the guard instead of exec-ing it: the
    # guard's parent must be a process whose image is named cargo but is not
    # the CARGO the guard environment points at. The guard's own exit status
    # still reaches the runner.
    script = (f'{shlex.quote(str(guard))} --exact {TEST}\n'
              'status=$?\n/bin/true\nexit $status')
    launcher = [str(fake), '-c', script]
    result = run_direct('cargo-identity-fake', launcher=launcher)
    # The assertion names the CARGO value it compared against the runner image,
    # so the injected CARGO is the binding evidence, not the fake runner's path.
    return verdict(result, '#448 the CARGO executable in the guard environment',
                   extra=lambda evidence: cargo in evidence)


def scenario_cargo_out_of_tree_config():
    attack = Path(tempfile.mkdtemp(prefix='pdrs448-out-of-tree-'))
    try:
        (attack / 'rust-toolchain.toml').write_text((root / 'rust-toolchain.toml').read_text())
        (attack / '.cargo').mkdir()
        config = attack / '.cargo' / 'config.toml'
        config.write_text('[build]\nrustflags = ["--cfg", "pdrs_448_negative_control"]\n')
        (out / 'out-of-tree-config.toml').write_text(config.read_text())
        result = run_cargo('cargo-out-of-tree-config', cwd=attack)
        return verdict(result, '#448 cargo invocation directory Cargo configuration',
                       extra=lambda evidence: str(config) in evidence)
    finally:
        shutil.rmtree(attack, ignore_errors=True)


def scenario_baseline_restored():
    return pass_verdict(run_cargo('baseline-cargo-restored'),
                        'guard passed again after the out-of-tree attack')


SCENARIOS = {
    'baseline-cargo': scenario_baseline_cargo,
    'baseline-direct': scenario_baseline_direct,
    'env-rustdoc': scenario_env_rustdoc,
    'env-rustc-bootstrap': scenario_env_rustc_bootstrap,
    'env-rustup-toolchain-exported': scenario_env_rustup_exported,
    'env-rustup-toolchain-foreign': scenario_env_rustup_foreign,
    'cli-config-token': scenario_cli_config_token,
    'cli-config-joined': scenario_cli_config_joined,
    'cargo-identity-fake': scenario_cargo_identity_fake,
    'cargo-out-of-tree-config': scenario_cargo_out_of_tree_config,
    'baseline-cargo-restored': scenario_baseline_restored,
}

order = [name for name in SCENARIOS if not selected or name in selected]
if selected and len(order) != len(selected):
    raise SystemExit('unknown scenario name')

summary = []
for name in order:
    started = time.monotonic()
    try:
        status, reason = SCENARIOS[name]()
    except Exception as error:
        status, reason = 'INVALID', f'{type(error).__name__}: {error}'
    row = {'name': name,
           'expected': 'PASS' if name.startswith('baseline') else 'KILLED',
           'status': status, 'reason': reason,
           'seconds': round(time.monotonic() - started, 1)}
    summary.append(row)
    publish(out / 'summary.json', json.dumps(summary, indent=2, ensure_ascii=False) + '\n')
    print(f"{name}: {status} ({row['seconds']}s): {reason}", flush=True)

counts = {state: sum(row['status'] == state for row in summary)
          for state in ('PASS', 'KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
failed = [row['name'] for row in summary if row['status'] != row['expected']]
if failed:
    print(f"unexpected results: {', '.join(failed)}", file=sys.stderr)
    raise SystemExit(1)
print(f"{len(summary)}/{len(summary)} scenarios in their expected state; "
      'no source was modified', flush=True)
