// Execute the fixed real TS loader with owned files, no provisioning or network.
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {pathToFileURL} from 'node:url';
const [baselineArg, outputArg] = process.argv.slice(2);
if (!baselineArg || !outputArg) throw Error('usage: node --import <tsx-loader> generate.mjs <fixed-baseline> <new-output>');
const baseline = fs.realpathSync(baselineArg), output = path.resolve(outputArg);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
if (process.version !== 'v22.22.0' || process.versions.icu !== '78.2') throw Error('Node/ICU mismatch');
if (execFileSync('git', ['-C',baseline,'rev-parse','HEAD'], {encoding:'utf8'}).trim() !== commit) throw Error('baseline mismatch');
const sources = {};
for (const name of ['src/pilot/config/loadPilotConfig.ts','src/pilot/staffdeck/projectBinding.ts']) {
  const expected = execFileSync('git',['-C',baseline,'show',`${commit}:${name}`]);
  const actual = fs.readFileSync(path.join(baseline,name));
  if (!expected.equals(actual)) throw Error(`modified baseline ${name}`);
  sources[name] = createHash('sha256').update(actual).digest('hex');
}
if (sources['src/pilot/staffdeck/projectBinding.ts'] !== '845bcf26546ff25901ce31aa93a588cd0971a639fe3af05cb8109f2e53fceca2') throw Error('binding source mismatch');
fs.mkdirSync(output);
process.env.HOME = path.join(output,'bootstrap-home');
process.env.PILOT_HOME = process.env.HOME;
fs.mkdirSync(process.env.HOME);
let network = 0;
globalThis.fetch = () => { network++; throw Error('network forbidden in readonly binding oracle'); };
const {loadPilotConfig} = await import(pathToFileURL(path.join(baseline,'src/pilot/config/loadPilotConfig.ts')));
const yaml = 'schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: https://model.invalid/v1\n      apiKey: mock-only\n      models: {mock: {}}\nmodules:\n  knowledge: {enabled: true}\n  sop: {enabled: true, timeoutMs: 123, discoveryTimeoutMs: 456}\n';
const env = {STAFFDECK_PROJECT_BINDING_ENABLED:'true',STAFFDECK_COPY_TENANT_ID:'fixture-tenant',STAFFDECK_FORMAL_API_ORIGIN:'https://staffdeck.invalid/',STAFFDECK_SOP_RUNTIME_ORIGIN:'https://staffdeck.invalid/runtime',STAFFDECK_SOP_MANAGEMENT_API_KEY:'mock-management-only'};
const binding = {projectRoot:'<project>',bindingId:'fixture-binding',agentName:'fixture',origin:'https://staffdeck.invalid',tenantId:'fixture-tenant',agentId:'agent /中',status:'bound',updatedAt:'2026-01-01T00:00:00.000Z'};
const store = row => JSON.stringify({version:1,installationId:'fixture-installation',projects:[{...binding,...row}]});
const cases = [
  {id:'missing-store'},
  {id:'empty-store',store:''},
  {id:'malformed-store',store:'{bad'},
  {id:'null-store',store:'null'},
  {id:'wrong-version',store:'{"version":2,"installationId":"fixture","projects":[]}'},
  {id:'wrong-projects',store:'{"version":1,"installationId":"fixture","projects":{}}'},
  {id:'null-project-row',store:'{"version":1,"installationId":"fixture","projects":[null]}'},
  {id:'project-mismatch',store:store({projectRoot:'<project>/other'})},
  {id:'pending',store:store({status:'pending'})},
  {id:'failed',store:store({status:'failed'})},
  {id:'tenant-mismatch',store:store({tenantId:'other'})},
  {id:'origin-mismatch',store:store({origin:'https://other.invalid'})},
  {id:'bound-missing-sops',store:store({})},
  {id:'bound-valid-sops',store:store({}),sops:'sops:\n  - id: fixture-sop\n'},
  {id:'bound-legacy-sop-id',store:store({}),sops:'sops:\n  - skill_id: legacy-sop\n'},
  {id:'bound-empty-default',store:store({}),sops:'sops:\n  - id: ""\n'},
  {id:'bound-malformed-sops',store:store({}),sops:'{bad'},
  {id:'binding-live',store:store({status:'binding',runtimeReady:true}),lock:'alive',sops:'sops:\n  - id: fixture-sop\n'},
  {id:'binding-dead',store:store({status:'binding',runtimeReady:true}),lock:'dead'},
  {id:'binding-unready',store:store({status:'binding',runtimeReady:false}),lock:'alive'},
  {id:'binding-no-lock',store:store({status:'binding',runtimeReady:true})},
  {id:'binding-malformed-lock',store:store({status:'binding',runtimeReady:true}),lock:'not-a-pid'},
  {id:'number-binding-id',store:store({bindingId:4})},
  {id:'null-binding-id',store:store({bindingId:null})},
  {id:'surrogate-agent-uri',store:store({agentId:'\ud800'})},
  {id:'store-before-agent-fatal',yaml:'schemaVersion: 1\n',store:'{bad'},
  {id:'store-before-yaml-fatal',yaml:'a: [\n',store:'{bad'},
  {id:'yaml-fatal-without-store',yaml:'a: [\n'},
  {id:'agent-fatal-without-store',yaml:'schemaVersion: 1\n'},
  {id:'host-required-remains-explicit',yaml:'schemaVersion: 1\nagent: {model: constructor/m}\nmodel: {}\n',nativeTodo:true},
];
function manifest(root) {
  const result = {};
  const walk = dir => { for (const item of fs.readdirSync(dir,{withFileTypes:true})) {
    const file = path.join(dir,item.name);
    if (item.isDirectory()) walk(file); else result[path.relative(root,file)] = createHash('sha256').update(fs.readFileSync(file)).digest('hex');
  }};
  walk(root);return result;
}
const originalCwd=process.cwd(), rows=[];
try {
  for (const test of cases) {
    const root=path.join(output,test.id), home=path.join(root,'home'), project=path.join(root,'project');
    fs.mkdirSync(root);fs.mkdirSync(home);fs.mkdirSync(project);fs.mkdirSync(path.join(home,'staffdeck'));
    fs.writeFileSync(path.join(home,'pilotdeck.yaml'),test.yaml??yaml);
    if (test.store!==undefined) fs.writeFileSync(path.join(home,'staffdeck/project-bindings.json'),test.store.replaceAll('<project>',project));
    if (test.sops!==undefined) {fs.mkdirSync(path.join(home,'staffdeck/fixture-binding'));fs.writeFileSync(path.join(home,'staffdeck/fixture-binding/sops.json'),test.sops);}
    if (test.lock!==undefined) fs.writeFileSync(path.join(home,'staffdeck/project-bindings.json.lock'),test.lock==='alive'?String(process.pid):test.lock==='dead'?'2147483647':test.lock);
    const before=manifest(root), actualEnv={...env,HOME:home,PILOT_HOME:home};
    process.chdir(project);
    let result;
    try { const snapshot=loadPilotConfig({env:actualEnv,projectRoot:'.'}); result={ok:true,modulesJson:JSON.stringify(snapshot.config.modules)}; }
    catch(error) {result={ok:false,name:error.name,code:error.code??null,message:error.message};}
    const after=manifest(root);
    if (JSON.stringify(before)!==JSON.stringify(after)) throw Error('readonly loader modified files');
    const raw={...test,yaml:test.yaml??yaml,env,result,manifestUnchanged:true,network};
    fs.writeFileSync(path.join(root,'raw.json'),JSON.stringify(raw,null,2)+'\n',{flag:'wx'});
    rows.push(JSON.parse(JSON.stringify(raw).replaceAll(root,'<root>')));
  }
} finally {process.chdir(originalCwd);}
if (network!==0) throw Error('unexpected network');
fs.writeFileSync(path.join(output,'oracle.json'),JSON.stringify({baseline:commit,node:process.version,icu:process.versions.icu,sources,cases:rows},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({cases:rows.length,network,output}));
