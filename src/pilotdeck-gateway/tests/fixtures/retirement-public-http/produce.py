"""Collect one source-bound profile inside the common /app layout, then finalize outside."""
import argparse,hashlib,importlib.util,json,os,subprocess,sys
from pathlib import Path
HERE=Path(__file__).resolve().parent
sha=lambda p:hashlib.sha256(Path(p).read_bytes()).hexdigest()
def ref(p):return {'path':str(Path(p).resolve()),'sha256':sha(p)}
def load(name,p):
 spec=importlib.util.spec_from_file_location(name,p);m=importlib.util.module_from_spec(spec);sys.modules[name]=m;spec.loader.exec_module(m);return m
layout=load('public_http_layout',HERE/'layout.py')
p=argparse.ArgumentParser();p.add_argument('--mode',choices=['package','collect','finalize'],required=True)
for k in ['repo','baseline','node','source-manifest','candidate-manifest','out']:p.add_argument('--'+k,type=Path,required=True)
p.add_argument('--head',required=True);p.add_argument('--source-sha',required=True);p.add_argument('--engine',choices=['ts','native']);p.add_argument('--profile',choices=['configured','skills-disabled','approval-unconfigured','feishu']);p.add_argument('--deployment',type=Path);p.add_argument('--run',type=Path);a=p.parse_args()
source=json.loads(a.source_manifest.read_text());assert sha(a.source_manifest)==a.source_sha and source['head']==a.head
tracked={str(a.repo/x['path']):x['sha256'] for x in source['files']}
shared=HERE.parent/'native-channel-production';observer=shared/'owned-observer.mjs';finalizer=shared/'finalize-process.py'
for f in [Path(__file__),HERE/'layout.py',HERE/'package.py',HERE/'producer.py',HERE/'launch-owned.mjs',HERE/'driver-gold.mts',HERE/'driver-disabled.mts',HERE/'driver-unconfigured.mts',HERE/'approval-seed.json',HERE/'feishu-http.mjs',HERE/'feishu-preload.mjs',observer,finalizer]:assert tracked.get(str(f))==sha(f),str(f)
if a.mode=='package':
 subprocess.run([sys.executable,'-I','-B',str(HERE/'package.py'),'--repo',str(a.repo),'--candidate',str(a.candidate_manifest.parent),'--source-manifest',str(a.source_manifest),'--head',a.head,'--source-sha',a.source_sha,'--baseline',str(a.baseline),'--out',str(a.deployment)],check=True)
 raise SystemExit(0)
assert a.run and a.profile and a.engine and a.deployment
run=a.run.resolve();capture=run/'capture';engine=a.engine;profile=a.profile
if a.mode=='collect':
 env={'PATH':str(a.node.parent)+':/usr/bin:/bin','LANG':'C.UTF-8','HOME':'/app/home','PILOT_HOME':'/app/home','TMPDIR':'/app/tmp','LD_LIBRARY_PATH':os.environ.get('LD_LIBRARY_PATH',''),'PDRS_BASELINE':'/app/source','PDRS_SOURCE_MANIFEST_SHA256':a.source_sha}
 if profile=='feishu':
  env.update(PDRS_OWNED_PROCESS_DIR=str(run/'observation'),PDRS_EXPECTED_EXECUTABLE=str(a.node) if engine=='ts' else '/app/source/bin/pilotdeck-gateway')
  command=[str(a.node),'--import',str(observer),str(HERE/'feishu-http.mjs'),'/app/source','/app/source/bin/pilotdeck-gateway','/app/source/bin/pilotdeck-agent-loop-sidecar',str(capture),'feishu',engine];producer=HERE/'feishu-http.mjs';driver=producer;raw=capture/'evidence.json';observation=run/'observation'
 else:
  command=[sys.executable,'-I','-B',str(HERE/'producer.py'),'--engine',engine,'--baseline','/app/source','--out',str(capture),'--candidate-manifest',str(a.candidate_manifest),'--source-manifest',str(a.source_manifest),'--head',a.head,'--expected-source-sha',a.source_sha,'--node',str(a.node)]
  if engine=='native':command+=['--gateway','/app/source/bin/pilotdeck-gateway','--sidecar','/app/source/bin/pilotdeck-agent-loop-sidecar']
  if profile!='configured':command+=['--'+profile]
  producer=HERE/'producer.py';driver=HERE/('driver-gold.mts' if profile=='configured' else 'driver-disabled.mts' if profile=='skills-disabled' else 'driver-unconfigured.mts');raw=capture/'driver-pause/result.json';observation=capture/'observation'
 (run/'collect-command.json').write_text(json.dumps({'argv':command,'environment':env},indent=2)+'\n')
 with (run/'collect.stdout').open('x') as so,(run/'collect.stderr').open('x') as se:
  try:code=subprocess.run(command,env=env,stdout=so,stderr=se,timeout=180).returncode
  except subprocess.TimeoutExpired:code=124
 info={'id':profile+'-'+engine,'profile':profile,'engine':engine,'collectorExit':code,'rawPath':str(raw),'observationDir':str(observation),'producer':ref(producer),'driver':ref(driver),'sources':[ref(f) for f in [Path(__file__),producer,driver,HERE/'launch-owned.mjs',HERE/'approval-seed.json',observer,finalizer]],'home':str(run/'home'),'projectFiles':str(run/'project'),'runtime':layout.RUNTIME}
 if profile=='feishu':
  info['preloads']=[ref(HERE/'feishu-preload.mjs')] if engine=='ts' else []
  provenance=run/'provenance.json';provenance.write_text(json.dumps({'baseline':'/app/source','baselineHost':str(a.baseline),'head':'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85','nativeHead':a.head,'engine':engine,'sourceManifestSha256':a.source_sha,'candidateManifestSha256':sha(a.candidate_manifest),'producerSha256':sha(producer),'driverSha256':sha(driver)},indent=2)+'\n');info['provenance']=ref(provenance)
 else:info['provenance']=ref(capture/'provenance.json') if (capture/'provenance.json').exists() else None
 (run/'collected.json').write_text(json.dumps(info,indent=2)+'\n');raise SystemExit(code)
info=json.loads((run/'collected.json').read_text());info['raw']=ref(info.pop('rawPath'));info['processReceiptPath']=str(run/'process-receipt.json')
command=[sys.executable,'-I','-B',str(finalizer),'--channel','public-http:'+profile+':'+engine,'--observation-dir',info['observationDir'],'--raw',info['raw']['path'],'--producer',info['producer']['path'],'--source-manifest',str(a.source_manifest),'--candidate-manifest',str(a.candidate_manifest),'--network-preflight',str(run/'network-preflight/network-isolation.json'),'--container-cleanup',str(run/'container-cleanup.json'),'--home',str(run/'home'),'--project',str(run/'project'),'--out',info['processReceiptPath']]
subprocess.run(command,check=True);info['processReceipt']=ref(info.pop('processReceiptPath'))
if profile!='feishu':
 info['models']=ref(capture/'model-requests.json');info['formalRequests']=ref(capture/'formal-requests.json');info['providerWire']=ref(capture/'provider-wire.jsonl')
info['layoutProof']={'command':ref(run/'container-command.json'),'inspect':ref(run/'container-inspect.json'),'cleanup':ref(run/'container-cleanup.json')}
(run/'profile.json').write_text(json.dumps(info,indent=2)+'\n')
