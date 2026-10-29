#!/usr/bin/env -S python3 -I
"""Capture logical directory contents after owned services have stopped."""
# Isolated bootstrap: verify the source loader before executing local code.
import os as _boot_os
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (fix-r3 P1, same defect as #701 fix-r2). Nonexistent sys.path entries skip.
if any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.isdir(root)
                and _boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))
        for entry in _boot_sys.path):
    print('{"verdict":"FAIL","errors":["apparatus directory on sys.path; run the entry file directly with -I"]}')
    raise SystemExit(1)
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
import hashlib as _boot_hashlib
import subprocess as _boot_subprocess
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_os.path.isfile(_boot_manifest):
        with open(_boot_manifest) as _boot_file:
            _boot_pins = [line.split()[0] for line in _boot_file
                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]
        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:
            raise ValueError('apparatus bootstrap hash mismatch')
    else:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import argparse
import base64
import hashlib
import json
from pathlib import Path
import sqlite3
import shutil
import tempfile
from contextlib import closing
import stat


def sqlite_value(value):
    return {'blob_b64': base64.b64encode(value).decode()} if isinstance(value, bytes) else value


def capture(directory):
    directory = Path(directory)
    entries = []
    for root_name in ('home', 'project'):
        root = directory / root_name
        if not root.is_dir():
            raise ValueError(f'missing snapshot root {root_name}')
        for path in [root, *sorted(root.rglob('*'))]:
            mode = path.lstat().st_mode
            item = {'path': path.relative_to(directory).as_posix(), 'mode': stat.S_IMODE(mode)}
            if path.is_symlink():
                item.update(kind='symlink', target=str(path.readlink()))
            elif path.is_dir():
                item['kind'] = 'directory'
            elif path.is_file():
                raw = path.read_bytes()
                if raw.startswith(b'SQLite format 3\0'):
                    tables = {}
                    # Copy the complete DB/WAL bundle before opening it. A read-only
                    # SQLite connection can create SHM; never mutate evidence roots.
                    with tempfile.TemporaryDirectory(prefix='b2-sqlite-') as temporary:
                        copied = Path(temporary) / 'database.db'
                        shutil.copyfile(path, copied)
                        for suffix in ('-wal', '-shm'):
                            source = Path(str(path) + suffix)
                            if source.is_file():
                                shutil.copyfile(source, Path(str(copied) + suffix))
                        with closing(sqlite3.connect(copied.as_uri() + '?mode=ro', uri=True)) as db:
                            if db.execute('PRAGMA integrity_check').fetchone() != ('ok',):
                                raise ValueError(f'corrupt SQLite {item["path"]}')
                            schema = [dict(zip(('type', 'name', 'table', 'sql'), row)) for row in db.execute(
                                'SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name')]
                            for name, sql in db.execute("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name"):
                                quote = lambda name: '"' + name.replace('"', '""') + '"'
                                quoted = quote(name)
                                info = list(db.execute(f'PRAGMA table_info({quoted})'))
                                primary = [c[1] for c in sorted(info, key=lambda c: c[5]) if c[5]]
                                order = ','.join(map(quote, primary)) if primary else 'rowid'
                                cursor = db.execute(f'SELECT * FROM {quoted} ORDER BY {order}')
                                columns = [entry[0] for entry in cursor.description]
                                tables[name] = {'sql': sql, 'rows': [dict(zip(columns, map(sqlite_value, row))) for row in cursor]}
                    item.update(kind='sqlite', data={'schema': schema, 'tables': tables})
                else:
                    try:
                        text = raw.decode('utf-8')
                    except UnicodeError:
                        item.update(kind='binary', bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
                    else:
                        try:
                            data = json.loads(text)
                        except ValueError:
                            if path.suffix == '.jsonl':
                                try:
                                    data = [json.loads(line) for line in text.splitlines() if line]
                                except ValueError:
                                    item.update(kind='text', data=text)
                                else:
                                    item.update(kind='jsonl', data=data)
                            else:
                                item.update(kind='text', data=text)
                        else:
                            item.update(kind='json', data=data)
            else:
                raise ValueError(f'unsupported snapshot entry {path}')
            entries.append(item)
    return {'schema': 1, 'roots': ['home', 'project'], 'entries': entries}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    (args.directory / 'snapshot.json').write_text(json.dumps(capture(args.directory), ensure_ascii=False, indent=2) + '\n')


if __name__ == '__main__':
    main()
