"""#1128 HEAD provenance, shared by measurement and harness controls.

Port of sdk-decode-budget's #983 r9x tree_matches_head. Protocol has no
workspace path dependencies; CLI is included because object_density.rs
reads its main.rs with include_str!. Keep the former measurement guard's
toolchain inputs too. This fixture-local module avoids importing a runner
and its mutation table merely to check provenance.
"""
import subprocess


BUILD_INPUTS = ('src/pilotdeck-protocol', 'src/pilotdeck-cli', 'src/Cargo.toml',
                'src/Cargo.lock', '.cargo', 'src/.cargo',
                'rust-toolchain', 'rust-toolchain.toml')
LIB_RS = 'src/pilotdeck-protocol/src/lib.rs'


def tree_matches_head(root, allow=()):
    """Reject tracked drift, hidden index flags and untracked build inputs.

    Only the pre window's intentional lib.rs worktree edit can be allowed;
    staged changes and hidden index flags are never exempted. Git errors
    fail closed, including status diagnostics and inventory commands.
    """
    if any(path != LIB_RS for path in allow):
        raise ValueError('#1128 only pre-window lib.rs may differ from HEAD')
    command = ['git', '-C', str(root)]
    staged = subprocess.run([*command, 'diff', '--cached', '--quiet', 'HEAD'])
    unstaged = subprocess.run([*command, 'diff', '--quiet', 'HEAD', '--', '.',
                              *(f':(exclude,literal){path}' for path in allow)])
    if staged.returncode != 0 or unstaged.returncode != 0:
        listing = subprocess.run([*command, 'status', '--porcelain', '--untracked-files=no'],
                                 check=True, capture_output=True, text=True).stdout
        raise RuntimeError('#1128 tracked tree differs from HEAD before build:\n' + listing)
    # #983 r9x: hidden index flags let modified files look clean to both diffs.
    flags = subprocess.run([*command, 'ls-files', '-v'],
                           check=True, capture_output=True, text=True).stdout.splitlines()
    hidden = [line for line in flags if line[:1].islower() or line[:1] == 'S']
    if hidden:
        raise RuntimeError('#1128 index has assume-unchanged / skip-worktree entries:\n'
                           + '\n'.join(hidden[:20]))
    untracked = subprocess.run([*command, 'ls-files', '--others', '--exclude-standard', '-z', '--',
                                *BUILD_INPUTS], check=True, capture_output=True).stdout.decode().split('\0')
    untracked = [name for name in untracked if name]
    for config in (root / '.cargo', root / 'src/.cargo'):
        if config.exists():
            untracked.append(str(config.relative_to(root)) + ' (cargo config dir present)')
    if untracked:
        raise RuntimeError('#1128 untracked build inputs present:\n' + '\n'.join(untracked[:20]))
