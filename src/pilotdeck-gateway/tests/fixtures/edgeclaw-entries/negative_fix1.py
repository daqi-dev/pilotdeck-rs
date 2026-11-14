#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative function-body slicing and multiple run calls
"""Fix-round security / snapshot-lock mutations; each must fail at runtime.

#1087 fix-r5 (sibling #3): the verdict rows stay in memory for the whole
guarded window and one final ``summary.json`` is published by a single rename
only after ``mutating_sources`` restored and byte-verified the guarded sources
(the same skeleton as the edgeclaw tables). An abort inside the window — a
cancellation, a restore failure, the source guard — voids the recorded rows,
publishes one INVALID summary and exits 3; the old ``finally``-inside-the-window
write left a partial all-killed-looking summary behind when the run was
cancelled mid-suite. Every guarded-source write in the window — mutation and
restore alike — is one atomic rename (``write_source_atomic``), never a
truncating write. The pre-run anchor check is a refusal: exit 2 before any
compile, nothing measured, nothing published.
"""
import argparse, hashlib, json, os, pathlib, sys
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from negative_process import (dead_stream_print, detach_dead_streams,
                              mutating_sources, new_run_token, refuse_summary,
                              run_owned, unique_summary_tmp, wait_for_slot,
                              write_source_atomic)
p=argparse.ArgumentParser();p.add_argument('--out',required=True);p.add_argument('--only');args=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[5]
out=pathlib.Path(args.out).resolve();out.mkdir(parents=True,exist_ok=True)
# #1087 fix-r6 (P1a) / fix-r8 (S1-b): every evidence name carries the shared
# per-run token (pid.serial.time_ns) — the OUT_DIR above is a plain reused
# mkdir, so two overlapping runs of this table must not truncate each other's
# fixed-name artifacts, and the time_ns component survives pid recycling
# (a recycled pid must not replay another run's token).
run_token=new_run_token()
# #1087 fix-r8 (S2-a): the verdict summary is per-run, too — this run
# publishes its own file and never overwrites another run's green verdict
# recorded in the same reused OUT_DIR.
summary_name=f'summary.{run_token}.json'
f=root/'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
e=root/'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/entries.rs'
# The rows this run may record, in execution order; --only selects one.
expected=['path-'+op for op in ['read','store-read','edit','delete','deprecate','restore']]
expected+=['path-symlink','path-write','inaccessible-missing-read','path-preflight','snapshot-lock']
if args.only:
 expected=[args.only] if args.only in expected else []
rows=[]
originals={}
def write_summary():
 # #1087 fix-r5 (sibling #3, concurrency #3): one staging name per process, so
 # two overlapping runs of the same table never share one fixed temp file.
 # #1087 fix-r8 (S2-a): the publish target is this run's per-run summary
 # name and the payload names the run — the twin test_negative_publication
 # globs the per-run shape.
 tmp=unique_summary_tmp(out,summary_name)
 tmp.write_text(json.dumps({'runToken':run_token,'results':rows,'restoredSha256':{str(p.relative_to(root)):hashlib.sha256(b).hexdigest() for p,b in originals.items()}},indent=2)+'\n')
 tmp.replace(out/summary_name)
def _void_pending(error):
 """Void the recorded rows and publish one INVALID summary (sibling #3)."""
 reason=f'runner: {type(error).__name__}: {error}'
 for row in rows:
  row['killed']=False;row['voided']=reason
 recorded={row['name'] for row in rows}
 for name in expected:
  if name not in recorded:
   rows.append({'name':name,'exit':None,'failures':None,'killed':False,'voided':reason})
 write_summary()
 # #1087 fix-r8 (S1-a): the shared guarded print — the summary
 # is published, "aborted = 3" is the only contract left to keep.
 dead_stream_print(f'voided {len(rows)} row(s): {reason}',stream='stderr')
 if isinstance(error,(OSError,ValueError)):
  # #1087 fix-r5 (concurrency #2): detach dead fds so the shutdown flush
  # cannot rewrite the exit code below to 120.
  detach_dead_streams()
 if isinstance(error,SystemExit):
  raise SystemExit(3) from error
 if not isinstance(error,Exception):
  raise
 raise SystemExit(3) from error
def _refuse_summary(reason):
 # #1087 fix-r6 (P1c) / fix-r8 (S1-c + S2-a): the refused marker is this
 # run's own per-run summary file — one rename via the shared writer, never
 # another run's verdict, and a failing write reports through the guarded
 # print instead of a traceback (rc=1, "ran and was red").
 refuse_summary(out,reason,run_token,summary_name=summary_name)
try:
 with mutating_sources(root,[f,e]) as originals_snapshot:
  originals=originals_snapshot
  text=originals[f].decode()
  # #597 moved every escape/symlink check from `within` into `checked_path` (the
  # fd-based mutations pin through it too), so the no-validation regression now
  # stubs `checked_path`'s own body; the old `within`..`ensure_layout` span would
  # also swallow mkdir/write_bytes/unlink/read_file outright (E0599).
  for anchor in ('    fn checked_path(&self, relative: &str) -> Result<(PathBuf, PathBuf)> {','    fn mutation_error('):
   if text.count(anchor)!=1:
    # #1087 fix-r8 (S1-a): the shared guarded print — a dead stderr
    # must not turn this exit-2 refusal into an in-window abort (exit 3
    # with a voided INVALID summary), an exec-time ``2>&-`` reroute, or a 120.
    dead_stream_print(f'STALE ANCHOR fix1: {text.count(anchor)} matches: {anchor!r}',stream='stderr')
    raise SystemExit(2)
  a=text.index('    fn checked_path(&self, relative: &str) -> Result<(PathBuf, PathBuf)> {');b=text.index('    fn mutation_error(',a)
  unsafe=text[:a]+'''    fn checked_path(&self, relative: &str) -> Result<(PathBuf, PathBuf)> {
        // regression: trust the joined path, like the old within() stub
        let path = absolute(&self.root.join(relative))?;
        Ok((fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone()), path))
    }
'''+text[b:]
  def run(name,path,changed,target,test,env_extra=None):
   if args.only and name != args.only: return
   # Reserve the slot before exposing the mutation; keep the tree lock for the
   # whole read/mutate/run/restore window.
   slot=wait_for_slot(root/'scripts/cargo-slots.sh')
   if slot.returncode:raise RuntimeError(slot.stdout+slot.stderr)
   # #1087 fix-r5 (sibling #3): one rename, never a truncating in-place write;
   # no `mode=` — the helper's own fallback stat covers the shape (boundary #5).
   write_source_atomic(root,path,changed.encode())
   try:
    env=os.environ.copy();env.update(env_extra or {})
    cmd=['cargo','test','--locked','--offline','-p','pilotdeck-gateway',*target,test,'--','--exact','--nocapture']
    r=run_owned(cmd,cwd=root/'src',env=env,timeout=120)
    log=slot.stdout+slot.stderr+r.stdout+r.stderr;(out/f'{name}.{run_token}.log').write_text(log)
    killed=(bool(r.returncode) and 'panicked at' in log and 'could not compile' not in log)
    if killed:
     failures=[line for line in log.splitlines() if 'panicked at' in line or 'unsafe ' in line or 'unfinished clear' in line or 'left:' in line or 'right:' in line]
    else:
     failures=[]
    rows.append({'name':name,'exit':r.returncode,'failures':failures,'killed':killed})
    print(name+(': killed; ' if killed else ': not killed; exit ')+('; '.join(failures)[:400] if killed else str(r.returncode)),flush=True)
   finally:
    # Same #1087 fix-r5 window: the in-loop restore is also one rename; the
    # guard's own exit verification covers the bytes.
    write_source_atomic(root,path,originals[path])
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
except SystemExit as error:
 if error.code==2:
  # The deliberate stale-anchor refusal: nothing was measured, so nothing is
  # published — and a reused directory's old green summary is invalidated so
  # it cannot pose as this run's evidence (#1087 fix-r6, P1c).
  _refuse_summary('stale anchors: negative_fix1.py is out of sync with its sources')
  raise
 _void_pending(error)
except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
 _void_pending(error)
# Reaching here means mutating_sources restored the guarded files and verified
# them against the baseline bytes. Only now may a summary exist.
write_summary()
killed=[row for row in rows if row['killed']]
print(f'summary: {len(killed)}/{len(rows)} killed',flush=True)
sys.exit(0 if rows and len(rows)==len(expected) and len(killed)==len(rows) else 1)
