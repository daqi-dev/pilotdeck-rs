// Real cc7c4e6 runner/writer and scope ownership; only the AgentLoop is a stub.
import assert from 'node:assert/strict';
import {existsSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, relative, join, resolve} from 'node:path';
import {createRequire} from 'node:module';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
const root = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(root);
const sources = ['agent/turn/TurnRunner.ts', 'agent/session/AgentSession.ts',
  'agent/session/AgentSessionEventRecorder.ts', 'session/transcript/JsonlTranscriptWriter.ts',
  'session/events/SessionRuntime.ts', 'session/persistence/JsonlSessionPersistence.ts',
  'agent/scope/AgentHandle.ts', 'agent/scope/AgentFactoryProvider.ts',
  'agent/scope/AgentRegistry.ts', 'agent/scope/ScopedServiceRegistry.ts'];
const ts = createRequire(join(root, 'package.json'))('typescript');
function runtimeSources(entries: string[]): string[] {
  const seen = new Set<string>();
  const visit = (path: string) => {
    if (seen.has(path)) return;
    assert.ok(path.startsWith('src/'), `source outside baseline src: ${path}`);
    seen.add(path);
    const code = ts.transpileModule(readFileSync(join(root,path),'utf8'), {
      compilerOptions: {target:ts.ScriptTarget.ESNext,module:ts.ModuleKind.ESNext}
    }).outputText;
    const tree = ts.createSourceFile(path,code,ts.ScriptTarget.Latest,true);
    const scan = (node: any) => {
      const specifier = (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) ? node.moduleSpecifier
        : ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword || node.expression.getText(tree) === 'require')
          ? node.arguments[0] : undefined;
      if (specifier && ts.isStringLiteral(specifier) && specifier.text.startsWith('.')) {
        const base = resolve(root,dirname(path),specifier.text);
        const candidates = [base.replace(/\.js$/,'.ts'),base,`${base}.ts`,join(base,'index.ts')];
        const found = candidates.find(p=>existsSync(p) && !p.endsWith('/'));
        assert.ok(found, `unresolved import ${path}: ${specifier.text}`);
        visit(relative(root,found));
      }
      ts.forEachChild(node,scan);
    };
    scan(tree);
  };
  entries.forEach(visit);
  return [...seen].sort();
}
const sourceSha256 = await baselineSourceHashes(root, runtimeSources(sources.map(p => `src/${p}`)));
const [{TurnRunner}, {AgentSession}, , {JsonlTranscriptWriter}, {SessionRuntime},
  {JsonlSessionPersistence}, {AgentHandle}, {AgentFactoryProvider}] =
  await Promise.all(sources.map(p => import(pathToFileURL(join(root,'src',p)).href)));
mkdirSync(output, {recursive:true});
const now = () => new Date('2026-10-26T00:00:00.000Z');
function gate() { let resolve!: () => void; const promise = new Promise<void>(r => {resolve=r;}); return {promise,resolve}; }
async function bounded<T>(p: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  try { return await Promise.race([p, new Promise<never>((_,r) => {timer=setTimeout(()=>r(new Error('gate timeout')),5000);})]); }
  finally { clearTimeout(timer!); }
}
const matrix:any[] = [];
for (const [phase,kind] of [['start','turn_started'],['accept','accepted_input'],['complete','turn_result']]) {
  for (const action of ['cancel','drop','resubmit','dispose']) {
    const name = `${phase}-${action}`, entered = gate(), release = gate();
    const path = join(output, `${name}.jsonl`); writeFileSync(path, '', {flag:'wx'});
    let ids=0, attempts=0, nextDone=false, returnDone=false, disposeDone=false;
    const runtime = new SessionRuntime({now,uuid:()=>`id-${++ids}`});
    const persistence = new JsonlSessionPersistence({path});
    runtime.subscribe(async (entry:any) => {
      if(entry.type === kind && ++attempts === 1) {entered.resolve(); await release.promise;}
      await persistence.append(entry);
    }, {failureMode:'propagate',flush:()=>persistence.flush()});
    const writer = new JsonlTranscriptWriter({path,eventStore:runtime});
    const result = {type:'success',sessionId:'s',turnId:'t',usage:{},permissionDenials:[],turns:1,
      stopReason:'completed',startedAt:now().toISOString(),completedAt:now().toISOString()};
    const loop = {async *run() {yield {type:'turn_completed',sessionId:'s',turnId:'t',result}; return {result,messages:[]};},
      snapshotFileState:()=>({allowedReadFiles:[]})};
    const runner = new TurnRunner(loop as any,writer,undefined,now,undefined,{cwd:output,transcriptPath:path,collectFileArtifacts:false});
    const session = new AgentSession({sessionId:'s',turnRunner:runner});
    const factory = new AgentFactoryProvider();
    const handle = await bounded(factory.create('s',()=>new AgentHandle(session)));
    const stream = handle.submit({type:'text',text:'original'},{turnId:'t'});
    let pending: Promise<any>;
    for(let i=0;i<20;i++) {
      nextDone=false;
      pending=stream.next().then(v=>{nextDone=true;return v;});
      const checkpoint=await bounded(Promise.race([entered.promise.then(()=> 'gate'),pending.then(v=>v.done?'eof':'event')]));
      assert.notEqual(checkpoint,'eof');
      if(checkpoint==='gate') break;
    }
    assert.equal(nextDone,false,'target I/O entered before its event');
    let returning:Promise<any>|undefined, disposing:Promise<void>|undefined;
    if(action==='cancel') {
      const cancelled=gate(); cancelled.resolve();
      assert.equal(await Promise.race([pending.then(()=>false),cancelled.promise.then(()=>true)]),true);
    }
    if(action==='drop') returning=stream.return().then(v=>{returnDone=true;return v;});
    let rejection='';
    try {await bounded(handle.submit({type:'text',text:'replacement'},{turnId:'replacement'}).next());}
    catch(e:any) {rejection=e.message;}
    assert.equal(rejection,'Agent handle already has an active turn.');
    if(action==='dispose') {
      disposing=factory.dispose().then(()=>{disposeDone=true;});
      // Deterministic microtask checkpoint: no I/O can complete while gate is closed.
      for(let i=0;i<100;i++) await Promise.resolve();
      assert.equal(handle.state,'draining');
    }
    const before={active:handle.inFlight,attempts,nextDone,returnDone,disposeDone,
      writes:readFileSync(path,'utf8').split('\n').filter(Boolean).map(l=>JSON.parse(l).type).filter(t=>t===kind).length};
    assert.deepEqual(before,{active:1,attempts:1,nextDone:false,returnDone:false,disposeDone:false,writes:0});
    release.resolve();
    const event=(await bounded(pending)).value;
    const afterWriteActive=handle.inFlight;
    if(returning) await bounded(returning); else await bounded(stream.return());
    if(disposing) await bounded(disposing);
    const after={active:handle.inFlight,attempts,writes:readFileSync(path,'utf8').split('\n').filter(Boolean).map(l=>JSON.parse(l).type).filter(t=>t===kind).length};
    assert.deepEqual(after,{active:0,attempts:1,writes:1});
    let replacementStarted=false, disposedRejection: string|null=null;
    if(action!=='dispose') {
      const replacement=handle.submit({type:'text',text:'next'},{turnId:'next'});
      for(let i=0;i<20;i++) {const item=await bounded(replacement.next()); if(item.value?.type==='turn_started'){replacementStarted=true;break;} assert.equal(item.done,false);}
      assert.equal(replacementStarted,true); await bounded(replacement.return());
    } else {
      try { await bounded(handle.submit({type:'text',text:'after dispose'},{turnId:'after-dispose'}).next()); }
      catch(e:any) { disposedRejection=e.message; }
      assert.equal(disposedRejection,'Cannot submit a turn; agent handle is disposed.');
    }
    matrix.push({name,before,after,afterWriteActive,event:event?.type,rejection,replacementStarted,disposedRejection,factoryDisposed:factory.state==='disposed'});
    await bounded(factory.dispose());
  }
}
assertBaseline(root);
writeFileSync(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,matrix},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify(matrix,null,2));
