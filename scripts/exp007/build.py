#!/usr/bin/env python3
"""Reproducible common build for fair payload comparison; optional official bundlers."""
import argparse, gzip, hashlib, json, os, pathlib, subprocess, time
ROOT = pathlib.Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / '_tmp/exp007'
TOOLS = ROOT / '_build/tools/bin'
parser = argparse.ArgumentParser()
parser.add_argument('--bundlers', action='store_true')
args = parser.parse_args()
EVIDENCE.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, CARGO_HOME=str(ROOT/'_build/cargo-home'), CARGO_TARGET_DIR=str(ROOT/'_build/probe-target'), DX_HOME=str(ROOT/'_build/dx-home'))
env['PATH'] = str(TOOLS) + os.pathsep + env['PATH']
env.pop('NO_COLOR', None) # trunk 0.21.14 parses NO_COLOR as bool; inherited "1" is rejected.
env['CARGO_BUILD_JOBS'] = '2'
results = []
def run(command, cwd, log):
    start = time.monotonic()
    with (EVIDENCE/log).open('w') as f:
        subprocess.run(command, cwd=cwd, env=env, stdout=f, stderr=subprocess.STDOUT, check=True)
    return round(time.monotonic()-start, 3)
for candidate in ['leptos', 'dioxus']:
    crate = ROOT / f'ui-rs/probe-{candidate}'
    cargo_s = run(['cargo', 'build', '--locked', '--release', '--target', 'wasm32-unknown-unknown'], crate, f'common-build-{candidate}.log')
    dist = ROOT / f'_build/dist-{candidate}'
    dist.mkdir(parents=True, exist_ok=True)
    bindgen_s = run([str(TOOLS/'wasm-bindgen'), '--target', 'web', '--out-name', 'probe', '--out-dir', str(dist), str(ROOT/f'_build/probe-target/wasm32-unknown-unknown/release/probe-{candidate}.wasm')], crate, f'bindgen-{candidate}.log')
    (dist/'index.html').write_text(f'''<!doctype html><html lang="zh"><head><meta charset="utf-8"><title>F1 {candidate} probe</title><link rel="stylesheet" href="/vendor/xterm.css"><link rel="stylesheet" href="/shared/probe.css"><script src="/vendor/xterm.js"></script></head><body><div id="main"></div><script type="module">import init from '/probe/{candidate}/probe.js';await init();</script></body></html>''')
    (dist/'package.json').write_text('{"type":"module"}\n')
    wasm = (dist/'probe_bg.wasm').read_bytes()
    javascript = list(dist.rglob('*.js'))
    data = dict(candidate=candidate,cargoSeconds=cargo_s,bindgenSeconds=bindgen_s,cache='warm, not cold build benchmark',wasmBytes=len(wasm),wasmGzipBytes=len(gzip.compress(wasm,mtime=0)),wasmSHA256=hashlib.sha256(wasm).hexdigest(),jsBytes=sum(p.stat().st_size for p in javascript),jsGzipBytes=sum(len(gzip.compress(p.read_bytes(),mtime=0)) for p in javascript),wasmOpt=False,profile=dict(optLevel='z',lto=True,codegenUnits=1,strip=False))
    if args.bundlers:
        if candidate=='leptos':
            data['bundlerSeconds']=run([str(TOOLS/'trunk'),'build','--release','--locked','--dist',str(ROOT/'_build/trunk-dist')],crate,'trunk-build-measured.log')
        else:
            data['bundlerSeconds']=run([str(TOOLS/'dx'),'build','--web','--release','--locked','--debug-symbols','false','--base-path','/probe/dioxus/','--session-cache-dir',str(ROOT/'_build/dx-session')],crate,'dx-build-measured.log')
    results.append(data)
    print(f'{candidate}: wasm {len(wasm)} bytes, gzip {data["wasmGzipBytes"]}, common build {cargo_s+bindgen_s:.3f}s',flush=True)
(EVIDENCE/'build.json').write_text(json.dumps(results,indent=2)+'\n')
