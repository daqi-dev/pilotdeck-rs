#!/usr/bin/env python3
# anchor-selfcheck: unsupported #2431 rc2 startup-refusal evidence probe, no source replacement table
"""#2431: raw startup-refusal evidence for same-json-surfaces' rc2 contract.

Three probes drive ``main()`` in-process with the guard's ``__enter__``
raising, one per exception class the startup phase can produce: the memory
branch's tree lock raising ``OSError``, and the guarded branch's
``mutating_sources`` raising ``RuntimeError`` then
``subprocess.TimeoutExpired`` (the workspace-scanner shapes).  Each probe
prints the raw observation -- what ``main()`` raised or returned, whether the
claimed directory was withdrawn, whether a ``summary.json`` exists, and the
refusal message -- and the probe exits 0 only when every probe is a refusal:
``SystemExit 2``, claim directory gone, no ``summary.json``.  A non-refusal
outcome (a return, or an uncaught exception) is printed as evidence, never
masked by a crash.  The committed regression pins are the SameJsonSurfaces
tests in ``scripts/gateway-parity/test_safety.py``; this script is the
re-runnable evidence archive the r3 review asked to keep (#2431).  Run with
``python3 -I``; nothing here writes to the source tree.
"""
import importlib.util
import io
import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
from unittest.mock import patch

FIXTURE = Path(__file__).resolve().parent / 'negative_controls.py'


def probe(label, memory, enter_error):
    """One startup-refusal observation: run main() against a refusing guard."""
    with tempfile.TemporaryDirectory(prefix='pdrs-2431-probe-') as folder:
        out = Path(folder) / 'out'
        spec = importlib.util.spec_from_file_location('samejson_rc2_probe_' + label, FIXTURE)
        module = importlib.util.module_from_spec(spec)
        exec(compile(FIXTURE.read_bytes(), str(FIXTURE), 'exec'), module.__dict__)

        class refusing_guard:
            def __enter__(self):
                raise enter_error

            def __exit__(self, *_exc):
                return None

        # The guarded branch installs the real terminate handlers before the
        # guard; restore the interpreter's dispositions afterwards either way.
        names = [signal.SIGTERM] + [getattr(signal, name) for name in ('SIGHUP', 'SIGQUIT')
                                    if hasattr(signal, name)]
        saved = {name: signal.getsignal(name) for name in names}
        stdout, stderr = io.StringIO(), io.StringIO()
        argv = [str(FIXTURE), str(out),
                'memory-rehearsal-without-lock' if memory else 'surfaces-frames-bare-not-equal']
        if memory:
            argv.insert(2, '--memory')
        observation = {'probe': label}
        try:
            with patch.object(module, 'tree_lock' if memory else 'mutating_sources',
                              lambda *_a, **_k: refusing_guard()), \
                    patch('sys.stdout', stdout), patch('sys.stderr', stderr), \
                    patch.object(sys, 'argv', argv):
                try:
                    module.main()
                    observation['exit'] = 'returned normally'
                except SystemExit as code:
                    observation['exit'] = 'SystemExit ' + str(code.code)
                except Exception as error:
                    observation['exit'] = 'uncaught ' + repr(error)
        finally:
            for name, disposition in saved.items():
                # getsignal returns None when a non-Python handler holds
                # the signal; signal.signal refuses None, so map that back
                # to the default disposition (same guard as the #2552 r9
                # legs in test_safety.py, DS r10 sibling#2).
                signal.signal(name, disposition if disposition is not None
                              else signal.SIG_DFL)
        observation['claimed_dir_exists'] = out.exists()
        observation['summary_json'] = (out / 'summary.json').exists()
        observation['stderr'] = stderr.getvalue().strip().splitlines()[:2]
        observation['refusal'] = (observation.get('exit') == 'SystemExit 2'
                                 and not observation['claimed_dir_exists']
                                 and not observation['summary_json'])
    return observation


def main():
    observations = [
        probe('memory-tree-lock-OSError', True, OSError('probe: tree lock unavailable')),
        probe('guarded-scanner-RuntimeError', False, RuntimeError('probe: scanner staging failed')),
        probe('guarded-scanner-TimeoutExpired', False,
              subprocess.TimeoutExpired('workspace scanner', 60)),
    ]
    print(json.dumps(observations, indent=2))
    return 0 if all(row['refusal'] for row in observations) else 1


if __name__ == '__main__':
    raise SystemExit(main())
