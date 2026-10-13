#!/usr/bin/env python3
import json, os, pathlib, subprocess, time
ROOT=pathlib.Path(__file__).resolve().parents[2]
EVIDENCE=ROOT/'_tmp/exp007'
env=dict(os.environ,CARGO_HOME=str(ROOT/'_build/cargo-home'),CARGO_TARGET_DIR=str(ROOT/'_build/ci-target'),CARGO_BUILD_JOBS='2')
checks=[]
commands=[('root-fmt',['cargo','fmt','--all','--check']),('root-clippy',['cargo','clippy','--all-targets','--locked','--','-D','warnings']),('root-test',['cargo','test','--all','--locked'])]
for name,command in commands:
 start=time.monotonic()
 with (EVIDENCE/f'{name}.log').open('w') as log:
  result=subprocess.run(command,cwd=ROOT/'src',env=env,stdout=log,stderr=subprocess.STDOUT)
 checks.append(dict(name=name,command=command,exitCode=result.returncode,seconds=round(time.monotonic()-start,3)))
 print(f'{name}: exit {result.returncode}',flush=True)
(EVIDENCE/'ci.json').write_text(json.dumps(checks,indent=2)+'\n')
raise SystemExit(any(c['exitCode'] for c in checks))
