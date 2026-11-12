"""Five existing public HTTP entries: source-bound raw business and owned lifecycle."""
# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)
# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution.  It must run before every other standard-library import: a
# shadowed argparse/pathlib would otherwise execute before this check.  ``sys``
# is builtin; os is imported only after the frozen-module preflight below.
import sys

if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import importlib.util
from pathlib import Path
import tempfile

# -B only stops cache writes, so redirect the bytecode prefix too (fix-r5).
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')
sys.dont_write_bytecode = True
# CPython has already compiled the CLI entry before it can acquire a lease.
# Capture that executing object, not a later read of the restored entry file.
_apparatus_code = sys._getframe().f_code

# Load the one guard by explicit path, never sys.path: a same-named package or
# extension module beside the source would win before the scan (fix-r6 P1).
# Inline on purpose: importing the guard is what this block defends.
_here = Path(__file__).resolve().parent
_apparatus = _here.parent if _here.name == 'controls' else _here
_guard_path = _apparatus / '_guard.py'
_guard = sys.modules.get('_guard')
if not (getattr(_guard, '__file__', None)
        and Path(_guard.__file__).resolve() == _guard_path
        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):
    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)
    _guard = importlib.util.module_from_spec(_spec)
    sys.modules['_guard'] = _guard
    _guard_source = _guard_path.read_bytes()
    try:
        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)
    except BaseException:
        sys.modules.pop('_guard', None)
        raise
    _guard._apparatus_source = _guard_source
if Path(_guard.__file__).resolve() != _guard_path:
    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')

_load = _guard.load
_has_git_checkout = _guard._has_git_checkout
_verify_cli_imports = _guard.verify_cli_imports
run_entry = _guard.run_entry
common = _load('common', _apparatus / 'common.py')
# <<< apparatus bootstrap
import atexit,hashlib,json,re,shutil,subprocess
from http.client import HTTPException
from pathlib import Path
atexit.register(shutil.rmtree,sys.pycache_prefix,ignore_errors=True)
IDS=('gateway.http:ANY /feishu/webhook',*(f'gateway.http:POST /api/module-host/{name}' for name in ('describe','call','approvals/status','approvals/resume')))
PROFILES=('configured','skills-disabled','approval-unconfigured','feishu')
def sha(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def ref(path):return {'path':str(Path(path).resolve()),'sha256':sha(path)}
def artifact(d,value):
 p=Path(value['path'])
 if not p.is_file():raise FileNotFoundError(str(p))
 d.require(p.is_absolute() and p.is_file() and not p.is_symlink() and p.resolve()==p and p.is_relative_to(d.ROOT/'_tmp'),'PUBLIC_HTTP_RAW','missing or aliased public HTTP original');contracts=d._load('retirement_public_http_contracts',d.ROOT/'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/contracts.py');contracts.pinned_bytes(p,value['sha256']);return p

def authenticated_contracts(d,native):
 """Authenticate the bytes actually executed, including cached fixture code."""
 path=d.ROOT/'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/contracts.py'
 contracts=d._load('retirement_public_http_contracts',path)
 expected=[row['sha256'] for row in native.source['files'] if row['path']==str(path.relative_to(d.ROOT))]
 executed=getattr(contracts,'_apparatus_source',None)
 d.require(len(expected)==1 and isinstance(executed,bytes)
           and Path(contracts.__file__).resolve()==path.resolve()
           and hashlib.sha256(executed).hexdigest()==expected[0]
           and sha(path)==expected[0],
           'PUBLIC_HTTP_CONTRACT_SOURCE','executed contract source differs from candidate manifest')
 return contracts

def verify_package(d,native,deployment_ref):
 """Permit only this source's byte-identical new install-shaped deployment."""
 authenticated_contracts(d,native)
 file=artifact(d,deployment_ref);value=d.read(file);root=Path(value['root']);tracked={str(d.ROOT/x['path']):x['sha256'] for x in native.source['files']}
 d.require(root.is_absolute() and root.resolve()==root and root.is_relative_to(d.ROOT/'_tmp') and root.is_dir(),'PUBLIC_HTTP_DEPLOYMENT','invalid package root')
 d.require(value['sourceHead']==native.source['head'] and value['sourceManifest']=={'path':str(native.source_manifest),'sha256':native.source_sha} and value['originalCandidate']==str(native.manifest.parent),'PUBLIC_HTTP_DEPLOYMENT_SOURCE','deployment source mismatch')
 producer=value['producer'];d.require(tracked.get(producer['path'])==producer['sha256'] and sha(producer['path'])==producer['sha256'],'PUBLIC_HTTP_DEPLOYMENT_PRODUCER','untracked packaging producer')
 manifest=root/'bin/manifest.json';libraries=root/'bin/libraries.json';original_libraries=Path(native.source['runtimeLibraries']['path'])
 d.require(manifest.read_bytes()==native.manifest.read_bytes() and libraries.read_bytes()==original_libraries.read_bytes() and sha(manifest)==value['binaryManifestSha256'] and sha(libraries)==value['librariesManifestSha256'],'PUBLIC_HTTP_DEPLOYMENT_MANIFEST','relocated manifests differ from source-bound originals')
 originals=[deployment_ref,ref(manifest),ref(libraries)];executables={};expected={'pilotdeck','pilotdeck-control','pilotdeck-gateway','pilotdeck-agent-loop-sidecar'}
 rows=d.read(native.manifest);d.require(len(rows)==4 and {x['path'] for x in rows}==expected,'PUBLIC_HTTP_DEPLOYMENT_BIN','four-bin set changed')
 for row in rows:
  p=root/'bin'/row['path'];d.require(p.is_file() and not p.is_symlink() and p.resolve()==p and sha(p)==row['sha256'],'PUBLIC_HTTP_DEPLOYMENT_BIN','relocated binary differs');executables[row['path']]=ref(p)
 librows=d.read(libraries);names=set();profiles=set()
 for row in librows:
  rel=Path(row['path']);d.require(len(rel.parts)==2 and rel.parts[0].startswith('pilotdeck-libs-') and '..' not in rel.parts,'PUBLIC_HTTP_DEPLOYMENT_LIBS','unsafe runtime path')
  p=root/'bin'/rel;d.require(p.is_file() and not p.is_symlink() and p.resolve()==p and p.stat().st_size==row['bytes'] and sha(p)==row['sha256'],'PUBLIC_HTTP_DEPLOYMENT_LIBS','copied runtime differs');names.add(str(rel));profiles.add(rel.parts[0])
 d.require(len(names)==len(librows) and len(profiles)==1,'PUBLIC_HTTP_DEPLOYMENT_LIBS','duplicate/empty runtime set')
 profile=next(iter(profiles));d.require({str(p.relative_to(root/'bin')) for p in (root/'bin'/profile).iterdir()}==names and {p.name for p in (root/'bin').iterdir()}==expected|{profile,'manifest.json','libraries.json'},'PUBLIC_HTTP_DEPLOYMENT_LIBS','unregistered package bytes')
 d.require((root/'skills').is_symlink() and (root/'skills').readlink()==Path('source/skills') and (root/'skills').resolve()==root/'source/skills','PUBLIC_HTTP_DEPLOYMENT_SKILLS','default installed skills mapping changed')
 skills=root/'source/skills';actual={str(p.relative_to(skills)):sha(p) for p in skills.rglob('*') if p.is_file()}
 d.require(actual==value['allSkillFiles'] and all(not p.is_symlink() for p in skills.rglob('*')) and all(tracked.get(str(d.ROOT/'skills'/rel))==digest for rel,digest in actual.items()),'PUBLIC_HTTP_DEPLOYMENT_SKILLS','skills differ from actual tracked source')
 assets=d.read(skills/'upstream-assets.json')['files'];d.require(len(assets)==109 and len({x['path'] for x in assets})==109 and value['assets109']==[{'path':x['path'],'sha256':sha(skills/x['path']),'bytes':(skills/x['path']).stat().st_size} for x in assets] and all(sha(skills/x['path'])==x['sha256'] and (skills/x['path']).stat().st_size==x['bytes'] for x in assets),'PUBLIC_HTTP_DEPLOYMENT_SKILLS','109 asset set changed')
 archive=subprocess.check_output(['git','-C',str(d.ROOT),'archive','--format=tar',native.source['head'],'skills','LICENSE'],timeout=60)
 d.require(hashlib.sha256(archive).hexdigest()==value['sourceArchiveSha256'] and sha(root/'source/LICENSE')==tracked.get(str(d.ROOT/'LICENSE')),'PUBLIC_HTTP_DEPLOYMENT_SOURCE','archive or license source changed')
 return {'executables':executables,'originals':originals,'skills':str(root/'skills'),'skillHashes':actual}

def verify_deployment(d,native,deployment_ref):
 """Certify logical container paths only from the actual retained Docker mounts."""
 authenticated_contracts(d,native)
 file=artifact(d,deployment_ref);value=d.read(file);package=verify_package(d,native,value['packaging'])
 tracked={str(d.ROOT/x['path']):x['sha256'] for x in native.source['files']}
 producer=value['producer'];d.require(tracked.get(producer['path'])==producer['sha256'] and sha(producer['path'])==producer['sha256'],'PUBLIC_HTTP_LAYOUT_SOURCE','untracked layout producer')
 runtime={'source':'/app/source','bin':'/app/source/bin','home':'/app/home','project':'/app/project','tmp':'/app/tmp','workspaces':'/app/workspaces','controlPort':28610,'providerPort':28690}
 d.require(value['runtime']==runtime,'PUBLIC_HTTP_LAYOUT','runtime layout changed')
 baseline=Path(value['baseline']['path']);node=Path(value['node']['path']);root=Path(d.read(artifact(d,value['packaging']))['root'])
 d.require(value['baseline']['revision']=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85' and baseline.is_absolute() and baseline.resolve()==baseline and subprocess.check_output(['git','-C',str(baseline),'rev-parse','HEAD'],text=True).strip()==value['baseline']['revision'] and not subprocess.check_output(['git','-C',str(baseline),'status','--porcelain'],text=True).strip(),'PUBLIC_HTTP_LAYOUT_BASELINE','TS source is not fixed and clean')
 d.require(node.is_absolute() and node.resolve()==node and node.is_file() and sha(node)==value['node']['sha256'],'PUBLIC_HTTP_LAYOUT_NODE','Node binary changed')
 dependencies=(baseline/'node_modules').resolve(strict=True)
 d.require(dependencies.is_relative_to(baseline) and value['baselineDependencies']=={'path':str(dependencies),'sha256':d.common.tree_digest(dependencies)},'PUBLIC_HTTP_LAYOUT_DEPS','TS dependency tree changed')
 packaging=d.read(artifact(d,value['packaging']));critical={x['path'] for x in packaging['assets109']}|{str(x.relative_to(root/'source/skills')) for x in (root/'source/skills').glob('*/SKILL.md')}
 d.require(set(packaging['preservedMtimes'])==critical,'PUBLIC_HTTP_LAYOUT_MTIME','incomplete preserved source timestamps')
 for rel,row in packaging['preservedMtimes'].items():
  source=baseline/'skills'/rel;dest=root/'source/skills'/rel
  d.require(row=={'sourcePath':str(source.resolve()),'sha256':sha(source),'mtimeNs':source.stat().st_mtime_ns} and sha(dest)==sha(source) and dest.stat().st_mtime_ns==source.stat().st_mtime_ns,'PUBLIC_HTTP_LAYOUT_MTIME','skill source bytes/timestamp changed')
 d.require(len(value['runs'])==8 and {(x['profile'],x['engine']) for x in value['runs']}=={(p,e) for p in PROFILES for e in ('ts','native')},'PUBLIC_HTTP_LAYOUT_RUNS','exact eight isolated runs required')
 originals=[deployment_ref,*package['originals']];runs={}
 for row in value['runs']:
  proof=row['proof'];command_file=artifact(d,proof['command']);run=command_file.parent;command=d.read(command_file);inspections=d.read(artifact(d,proof['inspect']));cleanup=d.read(artifact(d,proof['cleanup']))
  d.require(len(inspections)==1,'PUBLIC_HTTP_LAYOUT_INSPECT','one owned container required');inspection=inspections[0];config=inspection['Config'];host=inspection['HostConfig']
  state=inspection.get('State',{})
  d.require(isinstance(state,dict) and state.get('Running') is True and type(state.get('Pid')) is int and state['Pid']>0,'PUBLIC_HTTP_LAYOUT_INSPECT','container inspection was not captured while running')
  d.require(inspection['Name']=='/'+command['name'] and cleanup['name']==command['name'] and cleanup['ownedContainerStillRunning']=='' and type(cleanup['clientExitCode']) is int and cleanup['clientExitCode']==0,'PUBLIC_HTTP_LAYOUT_CLEANUP','owned container did not reap cleanly')
  d.require(host['NetworkMode']=='none' and host['ReadonlyRootfs'] is True and config['WorkingDir']=='/app/project' and config['Image']=='sha256:eb58eb269804b8a9b2bb9ceeb9a175cac9b79612d4c8dafe0a987b277bd55683','PUBLIC_HTTP_LAYOUT_ISOLATION','container profile changed')
  actual={x['Destination']:(x['Source'],not x['RW']) for x in inspection['Mounts'] if x['Type']=='bind'}
  expected={str(x):(str(x),True) for x in [Path('/usr'),Path('/lib'),Path('/etc/ld.so.cache'),d.ROOT,baseline,node.parent.parent,Path('/home/clawbox-08/opt/libuv-1.52.1')]}
  expected.update({'/app/source':(str(baseline),True),'/app/source/bin':(str(root/'bin'),True),str(run):(str(run),False)})
  if row['engine']=='native':expected['/app/source/skills']=(str(root/'source/skills'),True)
  expected.update({runtime[x]:(str(run/x),False) for x in ('home','project','tmp','workspaces')})
  d.require(actual==expected and len(actual)==len([x for x in inspection['Mounts'] if x['Type']=='bind']),'PUBLIC_HTTP_LAYOUT_MOUNTS','actual mount mapping differs from fixed deployment')
  argv=config['Cmd'];preflight=str(d.ROOT/'src/pilotdeck-gateway/tests/fixtures/native-channel-production/network-preflight.py');collector=str(d.ROOT/'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/produce.py')
  d.require(argv[:7]==['/usr/libexec/docker/docker-init','--','/usr/bin/python3','-I','-B',preflight,str(run/'network-preflight')] and argv[7]==collector and '--mode' in argv and argv[argv.index('--mode')+1]=='collect' and '--engine' in argv and argv[argv.index('--engine')+1]==row['engine'] and '--profile' in argv and argv[argv.index('--profile')+1]==row['profile'],'PUBLIC_HTTP_LAYOUT_COMMAND','actual container collector argv differs')
  for path_ in (preflight,collector):d.require(tracked.get(path_)==sha(path_),'PUBLIC_HTTP_LAYOUT_SOURCE','untracked collector/preflight')
  runs[(row['profile'],row['engine'])]=str(run);originals.extend(proof.values())
 executables={role:{'path':'/app/source/bin/'+role,'hostPath':r['path'],'sha256':r['sha256']} for role,r in package['executables'].items()}
 executables['ts-node']={'path':str(node),'hostPath':str(node),'sha256':value['node']['sha256']}
 preload=d.ROOT/'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/feishu-preload.mjs'
 return {**package,'executables':executables,'originals':originals,'runs':runs,'baseline':{'path':'/app/source','hostPath':str(baseline)},'preloads':[{'path':str(preload),'hostPath':str(preload),'sha256':sha(preload)}],'node':value['node'],'baselineDependencies':value['baselineDependencies']}


def http_projection(contracts,rows,records,normalizer,*,feishu=False,pairs=None):
 """Project the exact request-bound wire exchange, never a body-only match."""
 origins={'http://127.0.0.1:28610'}
 if pairs is None:pairs=contracts.driver_wire(rows,records,origins,feishu=feishu)
 wires=contracts.bind_raw_http(pairs,records,origins)
 contracts.need(len(rows)==len(wires),'HTTP_RAW_ROW_COUNT')
 result=[];excluded={'date','connection','keep-alive','transfer-encoding'}
 for row,wire in zip(rows,wires):
  request_,response=wire['request'],wire['response'];headers=response['headers'];text=response['body'].decode('utf-8')
  types=headers.get('content-type',[]);contracts.need(len(types)==1,'HTTP_CONTENT_TYPE')
  content=text if types[0].split(';')[0] in ('application/x-ndjson','text/event-stream') else normalizer.value(json.loads(text))
  result.append({'method':request_['method'],'path':request_['target'],'requestLine':request_['line'],
      'requestHeaders':request_['headers'],'requestBodyHex':request_['body'].hex(),'requestTrailers':request_['trailers'],
      'status':response['status'],'statusLine':response['statusLine'],
      'headers':{k:v for k,v in sorted(headers.items()) if k not in excluded},
      'trailers':response['trailers'],'informational':[{**item,'headers':{k:v for k,v in item['headers'].items() if k not in excluded}} for item in response['informational']],
      'response':content})
 return result


def report(d,native,baseline,capture,rounds):
 results={key:{'id':key,'status':'BLOCKED','detail':{'missing':['current-head-public-http-capture']},'originals':[]} for key in IDS}
 if capture is None:return results
 path=Path(capture);path=path/'public-http.json' if path.is_dir() else path
 if not path.is_file():return results
 originals=[];missing=[]
 try:
  contracts=authenticated_contracts(d,native);compare=d._load('compare',d.SCRIPT/'compare.py');_verify_cli_imports(_apparatus);value=d.read(path)
  d.require(value['schemaVersion']==1,'PUBLIC_HTTP_SCHEMA','unsupported capture');artifact(d,value['sourceManifest']);artifact(d,value['candidateManifest'])
  if value['sourceManifest']!={'path':str(native.source_manifest),'sha256':native.source_sha} or value['candidateManifest']!={'path':str(native.manifest),'sha256':native.manifest_sha}:return results
  d.require(value['runtimeLibraries']==native.source['runtimeLibraries'],'PUBLIC_HTTP_SOURCE','runtime inventory differs')
  deployment=verify_deployment(d,native,value['deployment']);originals.extend(deployment['originals']);baseline=Path(baseline)
  d.require(value['baseline']=={'path':str(baseline),'revision':'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'} and deployment['baseline']['hostPath']==str(baseline) and value['node']==deployment['node'] and value['baselineDependencies']==deployment['baselineDependencies'],'PUBLIC_HTTP_TS_SOURCE','fixed TS binding differs')
  entries=d._load('retirement_entries',d.SCRIPT/'retirement_entries.py');business={};profiles={};tracked={str(d.ROOT/x['path']):x['sha256'] for x in native.source['files']}
  d.require(len(value['profiles'])==8 and {(x['profile'],x['engine']) for x in value['profiles']}=={(p,e) for p in PROFILES for e in ('ts','native')},'PUBLIC_HTTP_PROFILES','exact eight profiles required')
  cases=d.read(d.ROOT/'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/feishu-cases.json')['cases'];seed=d.read(d.ROOT/'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/approval-seed.json')
  for item in value['profiles']:
   key=(item['profile'],item['engine']);run=Path(deployment['runs'][key]);d.require(item['home']==str(run/'home') and item['projectFiles']==str(run/'project') and type(item['collectorExit']) is int and item['collectorExit']==0,'PUBLIC_HTTP_RUN','run paths or exit differ')
   raw_file=artifact(d,item['raw']);d.require(raw_file.is_relative_to(run),'PUBLIC_HTTP_RAW','raw belongs to a different run');raw=d.read(raw_file);prov=d.read(artifact(d,item['provenance']));originals.extend([item['raw'],item['provenance']])
   d.require(prov['sourceManifestSha256']==native.source_sha and prov['engine']==item['engine'] and prov['baseline']=='/app/source' and prov['nativeHead']==native.source['head'] and prov['candidateManifestSha256']==native.manifest_sha,'PUBLIC_HTTP_PROFILE_SOURCE','profile source differs')
   for producer in item['sources']:d.require(tracked.get(producer['path'])==producer['sha256'] and sha(producer['path'])==producer['sha256'],'PUBLIC_HTTP_PRODUCER','source producer changed')
   kwargs={'deployment_ref':value['deployment']}
   if item['engine']=='ts':kwargs.update(executable_role='ts-node',node_ref=value['node'],baseline=baseline,preload_refs=item.get('preloads',[]))
   process=entries.verify_owned_process(d,native,item['processReceipt'],item['raw'],'public-http:'+item['profile']+':'+item['engine'],**kwargs);missing.extend(process['missing']);originals.extend(process['originals']);records=process['transportRecords']
   expected_argv=(['/app/source/bin/pilotdeck-gateway','--native','--home','/app/home','--project','/app/project','--listen','127.0.0.1:28610','--token-file','/app/home/token','--sidecar','/app/source/bin/pilotdeck-agent-loop-sidecar','--drain-ms','2000'] if item['engine']=='native' else [value['node']['path'],*(['--import',str(d.ROOT/'src/pilotdeck-gateway/tests/fixtures/retirement-public-http/feishu-preload.mjs')] if item['profile']=='feishu' else []),'--import','/app/source/node_modules/tsx/dist/loader.mjs','/app/source/src/cli/pilotdeck.ts','server'])
   d.require(process['receipt']['process']['argv']==expected_argv and process['receipt']['process']['cwd']=='/app/project' and {x['port'] for x in process['receipt']['process']['readyListeners']}=={28610},'PUBLIC_HTTP_PROCESS_ARGV','full owned Server argv/cwd/listener differs')
   normalizer=compare.entity_normalizer(run,{});normalizer.index(raw);outbound=None
   if item['profile']=='feishu':
    checked=contracts.feishu_contract(raw,cases);rows=[r for r in raw['steps'] if r.get('event')=='configured-http-webhook'];pairs=contracts.driver_wire(rows,records,{'http://127.0.0.1:28610'},feishu=True);outbound=contracts.feishu_outbound_wire(raw,records)
   else:
    models=d.read(artifact(d,item['models']));formal=d.read(artifact(d,item['formalRequests']));wire=[json.loads(x) for x in artifact(d,item['providerWire']).read_text().splitlines()];outbound=contracts.provider_wire(models,formal,wire,expected_public=1 if item['profile']=='configured' else 0)
    config=run/'home/pilotdeck.yaml';files={x['path']:x for x in process['afterReapFiles']};d.require(str(config) in files and artifact(d,files[str(config)])==config,'PUBLIC_HTTP_CONFIG','missing post-reap config')
    origins=set(re.findall(r'http://127\.0\.0\.1:\d+',config.read_text()));d.require(origins=={'http://127.0.0.1:28690'},'PUBLIC_HTTP_CONFIG','mock origin differs')
    skills=deployment['skills'] if item['engine']=='native' else str(baseline/'skills');context={'project':'/app/project','mockOrigin':'http://127.0.0.1:28690','skills':skills,'skillsDisplay':'/app/source/skills','skillHashes':deployment['skillHashes'],'seedCreatedAt':seed['wait']['createdAt']}
    if item['profile']=='configured':
     durable=run/'home/staffdeck/binding/state/sessions/d2ViOnNvcC1maXh0dXJl.json';d.require(str(durable) in files,'PUBLIC_HTTP_DURABLE','missing post-reap SOP');context['durable']=d.read(artifact(d,files[str(durable)]))
    checked=contracts.module_contract(raw,item['profile'],context,models);rows=raw['hostRows']+raw['approvalRows'] if item['profile']=='configured' else raw['rows'];pairs=contracts.driver_wire(rows,records,{'http://127.0.0.1:28610'})
   business[key]={'contract':normalizer.value(checked),'http':http_projection(contracts,rows,records,normalizer,feishu=item['profile']=='feishu',pairs=pairs),'disk':compare.disk(run,normalizer)}
   if outbound is not None:business[key]['outbound']=normalizer.value(outbound)
   profiles[key]=item
  unequal=[p for p in PROFILES if contracts.canonical(business[(p,'ts')])!=contracts.canonical(business[(p,'native')])]
  d.require(not unequal,'PUBLIC_HTTP_BEHAVIOR','TS/native raw headers/body/home/project differ: '+str(unequal))
  if not rounds or not all(x.get('nativeWitness',{}).get('status')=='PASS' for x in rounds):missing.append('same-head-all-rounds-nativeWitness-PASS')
  for row in results.values():row.update(status='BLOCKED' if missing else 'PASS',detail={'profiles':sorted(p+'-'+e for p,e in profiles),'missing':sorted(set(missing)),'scope':'five retained entries; frozen metadata only; full HTTP headers and home/project'},originals=originals)
 except (FileNotFoundError,KeyError) as error:
  for row in results.values():row.update(status='BLOCKED',detail={'missing':['complete-public-http-originals-and-required-fields'],'error':str(error)},originals=originals)
 except (ValueError,TypeError,OSError,AssertionError,RuntimeError,IndexError,AttributeError,OverflowError,HTTPException,subprocess.SubprocessError) as error:
  for row in results.values():row.update(status='FAIL',detail={'rule':getattr(error,'rule','PUBLIC_HTTP_INVALID_EVIDENCE'),'error':str(error)},originals=originals)
 return results
