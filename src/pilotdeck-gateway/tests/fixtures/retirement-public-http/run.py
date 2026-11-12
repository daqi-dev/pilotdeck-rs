"""Run the eight retained HTTP5 profiles in identical isolated /app paths, one at a time."""
import argparse,hashlib,importlib.util,json,os,signal,subprocess,sys,time
from pathlib import Path
HERE=Path(__file__).resolve().parent
IMAGE='sha256:eb58eb269804b8a9b2bb9ceeb9a175cac9b79612d4c8dafe0a987b277bd55683'
PROFILES=('configured','skills-disabled','approval-unconfigured','feishu')
CASE_TIMEOUT=240
def load(name,path):
 spec=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(spec);sys.modules[name]=m;spec.loader.exec_module(m);return m
layout=load('http5_layout',HERE/'layout.py')
sha=lambda p:hashlib.sha256(Path(p).read_bytes()).hexdigest()
def ref(p):return {'path':str(Path(p).resolve()),'sha256':sha(p)}
def save(p,value):
 with Path(p).open('x') as f:json.dump(value,f,indent=2);f.write('\n')
p=argparse.ArgumentParser()
for k in ['repo','baseline','node-root','source-manifest','candidate-manifest','out']:p.add_argument('--'+k,type=Path,required=True)
p.add_argument('--head',required=True);p.add_argument('--source-sha',required=True);a=p.parse_args()
a.repo=a.repo.resolve(strict=True);a.baseline=a.baseline.resolve(strict=True);a.out=a.out.absolute();a.node_root=a.node_root.resolve(strict=True)
assert a.out.is_relative_to(a.repo/'_tmp') and not a.out.exists() and not a.out.is_symlink();a.out.mkdir(mode=0o700)
node=a.node_root/'bin/node';uv=Path('/home/clawbox-08/opt/libuv-1.52.1');shared=HERE.parent/'native-channel-production'
source=json.loads(a.source_manifest.read_text());assert sha(a.source_manifest)==a.source_sha and source['head']==a.head
assert subprocess.check_output(['git','-C',str(a.repo),'rev-parse','HEAD'],text=True).strip()==a.head
tracked={str(a.repo/x['path']):x['sha256'] for x in source['files']}
for f in [Path(__file__),HERE/'produce.py',HERE/'layout.py',shared/'owned-observer.mjs',shared/'finalize-process.py',shared/'network-preflight.py']:assert tracked.get(str(f))==sha(f),str(f)
common_path=a.repo/'scripts/gateway-parity/common.py';common=load('http5_registered_common',common_path);common._apparatus_source=common_path.read_bytes()
base=[sys.executable,'-I','-B',str(HERE/'produce.py'),'--repo',str(a.repo),'--baseline',str(a.baseline),'--node',str(node),'--source-manifest',str(a.source_manifest),'--candidate-manifest',str(a.candidate_manifest),'--head',a.head,'--source-sha',a.source_sha,'--out',str(a.out),'--deployment',str(a.out/'deployment')]
subprocess.run([*base,'--mode','package'],check=True)
node_versions=json.loads(subprocess.check_output([str(node),'-p','JSON.stringify(process.versions)'],env={'PATH':str(node.parent)+':/usr/bin:/bin','LD_LIBRARY_PATH':str(uv/'lib')},text=True));assert [node_versions[k] for k in ['node','icu','uv']]==['22.22.0','78.2','1.52.1']
dependencies={'path':str((a.baseline/'node_modules').resolve()),'sha256':common.tree_digest(a.baseline/'node_modules')}
results=[];children=[];active_name=None

def interrupt(signum,frame):raise KeyboardInterrupt('owned HTTP5 run interrupted')
for sig in [signal.SIGINT,signal.SIGTERM]:signal.signal(sig,interrupt)
with common.parity_lock(timeout=1):
 for profile in PROFILES:
  for engine in ['ts','native']:
   run=layout.prepare_run(a.out/(profile+'-'+engine));name='pdrs-http5-'+str(os.getpid())+'-'+profile+'-'+engine;active_name=name
   mounts=layout.app_mounts(run,a.baseline,a.out/'deployment',engine)
   infra=[{'source':str(x),'destination':str(x),'readonly':True} for x in [Path('/usr'),Path('/lib'),Path('/etc/ld.so.cache'),a.repo,a.baseline,a.node_root,uv]]
   mounts=[*infra,*mounts,{'source':str(run),'destination':str(run),'readonly':False}]
   collect=[*base,'--mode','collect','--profile',profile,'--engine',engine,'--run',str(run)]
   command=['docker','run','--rm','--pull','never','--name',name,'--network','none','--read-only','--user',f'{os.getuid()}:{os.getgid()}','--tmpfs','/tmp:rw,exec,nosuid,nodev,mode=1777,size=512m','--workdir','/app/project','--env','PATH='+str(node.parent)+':/usr/bin:/bin','--env','LD_LIBRARY_PATH='+str(uv/'lib'),*layout.docker_mount_args(mounts),IMAGE,'/usr/libexec/docker/docker-init','--','/usr/bin/python3','-I','-B',str(shared/'network-preflight.py'),str(run/'network-preflight'),str(HERE/'produce.py'),*collect[4:]]
   # collect begins [python,-I,-B,produce.py]; preflight execs that exact producer.
   save(run/'container-command.json',{'argv':command,'name':name,'expectedMounts':mounts,'runtime':layout.RUNTIME})
   child=None;code=2
   try:
    with (run/'container.stdout').open('x') as so,(run/'container.stderr').open('x') as se:
     with common.deferred_signals():child=subprocess.Popen(command,stdout=so,stderr=se,start_new_session=True);common.LIVE_GROUPS.add(child.pid)
     # Startup and collection share one budget; a slow create must not lose
     # its running inspection just because an unrelated poll count expired.
     deadline=time.monotonic()+CASE_TIMEOUT
     while child.poll() is None and time.monotonic()<deadline:
      try:inspection=subprocess.run(['docker','inspect',name],capture_output=True,text=True,timeout=min(5,max(.001,deadline-time.monotonic())))
      except subprocess.TimeoutExpired:continue
      if inspection.returncode==0:
       inspected=json.loads(inspection.stdout)
       # A created container is inspectable before its process has started.
       # Keep polling until the retained snapshot proves the live collector.
       state=inspected[0].get('State') if len(inspected)==1 and isinstance(inspected[0],dict) else None
       if isinstance(state,dict) and state.get('Running') is True and type(state.get('Pid')) is int and state['Pid']>0:
        save(run/'container-inspect.json',inspected);break
      if child.poll() is not None:break
      time.sleep(min(.05,max(0,deadline-time.monotonic())))
     try:code=child.wait(timeout=max(0,deadline-time.monotonic()))
     except subprocess.TimeoutExpired:code=124
   finally:
    with common.deferred_signals():
     if child is not None and child.poll() is None:subprocess.run(['docker','stop','--time','30',name],capture_output=True,timeout=45);child.wait(timeout=10)
     remaining=subprocess.check_output(['docker','ps','--filter','name=^/'+name+'$','--format','{{.ID}}'],text=True,timeout=10).strip()
     if child is not None and not remaining:common.LIVE_GROUPS.discard(child.pid)
     inspected=json.loads((run/'container-inspect.json').read_text()) if (run/'container-inspect.json').is_file() else []
     container_id=inspected[0].get('Id') if len(inspected)==1 else None
     save(run/'container-cleanup.json',{'name':name,'containerId':container_id,'ownedContainerStillRunning':remaining,'clientExitCode':None if child is None else child.returncode})
   assert not remaining,'owned container cleanup unresolved; retain lease'
   save(run/'container-result.json',{'exit':code,'sourceManifest':ref(a.source_manifest),'candidateManifest':ref(a.candidate_manifest)})
   try:
    subprocess.run([*base,'--mode','finalize','--profile',profile,'--engine',engine,'--run',str(run)],check=True)
    result=json.loads((run/'profile.json').read_text())
   except (OSError,subprocess.SubprocessError) as error:result={'id':profile+'-'+engine,'profile':profile,'engine':engine,'collectorExit':code,'missing':['complete-raw-and-owned-process-receipt'],'error':str(error)}
   results.append(result);save(run/'final-status.json',result)
   print(json.dumps({'profile':profile,'engine':engine,'exit':code,'finalized':'raw' in result}),flush=True)
# No mutable execution remains before producing the source-bound index.
layout_receipt={'schemaVersion':1,'packaging':ref(a.out/'deployment/deployment-receipt.json'),'baseline':{'path':str(a.baseline),'revision':'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'},'baselineDependencies':dependencies,'node':ref(node),'runtime':layout.RUNTIME,'producer':ref(Path(__file__)),'runs':[{'profile':x['profile'],'engine':x['engine'],'proof':x.get('layoutProof')} for x in results]}
save(a.out/'runtime-deployment.json',layout_receipt)
index={'schemaVersion':1,'sourceManifest':ref(a.source_manifest),'candidateManifest':ref(a.candidate_manifest),'runtimeLibraries':source['runtimeLibraries'],'baseline':layout_receipt['baseline'],'baselineDependencies':dependencies,'node':ref(node),'deployment':ref(a.out/'runtime-deployment.json'),'profiles':results,'producer':ref(Path(__file__)),'scope':'five existing Gateway HTTP entries; raw paths and ports identical; no new score denominator'}
save(a.out/'public-http.json',index)
assert sha(a.source_manifest)==a.source_sha
raise SystemExit(0 if all(x.get('collectorExit')==0 and 'raw' in x for x in results) else 1)
