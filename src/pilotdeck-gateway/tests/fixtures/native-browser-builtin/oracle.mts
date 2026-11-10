import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
const [baseline,out]=process.argv.slice(2);await mkdir(out,{recursive:false});assert.equal(process.version,'v22.22.0');
assert.equal(execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const home=out+'/home',project=out+'/project';await mkdir(home);await mkdir(project);process.env.HOME=home;process.env.PILOT_HOME=home;
const {PluginRuntime}=await import(pathToFileURL(baseline+'/src/extension/plugins/runtime/PluginRuntime.ts').href);
const {loadPluginFromPath}=await import(pathToFileURL(baseline+'/src/extension/plugins/loading/PluginLoader.ts').href);
const {loadBuiltinPlugins}=await import(pathToFileURL(baseline+'/src/extension/plugins/builtin/loadBuiltinPlugins.ts').href);
const pluginPath=baseline+'/dist/src/extension/plugins/builtin/browser-use';const plugin=await loadPluginFromPath(pluginPath,'builtin');
const sha=(v:string|Buffer)=>createHash('sha256').update(v).digest('hex');const cases=[];
for(const enabled of [true,false]){const runtime=new PluginRuntime({projectRoot:project,pilotHome:home,builtinPlugins:[plugin],builtinPluginsEnabled:{'browser-use':enabled}});
try{await runtime.refresh();const skills=runtime.getAllSkills();const qualified=await runtime.loadSkillPrompt('browser-use:browser-use-install');const short=await runtime.loadSkillPrompt('browser-use-install');cases.push({enabled,skills:skills.map((s:any)=>({name:s.name,description:s.description,pathSuffix:s.path.split('/browser-use/').at(-1),contentSha256:sha(s.content)})),mcpServers:runtime.snapshotSessionContributions().mcpServers,readQualifiedSha256:qualified===undefined?null:sha(qualified),readShortSha256:short===undefined?null:sha(short)});}finally{await runtime.dispose();}}
const sources:any={};for(const path of ['src/extension/plugins/runtime/PluginRuntime.ts','src/extension/plugins/loading/PluginLoader.ts','src/extension/plugins/builtin/loadBuiltinPlugins.ts','dist/src/extension/plugins/builtin/browser-use/plugin.json','dist/src/extension/plugins/builtin/browser-use/skills/browser-use-install/SKILL.md'])sources[path]=sha(await readFile(baseline+'/'+path));
await writeFile(out+'/oracle.json',JSON.stringify({baselineCommit:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',node:process.versions,platform:process.platform,defaultBuiltinNames:loadBuiltinPlugins().map((p:any)=>p.name),sources,cases,normalization:'Only skill path is represented as actual suffix below browser-use; model body checked by SHA256, not rewritten.'},null,2)+'\n');
