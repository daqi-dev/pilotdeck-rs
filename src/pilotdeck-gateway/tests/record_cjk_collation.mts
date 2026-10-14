// Fresh-process, offline Node v22 oracle. Launch through probe_cjk_collation.py.
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
const baseline = path.resolve(process.argv[2]);
const scratch = path.resolve(process.argv[3]);
if (process.versions.node.split('.')[0] !== '22') throw Error('Node v22 required');
const source = (name: string) => path.join(baseline, 'src', name);
const load = (name: string) => import(pathToFileURL(source(name)).href);
const { listModelCatalog } = await load('gateway/dialog/modelCatalog.ts');
const { listProjectFiles } = await load('gateway/dialog/projectFiles.ts');
const { GatewayWsConnection } = await load('gateway/server/GatewayWsConnection.ts');
await fs.mkdir(scratch, { recursive: true });
const cwd = await fs.realpath(await fs.mkdtemp(path.join(scratch, 'cjk-')));
const home = path.join(cwd, 'home');
await fs.mkdir(home);
process.env.PILOT_HOME = home;
const env = Object.fromEntries(['LANG', 'LC_ALL', 'LC_MESSAGES'].filter(k => k in process.env).map(k => [k, process.env[k]]));
const input = ['中', '中文', '㐀', '㐁', '㐂', '㐃', '㐄', '㐅', '䶿', '一', '丁', '七', '万', '丈', '上', '下', '不', '与', '丐', '丑', '丙', '龥', '龦', '龿', '鿿', '𠀀', '𠀁', '𠀂', '𪛟', '豈', '更', '﨎', '﨑', '龎', '丽', '丸', '𪘀', 'a', 'A', 'ä', 'e', 'é', 'z', 'Z', '😀'];
const order = (names: string[], locale?: string) => names.slice().sort((a, b) => a.localeCompare(b, locale));
const explicit = ['en-US', 'und', 'en-US-u-co-unihan', 'zh-CN', 'zh-u-co-pinyin', 'zh-TW', 'zh-u-co-stroke', 'ja-JP', 'ja-u-co-unihan', 'zh-u-co-unihan'].map(locale => ({ locale, resolved: new Intl.Collator(locale).resolvedOptions(), order: order(input, locale) }));
const cases: any[] = [];
async function capture(method: string, params: any, tag: string) {
  let receive: any, finish: any;
  const pending = new Promise<string>(done => finish = done);
  const ws = {onMessage(fn: any) {receive = fn}, onClose() {}, sendText(text: string) {if (JSON.parse(text).type === 'response') finish(text)}, close() {throw Error('closed')}};
  const gateway = {describeServer: async () => ({}), modelCatalogList: (input: any) => listModelCatalog(input, process.env), projectFilesList: listProjectFiles};
  new GatewayWsConnection(ws as any, {gateway: gateway as any, token: 'offline', serverVersion: 'offline'});
  receive(JSON.stringify({type: 'hello', protocolVersion: '1.2', clientName: 'web', clientVersion: 'offline', token: 'offline'}));
  await new Promise(done => setImmediate(done));
  const request = JSON.stringify({type: 'request', id: tag, method, params});
  receive(request);
  const response = await pending;
  if (!JSON.parse(response).ok) throw Error(response);
  cases.push({tag, request, response});
  return JSON.parse(response).result;
}
const yamls: Record<string, string> = {};
for (const [tag, names] of [['minimal', ['中', '㐀']], ['mixed', input], ['basic', ['中', '中文', '一', '丁', '不']]] as const) {
  const yaml = 'schemaVersion: 1\nagent:\n  model: parity/中\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n' + names.map(name => `        ${JSON.stringify(name)}: {}\n`).join('');
  yamls[tag] = yaml;
  await fs.writeFile(path.join(home, 'pilotdeck.yaml'), yaml);
  await capture('model_catalog_list', {}, `catalog-${tag}`);
  const project = path.join(cwd, tag);
  await fs.mkdir(project);
  await fs.mkdir(path.join(home, 'projects', tag), {recursive: true});
  await fs.writeFile(path.join(home, 'projects', tag, '.cwd'), project);
  for (const name of names) await fs.writeFile(path.join(project, name), 'fixture');
  await capture('project_files_list', {projectKey: project}, `files-${tag}`);
  const first = await capture('project_files_list', {projectKey: project, limit: 1}, `files-${tag}-page1`);
  await capture('project_files_list', {projectKey: project, limit: 1, cursor: first.nextCursor}, `files-${tag}-page2`);
}
const files: Record<string, any> = {};
async function snapshot(p: string) {
  const s = await fs.stat(p);
  files[p] = {directory: s.isDirectory(), size: s.size, birth_ms: s.birthtimeMs, ctime_ms: s.ctimeMs, mtime_ms: s.mtimeMs, mtime_ms_exact: s.mtimeMs.toString()};
  if (s.isDirectory()) {
    files[p].entries = (await fs.readdir(p, {withFileTypes: true})).map(e => [e.name, e.isDirectory()]);
    for (const [name] of files[p].entries) await snapshot(path.join(p, name));
  } else files[p].base64 = (await fs.readFile(p)).toString('base64');
}
await snapshot(cwd);
const sourceSha256 = Object.fromEntries(await Promise.all(['gateway/dialog/modelCatalog.ts', 'gateway/dialog/projectFiles.ts'].map(async name => [name, createHash('sha256').update(await fs.readFile(source(name))).digest('hex')])));
console.log(JSON.stringify({node: process.version, icu: process.versions.icu, cldr: process.versions.cldr, unicode: process.versions.unicode, env, resolved: new Intl.Collator().resolvedOptions(), minimal: order(['中', '㐀']), input, order: order(input), explicit, cwd, home, files, yamls, cases, sourceSha256}));
