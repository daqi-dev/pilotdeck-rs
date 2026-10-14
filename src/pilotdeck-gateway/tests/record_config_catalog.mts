// Offline source + WS encoding oracle, with an injected transport (no listener).
// Usage: node --import <baseline>/node_modules/tsx/dist/loader.mjs <this>
//        <baseline> <output.json> <scratch-directory>
import { mkdir, mkdtemp, writeFile, readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';

const baseline = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
const scratch = resolve(process.argv[4]);
if (Number(process.versions.node.split('.')[0]) !== 22) throw Error('Use Node v22');
await mkdir(scratch, {recursive: true});
const root = await mkdtemp(join(scratch, 'catalog-'));
const source = (path: string) => import(pathToFileURL(join(baseline, 'src', path)).href);
const { listModelCatalog } = await source('gateway/dialog/modelCatalog.ts');
const { loadPilotConfig } = await source('pilot/config/loadPilotConfig.ts');
const { GatewayWsConnection } = await source('gateway/server/GatewayWsConnection.ts');
const base = 'schemaVersion: 1\nagent:\n  model: parity/deterministic\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n';
const cases: any[] = [];
const add = (id: string, yaml: string, params: any = {}, env: Record<string, string> = {}) => cases.push({id, yaml, params, env});
add('basic', base);
cases.push({id: 'input-missing', yaml: base, env: {}});
add('input-null', base, null);
add('query-number', base, {query: 7});
add('project-unregistered', base, {projectKey: '/not/registered'});
add('query-trim-case', base, {query: '  PARITY  '});
add('unknown-filter-order-ignored', base, {filter: 'nothing', order: 'reverse'});
for (const [id, literal] of [
  ['high-surrogate', '"\\uD800"'], ['low-surrogate', '"\\uDC00"'],
  ['surrogate-pair', '"\\uD83D\\uDE00"'], ['codepoint', '"\\U0001F600"'],
] as const) add(id, base.replace('deterministic: {}', `deterministic: {displayName: ${literal}}`));
add('version-11-model', '%YAML 1.1\n---\n' + base.replace('deterministic: {}', 'deterministic: {displayName: yes}'));
add('version-12-model', '%YAML 1.2\n---\n' + base.replace('deterministic: {}', 'deterministic: {displayName: yes}'));
add('duplicate-version', '%YAML 1.1\n%YAML 1.2\n---\n' + base);
add('two-tag-directives', '%TAG !e! tag:example.com,2020:\n%TAG !f! tag:example.org,2021:\n---\n' + base.replace('deterministic: {}', 'deterministic: {displayName: !e!name Example}'));
add('bad-yaml-before-query', 'a: [\n', {query: 7});
add('missing-agent-before-query', '{}\n', {query: 7});
add('provider-warning', base.replace('    parity:', '    broken: {protocol: not-supported}\n    parity:'));
add('provider-warning-agent-fatal', base.replace('protocol: openai', 'protocol: not-supported'));
add('env-empty', base, {}, {PILOT_AGENT_MODEL: ''});
add('env-untrimmed', base, {}, {PILOT_AGENT_MODEL: ' parity/deterministic '});
add('env-replaces-primitive-agent', base.replace('agent:\n  model: parity/deterministic', 'agent: 7'), {}, {PILOT_AGENT_MODEL: 'parity/deterministic'});
add('extension-before-gateway', base + 'extension: 7\ngateway: 7\n');
add('memory-throw-before-extension-fatal', base + 'extension: 7\nmemory: {maxMessageChars: -1}\n');
add('modules-invalid', base + 'modules: 7\n');
add('staffdeck-number-modules', base + 'modules: 7\n', {}, {STAFFDECK_PROJECT_BINDING_ENABLED: 'true'});
add('staffdeck-string-modules', base + 'modules: text\n', {}, {STAFFDECK_PROJECT_BINDING_ENABLED: 'true'});
add('staffdeck-false-modules', base + 'modules: false\n', {}, {STAFFDECK_PROJECT_BINDING_ENABLED: 'true'});
add('staffdeck-null-modules', base + 'modules: null\n', {}, {STAFFDECK_PROJECT_BINDING_ENABLED: 'true'});
add('knowledge-invalid', base + 'modules: {knowledge: 7, sop: 7}\n');
add('staffdeck-disables-knowledge-sop', base + 'modules: {knowledge: 7, sop: 7}\n', {}, {STAFFDECK_PROJECT_BINDING_ENABLED: 'true'});
add('staffdeck-case-sensitive', base + 'modules: {knowledge: 7, sop: 7}\n', {}, {STAFFDECK_PROJECT_BINDING_ENABLED: 'TRUE'});

if (process.argv[5]) cases.splice(0, cases.length, ...JSON.parse(await readFile(resolve(process.argv[5]), 'utf8')));
const results: any[] = [];
for (const test of cases) {
  const home = join(root, test.id);
  await mkdir(home);
  let configPath = join(home, 'pilotdeck.yaml');
  const env: Record<string,string> = {PILOT_HOME: home, ...test.env};
  if (test.sourceKind === 'blank') env.PILOTDECK_CONFIG_PATH = '';
  if (test.sourceKind === 'spaces') env.PILOTDECK_CONFIG_PATH = '  ';
  if (test.sourceKind === 'directory') {await mkdir(configPath);}
  else if (test.sourceKind === 'bytes') await writeFile(configPath,Buffer.concat([Buffer.from(test.yaml),Buffer.from('unused: "'),Buffer.from([0xff,0xfe,0x80]),Buffer.from('"\n')]));
  else if (test.sourceKind !== 'missing') await writeFile(configPath, test.yaml);
  if (test.sourceKind === 'relative') env.PILOTDECK_CONFIG_PATH = configPath.slice(process.cwd().length + 1);
  if (test.sourceKind === 'alternate') {const alternate = join(home,'other.yaml');await writeFile(alternate,test.yaml);env.PILOTDECK_CONFIG_PATH='  '+alternate+'  ';}

  let load: any;
  try {
    const snapshot = loadPilotConfig({env});
    load = {ok: true, diagnostics: snapshot.diagnostics};
  } catch (error: any) {
    load = {ok: false, name: error.name, code: error.code, message: error.message, diagnostics: error.diagnostics};
  }
  let receive: (text: string) => void = () => {throw Error('receiver not registered')};
  let finish: (text: string) => void = () => {throw Error('response not registered')};
  const pending = new Promise<string>(done => {finish = done});
  const ws = {
    onMessage(fn: typeof receive) {receive = fn}, onClose() {},
    sendText(text: string) {if (JSON.parse(text).type === 'response') finish(text)},
    close() {throw Error('unexpected close')},
  };
  const gateway = {describeServer: async () => ({}), modelCatalogList: (input: any) => listModelCatalog(input, env)};
  new GatewayWsConnection(ws as any, {gateway: gateway as any, token: 'offline', serverVersion: 'offline'});
  receive(JSON.stringify({type: 'hello', protocolVersion: '1.2', clientName: 'web', clientVersion: 'offline', token: 'offline'}));
  await new Promise(done => setImmediate(done));
  const request = JSON.stringify({type: 'request', id: test.id, method: 'model_catalog_list', ...('params' in test ? {params: test.params} : {})});
  receive(request);
  let timer: NodeJS.Timeout | undefined;
  const timeout = new Promise<never>((_, reject) => {timer = setTimeout(() => reject(Error(`No response: ${test.id}`)), 5000)});
  let response: string;
  try {response = await Promise.race([pending, timeout])} finally {clearTimeout(timer)}
  // Only diagnostic file paths depend on scratch; the wire responses do not.
  let serialized = JSON.stringify(load).split(home).join('<PILOT_HOME>');
  if (test.sourceKind === 'relative') serialized = serialized.split(env.PILOTDECK_CONFIG_PATH).join('relative.yaml');
  load = JSON.parse(serialized);
  results.push({...test, load, request, response});
}
const baselineCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], {encoding: 'utf8'}).trim();
await writeFile(output, JSON.stringify({baselineCommit, node: process.version, icu: process.versions.icu, cases: results}, null, 2) + '\n');
console.log(JSON.stringify({node: process.version, cases: results.length, output, scratch: root}));
