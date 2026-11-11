"""Prepare a fresh install-shaped mock artifact from existing source-bound binaries.
No build, download, env resource-root injection, or mutation of the source candidate.
This is not the source-install acceptance workflow.
"""
from pathlib import Path
import argparse,hashlib,io,json,os,shutil,subprocess,tarfile
p=argparse.ArgumentParser();p.add_argument('--repo',type=Path,required=True);p.add_argument('--candidate',type=Path,required=True);p.add_argument('--source-manifest',type=Path,required=True);p.add_argument('--out',type=Path,required=True);p.add_argument('--baseline',type=Path,required=True);p.add_argument('--head',required=True);p.add_argument('--source-sha',required=True);a=p.parse_args()
head=a.head;source_sha=a.source_sha
sha=lambda f:hashlib.sha256(Path(f).read_bytes()).hexdigest()
assert sha(a.source_manifest)==source_sha and json.loads(a.source_manifest.read_text())["head"]==head
rows=json.loads((a.candidate/'manifest.json').read_text());libraries=json.loads((a.candidate/'libraries.json').read_text());assert len(rows)==4 and 0<len(libraries)<=64
for r in rows:assert sha(a.candidate/r['path'])==r['sha256']
for r in libraries:assert sha(a.candidate/r['path'])==r['sha256']
a.out.mkdir(mode=0o700,parents=False,exist_ok=False);(a.out/'bin').mkdir();(a.out/'source').mkdir()
for r in rows:
 dest=a.out/'bin'/r['path'];assert len(Path(r['path']).parts)==1;shutil.copyfile(a.candidate/r['path'],dest);dest.chmod(0o755);assert sha(dest)==r['sha256']
for r in libraries:
 rel=Path(r['path']);assert len(rel.parts)==2 and rel.parts[0].startswith('pilotdeck-libs-') and '..' not in rel.parts
 dest=a.out/'bin'/rel;dest.parent.mkdir(exist_ok=True);shutil.copyfile(a.candidate/rel,dest);dest.chmod(0o755);assert sha(dest)==r['sha256']
# Byte-for-byte original receipts. Runtime executable paths change, their bytes do not.
(a.out/'bin/manifest.json').write_bytes((a.candidate/'manifest.json').read_bytes());(a.out/'bin/libraries.json').write_bytes((a.candidate/'libraries.json').read_bytes())
archive=subprocess.check_output(['git','-C',str(a.repo),'archive','--format=tar',head,'skills','LICENSE'])
with tarfile.open(fileobj=io.BytesIO(archive)) as t:
 for m in t.getmembers():
  rel=Path(m.name);assert not rel.is_absolute() and '..' not in rel.parts
  dest=a.out/'source'/rel
  if m.isdir():dest.mkdir(parents=True,exist_ok=True)
  else:
   assert m.isfile(),('non-regular source asset',m.name)
   dest.parent.mkdir(parents=True,exist_ok=True);dest.open('xb').write(t.extractfile(m).read());dest.chmod(m.mode);os.utime(dest,(m.mtime,m.mtime))
os.symlink('source/skills',a.out/'skills')
assets=json.loads((a.out/'source/skills/upstream-assets.json').read_text());assert assets['file_count']==109 and len(assets['files'])==109
asset_rows=[]
for r in assets['files']:
 f=a.out/'source/skills'/r['path'];assert sha(f)==r['sha256'] and f.stat().st_size==r['bytes'];asset_rows.append({'path':r['path'],'sha256':sha(f),'bytes':f.stat().st_size})
assert len(list((a.out/'source/skills').glob('*/SKILL.md')))==32
assert subprocess.check_output(['git','-C',str(a.baseline),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
critical={x['path'] for x in assets['files']}|{str(x.relative_to(a.out/'source/skills')) for x in (a.out/'source/skills').glob('*/SKILL.md')}
mtime_rows={}
for rel in sorted(critical):
 src=a.baseline/'skills'/rel;dest=a.out/'source/skills'/rel
 assert sha(src)==sha(dest),('skill bytes differ from fixed TS',rel)
 info=src.stat();os.utime(dest,ns=(info.st_atime_ns,info.st_mtime_ns));assert dest.stat().st_mtime_ns==info.st_mtime_ns
 mtime_rows[rel]={'sourcePath':str(src.resolve()),'sha256':sha(src),'mtimeNs':info.st_mtime_ns}

receipt={'preservedMtimes':mtime_rows,'root':str(a.out.resolve()),'producer':{'path':str(Path(__file__).resolve()),'sha256':sha(__file__)},'sourceManifest':{'path':str(a.source_manifest.resolve()),'sha256':source_sha},'sourceHead':head,'sourceManifestSha256':source_sha,'originalCandidate':str(a.candidate),'binaryManifestSha256':sha(a.out/'bin/manifest.json'),'librariesManifestSha256':sha(a.out/'bin/libraries.json'),'skillsLink':'source/skills','skillsLinkResolved':str((a.out/'skills').resolve()),'sourceArchiveSha256':hashlib.sha256(archive).hexdigest(),'assets109':asset_rows,'allSkillFiles':{str(f.relative_to(a.out/'source/skills')):sha(f) for f in sorted((a.out/'source/skills').rglob('*')) if f.is_file()},'scope':'isolated install-shaped mock package, existing compiled binaries; NOT full source-install PASS'}
(a.out/'deployment-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n');print(json.dumps({'out':str(a.out),'assets':len(asset_rows),'skills':32,'receiptSha256':sha(a.out/'deployment-receipt.json')}))
