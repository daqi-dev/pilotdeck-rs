#!/usr/bin/env python3
"""Mutate one production behavior at a time, require a runtime assertion, restore bytes.
Run from repository root after normal entries tests pass. Logs stay in --out.

#1087 fix-r5 (sibling #3): the verdict rows stay in memory for the whole
guarded window and one final ``summary.json`` is published by a single rename
only after ``mutating_sources`` restored and byte-verified the guarded sources
(the same skeleton as the edgeclaw tables). An abort inside the window — a
cancellation, a restore failure, the source guard — voids the recorded rows,
publishes one INVALID summary and exits 3; the old ``finally``-inside-the-window
write left a partial all-killed-looking summary behind when the run was
cancelled mid-suite. Every guarded-source write in the window — mutation and
restore alike — is one atomic rename (``write_source_atomic``), never a
truncating write. A stale anchor is a refusal: exit 2 before any compile,
nothing measured, nothing published.
"""
import argparse, hashlib, json, os, pathlib, sys
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from negative_process import (dead_stream_print, detach_dead_streams,
                              mutating_sources, new_run_token, refuse_summary,
                              run_owned, unique_summary_tmp, wait_for_slot,
                              write_source_atomic)
p=argparse.ArgumentParser();p.add_argument('--out',required=True);args=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[5];os.chdir(root)
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
base=pathlib.Path('src/pilotdeck-gateway/src/runtime/memory/edgeclaw')
e=base/'storage/entries.rs';a=base/'storage/accessors.rs';f=base/'file_memory.rs';o=base/'storage/overview.rs'
mutations=[
 ('exposed-prefix',e,'"global/".encode_utf16().collect()','"broken/".encode_utf16().collect()','entry-lifecycle'),
 ('global-classification',e,'&& matches!(id.units()[6], 47 | 92)','&& false','entry-lifecycle'),
 ('records-global-map',e,'.map(map_record)','.map(Ok)','entry-lifecycle'),
 ('io-message',f,'Some((_, message)) => write!(f, "{message}"),','Some((_, _message)) => write!(f, "{e}"),','edit-entry-write-fail'),
 ('count-cap',o,'''
        let mut input = options.clone();
        input.insert("limit".into(), JsValue::Number(5000.0));
        input.insert("offset".into(), JsValue::Number(0.0));
        let mut count = 0.0;''','''
        let mut input = options.clone();
        input.insert("limit".into(), JsValue::Number(1.0));
        input.insert("offset".into(), JsValue::Number(0.0));
        let mut count = 0.0;''','entry-lifecycle'),
 ('record-duplicates',e,'ids.iter().filter_map(|id| by_id.get(id).cloned()).collect()','ids.iter().filter_map(|id| by_id.get(id).cloned()).take(1).collect()','entry-lifecycle'),
 ('external-records',e,'records.extend(self.entry_external_records(&external, max_lines)?);','let _ = (&external,max_lines);','general-meta'),
 ('edit-meta',e,'self.snapshot_operation(|| self.workspace_memory.edit_project_meta(input))','self.snapshot_operation(|| self.workspace_memory.ensure_project_meta(input))','entry-lifecycle'),
 ('ensure-meta',e,'self.snapshot_operation(|| self.workspace_memory.ensure_project_meta(input))','self.get_project_meta().map(|_| {let _ = input;JsValue::Null})','empty-meta'),
 ('read-meta',e,'self.workspace_memory.get_project_meta(&JsObject::new())','Ok(JsValue::Null)','entry-lifecycle'),
 ('edit-global-map',e,'map_record(record)','Ok(record)','entry-lifecycle'),
 ('delete',e,'self.mutate_entries(&ids, "deleteEntries")','self.mutate_entries(&ids, "restoreEntries")','entry-lifecycle'),
 ('deprecate',e,'self.mutate_entries(&ids, "markEntriesDeprecated")','self.mutate_entries(&ids, "restoreEntries")','entry-lifecycle'),
 ('restore',e,'self.mutate_entries(&ids, "restoreEntries")','self.mutate_entries(&ids, "markEntriesDeprecated")','entry-lifecycle'),
 ('archive-error',e,'self.workspace_memory.archive_tmp_entries(&next)','Ok(JsValue::Null)','invalid-entry-operations'),
 ('snapshot-manifest',e,'.filter(|f| field(f, "relativePath") != &s("MEMORY.md"))','.filter(|_| true)','counts-and-version'),
 ('current-settings',e,'let settings = if current {','let settings = if false {','clearCurrentWorkspaceMemoryData'),
 ('all-global',e,'if !current {\n                self.global_user_memory.execute("clearAllData", &options)?;\n            }','if false {\n                self.global_user_memory.execute("clearAllData", &options)?;\n            }','clearAllMemoryData'),
 ('session-trim',e,'let session = js_trim(session);','let session = session;','clear-session-selection'),
 ('clear-autocommit',e,'DELETE FROM l0_sessions; DELETE FROM pipeline_state;','BEGIN; DELETE FROM l0_sessions; DELETE FROM pipeline_state; COMMIT;','clearAllMemoryData-sql-fail'),
 ('sqlite-error-text',f,'Self::Storage(StorageError::Sqlite(e)) => e.to_string().into(),','Self::Storage(StorageError::Sqlite(e)) => format!("sqlite: {e}").into(),','clearAllMemoryData-sql-fail'),
]
# `clear` removes a dangling root through the Unix branch; the old
# node_fs::rm_sync anchor only exists under cfg(not(unix)). Keep both controls
# so the suite does not silently stop at this point on macOS/Linux.
if sys.platform == 'win32':
 mutations.append(('dangling-root-remove',f,'super::node_fs::rm_sync(&self.root)?;','if self.root.exists() {super::node_fs::rm_sync(&self.root)?;}','clearAllMemoryData-dangling-root'))
else:
 mutations.append(('dangling-root-remove',f,'            let canonical = fs::canonicalize(parent)?;','            let canonical = fs::canonicalize(parent)?;\n            if !self.root.exists() { return Ok(()); }','clearAllMemoryData-dangling-root'))
rows=[]
# The guard snapshot's sha256 map, published only after the window closes;
# None when the guard refused before it snapshotted anything.
restored_sha=None
def write_summary():
 # #1087 fix-r5 (sibling #3, concurrency #3): one staging name per process, so
 # two overlapping runs of the same table never share one fixed temp file.
 # #1087 fix-r8 (S2-a): the publish target is this run's per-run summary
 # name and the payload names the run — the twin test_negative_publication
 # globs the per-run shape.
 tmp=unique_summary_tmp(out,summary_name)
 tmp.write_text(json.dumps({'runToken':run_token,'results':rows,'restoredSha256':restored_sha},indent=2)+'\n')
 tmp.replace(out/summary_name)
def _void_pending(error):
 """Void the recorded rows and publish one INVALID summary (sibling #3)."""
 reason=f'runner: {type(error).__name__}: {error}'
 for row in rows:
  row['killed']=False;row['voided']=reason
 for name,_path,_old,_new,case in mutations[len(rows):]:
  rows.append({'name':name,'case':case,'exit':None,'failure':None,'killed':False,'voided':reason})
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
 # #1087 fix-r5 (sibling #3): the snapshot name has exactly one binding
 # (the with-as capture), so anchor_selfcheck can keep reading it.
 with mutating_sources(root,[e,a,f,o]) as originals:
  restored_sha={str(p):hashlib.sha256(b).hexdigest() for p,b in originals.items()}
  # A stale anchor is a harness defect: refuse before any compile (exit 2,
  # nothing measured, nothing published).
  for name,path,old,_new,_case in mutations:
   count=originals[path].decode().count(old)
   if count!=1:
    # #1087 fix-r8 (S1-a): the shared guarded print — a dead stderr
    # must not turn this exit-2 refusal into an in-window abort (exit 3
    # with a voided INVALID summary), an exec-time ``2>&-`` reroute, or a 120.
    dead_stream_print(f'STALE ANCHOR {name}: {count} matches',stream='stderr')
    raise SystemExit(2)
  for name,path,old,new,case in mutations:
   env=os.environ.copy();env['PDRS_ENTRIES_CASE']=case
   # Hold the source lock, reserve a Cargo slot, then expose the mutation for
   # the shortest possible window (the owned test run only).
   slot=wait_for_slot(root/'scripts/cargo-slots.sh')
   if slot.returncode:raise RuntimeError('cargo slots full')
   # #1087 fix-r5 (sibling #3): one rename, never a truncating in-place write;
   # no `mode=` — the helper's own fallback stat covers the shape (boundary #5).
   write_source_atomic(root,path,originals[path].decode().replace(old,new).encode())
   cmd=['cargo','test','--manifest-path','src/Cargo.toml','--locked','--offline','-p','pilotdeck-gateway','--test','edgeclaw_entries','--','--exact','ts_entries_and_failure_residue_replay','--nocapture']
   try:
    run=run_owned(cmd,env=env,timeout=100)
   finally:
    # Same #1087 fix-r5 window: the in-loop restore is also one rename; the
    # guard's own exit verification covers the bytes.
    write_source_atomic(root,path,originals[path])
   log=run.stdout+run.stderr;(out/f'{name}.{run_token}.log').write_text(log)
   killed=(run.returncode!=0 and 'scenario ' in log and 'observation ' in log and 'could not compile' not in log)
   row={'name':name,'case':case,'exit':run.returncode,'killed':killed}
   if killed:
    failure=next(line.strip() for line in log.splitlines() if 'scenario ' in line and 'observation ' in line)
    diff=pathlib.Path(failure.rsplit('diff ',1)[1])
    # #1087 fix-r8 (S2-d): the diff evidence name is per-run, and the row
    # records it — two overlapping runs on the reused OUT_DIR keep separate
    # evidence files and the summary names its own.
    evidence=f'{name}.{run_token}.json'
    (out/evidence).write_bytes(diff.read_bytes())
    row['evidence']=evidence
    row['failure']=failure
    print(name+': killed; '+failure,flush=True)
   else:
    row['failure']=None
    print(name+': not killed; exit '+str(run.returncode),flush=True)
   rows.append(row)
except SystemExit as error:
 if error.code==2:
  # The deliberate stale-anchor refusal: nothing was measured, so nothing is
  # published — and a reused directory's old green summary is invalidated so
  # it cannot pose as this run's evidence (#1087 fix-r6, P1c).
  _refuse_summary('stale anchors: negative.py is out of sync with its sources')
  raise
 _void_pending(error)
except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
 _void_pending(error)
# Reaching here means mutating_sources restored the guarded files and verified
# them against the baseline bytes. Only now may a summary exist.
write_summary()
killed=[row for row in rows if row['killed']]
print(f'summary: {len(killed)}/{len(rows)} killed',flush=True)
sys.exit(0 if rows and len(killed)==len(rows) else 1)
