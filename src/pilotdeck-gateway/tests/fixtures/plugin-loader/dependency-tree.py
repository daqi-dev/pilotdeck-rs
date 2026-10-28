#!/usr/bin/env python3
# anchor-selfcheck: unsupported dependency fixture generator, no mutation definitions
"""Capture/replay the installed dependency tree without writing to node_modules.

Follow source directory aliases as distinct logical directories (like a
dereferenced copy). Preserve every visible path and all Markdown bytes; other
file contents are unobserved by PluginCommandLoader and replay as empty files.
Replay directories live in a flat backing pool, with lexical directory links.
No file is created through a node_modules path. Each logical directory has a
unique physical identity, so this input isolates the old candidate budget from
our separately tested alias de-duplication difference.
"""
import gzip
import json
import os
from pathlib import Path
import sys


def capture(source, destination):
    records = []
    contents, content_ids = [], {}
    pending = [(source, '')]
    while pending:
        directory, relative = pending.pop()
        files, children = [], []
        for entry in sorted(directory.iterdir()):
            try:
                metadata = entry.stat()
            except FileNotFoundError:
                continue  # TS stat failure is skipped too.
            if entry.is_dir():
                child = relative + '/' + entry.name if relative else entry.name
                children.append((entry.name, child))
                pending.append((entry, child))
            else:
                content_id = None
                if entry.name.lower().endswith('.md'):
                    content = entry.read_bytes().decode('utf-8', errors='replace')
                    if content not in content_ids:
                        content_ids[content] = len(contents)
                        contents.append(content)
                    content_id = content_ids[content]
                files.append([entry.name, content_id])
        records.append([relative, children, files])
    records.sort(key=lambda r: r[0])
    raw = (json.dumps({'contents': contents, 'directories': records}, ensure_ascii=False, separators=(',', ':'))+'\n').encode()
    destination.write_bytes(gzip.compress(raw, mtime=0))
    print(json.dumps({'directories': len(records),
                      'files': sum(len(r[2]) for r in records),
                      'markdown': sum(f[1] is not None for r in records for f in r[2])}))


def materialize(snapshot, pool):
    data = json.loads(gzip.decompress(snapshot.read_bytes()))
    records = data['directories']
    pool.mkdir()
    empty_count = 0
    paths = {r[0]: pool / f'd{i:06}' for i, r in enumerate(records)}
    for directory in paths.values():
        directory.mkdir()
    for relative, children, files in records:
        directory = paths[relative]
        for name, child in children:
            (directory / name).symlink_to(os.path.relpath(paths[child], directory))
        for name, content_id in files:
            if content_id is None:
                # Bound link counts for filesystems with a 64K link limit.
                empty = pool / f'empty-{empty_count // 10000}'
                if empty_count % 10000 == 0:
                    empty.touch()
                os.link(empty, directory / name)
                empty_count += 1
            else:
                (directory / name).write_bytes(data['contents'][content_id].encode())
    print(paths[''])


if __name__ == '__main__':
    mode, source, target = sys.argv[1:]
    if mode == 'capture':
        capture(Path(source), Path(target))
    else:
        assert mode == 'materialize'
        materialize(Path(source), Path(target))
