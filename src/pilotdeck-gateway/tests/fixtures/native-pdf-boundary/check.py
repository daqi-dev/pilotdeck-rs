#!/usr/bin/env python3
"""Compile the real C wrappers against locally built MuPDF; inject two failures."""
import argparse
import hashlib
import json
from pathlib import Path
import signal
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--library-dir', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
root = Path(__file__).resolve().parents[5]
source = root / 'src/vendor/mupdf-sys'
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=False)
libraries = [args.library_dir.resolve() / name for name in ('libmupdf.a', 'libmupdf-third.a')]
inputs = [Path(__file__).resolve(), Path(__file__).with_name('probe.c').resolve(),
          source / 'wrapper/context.c', source / 'wrapper/pixmap.c',
          source / 'wrapper/error.c', *libraries]
manifest = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in inputs}
(output / 'inputs.json').write_text(json.dumps(manifest, indent=2) + '\n')
context = inputs[2].read_text()
pixmap = inputs[3].read_text()
context_anchor = '        fz_ignore_error(ctx);\n        mupdf_drop_base_context(ctx);\n        return NULL;'
buffer_anchor = '        fz_drop_buffer(ctx, buf);\n        buf = NULL;'
assert context.count(context_anchor) == pixmap.count(buffer_anchor) == 1
cases = [
    ('real', context, pixmap, True),
    ('missing-initialization-return', context.replace(context_anchor,
        '        fz_ignore_error(ctx);\n        mupdf_drop_base_context(ctx);'), pixmap, False),
    ('missing-error-buffer-drop', context, pixmap.replace(buffer_anchor, '        buf = NULL;'), False),
]
results = []
for name, context_text, pixmap_text, succeeds in cases:
    folder = output / name
    folder.mkdir()
    (folder / 'context.c').write_text(context_text)
    (folder / 'pixmap.c').write_text(pixmap_text)
    command = ['/usr/bin/gcc', '-O1', '-g', '-Werror=implicit-function-declaration',
               '-I' + str(source / 'mupdf/include'), '-I' + str(source / 'wrapper'),
               str(inputs[1]), str(folder / 'context.c'), str(folder / 'pixmap.c'), str(inputs[4]),
               '-Wl,--wrap=fz_register_document_handlers', '-Wl,--wrap=fz_write_pixmap_as_jpeg',
               '-Wl,--wrap=fz_new_buffer', '-Wl,--wrap=fz_drop_buffer', '-Wl,--start-group',
               *map(str, libraries), '-Wl,--end-group', '-lpthread', '-lm', '-ldl',
               '-o', str(folder / 'probe')]
    (folder / 'command.json').write_text(json.dumps(command, indent=2) + '\n')
    with (folder / 'build.log').open('x') as log:
        built = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=120).returncode
    code = None
    if built == 0:
        with (folder / 'run.log').open('x') as log:
            code = subprocess.run([str(folder / 'probe')], stdout=log,
                                  stderr=subprocess.STDOUT, timeout=30).returncode
    rejection = {'missing-initialization-return': 'mupdf_new_base_context() == NULL',
                 'missing-error-buffer-drop': 'created_buffers - destroyed_buffers == retained_before_encoding'}.get(name)
    log = (folder / 'run.log').read_text(errors='replace') if code is not None else ''
    passed = built == 0 and code is not None and (
        (code == 0) if succeeds else (code == -signal.SIGABRT and rejection in log))
    results.append({'name': name, 'buildCode': built, 'runCode': code,
                    'expectedRejection': rejection, 'passed': passed})
    print(results[-1], flush=True)
stable = all(hashlib.sha256(Path(path).read_bytes()).hexdigest() == digest
             for path, digest in manifest.items())
(output / 'result.json').write_text(json.dumps({'results': results, 'inputsStable': stable}, indent=2) + '\n')
raise SystemExit(0 if stable and all(row['passed'] for row in results) else 1)
