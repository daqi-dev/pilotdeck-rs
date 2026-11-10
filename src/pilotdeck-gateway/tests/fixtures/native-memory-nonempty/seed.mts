import assert from 'node:assert/strict';
import {readFileSync,writeFileSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const [base,workspaceArg,rootArg,out]=process.argv.slice(2);
const baseline=resolve(base),workspace=resolve(workspaceArg),root=resolve(rootArg),commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
assert.equal(process.versions.node,'22.22.0');assert.equal(process.versions.icu,'78.2');
assert.equal(execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),commit);
const servicePath='src/context/memory/edgeclaw-memory-core/src/service.ts',repoPath='src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const sourceSha256:any={};
for(const file of [servicePath,repoPath]){const raw=execFileSync('git',['-C',baseline,'show',commit+':'+file],{maxBuffer:4*1024*1024});assert.deepEqual(readFileSync(join(baseline,file)),raw);sourceSha256[file]=createHash('sha256').update(raw).digest('hex');}
const {EdgeClawMemoryService}=await import(pathToFileURL(join(baseline,servicePath)).href);
const {MemoryRepository}=await import(pathToFileURL(join(baseline,repoPath)).href);
const service=new EdgeClawMemoryService({workspaceDir:workspace,rootDir:root});
const paths={workspaceDir:service.workspaceDir,dataDir:service.dataDir,dbPath:service.dbPath,memoryDir:service.memoryDir,globalRootDir:join(root,'global')};service.close();
const repo=new MemoryRepository(paths.dbPath,{memoryDir:paths.memoryDir,globalRootDir:paths.globalRootDir,workspaceDir:paths.workspaceDir});
try{
 const store=repo.getFileMemoryStore();
 store.upsertProjectMeta({projectName:'Deployment fixture',description:'Private mock deployment project',status:'active'});
 const marker='MEMORY_COBALT_LOCK_7f4c0a';
 const record=store.upsertCandidate({type:'project',scope:'project',name:'Deployment rule',description:'A durable deployment rule',capturedAt:'2026-11-10T00:00:00.000Z',sourceSessionKey:'seed-memory-session',summary:'Always apply '+marker+' before deployment.'});
 store.repairManifests();
 const listed=repo.listMemoryEntries({limit:100});assert(JSON.stringify(listed).includes(record.relativePath));assert(readFileSync(join(paths.memoryDir,record.relativePath),'utf8').includes(marker));
 writeFileSync(out,JSON.stringify({sourceCommit:commit,sourceSha256,marker,record,listed,paths},null,2)+'\n',{flag:'wx'});
}finally{repo.close();}
