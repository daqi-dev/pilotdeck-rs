// Offline Node v22 oracle: run the pinned production list and WS wrapper.
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';

if (process.versions.node.split('.')[0] !== '22') throw Error('Node v22 required');
const baseline = path.resolve(process.argv[2]);
const output = path.resolve(process.argv[3]);
const scratch = path.resolve(process.argv[4]);
const locale = new Intl.Collator().resolvedOptions().locale;
if (locale !== 'tr-TR') throw Error(`Start with LANG=tr_TR and unset LC_*; got ${locale}`);
await fs.mkdir(scratch, {recursive: true});
const cwd = await fs.realpath(await fs.mkdtemp(path.join(scratch, 'files-')));
// The control parents must be Latin-1 all the way to the filesystem root.
if ([...cwd].some(c => c.codePointAt(0)! > 0xff)) throw Error('Use a Latin-1 scratch path');
process.chdir(cwd);
const home = path.join(cwd, 'home');
const roots: Record<string, string> = {};
for (const [label, name] of [['chinese', '中文'], ['latin1', 'é'], ['ascii', 'project']]) {
  const root = roots[label] = path.join(cwd, name);
  await fs.mkdir(path.join(home, 'projects', label), {recursive: true});
  await fs.writeFile(path.join(home, 'projects', label, '.cwd'), root);
  for (const n of [12, 13]) {
    // Long segments at the leaf, parent, and middle; total path length alone
    // exceeds 13 even when every segment is short. A long A segment checks
    // that the representation also affects a separate, one-unit I segment.
    for (const relative of [
      `a/b/${'I'.repeat(n)}`, `${'I'.repeat(n)}/x`,
      `a/${'I'.repeat(n)}/z`, `${'A'.repeat(n)}/I`,
    ]) {
      await fs.mkdir(path.dirname(path.join(root, relative)), {recursive: true});
      await fs.writeFile(path.join(root, relative), 'boundary');
    }
  }
}
// An ASCII project key resolves to the Chinese canonical root.
const alias = path.join(cwd, 'alias');
await fs.symlink(roots.chinese, alias);
await fs.mkdir(path.join(home, 'projects', 'alias'), {recursive: true});
await fs.writeFile(path.join(home, 'projects', 'alias', '.cwd'), alias);
process.env.PILOT_HOME = home;
const source = (file: string) => path.join(baseline, 'src', file);
const load = (file: string) => import(pathToFileURL(source(file)).href);
const {GatewayWsConnection} = await load('gateway/server/GatewayWsConnection.ts');
const {InProcessGateway} = await load('gateway/client/InProcessGateway.ts');
const {listWebProjects} = await load('web/server/listProjects.ts');
const catalog = () => listWebProjects({pilotHome: home});
const gateway: any = {
  options: {listProjects: catalog}, listProjects: catalog, describeServer: async () => ({}),
  projectFilesList(input: any) {return InProcessGateway.prototype.projectFilesList.call(this, input)},
};
const cases: any[] = [];
async function capture(projectKey: string, query: string, tag: string) {
  let receive: any, finish: any;
  const pending = new Promise<string>(resolve => finish = resolve);
  const ws = {
    onMessage(fn: any) {receive = fn}, onClose() {},
    sendText(text: string) {if (JSON.parse(text).type === 'response') finish(text)},
    close() {throw Error('closed')},
  };
  new GatewayWsConnection(ws as any, {gateway, token: 'offline', serverVersion: 'offline'});
  receive(JSON.stringify({type: 'hello', protocolVersion: '1.2', clientName: 'web', clientVersion: 'offline', token: 'offline'}));
  await new Promise(resolve => setImmediate(resolve));
  const request = JSON.stringify({type: 'request', id: `round5-${cases.length + 1}`, method: 'project_files_list', params: {projectKey, query}});
  receive(request);
  const response = await pending;
  const parsed = JSON.parse(response);
  if (!parsed.ok) throw Error(response);
  cases.push({tag, request, response});
  console.log(JSON.stringify({tag, query, items: parsed.result.items.map((item: any) => ({path: item.relativePath, matches: item.matches}))}));
}
for (const [label, root] of Object.entries(roots)) {
  for (const n of [12, 13]) {
    await capture(root, 'I'.repeat(n), `${label}-ascii-${n}`);
    await capture(root, 'ı'.repeat(n), `${label}-dotless-${n}`);
  }
  await capture(root, 'i', `${label}-short-ascii`);
  await capture(root, 'ı', `${label}-short-dotless`);
}
await capture(alias, 'I'.repeat(13), 'canonical-root-alias-ascii-13');
await capture(alias, 'ı'.repeat(13), 'canonical-root-alias-dotless-13');

const files: Record<string, any> = {};
async function snapshot(p: string) {
  const l = await fs.lstat(p);
  if (l.isSymbolicLink()) {files[p] = {link: await fs.readlink(p)}; return}
  const s = await fs.stat(p);
  files[p] = {
    directory: s.isDirectory(), size: s.size, birth_ms: s.birthtimeMs,
    ctime_ms: s.ctimeMs, mtime_ms: s.mtimeMs,
    // Preserve the stat input's exact decimal for Rust f64::from_str; the
    // default serde_json float parser can round this number one ULP away.
    mtime_ms_exact: s.mtimeMs.toString(),
  };
  if (s.isDirectory()) {
    files[p].entries = (await fs.readdir(p, {withFileTypes: true})).map(e => [e.name, e.isDirectory()]);
    for (const [name] of files[p].entries) await snapshot(path.join(p, name));
  } else files[p].base64 = (await fs.readFile(p)).toString('base64');
}
await snapshot(cwd);
const result = {
  node: process.version, v8: process.versions.v8, icu: process.versions.icu,
  platform: process.platform, env: {LANG: process.env.LANG}, locale,
  listSourceSha256: createHash('sha256').update(await fs.readFile(source('gateway/dialog/projectFiles.ts'))).digest('hex'),
  cwd, home, roots, alias, files, cases,
};
await fs.mkdir(path.dirname(output), {recursive: true});
await fs.writeFile(output, JSON.stringify(result, null, 2) + '\n');
console.log(JSON.stringify({node: result.node, v8: result.v8, locale, output, cases: cases.length}));
