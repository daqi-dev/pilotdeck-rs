#!/usr/bin/env python3
# anchor-selfcheck: unsupported fixture regeneration helper, no source mutations
"""#1265 bounded regeneration; fresh output, exact bytes, no source mutation."""
from pathlib import Path
import sys
import tempfile
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned

def main():
    here = Path(__file__).resolve().parent
    if sys.argv[1:] != ['rquickjs-regexp']:
        raise SystemExit('#1265 expected rquickjs-regexp fixture')
    with tempfile.TemporaryDirectory(prefix='pdrs-1265-regenerate-') as directory:
        result = run_owned(['node', str(here / 'generate.mjs'), directory], timeout=300)
        print(result.stdout, end='')
        if result.returncode:
            raise SystemExit(f'#1265 generator failed: {result.stderr}')
        for name in ['oracle.json', 'regress-oracle.json']:
            if (Path(directory) / name).read_bytes() != (here / name).read_bytes():
                raise SystemExit(f'#1265 regenerated {name} differs')
        print('#1265 both fixtures regenerated identically')

if __name__ == '__main__':
    main()
