"""Deterministic Seatbelt policy, shared by recorder and evidence validator."""
import json
from pathlib import Path

from common import SCRIPT


def sandbox_profile(runtime, baseline, binary, side):
    if side not in ('ts', 'rust'):
        raise ValueError('unknown sandbox side')
    runtime, baseline, binary = Path(runtime), Path(baseline), Path(binary)
    # Rust has no Homebrew or baseline access. System libraries are required by
    # native macOS executables; an unsupported external dylib fails closed.
    read_dirs = [runtime, Path('/System'), Path('/usr/lib'), Path('/Library/Apple')]
    # dyld 启动任何 Mach-O 都要读根目录对象（literal，非子树）；TS/Rust 两侧都需要（真实宿主实测：缺它即 SIGABRT）
    read_files = [binary, Path('/'), Path('/dev/null'), Path('/dev/urandom'), Path('/dev/random')]
    if side == 'rust':
        # Staged release layout <stage>/bin/pilotdeck + <stage>/skills (run_round.stage_rust);
        # the binary resolves bundled skills from <exe_dir>/../skills, never the baseline.
        if binary.parent.name != 'bin':
            raise ValueError('Rust binary must be the staged <stage>/bin copy')
        read_dirs.append(binary.parent.parent / 'skills')
    if side == 'ts':
        read_dirs += [baseline / name for name in ('dist', 'node_modules', 'packages', 'skills')]
        read_dirs += [Path('/opt/homebrew/Cellar'), Path('/opt/homebrew/opt')]
        read_files += [baseline / 'package.json', SCRIPT / 'isolation_probe.mjs']
    # Metadata is limited to readable objects and their ancestor directory
    # objects, not ancestor subtrees. No global stat/exists or directory service.
    ancestors = {parent for path in read_dirs + read_files for parent in path.parents}
    def item(kind, path):
        # Inputs are already canonicalized by the caller. Do not inspect the
        # current filesystem while validating archived policy text.
        return f'({kind} {json.dumps(str(path))})'
    readable = ' '.join(item('subpath', p) for p in read_dirs)
    readable += ' ' + ' '.join(item('literal', p) for p in read_files)
    rules = ['(version 1)', '(deny default)', '(allow sysctl-read)',
             '(allow file-read-metadata ' + readable + ' ' +
             ' '.join(item('literal', p) for p in sorted(ancestors)) + ')',
             '(allow file-read-data file-map-executable ' + readable + ')',
             '(allow file-write* ' + item('subpath', runtime) + ')',
             '(allow file-read* file-write* (literal "/dev/null") (literal "/dev/stdin") (literal "/dev/stdout") (literal "/dev/stderr"))',
             '(allow process-exec ' + item('literal', binary) + ')',
             '(deny network*)', '(deny process-fork)']
    return '\n'.join(rules) + '\n'
