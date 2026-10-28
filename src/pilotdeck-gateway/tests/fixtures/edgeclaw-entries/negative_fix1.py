#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative function-body slicing and multiple run calls
"""Fix-round security / snapshot-lock mutations; each must fail at runtime."""
import argparse, hashlib, json, os, pathlib, sys
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
p=argparse.ArgumentParser();p.add_argument('--out',required=True);p.add_argument('--only');args=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[5]
out=pathlib.Path(args.out).resolve();out.mkdir(parents=True,exist_ok=True)
f=root/'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
e=root/'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/entries.rs'
with mutating_sources(root, [f, e]) as originals:
 text=originals[f].decode()
 # #597 moved every escape/symlink check from `within` into `checked_path` (the
 # fd-based mutations pin through it too), so the no-validation regression now
 # stubs `checked_path`'s own body; the old `within`..`ensure_layout` span would
 # also swallow mkdir/write_bytes/unlink/read_file outright (E0599).
 for anchor in ('    fn checked_path(&self, relative: &str) -> Result<(PathBuf, PathBuf)> {','    fn mutation_error('):
  assert text.count(anchor)==1,anchor
 a=text.index('    fn checked_path(&self, relative: &str) -> Result<(PathBuf, PathBuf)> {');b=text.index('    fn mutation_error(',a)
 unsafe=text[:a]+'''    fn checked_path(&self, relative: &str) -> Result<(PathBuf, PathBuf)> {
        // regression: trust the joined path, like the old within() stub
        let path = absolute(&self.root.join(relative))?;
        Ok((fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone()), path))
    }
'''+text[b:]
 rows=[]
 def run(name,path,changed,target,test,env_extra=None):
  if args.only and name != args.only: return
  # Reserve the slot before exposing the mutation; keep the tree lock for the
  # whole read/mutate/run/restore window.
  slot=wait_for_slot(root/'scripts/cargo-slots.sh')
  if slot.returncode:raise RuntimeError(slot.stdout+slot.stderr)
  path.write_text(changed)
  try:
   env=os.environ.copy();env.update(env_extra or {})
   cmd=['cargo','test','--locked','--offline','-p','pilotdeck-gateway',*target,test,'--','--exact','--nocapture']
   r=run_owned(cmd,cwd=root/'src',env=env,timeout=120)
   log=slot.stdout+slot.stderr+r.stdout+r.stderr;(out/(name+'.log')).write_text(log)
   assert r.returncode and 'panicked at' in log and 'could not compile' not in log,(name,log[-2000:])
   failures=[line for line in log.splitlines() if 'panicked at' in line or 'unsafe ' in line or 'unfinished clear' in line or 'left:' in line or 'right:' in line]
   rows.append({'name':name,'exit':r.returncode,'failures':failures})
   print(name+': killed; '+'; '.join(failures)[:400],flush=True)
  finally:path.write_bytes(originals[path])
 try:
  for op in ['read','store-read','edit','delete','deprecate','restore']:
   run('path-'+op,f,unsafe,['--test','edgeclaw_entries'],'entry_paths_reject_escape_without_touching_outside_files',{'PDRS_ENTRY_SECURITY_OPERATION':op})
  # #597 moved the resolved-vs-root comparison into `checked_path`'s match;
  # the mutant must accept the resolved target without the boundary check.
  old='''            Ok(path) if path.starts_with(&root) => Ok((root, path)),
            Ok(_) => Err(denied()),
'''
  canonical_only = originals[f].decode();assert canonical_only.count(old)==1
  canonical_only = canonical_only.replace(old,'            Ok(path) => Ok((root, path)),\n')
  run('path-symlink',f,canonical_only,['--test','edgeclaw_entries'],'entry_paths_reject_escape_without_touching_outside_files')
  run('path-write',f,unsafe,['--lib'],'runtime::memory::edgeclaw::file_memory::entry_write_security_tests::direct_entry_write_rejects_unsafe_paths')
  text=originals[f].decode();old='                            | std::io::ErrorKind::PermissionDenied';assert text.count(old)==1
  run('inaccessible-missing-read',f,text.replace(old,''),['--test','edgeclaw_file_memory'],'rust_reads_and_writes_the_real_ts_transcripts')
  text=originals[e].decode()
  a=text.index('            // Reject the whole request before either store can repair/write.')
  b=text.index('            let workspace = ids', a)
  run('path-preflight',e,text[:a]+text[b:],['--test','edgeclaw_entries'],'entry_paths_reject_escape_without_touching_outside_files',{'PDRS_ENTRY_SECURITY_OPERATION':'delete'})
  text=originals[e].decode();old='self.snapshot_operation(|| self.snapshot_version_locked())';assert text.count(old)==1
  run('snapshot-lock',e,text.replace(old,'self.snapshot_version_locked()'),['--test','edgeclaw_entries'],'snapshot_version_waits_for_clear_to_finish')
 finally:
  for path,content in originals.items():path.write_bytes(content);assert path.read_bytes()==content
  (out/'summary.json').write_text(json.dumps({'results':rows,'restoredSha256':{str(p.relative_to(root)):hashlib.sha256(b).hexdigest() for p,b in originals.items()}},indent=2)+'\n')
 assert len(rows)==(1 if args.only else 11)
