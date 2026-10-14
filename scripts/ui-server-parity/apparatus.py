"""Source-only apparatus admission, shared by the checker and capture hashing.

Entry points execute this file's source before any non-bootstrap imports. Keep
its imports limited to the interpreter's sys/os bootstrap modules.
"""
import os
import sys

SPEC_FILES = ('acceptance.md', 'normalization.md', 'accepted-diffs.md', 'multimodal.md')
EXP008_FILES = ('run.py', 'compare.py', 'enumerate_hook.mjs', 'preload.mjs',
                'reconcile_routes.py', 'proxy/Cargo.toml', 'proxy/Cargo.lock', 'proxy/src/main.rs')


class ApparatusUnsafe(ValueError):
    pass


def scan(directory):
    """Never traverse links; reject links and Python caches rather than omit them."""
    directory = os.fspath(directory)
    errors, files = [], []
    if os.path.islink(directory):
        raise ApparatusUnsafe('apparatus symlink: ' + directory)
    if not os.path.isdir(directory):
        raise ApparatusUnsafe('apparatus directory missing: ' + directory)

    def walk_error(error):
        errors.append(str(error))

    for parent, directories, names in os.walk(directory, followlinks=False, onerror=walk_error):
        for name in directories + names:
            path = os.path.join(parent, name)
            if os.path.islink(path):
                errors.append('apparatus symlink: ' + path)
            if name == '__pycache__' or name.endswith('.pyc'):
                errors.append('forbidden Python cache: ' + path)
        files.extend(os.path.join(parent, name) for name in names if os.path.isfile(os.path.join(parent, name)))
    if errors:
        raise ApparatusUnsafe('; '.join(sorted(errors)))
    return sorted(files)


def preflight(directory):
    sys.dont_write_bytecode = True
    try:
        scan(directory)
        # B2 loads B1 Python source and executes its Node helpers too.
        scan(os.path.join(os.path.dirname(directory), 'exp008'))
    except ApparatusUnsafe as error:
        print(str(error), file=sys.stderr)
        # json itself may be shadowed: emit a fixed JSON failure without importing it.
        print('{"verdict":"FAIL","errors":["unsafe apparatus; see stderr"]}')
        raise SystemExit(1)
