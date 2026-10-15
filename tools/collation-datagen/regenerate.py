#!/usr/bin/env python3
"""Download verified Unicode data, build the locked tool from source, reproduce.

Run at any cwd: python3 tools/collation-datagen/regenerate.py --check
Or: ... --output NEW_DIRECTORY
--sources-dir reuses previously downloaded inputs only after hash verification.
Downloaded archives are passed as data to SourceDataProvider, never extracted or
executed. The script deletes only its own temporary downloads/build directories.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import urllib.request

TOOL = Path(__file__).resolve().parent
ROOT = TOOL.parents[1]
COMMITTED = ROOT / 'src/data/icu_collator_data'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def manifest(root):
    return {str(p.relative_to(root)): digest(p)
            for p in sorted(root.rglob('*')) if p.is_file()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--check', action='store_true')
    mode.add_argument('--output', type=Path)
    parser.add_argument('--sources-dir', type=Path)
    args = parser.parse_args()
    if args.output and args.output.exists():
        parser.error('--output must be a new directory')
    scratch_parent = ROOT / '_tmp/d6'
    scratch_parent.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix='reproduce-', dir=scratch_parent))
    downloads = scratch / 'downloads'
    downloads.mkdir()
    cargo_home = scratch / 'cargo-home'
    if 'CARGO_HOME' in os.environ:
        cargo_home = Path(os.environ['CARGO_HOME']).resolve()
    generated = args.output.resolve() if args.output else scratch / 'generated'
    generated.parent.mkdir(parents=True, exist_ok=True)
    inputs = json.loads((TOOL / 'sources.json').read_text())
    try:
        for source in inputs:
            path = (args.sources_dir.resolve() if args.sources_dir else downloads) / source['file']
            if not args.sources_dir:
                print('Download:', source['url'], flush=True)
                urllib.request.urlretrieve(source['url'], path)
            if path.stat().st_size != source['bytes'] or digest(path) != source['sha256']:
                raise ValueError('source size/SHA-256 mismatch: ' + str(path))
            source['path'] = str(path)
        license_input = next(s for s in inputs if s['file'] == 'ICU-78.2-LICENSE')
        if digest(TOOL / 'LICENSE.unicode') != license_input['sha256']:
            raise ValueError('embedded ICU license differs from the verified source')
        (scratch / 'downloads-manifest.json').write_text(json.dumps(inputs, indent=2) + '\n')
        env = {**os.environ, 'CARGO_HOME': str(cargo_home),
               'CARGO_TARGET_DIR': str(scratch / 'target'), 'CARGO_INCREMENTAL': '0'}
        subprocess.run(['cargo', 'fetch', '--manifest-path', str(TOOL / 'Cargo.toml'), '--locked'],
                       env=env, check=True)
        subprocess.run(['cargo', 'run', '--manifest-path', str(TOOL / 'Cargo.toml'),
                        '--locked', '--offline', '--',
                        next(s['path'] for s in inputs if 'icuexportdata' in s['file']),
                        next(s['path'] for s in inputs if s['file'].startswith('cldr-')),
                        str(generated)], env=env, check=True)
        actual = manifest(generated)
        result = {'generated': actual, 'bytes': sum(p.stat().st_size for p in generated.rglob('*') if p.is_file())}
        if args.check:
            expected = manifest(COMMITTED)
            differences = sorted(k for k in actual.keys() | expected.keys() if actual.get(k) != expected.get(k))
            result.update(byteIdentical=not differences, differences=differences)
        (scratch / 'generated-manifest.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2))
        if args.check and differences:
            raise ValueError('generated files differ from committed data')
        print('Retained verification records:', scratch)
    finally:
        # These paths are all created by this invocation. An explicitly supplied
        # Cargo source cache and source directory are retained.
        for path in [downloads, scratch / 'target', scratch / 'cargo-home', scratch / 'generated']:
            if path.exists():
                shutil.rmtree(path)


if __name__ == '__main__':
    main()
