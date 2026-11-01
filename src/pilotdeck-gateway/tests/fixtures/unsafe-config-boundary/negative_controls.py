#!/usr/bin/env python3
# anchor-selfcheck: unsupported environment/invocation-condition injection for one #448 guard test; no source replacement table
"""#448 negative controls: the unsafe guard must reject unseen cargo configuration.

``compiler_enforces_gateway_boundary`` now rejects cargo configuration that the
test process cwd cannot see: an out-of-tree ``.cargo/config.toml`` on the
invocation directory chain, ``--config`` overrides in the runner argv (the
direct parent's and every live cargo ancestor's — cargo exec-replaces into
external subcommands, so a real ``cargo nextest run`` has no live cargo; the
constructed nextest layout keeps a cargo grandparent alive), the
toolchain-selection variables (RUSTDOC / RUSTC_BOOTSTRAP /
a RUSTUP_TOOLCHAIN that does not come from this repository's pinned
rust-toolchain.toml — accepted only as the pinned channel or that channel plus
the host triple the running cargo reports), and a CARGO executable that matches
no cargo process in the run's ancestry (the out-of-tree ``[env] force = true``
impersonation). Those conditions live in the process environment and the
invocation context, not in product source, so this runner injects each
condition and requires the guard to fail at its own dedicated ``#448``
assertion; no source is replaced and the tree stays untouched throughout.

The out-of-tree attack directory carries this checkout's pinned
rust-toolchain.toml (so the toolchain-file boundary legitimately passes and the
attack reaches its target) plus a ``.cargo/config`` or ``.cargo/config.toml``
with ``rustflags = ["--cfg", "pdrs_448_negative_control"]`` — the required
rustflags-class injection (each name gets its own scenario, pinning the guard
helper's two-entry name list); cargo applies that configuration when invoked
from that directory, which is exactly why the guard must read the runner's
context through /proc instead of the test cwd.

Positive controls frame the attacks: cargo-mode and direct execution of the
compiled guard binary (the mode the guard's own remedy messages direct a human
to), the toolchain-file allow branch (pinned channel plus host triple), and
the two-layer nextest ancestry with a matching cargo grandparent; a restored
cargo-mode baseline at the end proves the out-of-tree attack left no lasting
effect on the shared target directory. Every scenario runs before the summary;
a scenario that cannot start, times out, fails to compile, or fails at any
other assertion is INVALID, a scenario the guard accepts is SURVIVED, and any
row outside its expected state makes the exit code non-zero.

Usage (Linux only; the /proc invocation boundary is Linux-specific, and on
other platforms the guard itself fails closed unless
PDRS_UNSAFE_BOUNDARY_ALLOW_NON_LINUX=1 is set):
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

# Mirror the Rust guard's parser (section() + strip_prefix("channel = ") +
# quote stripping) instead of a tolerant split: a differently formatted
# rust-toolchain.toml must fail loudly on both sides the same way, not parse
# here while the guard fails closed on it.
def pinned_channel():
    manifest = (root / 'rust-toolchain.toml').read_text()
    lines = [line.split('#', 1)[0].strip() for line in manifest.splitlines()]
    try:
        start = lines.index('[toolchain]') + 1
    except ValueError:
        raise SystemExit('#448 rust-toolchain.toml has no [toolchain] section')
    for line in lines[start:]:
        if line.startswith('['):
            break
        if line.startswith('channel = "'):
            value = line[len('channel = "'):]
            if not value.endswith('"'):
                raise SystemExit(f'#448 unquoted toolchain channel: {line}')
            return value[:-1]
    raise SystemExit('#448 pinned toolchain channel is not declared')


channel = pinned_channel()

# The host triple the guard derives from `$CARGO -vV`; the from-file positive
# control must name the same value the guard accepts.
host = None
for line in run_owned([cargo, '-vV'], timeout=120).stdout.splitlines():
    if line.startswith('host: '):
        host = line[len('host: '):].strip()
if not host:
    raise SystemExit('#448 the cargo on PATH did not report its host triple')

# Direct execution must not inherit a toolchain-selection environment: in the
# supported direct mode there is no rustup shim in play, so the guard's
# toolchain boundary sees exactly the caller's exported variables.
TOOLCHAIN_ENV_KEYS = ('RUSTUP_TOOLCHAIN', 'RUSTUP_TOOLCHAIN_SOURCE', 'RUSTDOC', 'RUSTC_BOOTSTRAP')

# The real toolchain cargo (not the PATH wrapper or the rustup shim): the
# guard's own `cargo metadata` must still succeed under the injected
# RUSTUP_TOOLCHAIN values, which the shim would refuse to resolve, and the
# nextest-layout grandparent must be an ELF that can answer it.
rustup = shutil.which('rustup')
toolchain_cargo = None
if rustup:
    probe_env = {key: value for key, value in os.environ.items()
                 if key not in TOOLCHAIN_ENV_KEYS}
    probe = run_owned([rustup, 'which', 'cargo'], env=probe_env, cwd=root, timeout=120)
    resolved = probe.stdout.strip()
    if probe.returncode == 0 and resolved:
        toolchain_cargo = Path(resolved)
if toolchain_cargo is None:
    raise SystemExit('#448 rustup is required to locate the real toolchain cargo')

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
    # `env` is what rustup 1.29.2 actually exports for an exported
    # RUSTUP_TOOLCHAIN (probed on cb8; `cli` for +toolchain).
    result = run_direct('env-rustup-toolchain-foreign',
                        {'RUSTUP_TOOLCHAIN': 'stable', 'RUSTUP_TOOLCHAIN_SOURCE': 'env'})
    return verdict(result, 'does not come from this repository\'s pinned rust-toolchain.toml')


def toolchain_first_environment(extra):
    """Direct-run environment with the real toolchain bin in front of PATH.

    The rustup shims resolve RUSTUP_TOOLCHAIN themselves, so an injected
    (uninstalled) name breaks `rustc -vV` inside the guard's own cargo
    metadata. The attack keeps the whole invocation working by pointing CARGO
    at the real toolchain cargo and resolving tools from the toolchain bin
    directory first, exactly like an installed linked toolchain would.
    """
    environment = {'PATH': f'{toolchain_cargo.parent}{os.pathsep}{os.environ["PATH"]}'}
    environment.update(extra)
    return environment


def scenario_env_rustup_from_file():
    # The normal flow's own shape: the pinned channel plus the host triple the
    # running cargo reports, with the toolchain-file source. CARGO points at
    # the real toolchain cargo, as it does in the cargo-invoked flow, so the
    # guard's own `cargo metadata` runs under the injected toolchain name.
    result = run_direct('env-rustup-toolchain-from-file',
                        toolchain_first_environment({
                            'CARGO': str(toolchain_cargo),
                            'RUSTUP_TOOLCHAIN': f'{channel}-{host}',
                            'RUSTUP_TOOLCHAIN_SOURCE': 'toolchain-file'}))
    return pass_verdict(result, 'guard passed for the pinned channel plus host '
                                'triple with the toolchain-file source')


def scenario_env_rustup_linked():
    # `rustup toolchain link 1.99-<fake> /tmp/fake` names a linked toolchain
    # whose name is neither the channel nor channel-<real host triple>.
    result = run_direct('env-rustup-toolchain-linked',
                        toolchain_first_environment({
                            'CARGO': str(toolchain_cargo),
                            'RUSTUP_TOOLCHAIN': f'{channel}-pdrs448-not-a-host-triple',
                            'RUSTUP_TOOLCHAIN_SOURCE': 'toolchain-file'}))
    return verdict(result, 'does not come from this repository\'s pinned rust-toolchain.toml',
                   extra=lambda evidence: 'pdrs448-not-a-host-triple' in evidence)


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


def out_of_tree_config_attack(label, name):
    """Attack from an invocation directory the test cwd cannot see.

    The attack directory carries this checkout's pinned rust-toolchain.toml
    (so the toolchain-file boundary legitimately passes and the attack
    reaches its target) plus a `.cargo/<name>` rustflags injection; the
    guard must read the runner's context through /proc instead.
    """
    attack = Path(tempfile.mkdtemp(prefix='pdrs448-out-of-tree-'))
    try:
        (attack / 'rust-toolchain.toml').write_text((root / 'rust-toolchain.toml').read_text())
        (attack / '.cargo').mkdir()
        config = attack / '.cargo' / name
        config.write_text('[build]\nrustflags = ["--cfg", "pdrs_448_negative_control"]\n')
        (out / f'{label}-attack.toml').write_text(config.read_text())
        result = run_cargo(label, cwd=attack)
    finally:
        shutil.rmtree(attack, ignore_errors=True)
    return verdict(result, '#448 cargo invocation directory Cargo configuration',
                   extra=lambda evidence: str(config) in evidence)


def scenario_cargo_out_of_tree_config():
    return out_of_tree_config_attack('cargo-out-of-tree-config', 'config.toml')


def scenario_cargo_out_of_tree_config_bare():
    # The guard's helper rejects both `.cargo/config` and `.cargo/config.toml`;
    # the bare name needs its own control so shrinking that name list cannot
    # pass unnoticed.
    return out_of_tree_config_attack('cargo-out-of-tree-config-bare', 'config')


# The `cargo nextest run` ancestry: the guard's parent is cargo-nextest and a
# cargo process above it, so its argv (and any `--config` overrides in it) is
# only reachable through the ancestor walk. cb8 has no nextest installed, and
# `cargo nextest` is an exec'd external subcommand anyway, so the layout is
# reproduced where cargo provably spawns a child: a copy of the real cargo
# binary (an ELF named cargo that can also answer the guard's own
# `cargo metadata` and `-vV` calls) running `cargo test` on a one-target
# sleeper whose harness-free test binary is named cargo-nextest and spawns
# the guard — cargo waits on it as the grandparent, the sleeper is the
# parent, and no exec optimization collapses the two layers.
def nextest_layout_runner():
    if toolchain_cargo is None:
        raise RuntimeError('rustup is unavailable to locate the real toolchain cargo')
    directory = out / 'nextest-layout'
    if not directory.is_dir():
        directory.mkdir()
        fake_cargo = directory / 'cargo'
        shutil.copyfile(toolchain_cargo, fake_cargo)
        fake_cargo.chmod(0o755)
        sleeper = directory / 'sleeper'
        (sleeper / 'src').mkdir(parents=True)
        (sleeper / 'Cargo.toml').write_text(
            '[package]\nname = "pdrs448-nextest-layout"\nversion = "0.0.0"\n'
            'edition = "2021"\n\n[[test]]\nname = "cargo-nextest"\n'
            'path = "src/cargo_nextest.rs"\nharness = false\n')
        # The sleeper hands the guard back the shared target directory it was
        # built in (only the grandparent's sleeper build uses the private one)
        # and the gateway package root as cwd: cargo test runs the test binary
        # with the sleeper package root as its cwd, which is outside the
        # workspace manifest tree the guard requires.
        shared_target = os.environ.get('CARGO_TARGET_DIR', '')

        def rust_string(value):
            return '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'

        (sleeper / 'src' / 'cargo_nextest.rs').write_text(
            'fn main() {\n'
            f'    let status = std::process::Command::new({rust_string(str(guard))})\n'
            f'        .args(["--exact", {rust_string(TEST)}])\n'
            f'        .env("CARGO_TARGET_DIR", {rust_string(shared_target)})\n'
            f'        .current_dir({rust_string(str(package))})\n'
            '        .status()\n'
            '        .expect("#448 sleeper must run the guard");\n'
            '    std::process::exit(status.code().unwrap_or(1));\n'
            '}\n')
    fake_cargo = directory / 'cargo'
    manifest = directory / 'sleeper' / 'Cargo.toml'
    # The sleeper builds into its own target directory so the grandparent
    # never touches the shared target the guard itself was built in.
    environment = {'CARGO': str(fake_cargo),
                   'CARGO_TARGET_DIR': str(directory / 'target')}
    return fake_cargo, manifest, environment


def scenario_nextest_layout_baseline():
    fake_cargo, manifest, environment = nextest_layout_runner()
    launcher = [str(fake_cargo), 'test', '--manifest-path', str(manifest), '--offline']
    result = run_direct('nextest-layout-baseline', environment, launcher=launcher,
                        timeout=1200)
    return pass_verdict(result, 'guard passed under a cargo-nextest parent and a '
                                'matching cargo grandparent')


def scenario_nextest_layout_config():
    # The CI shape: cargo --config <override> <subcommand> ... — the override is
    # in the grandparent's argv, which the direct-parent scan cannot see. The
    # injected value is harmless so the pre-fix guard can be shown accepting
    # the whole run; the token rejection itself is value-agnostic.
    fake_cargo, manifest, environment = nextest_layout_runner()
    launcher = [str(fake_cargo), '--config', 'build.jobs=7',
                'test', '--manifest-path', str(manifest), '--offline']
    result = run_direct('nextest-layout-config', environment, launcher=launcher,
                        timeout=1200)
    return verdict(result, '#448 cargo --config overrides',
                   extra=lambda evidence: QUOTED('--config') in evidence)


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
    'env-rustup-toolchain-from-file': scenario_env_rustup_from_file,
    'env-rustup-toolchain-linked': scenario_env_rustup_linked,
    'cli-config-token': scenario_cli_config_token,
    'cli-config-joined': scenario_cli_config_joined,
    'cargo-identity-fake': scenario_cargo_identity_fake,
    'cargo-out-of-tree-config': scenario_cargo_out_of_tree_config,
    'cargo-out-of-tree-config-bare': scenario_cargo_out_of_tree_config_bare,
    'nextest-layout-baseline': scenario_nextest_layout_baseline,
    'nextest-layout-config': scenario_nextest_layout_config,
    'baseline-cargo-restored': scenario_baseline_restored,
}

# Rows expected to end PASS (positive controls); every other row must die at
# its dedicated #448 assertion.
PASS_SCENARIOS = frozenset({
    'baseline-cargo',
    'baseline-direct',
    'env-rustup-toolchain-from-file',
    'nextest-layout-baseline',
    'baseline-cargo-restored',
})

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
           'expected': 'PASS' if name in PASS_SCENARIOS else 'KILLED',
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
