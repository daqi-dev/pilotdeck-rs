import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {bindSourceTree} from './baseline_source_guard.mjs';
function fixture() {
 const root=fs.mkdtempSync(path.join(os.tmpdir(),'metadata-source-'));
 const git=(...args)=>execFileSync('git',['-C',root,...args],{stdio:['ignore','pipe','pipe']}).toString().trim();
 git('init');fs.mkdirSync(path.join(root,'src'));fs.writeFileSync(path.join(root,'src/main.ts'),'export const value=1;\n');fs.writeFileSync(path.join(root,'.gitignore'),'src/ignored/\n');git('add','.');git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-m','fixture');
 return {root,git,ref:git('rev-parse','HEAD')};
}
for (const mutation of ['healthy','dirty','missing','untracked','ignored','symlink','assume-unchanged']) test(mutation,t=>{
 const f=fixture();t.after(()=>fs.rmSync(f.root,{recursive:true,force:true}));const file=path.join(f.root,'src/main.ts');
 if(mutation==='dirty')fs.writeFileSync(file,'changed');
 if(mutation==='missing')fs.unlinkSync(file);
 if(mutation==='untracked')fs.writeFileSync(path.join(f.root,'src/main.js'),'shadow');
 if(mutation==='ignored'){fs.mkdirSync(path.join(f.root,'src/ignored'));fs.writeFileSync(path.join(f.root,'src/ignored/shadow.ts'),'shadow');}
 if(mutation==='symlink'){fs.renameSync(file,path.join(f.root,'same-bytes'));fs.symlinkSync(path.join(f.root,'same-bytes'),file);}
 if(mutation==='assume-unchanged'){f.git('update-index','--assume-unchanged','src/main.ts');fs.writeFileSync(file,'changed');}
 if(mutation==='healthy')assert.equal(bindSourceTree(f.root,f.ref,'/usr/bin/git').count,1);else assert.throws(()=>bindSourceTree(f.root,f.ref,'/usr/bin/git'));
});

test('workspace-local Git shim rejected before execution',t=>{
 const f=fixture();t.after(()=>fs.rmSync(f.root,{recursive:true,force:true}));
 const shim=path.join(f.root,'git');fs.writeFileSync(shim,'#!/bin/sh\nexit 0\n');fs.chmodSync(shim,0o755);
 assert.throws(()=>bindSourceTree(f.root,f.ref,shim),/workspace-local Git/);
 assert.throws(()=>bindSourceTree(f.root,f.ref),/explicit absolute Git/);
});
