"""Version binding for calibration; no cached result certifies changed apparatus."""
import hashlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def apparatus_hashes():
    files = sorted((ROOT / 'scripts/ui-server-parity').rglob('*.py'))
    files += sorted((ROOT / 'scripts/ui-server-parity').glob('*.mjs'))
    files += [ROOT / 'scripts/ui-server-parity' / name for name in ('recording.rs', 'scenarios.json', 'coverage.json')]
    files += sorted((ROOT / 'specs/ui-server-parity').glob('*.md'))
    files += [ROOT / 'scripts/exp008' / name for name in ('run.py', 'compare.py', 'enumerate_hook.mjs', 'preload.mjs', 'proxy/src/main.rs', 'proxy/Cargo.toml', 'proxy/Cargo.lock')]
    return {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in files}


def source_hashes(baseline, recorder):
    """Capture actual executable/code bytes, not a git revision or current replay files."""
    import shutil
    baseline = Path(baseline)
    files = []
    for name in ('src', 'dist', 'ui/server'):
        directory = baseline / name
        if not directory.is_dir():
            raise ValueError(f'missing baseline code tree: {directory}')
        files.extend(p for p in directory.rglob('*') if p.is_file())
    for name in ('package.json', 'pnpm-lock.yaml', 'ui/package.json'):
        path = baseline / name
        if not path.is_file():
            raise ValueError(f'missing baseline dependency manifest: {path}')
        files.append(path)
    digest = hashlib.sha256()
    for path in sorted(set(files)):
        digest.update(str(path.relative_to(baseline)).encode() + b'\0')
        digest.update(hashlib.sha256(path.read_bytes()).digest())
    node = shutil.which('node')
    if not node:
        raise ValueError('node executable missing')
    return {'node': hashlib.sha256(Path(node).read_bytes()).hexdigest(),
            'baseline': digest.hexdigest(),
            'recorder_proxy': hashlib.sha256(Path(recorder).read_bytes()).hexdigest()}
