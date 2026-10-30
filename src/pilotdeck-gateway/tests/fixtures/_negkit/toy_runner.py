#!/usr/bin/env python3
# anchor-selfcheck: unsupported toy runner mutates only its own temporary text file
"""Disposable-file M6 demonstration; never edits product sources or uses git."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import contract


TEST = 'toy::value_is_original'
MARKER = '#1107 toy value must stay original'


def probe(source):
    failed = Path(source).read_text() != 'original\n'
    print('running 1 test')
    print(f'test {TEST} ... ' + ('FAILED' if failed else 'ok'))
    if failed:
        print(f"thread '{TEST}' panicked at toy.rs:1:1:\n{MARKER}")
    print('test result: ' + ('FAILED. 0 passed; 1 failed;' if failed else 'ok. 1 passed; 0 failed;')
          + ' 0 ignored; 0 measured; 0 filtered out; finished in 0.00s')
    return 101 if failed else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--scenario', choices=['pass', 'refuse', 'restore-fail', 'cancel'], default='pass')
    args = parser.parse_args(argv)
    case = contract.Case('toy-change-value', TEST, MARKER)
    with tempfile.TemporaryDirectory(prefix='negkit-toy-') as directory:
        scratch = Path(directory)
        source = scratch / 'value.txt'
        source.write_text('original\n')
        # Synthetic paths belong solely to this toy; no cargo or TS is run.
        env = dict(os.environ, CARGO_HOME=str(scratch), CARGO_TARGET_DIR=str(scratch / 'target'),
                   PDRS_BASELINE=str(scratch))

        @contextmanager
        def guard():
            print('guard: entered; summary=' + str((args.output / 'summary.json').exists()), flush=True)
            try:
                yield
            finally:
                source.write_text('original\n')
                if source.read_text() != 'original\n':
                    raise RuntimeError('#1107 toy restoration readback failed')
                print('guard: restored; summary=' + str((args.output / 'summary.json').exists()), flush=True)
                if args.scenario == 'restore-fail':
                    raise RuntimeError('#1107 injected toy verification failure')

        def execute(_case, _env):
            command = [sys.executable, '-I', '-B', str(Path(__file__).resolve()), '--probe', str(source)]
            positive = subprocess.run(command, text=True, capture_output=True, timeout=5)
            if contract.classify(positive, case)[0] != 'SURVIVED':
                raise RuntimeError('#1107 toy positive failed')
            source.write_text('mutant\n')
            if args.scenario == 'cancel':
                signal.raise_signal(signal.SIGINT)
            result = subprocess.run(command, text=True, capture_output=True, timeout=5)
            print(result.stdout, end='', flush=True)
            return result

        if args.scenario == 'cancel':
            signal.signal(signal.SIGINT, signal.SIG_IGN)
        result = contract.run([case], args.output, guard=guard, execute=execute,
                              oracle=[] if args.scenario == 'refuse' else ['original'], environ=env)
        print('exit=' + str(result.code) + '; summary=' + str((args.output / 'summary.json').exists()))
        print(json.dumps(result.document, sort_keys=True))
        return result.code


if __name__ == '__main__':
    if sys.argv[1:2] == ['--probe']:
        raise SystemExit(probe(sys.argv[2]))
    raise SystemExit(main())
