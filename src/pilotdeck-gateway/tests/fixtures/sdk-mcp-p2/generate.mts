// Real cc7c4e6 methods; hooks only intercept Map writes/config reads and
// enqueue the second operation as a microtask. No reimplementation of MCP.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
assert.equal(process.version, 'v22.22.0', '#347 exact Node baseline');
assert.ok(process.env.PDRS_BASELINE, '#347 set PDRS_BASELINE');
assert.ok(process.argv[2], '#347 provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const source = 'src/cli/ProjectRuntimeRegistry.ts';
const git = realpathSync(execFileSync('which', ['git'], {encoding:'utf8'}).trim());
bindBaseline({ baseline: baseline, commit: sourceCommit, gitExecutable: git });
const sourceSha256 = sourceClosureSha256(baseline, sourceCommit, git, [source]);
const { ProjectRuntimeRegistry } = await import(pathToFileURL(join(baseline, source)).href);
const input = (name = 'b') => ({sessionKey:'s', servers:{[name]:{type:'stdio',command:'node'}}});
function setup() {
  const events: string[] = [];
  const registry: any = Object.create(ProjectRuntimeRegistry.prototype);
  for (const key of ['sdkSessionConfigs','sdkSessionPlugins','sdkSessionMcpServers','sdkMcpPermissionOverrides','sdkMcpPermissionRules','sdkThinkingOverrides','sdkSessionProjects','sdkConfigChangeHandlers','sessionHookHandlers']) registry[key] = new Map();
  registry.clearTaskBudgetLedger = () => {};
  registry.resolve = () => ({projectRoot:'/pdrs-fixture/project',pluginRuntime:{refresh:async()=>{},snapshot:()=>[]}});
  registry.gateway = {closeSession:async()=>{}};
  return {registry,events};
}
const cases = [];
for (const id of ['set-then-toggle','clear-then-set','set-then-clear']) {
  const {registry:r,events} = setup();
  let second: Promise<any> | undefined;
  if (id === 'clear-then-set') {
    await r.setMcpServersForSdk(input('a'));
    const original = r.sdkSessionMcpServers.delete.bind(r.sdkSessionMcpServers);
    r.sdkSessionMcpServers.delete = (key:string) => {
      events.push('clear.servers');
      queueMicrotask(()=>{events.push('second.set');second=r.setMcpServersForSdk(input());});
      return original(key);
    };
    r.clearSdkSessionState('s');
    events.push('clear.return');
    assert.equal(r.sdkSessionProjects.has('s'),false, '#347 clear completes both writes');
    await Promise.resolve();
    await second;
    assert.deepEqual(events, ['clear.servers','clear.return','second.set']);
  } else {
    if (id === 'set-then-toggle') await r.setMcpServersForSdk(input());
    const first = input();
    if (id === 'set-then-toggle') {
      let queued = false;
      Object.defineProperty(first.servers.b, 'command', {enumerable:true, get() {
        if (!queued) {
          queued = true;
          events.push('set.config');
          queueMicrotask(()=>{events.push('second.toggle');second=r.toggleMcpServerForSdk({sessionKey:'s',serverName:'b',enabled:false});});
        }
        return 'node';
      }});
    }
    const original = r.sdkSessionMcpServers.set.bind(r.sdkSessionMcpServers);
    r.sdkSessionMcpServers.set = (key:string,value:any) => {
      events.push('set.servers');
      if (id === 'set-then-clear') queueMicrotask(()=>{events.push('second.clear');r.clearSdkSessionState('s');});
      return original(key,value);
    };
    const originalProject = r.sdkSessionProjects.set.bind(r.sdkSessionProjects);
    r.sdkSessionProjects.set = (key:string,value:any) => {events.push('set.projects'); return originalProject(key,value);};
    await r.setMcpServersForSdk(first);
    await second;
    assert.deepEqual(events, id === 'set-then-toggle'
      ? ['set.config','set.servers','set.projects','second.toggle']
      : ['set.servers','set.projects','second.clear']);
  }
  const row = {id, events, servers:r.sdkSessionMcpServers.has('s'), projects:r.sdkSessionProjects.has('s'), enabled:r.sdkSessionMcpServers.get('s')?.get('b')?.enabled ?? null, names:[...(r.sdkSessionMcpServers.get('s')?.keys() ?? [])], projectRoot:r.sdkSessionProjects.get('s') ?? null};
  assert.equal(row.servers, id !== 'set-then-clear');
  assert.equal(row.projects, row.servers);
  assert.equal(row.enabled, id === 'set-then-toggle' ? false : id === 'clear-then-set' ? true : null);
  cases.push(row);
  console.log(JSON.stringify(row));
}
await mkdir(output, {recursive:true});
await writeFile(join(output, 'oracle.json'), JSON.stringify({sourceCommit, sourceSha256, node:process.version, cases}, null, 2)+'\n', {flag:'wx'});
console.log('PASS: Node v22.22.0; 3 real TS interleavings');
