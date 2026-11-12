# anchor-selfcheck: unsupported observation sidecar finalizer, no product source mutations
"""Finalize truthful sidecar bindings after the fixture exits and Docker is cleaned.
Run on cb8. Missing observations remain missing/false; never infer process ownership
from producer success or fill an empty trace with claimed PASS values.
"""
import argparse,hashlib,json,os,stat
from pathlib import Path
p=argparse.ArgumentParser()
for key in ['channel','observation-dir','raw','producer','source-manifest','candidate-manifest','network-preflight','container-cleanup','home','project','out']:
 p.add_argument('--'+key,required=True,type=str if key=='channel' else Path)
for key in ['original-home','original-project','preservation-receipt']:p.add_argument('--'+key,type=Path)
a=p.parse_args();assert bool(a.original_home)==bool(a.original_project)==bool(a.preservation_receipt);sha=lambda f:hashlib.sha256(f.read_bytes()).hexdigest()
def ref(f):return {'path':str(f.resolve()),'sha256':sha(f)}
records=json.loads((a.observation_dir/'owned-processes.json').read_text());errors=[]
if len(records)!=1:errors.append('expected exactly one observed actual Server process')
r=records[0] if len(records)==1 else {};listeners={}
for row in r.get('observations',[]):
 for listener in row.get('readyListeners',[]):listeners[(listener['host'],listener['port'],listener['ownerPid'])]=listener
files=[];symlinks=[]
for root in [a.home,a.project]:
 if not root.is_dir():errors.append('missing owned business directory '+str(root));continue
 for folder,dirs,names in os.walk(root,followlinks=False):
  for name in names:
   f=Path(folder)/name;info=f.lstat()
   if stat.S_ISLNK(info.st_mode):symlinks.append({'path':str(f),'target':os.readlink(f)});continue
   if not stat.S_ISREG(info.st_mode):errors.append('non-regular owned business file '+str(f));continue
   row={'path':str(f.resolve()),'sha256':sha(f),'bytes':info.st_size}
   if a.preservation_receipt:
    original=a.original_home if root==a.home else a.original_project
    row['originalPath']=str(original/f.relative_to(root))
   files.append(row)
closed=r.get('closeObservedAt') is not None and r.get('identityStillPresent') is False
ports_closed=closed and bool(listeners) and r.get('portsStillListening')==[]
remaining=r.get('remainingOwnedIdentities')
if remaining is None:errors.append('missing actual post-close owned identity observation')
if r.get('errors'):errors.extend(r['errors'])
raw=ref(a.raw);candidate=ref(a.candidate_manifest)
result={'schemaVersion':1,'channel':a.channel,'sourceManifest':ref(a.source_manifest),'candidateManifest':candidate,'runtimeLibraries':ref(a.candidate_manifest.parent/'libraries.json'),'producer':ref(a.producer),'finalizer':ref(Path(__file__)),'rawEvidence':raw,'process':{'pid':r.get('pid'),'argv':r.get('argv'),'executable':r.get('executable'),'birth':r.get('birth'),'cwd':r.get('cwd'),'pidNamespace':r.get('pidNamespace'),'networkNamespace':r.get('networkNamespace'),'readyListeners':list(listeners.values()),'exit':r.get('exit'),'reaped':closed,'portsClosed':ports_closed,'remainingOwnedIdentities':remaining},'afterReapFiles':files,'networkPreflight':ref(a.network_preflight),'containerCleanup':ref(a.container_cleanup),'observationTrace':ref(a.observation_dir/'owned-processes.json'),'observer':ref(a.observation_dir/'observer-source.json'),'transportTrace':ref(a.observation_dir/'transport.jsonl'),'symlinksNotFollowed':symlinks,'bindingErrors':errors,'scope':'actual owned process and mock transport observations; not a claim of full behavior coverage'}
if a.preservation_receipt:
 preserved=json.loads(a.preservation_receipt.read_text());by_path={row['path']:row for row in preserved['files']}
 for row in files:
  previous=by_path.get(row['path']);assert previous and previous['sha256']==row['sha256'] and previous['originalPath']==row['originalPath']
 result['preservation']=ref(a.preservation_receipt);result['preserver']=preserved['preserver'];result['originalBusinessRoots']={'home':str(a.original_home),'project':str(a.original_project)}
inspection=a.container_cleanup.parent/'container-inspect.json'
if inspection.is_file():result['containerInspection']=ref(inspection)
a.out.parent.mkdir(parents=True,exist_ok=True)
with a.out.open('x') as f:f.write(json.dumps(result,indent=2)+'\n')
print(json.dumps({'channel':a.channel,'path':str(a.out),'sha256':sha(a.out),'bindingErrors':errors,'reaped':closed,'portsClosed':ports_closed,'remainingObservedOwnedIdentities':remaining}))
