#!/usr/bin/env python3
"""Capture one original #1314 scene, k5, with an actual TS host and stdio sidecar."""
import sys
if not sys.flags.isolated:
    raise SystemExit('use python3 -I')
import argparse,hashlib,importlib.util,json,os,subprocess
from collections import Counter
from pathlib import Path
sys.dont_write_bytecode=True
p=argparse.ArgumentParser();p.add_argument('--baseline',type=Path,required=True);p.add_argument('--node',type=Path,required=True);p.add_argument('--source-manifest',type=Path,required=True);p.add_argument('--binary-manifest',type=Path,required=True);p.add_argument('--out',type=Path,required=True);p.add_argument('--sidecar',choices=['ts','rust'],default='rust');p.add_argument('--case',required=True);p.add_argument('--lease-wait',type=float,default=10);a=p.parse_args()
root=Path(__file__).resolve().parents[2];out=a.out.resolve();out.mkdir(exist_ok=False,parents=True)
def sha(path):
 with Path(path).open('rb')as f:return hashlib.file_digest(f,'sha256').hexdigest()
def save(path,value):Path(path).write_text(json.dumps(value,indent=2)+'\n')
def load(name,path):
 spec=importlib.util.spec_from_file_location(name,path);module=importlib.util.module_from_spec(spec);sys.modules[name]=module;source=Path(path).read_bytes();exec(compile(source,str(path),'exec'),module.__dict__);module._apparatus_source=source;return module
source=json.loads(a.source_manifest.read_text());head=subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip();assert head==source['head'];assert not subprocess.check_output(['git','-C',str(root),'status','--porcelain','--untracked-files=no'],text=True).strip()
for row in source['files']:
 relative=Path(row['path']);assert not relative.is_absolute()and'..'not in relative.parts;assert sha(root/relative)==row['sha256'],str(relative)
programs=json.loads(a.binary_manifest.read_text());programs=programs if isinstance(programs,list)else programs['binaries'];assert{row['path']for row in programs}=={'pilotdeck','pilotdeck-control','pilotdeck-gateway','pilotdeck-agent-loop-sidecar'}
for row in programs:assert sha(a.binary_manifest.parent/row['path'])==row['sha256']
node=a.node.resolve(strict=True);baseline=a.baseline.resolve(strict=True);assert subprocess.check_output(['git','-C',str(baseline),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
assert not subprocess.check_output(['git','-C',str(baseline),'status','--porcelain','--untracked-files=no'],text=True).strip()
versions=json.loads(subprocess.check_output([str(node),'-p','JSON.stringify(process.versions)'],text=True));assert[versions[k]for k in ['node','icu','uv']]==['22.22.0','78.2','1.52.1']
adapter=Path(__file__).with_name('host-sidecar-adapter.mjs');harness=adapter.with_name('six_cells.mts');isolation=out/'isolation';env={k:v for k,v in os.environ.items()if k in ['PATH','LANG','LC_ALL']};env['PATH']='/usr/bin:/bin'
for key in ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME']:
 directory=isolation/key.lower();directory.mkdir(parents=True);env[key]=str(directory)
case=a.case
registration=json.loads((root/'scripts/gateway-parity/fixtures/session-calibration.json').read_text())
assert len(registration['cases'])==72 and case in registration['cases'], 'select one original registered case'
repeats=5
if a.sidecar=='rust':command=str((a.binary_manifest.parent/'pilotdeck-agent-loop-sidecar').resolve());args=[]
else:command=str(node);args=['--import',str(adapter.with_name('host-sidecar-source-loader.mjs')),str(baseline/'src/cli/pilotdeck-agent-loop-sidecar.ts')]
spec={'language':a.sidecar,'command':command,'args':args};save(out/'spec.json',spec)
env.update(A52_ISOLATION=str(isolation),A52_BASELINE=str(baseline),A52_OUTPUT=str(out/'raw'),A52_TMP=str(isolation/'measurements'),PILOTDECK_SKIP_BROWSER_OPEN='1',SKIP_BROWSER_OPEN='1',UV_USE_IO_URING='0',A19_HOST_ADAPTER=str(adapter),A19_HOST_SOURCE_ROOT=str(root),A19_HOST_BASELINE=str(baseline),A19_HOST_SIDECAR_SPEC=str(out/'spec.json'),A19_HOST_CASE=case)
for name in ['host-sidecar-adapter.mjs','host-sidecar-stimulus.mjs','host-sidecar-source-loader.mjs','six_cells.mts']:
 with(out/name).open('xb')as artifact:artifact.write(adapter.with_name(name).read_bytes())
input_contract=adapter.with_name('six_cells.mts');reference_case=case
witness=load('core_host_witness',root/'scripts/native-witness/witness.py');session=witness.frozen_auditor();git=Path('/usr/bin/git').resolve()
# The unchanged original apparatus uses this one Git probe before TS imports.
# It is explicitly witnessed, not hidden or mistaken for a sidecar.
contract={'host':'ts','sidecarLanguage':a.sidecar,'project':str(out),'externalEndpoints':[],'binaries':{
 'host':witness.identity(node,[str(node),'--experimental-transform-types',str(harness)]),
 'sidecar':witness.identity(command,[command,*args]),
 'baselineGitProbe':{'path':str(git),'argv':['git','-C',str(baseline),'rev-parse','HEAD'],'sha256':sha(git)},'worktreeGitProbe':{'path':str(git),'argv':['git','-C',str(root),'rev-parse','HEAD'],'sha256':sha(git)}}}
preflight={'head':head,'sourceManifest':{'path':str(a.source_manifest.resolve()),'sha256':sha(a.source_manifest)},'binaryManifest':{'path':str(a.binary_manifest.resolve()),'sha256':sha(a.binary_manifest)},'allFourProgramsVerified':True,'sourceFilesVerified':len(source['files']),'adapterSha256':sha(adapter),'producerSha256':sha(__file__),'baselineCleanTracked':True,'controlledEnvironment':{'UV_USE_IO_URING':'0','reason':'UV flag alone did not prevent ring setup; actual Docker default seccomp denial is independently recorded'},'case':case,'referenceCase':reference_case,'inputContractSha256':sha(input_contract),'seccomp':next(line for line in Path('/proc/self/status').read_text().splitlines()if line.startswith('Seccomp:')),'networkNamespace':os.readlink('/proc/self/ns/net'),'repeats':repeats};save(out/'preflight.json',preflight)
with session.common.verified_parity_lock(timeout=a.lease_wait):
 launch,evidence=witness.capture(contract,out/'capture',env,timeout=min(60,25*repeats),ownership=session.common.LIVE_GROUPS)
 raw,launch,evidence=witness.load_capture(out/'capture');checked=witness.checked_evidence(raw,launch,evidence);assert checked['cleanup']['allStopped']is True;assert all(checked[key]['complete'] is True for key in ['execWitness','networkWitness','transcriptWitness']);assert checked['networkWitness']['calls']==[];assert Counter(process['role']for process in checked['processes'])==Counter({'host':1,'sidecar':repeats,'baselineGitProbe':1,'worktreeGitProbe':1}), 'actual process count differs from declared independent rounds'
 assert sha(adapter)==preflight['adapterSha256'];assert sha(input_contract)==preflight['inputContractSha256'];assert sha(__file__)==preflight['producerSha256']
 summary=json.loads((out/'raw/manifest.json').read_text());assert summary['executedCases']==[case];assert len(summary['expectedCases'])==72;assert summary['totalMeasurements']==5;assert len(summary['missingCases'])==71
 for imported in summary['importedSources']:
  original=subprocess.check_output(['git','-C',str(baseline),'show','cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85:'+imported['path']]);assert hashlib.sha256(original).hexdigest()==imported['sha256'];assert sha(baseline/imported['path'])==imported['sha256']
 rows=json.loads((out/'raw'/f"{case.split('/')[0]}.json").read_text())['observations'];assert len(rows)==repeats
 transcript_opens={row['path'] for row in checked['transcriptWitness']['opens']}
 for observation in rows:
  assert observation['id']==case+'/'+str(observation['repetition'])
  transcript=(root/observation['durableStatus']['source']).resolve()
  assert transcript.is_relative_to(out) and transcript.read_text()==observation['transcriptRawText'] and str(transcript) in transcript_opens
  actual={key:observation[key] for key in ['frames','durableStatus','registry']};assert session._native_observation_structure(actual)is None
 receipt=out/'capture/receipt.json'
 result={'status':'HOST_CAPTURE_PARTITION','case':case,'repeats':repeats,'host':'ts','sidecar':a.sidecar,
         'receipt':{'path':str(receipt),'sha256':sha(receipt)},'rawSha256':{path.name:sha(path) for path in (out/'raw').glob('*.json')},
         'preflight':{'path':str(out/'preflight.json'),'sha256':sha(out/'preflight.json')},
         'expectedWholeHostCases':72,'wholeHost':'NOT_RUN'}
 save(out/'host-capture.json',result)
print(json.dumps(result,indent=2))
