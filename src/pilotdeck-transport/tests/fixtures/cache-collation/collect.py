#!/usr/bin/env python3
"""Reproduce the fixed pure CachePlan fixture; no build or provider calls."""
from pathlib import Path
import subprocess,sys,json,hashlib
baseline,node,out=map(Path,sys.argv[1:])
out.mkdir(mode=0o700)
here=Path(__file__).resolve().parent
locales=['en_US.UTF-8','C','', 'fr_CA.UTF-8','fr_Latn_CA.UTF-8','fr_Cyrl_CA.UTF-8','sv_SE.UTF-8','tr_TR.UTF-8','de_DE.UTF-8@collation=phonebook','garbage_x_foo.UTF-8']
rows=[]
for i,locale in enumerate(locales):
 run=out/('capture-'+str(i));run.mkdir(mode=0o700)
 for name in ['home','tmp']:(run/name).mkdir(mode=0o700)
 profile=run/'sandbox.sb'
 profile.write_text('(version 1) (allow default) (deny network*) (deny process-fork) (deny file-write*) (allow file-write* (subpath '+json.dumps(str(run))+'))\n')
 env={'PATH':'/usr/bin:/bin','HOME':str(run/'home'),'PILOT_HOME':str(run/'home'),'TMPDIR':str(run/'tmp'),'LANG':locale,'TZ':'UTC'}
 proc=subprocess.run(['/usr/bin/sandbox-exec','-f',str(profile),str(node),str(here/'capture.mjs'),str(baseline),str(run/'capture'),str(here/'seed.json')],cwd=run,env=env,capture_output=True,timeout=25)
 (run/'stdout').write_bytes(proc.stdout);(run/'stderr').write_bytes(proc.stderr)
 (run/'env.json').write_text(json.dumps(env,indent=2)+'\n')
 if proc.returncode or proc.stderr:raise RuntimeError('capture failed: '+str(i)+' '+str(proc.returncode)+' '+proc.stderr.decode(errors='replace'))
 rows.append(json.loads((run/'capture/oracle.json').read_text()))
first=rows[0];inputs=[{k:v for k,v in case.items() if k!='result'} for case in first['cases']]
for row in rows:
 assert row['source']==first['source'] and row['strings']==first['strings']
 assert [{k:v for k,v in case.items() if k!='result'} for case in row['cases']]==inputs
fixture={'baseline':first['baseline'],'node':first['node'],'icu':first['icu'],'typescript':first['typescript'],'source':first['source'],'inputs':inputs,'strings':first['strings'],'locales':[{'LANG':row['LANG'],'resolved':row['resolved'],'signs':row['signs'],'results':[case['result'] for case in row['cases']]} for row in rows],'originals':[{'path':'capture-'+str(i)+'/capture/oracle.json','sha256':hashlib.sha256((out/('capture-'+str(i))/'capture/oracle.json').read_bytes()).hexdigest()} for i in range(10)]}
encoded=(json.dumps(fixture,separators=(',',':'))+'\n').encode()
(out/'oracle.json').write_bytes(encoded)
print(json.dumps({'sha256':hashlib.sha256(encoded).hexdigest(),'locales':10,'cachePlans':70,'pairs':118810}))
