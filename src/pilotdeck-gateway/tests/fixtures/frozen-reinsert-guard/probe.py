#!/usr/bin/env python3
# anchor-selfcheck: unsupported read-only entry import-boundary probe, no source mutations
"""#1066 execute the real entry up to its first non-bootstrap import.

The import boundary is deliberately fatal, including for already loaded modules.
Thus accepting a reinserted path cannot earn a pass from a later HEAD/hash guard.
No source is rewritten and no apparatus CLI/lease/scoring code is executed.
"""
import builtins
import os
import sys

entry, inserted, premise = sys.argv[1:]
with open(entry, 'rb') as stream:
    code = compile(stream.read(), entry, 'exec')
if inserted != '<absent>':
    sys.path.insert(0, inserted)
if premise == 'old-python':
    sys.version_info = (3, 10, 0, 'final', 0)
elif premise == 'unloaded-os':
    sys.modules.pop('os', None)
elif premise != 'normal':
    raise SystemExit('#1066 unknown probe premise')

original_import = builtins.__import__


def boundary(name, *args, **kwargs):
    if name == 'os' and premise == 'unloaded-os':
        print('#1066 reached untrusted os import', file=sys.stderr)
        raise SystemExit(74)
    if name not in ('sys', 'os'):
        print('#1066 reached first stdlib import: ' + name, file=sys.stderr)
        raise SystemExit(73)
    return original_import(name, *args, **kwargs)


builtins.__import__ = boundary
exec(code, {'__file__': entry, '__name__': '__main__'})
raise SystemExit('#1066 entry unexpectedly completed without an import boundary')
