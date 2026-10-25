#!/usr/bin/env python3
"""Mutate one production behavior at a time, require a runtime assertion, restore bytes.
Run from repository root after normal entries tests pass. Logs stay in --out.
"""
import argparse, hashlib, json, os, pathlib, sys
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
p=argparse.ArgumentParser();p.add_argument('--out',required=True);args=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[5];os.chdir(root)
out=pathlib.Path(args.out).resolve();out.mkdir(parents=True,exist_ok=True)
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
with mutating_sources(root, [e, a, f, o]) as originals:
 rows=[]
 try:
  for name,path,old,new,case in mutations:
   env=os.environ.copy();env['PDRS_ENTRIES_CASE']=case
   # Hold the source lock, reserve a Cargo slot, then expose the mutation for
   # the shortest possible window (the owned test run only).
   slot=wait_for_slot(root/'scripts/cargo-slots.sh')
   if slot.returncode:raise RuntimeError('cargo slots full')
   text=originals[path].decode();assert text.count(old)==1,(name,text.count(old));path.write_text(text.replace(old,new))
   cmd=['cargo','test','--manifest-path','src/Cargo.toml','--locked','--offline','-p','pilotdeck-gateway','--test','edgeclaw_entries','--','--exact','ts_entries_and_failure_residue_replay','--nocapture']
   try:
    run=run_owned(cmd,env=env,timeout=100)
   finally:path.write_bytes(originals[path])
   log=run.stdout+run.stderr;(out/(name+'.log')).write_text(log)
   assert run.returncode!=0 and 'scenario ' in log and 'observation ' in log and 'could not compile' not in log,(name,log[-1500:])
   failure=next(line.strip() for line in log.splitlines() if 'scenario ' in line and 'observation ' in line)
   diff=pathlib.Path(failure.rsplit('diff ',1)[1]);(out/(name+'.json')).write_bytes(diff.read_bytes())
   rows.append({'name':name,'case':case,'exit':run.returncode,'failure':failure});print(name+': killed; '+failure,flush=True)
 finally:
  for path,content in originals.items():path.write_bytes(content)
  for path,content in originals.items():assert path.read_bytes()==content
  (out/'summary.json').write_text(json.dumps({'results':rows,'restoredSha256':{str(p):hashlib.sha256(b).hexdigest() for p,b in originals.items()}},indent=2)+'\n')
 assert len(rows)==len(mutations)
