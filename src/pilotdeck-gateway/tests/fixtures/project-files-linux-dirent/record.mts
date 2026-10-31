// #133 Linux dirent probe: run the pinned TS baseline's real projectFilesList
// against a tree whose filenames are raw non-UTF-8 bytes, and record the wire
// responses. Offline: no service is started, the baseline checkout is never
// written to, and every path comes from argv (the Rust test owns the tree).
//
// Usage: node --import <baseline>/node_modules/tsx/dist/loader.mjs record.mts \
//          <baseline> <pilotHome> <projectRoot> <requests.json> <output.json>
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import {pathToFileURL} from 'node:url';
import {spawnSync} from 'node:child_process';

const PINNED_NODE = 'v22.22.0';
const PINNED_ICU = '78.2';
const PINNED_BASELINE = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const blocked = (message: string) => {
  console.error(`BLOCKED #133 ${message}`);
  process.exit(3);
};

// Print the environment contract first; a wrong host is BLOCKED, not a parity
// failure. The coordinator's runner also prints this before anything else.
console.log(JSON.stringify({node: process.version, icu: process.versions.icu, uv: process.versions.uv, platform: process.platform, locale: new Intl.Collator().resolvedOptions().locale}));
if (process.version !== PINNED_NODE) blocked(`requires Node ${PINNED_NODE}, found ${process.version}`);
if (process.versions.icu !== PINNED_ICU) blocked(`requires ICU ${PINNED_ICU}, found ${process.versions.icu}`);
const locale = new Intl.Collator().resolvedOptions().locale;
if (locale !== 'en-US') blocked(`resolved locale ${locale}; run with LANG=C.UTF-8 and no LC_* override`);

const baseline = path.resolve(process.argv[2]);
const pilotHome = path.resolve(process.argv[3]);
const root = path.resolve(process.argv[4]);
const requestsPath = path.resolve(process.argv[5]);
const outputPath = path.resolve(process.argv[6]);

// The baseline must be the pinned checkout; a different HEAD is not the
// recorded oracle source. Recorded either way so the evidence shows it.
const git = (args: string[]) => spawnSync('git', args, {encoding: 'utf8'});
const head = git(['--no-replace-objects', '-C', baseline, 'rev-parse', 'HEAD']);
if (head.status !== 0) blocked(`cannot read baseline HEAD: ${head.stderr}`);
const status = git(['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no']);
if (status.status !== 0) blocked(`cannot read baseline status: ${status.stderr}`);
const baselineHead = head.stdout.trim();
const baselineStatus = status.stdout.trim();
if (baselineHead !== PINNED_BASELINE) blocked(`baseline HEAD ${baselineHead} is not the pinned ${PINNED_BASELINE}`);
if (baselineStatus) blocked(`baseline has uncommitted changes: ${baselineStatus.replace(/\n/g, '; ')}`);

// Same loading contract as record_project_files.mts: real pinned sources.
const source = (file: string) => path.join(baseline, 'src', file);
const load = (file: string) => import(pathToFileURL(source(file)).href);
const {GatewayWsConnection} = await load('gateway/server/GatewayWsConnection.ts');
const {InProcessGateway} = await load('gateway/client/InProcessGateway.ts');
const {listWebProjects} = await load('web/server/listProjects.ts');
const {createDialogProjectRegistry} = await load('gateway/dialog/projectRegistry.ts');

process.env.PILOT_HOME = pilotHome;
const catalog = () => listWebProjects({pilotHome});
const projects = createDialogProjectRegistry({pilotHome, listProjects: async () => (await catalog()).projects});
const gateway: any = {
  options: {listProjects: catalog},
  listProjects: catalog,
  describeServer: async () => ({}),
  projectFilesList(input: any) {
    return InProcessGateway.prototype.projectFilesList.call(this, input);
  },
};

// Dirent layer witness: Node's own view of the raw-byte tree root, before
// the walk's realpath/stat pipeline skips anything. readdir yields lossy
// names; the byte-order sort mirrors Rust's (stable sort by the decoded
// name's UTF-8 bytes), so equal names keep readdir order on both sides.
const rawDirents = await fs.readdir(root, {withFileTypes: true});
const dirent = (entry: any) => ({name: entry.name, isDirectory: entry.isDirectory()});
const byteSorted = [...rawDirents]
  .sort((a, b) => Buffer.compare(Buffer.from(a.name, 'utf8'), Buffer.from(b.name, 'utf8')))
  .map(dirent);

// Real request/response wire through GatewayWsConnection's own encoder.
const responses: any[] = [];
for (const request of JSON.parse(await fs.readFile(requestsPath, 'utf8'))) {
  let receive: any;
  let finish: any;
  const pending = new Promise<string>((resolve) => (finish = resolve));
  const ws = {
    onMessage(fn: any) { receive = fn; },
    onClose() {},
    sendText(text: string) { if (JSON.parse(text).type === 'response') finish(text); },
    close() { throw Error('closed'); },
  };
  new GatewayWsConnection(ws as any, {gateway, token: 'offline', serverVersion: 'offline'});
  receive(JSON.stringify({type: 'hello', protocolVersion: '1.2', clientName: 'web', clientVersion: 'offline', token: 'offline'}));
  await new Promise((resolve) => setImmediate(resolve));
  const text = JSON.stringify({type: 'request', id: request.id, method: request.method, params: request.params});
  receive(text);
  const response = await pending;
  if (JSON.parse(response).id !== request.id) throw Error(`response id mismatch for ${request.id}`);
  responses.push({request: text, response});
}

const document = {
  node: process.version,
  icu: process.versions.icu,
  uv: process.versions.uv,
  v8: process.versions.v8,
  platform: process.platform,
  locale,
  baselineHead,
  baselineStatus,
  requestsPath,
  dirent: {root, raw: rawDirents.map(dirent), byteSorted},
  responses,
};
// Atomic publish for the Rust reader (#166 r8: never let it see a partial file).
const temporary = `${outputPath}.tmp`;
await fs.writeFile(temporary, JSON.stringify(document, null, 2) + '\n');
await fs.rename(temporary, outputPath);
console.log(JSON.stringify({node: process.version, icu: process.versions.icu, baselineHead, responses: responses.length, output: outputPath}));
