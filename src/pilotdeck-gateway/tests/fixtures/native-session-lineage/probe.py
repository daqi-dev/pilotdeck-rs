#!/usr/bin/env python3
# anchor-selfcheck: unsupported Native lineage integration probe; no source mutation table.
"""Unfrozen product lineage WS regression reusing owned-process business harness."""
from pathlib import Path
import hashlib,sys,textwrap
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[4]
harness=ROOT/'src/pilotdeck-gateway/tests/fixtures/native-business-loop/probe.py'
source=harness.read_text()
assert hashlib.sha256(source.encode()).hexdigest()=='ee43dbb95e8be6dce2b21663e4564ced000e75aca18eeeb64b68ab56a32c8bde'
def replace_once(before,after):
 global source
 assert source.count(before)==1,('reuse anchor changed',before)
 source=source.replace(before,after,1)
replace_once("sha(HERE/'driver.mts')","sha(HERE/'capture.mts')")
replace_once('ensure_ascii=False','ensure_ascii=True')
start=source.index("  for side in ('ts','rust'):")
end=source.index('  assert not errors,errors;',start)
replacement='''  for side in ('ts','rust'):
   config(provider.server_port);rounddir=a.out/side;rounddir.mkdir();port=None
   for candidate in range(28670,28700):
    try:
     with socket.socket() as test:test.bind(('127.0.0.1',candidate));port=candidate;break
    except OSError:continue
   assert port;env=env_for(port)
   argv=['node',str(a.baseline/'dist/src/cli/pilotdeck.js'),'server'] if side=='ts' else [a.gateway,'--native','--home',home,'--project',project,'--listen',f'127.0.0.1:{port}','--token-file',home/'token','--sidecar',a.sidecar,'--drain-ms','2000']
   host=spawn(argv,env,rounddir);wait_ready(host,port)
   cmd=['node','--import',str(a.baseline/'node_modules/tsx/dist/loader.mjs'),str(HERE/'capture.mts'),'--baseline',str(a.baseline),'--out',str(rounddir/'driver'),'--url',f'ws://127.0.0.1:{port}/ws','--home',str(home),'--project',str(project),'--token-file',str(home/'token')]
   driver=spawn(cmd,env,rounddir);assert driver.wait(timeout=100)==0,('lineage WS driver failed',side)
   capture=json.loads((rounddir/'driver/capture.json').read_text());assert len(capture['observations'])==4
   halt(host);host=None
   host=spawn(argv,env,rounddir);wait_ready(host,port)
   cold_cmd=[*cmd];cold_cmd[cold_cmd.index('--out')+1]=str(rounddir/'cold');cold_cmd.extend(['--phase','verify-cold','--state',str(rounddir/'driver/capture.json')])
   driver=spawn(cold_cmd,env,rounddir);assert driver.wait(timeout=100)==0,('cold recovery WS driver failed',side)
   halt(host);host=None;checks.append({'side':side,'pass':True,'coldRecovery':True,'wire':str(rounddir/'driver/wire.jsonl'),'capture':str(rounddir/'driver/capture.json')})
   home.rename(a.out/(side+'-home'));home.mkdir()
'''
source=source[:start]+replacement+source[end:]
start=source.index('  # Compare factual protocol projections')
end=source.index('\nfinally:',start)
comparison='''  import re
  projected={}
  for label in ('ts','rust'):
   capture=json.loads((a.out/label/'driver/capture.json').read_text());ids={}
   def normalize(value,key=''):
    if key in ('createdAt','updatedAt','preparedAt'):return '<time>'
    if key=='pid':return '<owner-pid>'
    if isinstance(value,dict):return {k:normalize(v,k)for k,v in value.items()}
    if isinstance(value,list):return [normalize(v)for v in value]
    if isinstance(value,str):
     def identity(m):
      token=m.group(0)
      if token not in ids:ids[token]='id-'+str(len(ids))
      return ids[token]
     return re.sub(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',identity,value)
    return value
   projected[label]=[]
   for row in capture['observations']:
    if row['case']=='fork':
     aux=[{k:v for k,v in item.items()if k not in ('bytesBase64','sha256')}if 'rawText'in item else item for item in row['auxiliaryFiles']]
     projected[label].append(normalize({**{k:row[k]for k in ('case','fromEntryId','value','entries')},'auxiliaryFiles':aux}))
    elif row['case']=='replace-rollback':projected[label].append(normalize({'case':row['case'],'prepared':row['prepared'],'rewritten':[json.loads(line)for line in row['rewritten'].splitlines()if line],'finalized':row['finalized'],'rawRollbackEqual':row['originalSha256']==row['restoredSha256']}))
    else:projected[label].append(normalize({'case':row['case'],'prepared':row['prepared'],'rewritten':[json.loads(line)for line in row['rewritten'].splitlines()if line]}))
  alignment={'pass':projected['ts']==projected['rust'],'scope':'Actual WS fork results, full fork entries including carryover/reference paths, replacement prepared/result/rewritten entries, raw original rollback equality per backend. UUIDs mapped bijectively by encounter, times and live owner pid ignored; raw frames/artifacts unchanged. Not frozen scoring.','projection':projected};save(a.out/'comparison.json',alignment)
'''
source=source[:start]+comparison+source[end:]
exec(compile(source,str(harness),'exec'),globals())
