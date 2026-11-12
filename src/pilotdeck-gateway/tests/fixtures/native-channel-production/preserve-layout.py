# anchor-selfcheck: unsupported runtime observation layout preservation, no product source mutations
"""Preserve the actual stopped cur directory by rename, with original path/inode.
This is the existing parity layout rule, not a cache or a path normalization.
"""
import argparse,hashlib,json,os,stat
from pathlib import Path
p=argparse.ArgumentParser()
for key in ['layout','observation-dir','out']:p.add_argument('--'+key,required=True,type=Path)
a=p.parse_args();sha=lambda file:hashlib.sha256(file.read_bytes()).hexdigest()
ref=lambda file:{'path':str(file.resolve()),'sha256':sha(file)}
layout=json.loads(a.layout.read_text());cur=Path(layout['root']);assert cur.is_absolute() and cur.name=='cur' and cur.is_dir() and not cur.is_symlink();assert a.out.parent==cur.parent and a.out.is_dir();target=a.out/'preserved';assert not target.exists()
observations=a.observation_dir/'owned-processes.json';records=json.loads(observations.read_text());assert len(records)==1;r=records[0]
boot=Path('/proc/sys/kernel/random/boot_id').read_text().strip()
def present(identity):
 try:
  raw=Path('/proc')/str(identity['pid'])/'stat';parts=raw.read_text().rsplit(')',1)[1].strip().split();return identity['birth']['bootId']==boot and identity['birth']['startTimeTicks']==parts[19]
 except FileNotFoundError:return False
assert r.get('closeObservedAt') and r.get('birth') and not present(r),'owned Server must be actually reaped before preservation'
for child in r.get('ownedDescendants',[]):assert not present(child),'owned descendant still alive'
ports=set()
for file in ['/proc/net/tcp','/proc/net/tcp6']:
 for line in Path(file).read_text().strip().splitlines()[1:]:
  fields=line.split()
  if fields[3]=='0A':ports.add(int(fields[1].split(':')[1],16))
assert layout['mockPort'] not in ports and layout['controlPort'] not in ports,'layout ports must be closed before rename/reuse'
files=[]
for folder,dirs,names in os.walk(cur,followlinks=False):
 for name in dirs:
  assert not (Path(folder)/name).is_symlink(),'business symlink cannot be preserved as ordinary file'
 for name in names:
  file=Path(folder)/name;info=file.lstat();assert stat.S_ISREG(info.st_mode),'business file must be regular'
  files.append({'originalPath':str(file),'relativePath':str(file.relative_to(cur)),'sha256':sha(file),'bytes':info.st_size,'device':info.st_dev,'inode':info.st_ino})
cur.rename(target)
for row in files:
 file=target/row['relativePath'];info=file.stat();assert info.st_dev==row['device'] and info.st_ino==row['inode'] and info.st_size==row['bytes'] and sha(file)==row['sha256'];row['path']=str(file)
receipt={'schemaVersion':1,'method':'rename actual stopped cur directory','originalRoot':str(cur),'preservedRoot':str(target),'layout':ref(a.layout),'processObservation':ref(observations),'preserver':ref(Path(__file__)),'files':files,'actualClosedPorts':[layout['mockPort'],layout['controlPort']]}
with (a.out/'preservation.json').open('x') as file:file.write(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({'path':str(a.out/'preservation.json'),'sha256':sha(a.out/'preservation.json'),'files':len(files)}))
