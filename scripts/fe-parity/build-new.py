#!/usr/bin/env python3
"""Build only the selected Leptos probe, entirely from source (no binary installer)."""
import os
import pathlib
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
DIST = ROOT / '_build/dist-leptos'
TOOLS = ROOT / '_build/tools/bin'
DIST.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, CARGO_HOME=str(ROOT / '_build/cargo-home'),
           CARGO_TARGET_DIR=str(ROOT / '_build/probe-target'), CARGO_BUILD_JOBS='2')
bindgen = str(TOOLS / 'wasm-bindgen') if (TOOLS / 'wasm-bindgen').exists() else shutil.which('wasm-bindgen')
if not bindgen:
    raise SystemExit('Missing source-built wasm-bindgen 0.2.129. See scripts/fe-parity/README.md; no binary downloads.')
version = subprocess.check_output([bindgen, '--version'], text=True).strip()
if version != 'wasm-bindgen 0.2.129':
    raise SystemExit(f'Expected wasm-bindgen 0.2.129 to match Cargo.lock; got {version}')
subprocess.run(['cargo', 'build', '--locked', '--release', '--target', 'wasm32-unknown-unknown'],
               cwd=ROOT / 'ui-rs/probe-leptos', env=env, check=True)
subprocess.run([bindgen, '--target', 'web', '--out-name', 'probe', '--out-dir', str(DIST),
                str(ROOT / '_build/probe-target/wasm32-unknown-unknown/release/probe-leptos.wasm')],
               cwd=ROOT, env=env, check=True)
(DIST / 'index.html').write_text('''<!doctype html><html lang="zh"><head><meta charset="utf-8"><title>F2 Leptos probe</title><link rel="stylesheet" href="/vendor/xterm.css"><link rel="stylesheet" href="/shared/probe.css"><script src="/vendor/xterm.js"></script></head><body><script type="module">import init from '/probe/leptos/probe.js';await init();</script></body></html>''')
(DIST / 'package.json').write_text('{"type":"module"}\n')
print(f'LEPTOS_SOURCE_BUILD {DIST}')
