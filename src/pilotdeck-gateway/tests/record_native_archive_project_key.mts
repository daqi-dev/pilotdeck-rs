// Real TS native-archive project-key identity oracle.
// node --import <B>/node_modules/tsx/dist/loader.mjs <this> <B> <output> <scratch>
//
// Pins the `projectRegistry.ts:41-59` seam for the three `native_archive_*`
// methods: `path.resolve` folds `.`/`..` lexically before the registry compare,
// so a surrogate-bearing segment removed by `..` no longer prevents a lookup;
// if the folded key is still unregistered, `Unknown projectKey: ${projectKey}`
// keeps the *caller's* original UTF-16 units (not a lossy U+FFFD spelling).
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';

const baseline = path.resolve(process.argv[2]);
const output = path.resolve(process.argv[3]);
const scratch = await fs.realpath(path.resolve(process.argv[4]));
for (const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME']) {
  const value = await fs.realpath(process.env[key] ?? '');
  if (!value.startsWith(scratch + path.sep)) throw Error(`Unisolated ${key}`);
}
if (process.version !== 'v22.22.0') throw Error(`Node v22.22.0 required: ${process.version}`);
for (const key of ['PILOTDECK_SKIP_BROWSER_OPEN','PILOT_SKIP_BROWSER_OPEN','SKIP_BROWSER_OPEN','PILOTDECK_SKIP_BROWSER','GP_POLL_WATCH']) {
  if (process.env[key] !== '1') throw Error(`Missing guard ${key}`);
}
await fs.stat(output).then(() => { throw Error(`Output exists: ${output}`); }, (error) => {
  if (error.code !== 'ENOENT') throw error;
});
const head = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], {encoding: 'utf8'}).trim();
if (head !== 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85') throw Error(`Baseline HEAD ${head}`);

const cwd = path.join(scratch, 'project');
const home = path.join(scratch, 'pilot');
const chat = path.join(home, 'projects', 'registered', 'chats');
await fs.mkdir(cwd, {recursive: true});
await fs.mkdir(chat, {recursive: true});
await fs.writeFile(path.join(home, 'pilotdeck.yaml'), `schemaVersion: 1
agent: {model: test/model, maxContextTokens: 65536, maxOutputTokens: 8192}
model:
  providers:
    test:
      protocol: openai
      url: https://example.test/v1
      apiKey: fixture-key
      models: {model: {}}
extension:
  builtinPluginsEnabled: {windows-skills: false, browser-use: false, funasr: false}
telemetry: {enabled: false}
`);
await fs.writeFile(path.join(home, 'projects', 'registered', '.cwd'), cwd + '\n');
const entry = {type: 'message', sessionId: 'fixture', turnId: '', sequence: 0, createdAt: '2026-10-15T00:00:00Z', payload: {text: 'hello'}};
await fs.writeFile(path.join(chat, 'fixture.jsonl'), JSON.stringify(entry) + '\n');

process.chdir(cwd);
process.env.PILOT_HOME = home;
const load = (file: string) => import(pathToFileURL(path.join(baseline, 'src', file)).href);
const {createLocalGateway} = await load('cli/createLocalGateway.ts');
const {GatewayWsConnection} = await load('gateway/server/GatewayWsConnection.ts');
const local = createLocalGateway();
const cases: any[] = [];
async function record(id: string, projectKey: string) {
  let receive: any, finish: any;
  const pending = new Promise<string>((resolve) => { finish = resolve; });
  const ws = {
    onMessage(fn: any) { receive = fn; },
    onClose() {},
    close() { throw Error('unexpected close'); },
    sendText(raw: string) { if (JSON.parse(raw).type === 'response') finish(raw); },
  };
  const connection = new GatewayWsConnection(ws as any, {gateway: local.gateway, token: 'offline', serverVersion: 'offline'});
  receive(JSON.stringify({type: 'hello', protocolVersion: '1.2', clientName: 'web', clientVersion: 'offline', token: 'offline'}));
  await new Promise((resolve) => setImmediate(resolve));
  const request = JSON.stringify({type: 'request', id, method: 'native_archive_manifest', params: {sessionKey: 'fixture', projectKey}});
  receive(request);
  let timer: any;
  const response = await Promise.race([pending, new Promise<never>((_, reject) => { timer = setTimeout(() => reject(Error(`No response ${id}`)), 5000); })]).finally(() => clearTimeout(timer));
  cases.push({id, request, response});
}
try {
  // `..` folds away the surrogate-bearing segment; the folded key is unknown.
  await record('project-folded-unknown', `${scratch}/project-\ud800/../missing`);
  // Positive control for the same fold: the folded key is the registered cwd.
  await record('project-folded-registered', `${scratch}/project-\ud800/../project`);
  // Same fold with a relative spelling, resolved against the process cwd.
  await record('project-folded-unknown-relative', 'project-\ud800/../missing');
  // Positive control: literal U+FFFD is a different identity, not an alias.
  await record('project-literal-fffd-unknown', `${scratch}/project-\ufffd`);
  const files: any = {};
  const collect = async (target: string) => {
    const stat = await fs.stat(target);
    const fact: any = {directory: stat.isDirectory(), size: stat.size};
    files[target] = fact;
    if (stat.isDirectory()) {
      const entries = await fs.readdir(target, {withFileTypes: true});
      entries.sort((left, right) => left.name < right.name ? -1 : left.name > right.name ? 1 : 0);
      fact.entries = entries.map((item) => [item.name, item.isDirectory()]);
      for (const [name] of fact.entries) await collect(path.join(target, name));
    } else if (stat.size <= 10_000_000) {
      fact.base64 = (await fs.readFile(target)).toString('base64');
    }
  };
  await collect(home);
  await collect(cwd);
  const replace = (value: string) => value.split(scratch).join('/fixture');
  for (const fact of Object.values(files) as any[]) {
    if (!fact.base64) continue;
    const bytes = Buffer.from(fact.base64, 'base64');
    const parts: Buffer[] = [];
    let start = 0, at = 0;
    const needle = Buffer.from(scratch);
    while ((at = bytes.indexOf(needle, start)) >= 0) {
      parts.push(bytes.subarray(start, at), Buffer.from('/fixture'));
      start = at + needle.length;
    }
    parts.push(bytes.subarray(start));
    const normalized = Buffer.concat(parts);
    fact.base64 = normalized.toString('base64');
    fact.size = normalized.length;
  }
  const normalizedFiles: any = {};
  for (const [key, value] of Object.entries(files)) normalizedFiles[replace(key)] = value;
  const sources: any = {};
  for (const file of ['cli/createLocalGateway.ts','gateway/server/GatewayWsConnection.ts','gateway/dialog/projectRegistry.ts','session/transcript/TranscriptReader.ts']) {
    sources[file] = createHash('sha256').update(await fs.readFile(path.join(baseline, 'src', file))).digest('hex');
  }
  const normalizeCase = (item: any) => JSON.parse(replace(JSON.stringify(item)));
  await fs.mkdir(path.dirname(output), {recursive: true});
  await fs.writeFile(output, JSON.stringify({
    baselineCommit: head,
    node: process.version,
    icu: process.versions.icu,
    sources,
    home: replace(home),
    cwd: replace(cwd),
    files: normalizedFiles,
    cases: cases.map(normalizeCase),
  }, null, 2) + '\n');
  console.log(JSON.stringify({status: 'PASS', cases: cases.length, output}));
} finally {
  await local.dispose();
}
