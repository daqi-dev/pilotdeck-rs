#!/usr/bin/env python3
"""Fix-round security / snapshot-lock mutations; each must fail at runtime."""
import argparse, hashlib, json, os, pathlib, subprocess
p=argparse.ArgumentParser();p.add_argument('--out',required=True);p.add_argument('--only');args=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[5]
out=pathlib.Path(args.out).resolve();out.mkdir(parents=True,exist_ok=True)
f=root/'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
e=root/'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/entries.rs'
originals={p:p.read_bytes() for p in [f,e]}
text=originals[f].decode();a=text.index('    fn within(&self, relative:');b=text.index('    fn ensure_layout',a)
unsafe=text[:a]+'    fn within(&self, _relative: &str) -> Result<bool> { Ok(true) }\n'+text[b:]
rows=[]
def run(name,path,changed,target,test,env_extra=None):
    if args.only and name != args.only: return
    path.write_text(changed)
    try:
        slot=subprocess.run([str(root/'scripts/cargo-slots.sh')],capture_output=True,text=True)
        if slot.returncode:raise RuntimeError(slot.stdout+slot.stderr)
        env=os.environ.copy();env.update(env_extra or {})
        cmd=['cargo','test','--locked','--offline','-p','pilotdeck-gateway',*target,test,'--','--exact','--nocapture']
        r=subprocess.run(cmd,cwd=root/'src',env=env,capture_output=True,text=True,timeout=120)
        log=slot.stdout+slot.stderr+r.stdout+r.stderr;(out/(name+'.log')).write_text(log)
        assert r.returncode and 'panicked at' in log and 'could not compile' not in log,(name,log[-2000:])
        failures=[line for line in log.splitlines() if 'panicked at' in line or 'unsafe ' in line or 'unfinished clear' in line or 'left:' in line or 'right:' in line]
        rows.append({'name':name,'exit':r.returncode,'failures':failures})
        print(name+': killed; '+'; '.join(failures)[:400],flush=True)
    finally:path.write_bytes(originals[path])
try:
    for op in ['read','store-read','edit','delete','deprecate','restore']:
        run('path-'+op,f,unsafe,['--test','edgeclaw_entries'],'entry_paths_reject_escape_without_touching_outside_files',{'PDRS_ENTRY_SECURITY_OPERATION':op})
    canonical_only = originals[f].decode().replace('Ok(path) if path.starts_with(root) => Ok(true),', 'Ok(_) => Ok(true),').replace('            Ok(_) => Err(denied()),\n', '')
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
