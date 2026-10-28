# anchor-selfcheck: unsupported shared rustc/compiler-artifact selection, no source mutations
"""Select a rustc harness compiler and externs from the caller's Cargo build."""
import json
from pathlib import Path
import shutil


HARNESS_CRATES = ('pilotdeck_protocol', 'sha2', 'uuid', 'serde_json')


def cargo_compiler(env, run):
    """`run(command, label)` executes in the repo workspace and retains output.

    Do not resolve symlinks here: ~/.cargo/bin/cargo and rustc can both be
    rustup proxies, whose executable names select the requested tool.
    Cargo's sysroot query also observes its build.rustc configuration.
    """
    cargo = shutil.which(env.get('CARGO') or 'cargo', path=env.get('PATH'))
    if not cargo:
        raise RuntimeError('#743 toolchain: caller Cargo executable not found')
    cargo = str(Path(cargo).absolute())
    rustc = env.get('RUSTC') or str(Path(cargo).with_name('rustc'))
    query = run([cargo, 'rustc', '--offline', '-p', 'pilotdeck-protocol',
                 '--lib', '--', '--print', 'sysroot'], 'cargo-sysroot')
    query.check_returncode()
    sysroot = Path(query.stdout.strip())
    if not query.stdout.strip() or not sysroot.is_absolute():
        raise RuntimeError('#743 toolchain: Cargo returned no absolute sysroot')
    actual = run([str(sysroot / 'bin/rustc'), '-V'], 'cargo-rustc-version')
    actual.check_returncode()
    harness = run([rustc, '-V'], 'harness-rustc-version')
    harness.check_returncode()
    if not actual.stdout.startswith('rustc ') or actual.stdout != harness.stdout:
        raise RuntimeError('#743 toolchain mismatch: Cargo compiler '
                           f'{actual.stdout.strip()!r}; harness compiler '
                           f'{harness.stdout.strip()!r}')
    return cargo, rustc


def cargo_externs(output, target):
    """Never choose an unreported/stale rlib by filename or modification time."""
    deps = target / 'debug/deps'
    found = {crate: set() for crate in HARNESS_CRATES}
    for line in output.splitlines():
        message = json.loads(line)
        if message.get('reason') != 'compiler-artifact':
            continue
        crate = message['target']['name']
        if crate in found:
            for filename in message['filenames']:
                path = Path(filename)
                if path.suffix == '.rlib':
                    if path.parent != deps or not path.is_file():
                        raise RuntimeError(f'#743 extern path outside caller target or missing: {path}')
                    found[crate].add(path)
    externs = []
    for crate, paths in found.items():
        if len(paths) != 1:
            raise RuntimeError(f'#743 extern selection: {crate} has {len(paths)} Cargo artifacts')
        externs.extend(['--extern', f'{crate}={next(iter(paths))}'])
    return externs
