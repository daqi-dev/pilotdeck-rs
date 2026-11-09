#!/usr/bin/env python3
"""Bounded fixed-TS read probes; no real network or user PilotHome."""
import argparse,hashlib,json,os,signal,subprocess
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--baseline',required=True,type=Path);p.add_argument('--node',required=True,type=Path);p.add_argument('--output',required=True,type=Path);a=p.parse_args();base=a.baseline.resolve();node=a.node.resolve();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
sha='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';assert subprocess.check_output(['git','-C',str(base),'rev-parse','HEAD'],text=True).strip()==sha
sources={}
for name in ['src/pilot/config/loadPilotConfig.ts','src/pilot/staffdeck/projectBinding.ts']:
 data=(base/name).read_bytes();assert data==subprocess.check_output(['git','-C',str(base),'show',sha+':'+name]);sources[name]=hashlib.sha256(data).hexdigest()
versions=json.loads(subprocess.check_output([str(node),'-p','JSON.stringify({node:process.version,icu:process.versions.icu})'],text=True));assert versions=={'node':'v22.22.0','icu':'78.2'}
yaml=json.loads(Path(__file__).with_name('oracle.json').read_text())['cases'][0]['yaml'];rows=[]
for fifo in [True,False]:
 root=out/('store-fifo'if fifo else'large-config');home=root/'home';project=root/'project';home.mkdir(parents=True);project.mkdir();(home/'staffdeck').mkdir();config=home/'pilotdeck.yaml';config.write_text(yaml)
 target=home/'staffdeck/project-bindings.json'if fifo else config
 if fifo:os.mkfifo(target,0o600)
 else:config.write_text(yaml+'#'+'x'*(8*1024*1024+1-len(yaml.encode())-1))
 before=target.stat();config_hash=hashlib.sha256(config.read_bytes()).hexdigest();env={'HOME':str(home),'PILOT_HOME':str(home),'PATH':'/usr/bin:/bin','TMPDIR':str(root)}
 child=subprocess.Popen([str(node),'--import',str(base/'node_modules/tsx/dist/loader.mjs'),str(Path(__file__).with_name('resource-driver.mjs').resolve()),str(base),str(home),str(project)],cwd=project,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 try:stdout,stderr=child.communicate(timeout=3 if fifo else 12);timed=False
 except subprocess.TimeoutExpired:child.kill();stdout,stderr=child.communicate();timed=True
 after=target.stat();row={'id':root.name,'stdout':stdout.decode(),'stderr':stderr.decode(),'code':child.returncode,'timeout':timed,'size':before.st_size,'metadataUnchanged':(before.st_ino,before.st_mode,before.st_size)==(after.st_ino,after.st_mode,after.st_size),'configHashUnchanged':config_hash==hashlib.sha256(config.read_bytes()).hexdigest()}
 assert row['metadataUnchanged']and row['configHashUnchanged'];assert 'LOAD_BEGIN\n'in row['stdout']
 assert timed and child.returncode==-signal.SIGKILL if fifo else not timed and child.returncode==0 and 'LOAD_OK\n'in row['stdout']
 (root/'result.json').write_text(json.dumps(row,indent=2)+'\n');rows.append(row)
(out/'summary.json').write_text(json.dumps({'baseline':sha,**versions,'sources':sources,'cases':rows},indent=2)+'\n');print('confirmed fixed-TS FIFO blocking and >8MiB ordinary-config acceptance')
