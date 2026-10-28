// Real TS oracle for the U4 manager projection (`createLocalGateway.ts:1259-1270`).
//
// A real `createLocalGateway` is booted against an isolated HOME/PILOT_HOME.
// The catalog port and manager browser provider are injected and record every
// call; each operation's outcome is recorded verbatim as `{ok}` or
// `{error:{name,code?,message}}`. Project paths and the isolated root are
// normalized to `<dir>` so the corpus is stable across runs.
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, stat as statFile, utimes, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

const baseline = process.env.PDRS_BASELINE;
if (!baseline) throw new Error('set PDRS_BASELINE');
const outputArg = process.argv[2];
if (!outputArg) throw new Error('provide an output directory');
const root = resolve(baseline);
const output = resolve(outputArg);
const sourceCommit = assertBaseline(root);
assert.ok(process.versions.node.startsWith('22.'), `Node 22 required, got ${process.versions.node}`);

const sourcePaths = [
  'src/cli/createLocalGateway.ts',
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/gateway/GatewaySessionCatalog.ts',
  'src/gateway/dialog/projectRegistry.ts',
  'src/gateway/dialog/errors.ts',
  'src/pilot/paths.ts',
  'src/gateway/client/InProcessGateway.ts',
  'src/gateway/protocol/types.ts',
  'src/gateway/server/GatewayWsConnection.ts',
  'src/session/catalog/SessionCatalogPort.ts',
  'src/session/storage/SessionList.ts',
];
const sources = await baselineSourceHashes(root, sourcePaths);

await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
const isolatedHome = await mkdtemp(join(output, '.home-'));
process.env.HOME = isolatedHome;
process.env.XDG_CONFIG_HOME = join(isolatedHome, 'xdg');
process.env.XDG_CACHE_HOME = join(isolatedHome, 'xdg');
process.env.XDG_DATA_HOME = join(isolatedHome, 'xdg');
process.env.TMPDIR = join(isolatedHome, 'tmp');
await mkdir(process.env.TMPDIR, { recursive: true });
const project = join(scratch, 'project');
await mkdir(project, { recursive: true });
process.env.PILOT_HOME = project;
process.chdir(project);
const CONFIG = `
schemaVersion: 1
agent:
  model: test/test
  maxContextTokens: 8192
  maxOutputTokens: 1024
model:
  providers:
    test:
      protocol: openai
      url: http://127.0.0.1:1
      apiKey: test-only
      models:
        test:
          capabilities:
            supportsToolUse: true
            maxContextTokens: 8192
            maxOutputTokens: 1024
`;
await writeFile(join(project, 'pilotdeck.yaml'), CONFIG, 'utf8');

const { createLocalGateway } = await import(
  pathToFileURL(join(root, 'src/cli/createLocalGateway.ts')).href
);
const { DialogGatewayError } = await import(
  pathToFileURL(join(root, 'src/gateway/dialog/errors.ts')).href
);
const { listProjectSessions } = await import(
  pathToFileURL(join(root, 'src/session/storage/SessionList.ts')).href
);
const { getPilotProjectChatDir } = await import(
  pathToFileURL(join(root, 'src/pilot/paths.ts')).href
);

const clean = (value: any) =>
  value === undefined ? null : JSON.parse(JSON.stringify(value).split(project).join('<dir>'));
const expand = (value: any): any => {
  if (typeof value === 'string') return value.split('<dir>').join(project);
  if (Array.isArray(value)) return value.map(expand);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, expand(item)]));
  }
  return value;
};
const errorRecord = (error: any) => ({
  name: String(error?.name ?? 'Error'),
  ...(typeof error?.code === 'string' ? { code: error.code } : {}),
  message: String(error?.message ?? error).split(project).join('<dir>'),
});
const makeError = (spec: any) => {
  if (spec.name === 'DialogGatewayError') {
    return new DialogGatewayError(spec.code, spec.message);
  }
  const error: any = new Error(spec.message);
  error.name = spec.name;
  if (spec.code !== undefined) error.code = spec.code;
  return error;
};

const catalogCalls: any[] = [];
let catalogMode: any = { items: [] };
const catalog = {
  async list(input: any) {
    catalogCalls.push(clean(input));
    if (catalogMode.error) throw makeError(catalogMode.error);
    return JSON.parse(JSON.stringify(catalogMode.items ?? []));
  },
};
const browserCalls: any[] = [];
let browserMode: any = { result: { items: [] } };
const managerBrowsers = async (input: any) => {
  browserCalls.push(clean(input));
  if (browserMode.error) throw makeError(browserMode.error);
  return JSON.parse(JSON.stringify(browserMode.result ?? { items: [] }));
};

const local = createLocalGateway({
  projectRoot: project,
  pilotHome: project,
  sessionCatalog: catalog as never,
  managerBrowsers,
});

const S1 = {
  sessionId: 's-1',
  summary: 'one',
  lastModified: 3,
  fileSize: 10,
  customTitle: 'T',
  aiTitle: undefined,
  firstPrompt: 'hi',
  cwd: '/fixture',
  tag: undefined,
  createdAt: 1,
  parentSessionId: undefined,
  forkedFromTurnId: undefined,
  sessionKey: 's-1',
};
const S2 = {
  sessionId: 's-2',
  summary: 'two',
  lastModified: 2,
  fileSize: undefined,
  customTitle: undefined,
  aiTitle: undefined,
  firstPrompt: undefined,
  cwd: '/fixture',
  tag: undefined,
  createdAt: undefined,
  parentSessionId: undefined,
  forkedFromTurnId: undefined,
  sessionKey: 'alias-2',
};
const browserResult = { items: [{ browserId: 'b-1', state: 'running' }] };

type Operation = {
  name: string;
  method: 'manager_sessions' | 'manager_browsers';
  params: any;
  paramsUndefined?: boolean;
  catalog: any;
  browser: any;
};

const operations: Operation[] = [
  { name: 'sessions-all', method: 'manager_sessions', params: { projectKey: '<dir>' }, catalog: { items: [S1, S2] }, browser: { result: browserResult } },
  { name: 'sessions-filter-id', method: 'manager_sessions', params: { projectKey: '<dir>', sessionKey: 's-1' }, catalog: { items: [S1, S2] }, browser: { result: browserResult } },
  { name: 'sessions-filter-alias', method: 'manager_sessions', params: { projectKey: '<dir>', sessionKey: 'alias-2' }, catalog: { items: [S1, S2] }, browser: { result: browserResult } },
  { name: 'sessions-filter-none', method: 'manager_sessions', params: { projectKey: '<dir>', sessionKey: 'missing' }, catalog: { items: [S1, S2] }, browser: { result: browserResult } },
  { name: 'sessions-empty', method: 'manager_sessions', params: { projectKey: '<dir>' }, catalog: { items: [] }, browser: { result: browserResult } },
  { name: 'sessions-empty-string-key', method: 'manager_sessions', params: { projectKey: '<dir>', sessionKey: '' }, catalog: { items: [S1, S2] }, browser: { result: browserResult } },
  { name: 'sessions-zero-key', method: 'manager_sessions', params: { projectKey: '<dir>', sessionKey: 0 }, catalog: { items: [S1, S2] }, browser: { result: browserResult } },
  { name: 'sessions-number-key', method: 'manager_sessions', params: { projectKey: '<dir>', sessionKey: 5 }, catalog: { items: [S1, S2] }, browser: { result: browserResult } },
  { name: 'sessions-fallback', method: 'manager_sessions', params: {}, catalog: { items: [S1, S2] }, browser: { result: browserResult } },
  { name: 'sessions-unknown-project', method: 'manager_sessions', params: { projectKey: '<dir>/unknown' }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'sessions-catalog-structured-error', method: 'manager_sessions', params: { projectKey: '<dir>' }, catalog: { error: { name: 'DialogGatewayError', code: 'CATALOG_DOWN', message: 'catalog offline' } }, browser: { result: browserResult } },
  { name: 'sessions-catalog-raw-error', method: 'manager_sessions', params: { projectKey: '<dir>' }, catalog: { error: { name: 'TypeError', message: 'catalog exploded' } }, browser: { result: browserResult } },
  { name: 'sessions-null-params', method: 'manager_sessions', params: null, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'sessions-undefined-params', method: 'manager_sessions', params: undefined, paramsUndefined: true, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'sessions-projectKey-number', method: 'manager_sessions', params: { projectKey: 42 }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'sessions-projectKey-array', method: 'manager_sessions', params: { projectKey: [] }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'sessions-projectKey-object', method: 'manager_sessions', params: { projectKey: {} }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'sessions-projectKey-name-object', method: 'manager_sessions', params: { projectKey: { constructor: { name: {} } } }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'sessions-projectKey-name-array', method: 'manager_sessions', params: { projectKey: { constructor: { name: [] } } }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'sessions-projectKey-name-surrogate', method: 'manager_sessions', params: { projectKey: { constructor: { name: '\ud800' } } }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'sessions-projectKey-bool', method: 'manager_sessions', params: { projectKey: true }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'browsers-success', method: 'manager_browsers', params: { projectKey: '<dir>', sessionKey: 's-1' }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'browsers-empty', method: 'manager_browsers', params: { projectKey: '<dir>' }, catalog: { items: [S1] }, browser: { result: { items: [] } } },
  { name: 'browsers-missing-session', method: 'manager_browsers', params: { projectKey: '<dir>' }, catalog: { items: [S1] }, browser: { result: browserResult } },
  { name: 'browsers-structured-error', method: 'manager_browsers', params: { projectKey: '<dir>' }, catalog: { items: [S1] }, browser: { error: { name: 'DialogGatewayError', code: 'BROWSER_DOWN', message: 'browser offline' } } },
  { name: 'browsers-raw-error', method: 'manager_browsers', params: { projectKey: '<dir>' }, catalog: { items: [S1] }, browser: { error: { name: 'Error', message: 'browser exploded' } } },
  { name: 'browsers-absent', method: 'manager_browsers', params: { projectKey: '<dir>' }, catalog: { items: [S1] }, browser: 'absent' },
];

const results: any[] = [];
try {
  for (const operation of operations) {
    if (operation.browser === 'absent') continue;
    catalogMode = operation.catalog;
    browserMode = operation.browser;
    const catalogStart = catalogCalls.length;
    const browserStart = browserCalls.length;
    let outcome: any;
    try {
      const params = operation.paramsUndefined ? undefined : expand(operation.params);
      const value = operation.method === 'manager_sessions'
        ? await local.gateway.managerSessions(params as never)
        : await local.gateway.managerBrowsers(params as never);
      outcome = { ok: clean(value) };
    } catch (error: any) {
      outcome = { error: errorRecord(error) };
    }
    results.push({
      ...operation,
      params: clean(operation.params),
      catalog: clean(operation.catalog),
      browser: operation.browser === 'absent' ? 'absent' : clean(operation.browser),
      outcome,
      catalogCalls: catalogCalls.slice(catalogStart),
      browserCalls: browserCalls.slice(browserStart),
    });
  }

  // Deterministic real-TS catalog/writer matrix. The writer is a controlled
  // file write: before, complete, half-written, internal, and paged states.
  const matrixRoot = join(scratch, 'matrix-project');
  const matrixHome = project;
  await mkdir(matrixRoot, { recursive: true });
  const matrixChat = getPilotProjectChatDir(matrixRoot, matrixHome);
  const sessionLine = (text: string, sequence: number) => JSON.stringify({
    type: 'accepted_input',
    sessionId: `matrix-session-${sequence}`,
    turnId: 'turn-1',
    createdAt: '2026-10-18T00:00:00.000Z',
    sequence,
    messages: [{ content: [{ type: 'text', text }] }],
  }) + '\n';
  const line1 = sessionLine('matrix prompt one', 1);
  const line2 = sessionLine('matrix prompt two', 2);
  const line3 = sessionLine('matrix prompt three', 3);
  const line4 = sessionLine('matrix prompt four', 4);
  const line5 = sessionLine('matrix prompt five', 5);
  const metadataLine = (metadata: any, sequence: number) => JSON.stringify({
    type: 'session_metadata',
    sessionId: `matrix-session-${sequence}`,
    turnId: 'turn-1',
    createdAt: '2026-10-18T00:00:00.000Z',
    sequence,
    metadata,
  }) + '\n';
  const boundaryLine = (sessionId: string, sequence: number) => JSON.stringify({
    type: 'control_boundary',
    sessionId,
    turnId: 'turn-1',
    createdAt: '2026-10-18T00:00:00.000Z',
    sequence,
    boundary: { kind: 'compact' },
  }) + '\n';
  const file = (name: string, content: string, mtime: number) => ({ name, content, mtime });
  // Sub-millisecond timestamps are set from nanoseconds; the generator records
  // the filesystem's read-back value so the Rust side reproduces the exact ns.
  const fileNs = (name: string, content: string, mtimeNs: string) => ({ name, content, mtimeNs });
  const rawMetadataLine = (json: string) => `{"type":"session_metadata","metadata":${json}}\n`;
  const padLine = JSON.stringify({
    type: 'accepted_input',
    messages: [{ content: [{ type: 'text', text: 'x'.repeat(70000) }] }],
  }) + '\n';
  const fiveFiles = [
    file('s-1.jsonl', line1, 1_700_000_000_000),
    file('s-2.jsonl', line2, 1_700_000_001_000),
    file('s-3.jsonl', line3, 1_700_000_002_000),
    file('s-4.jsonl', line4, 1_700_000_003_000),
    file('s-5.jsonl', line5, 1_700_000_004_000),
  ];
  const matrixCases = [
    { name: 'empty', includeInternal: false, files: [] as any[] },
    { name: 'before-append', includeInternal: false, files: [] as any[] },
    { name: 'committed', includeInternal: false, files: [file('s-1.jsonl', line1, 1_700_000_000_000)] },
    { name: 'tail-partial-existing', includeInternal: false, files: [
      file('s-1.jsonl', line1 + line2.slice(0, Math.floor(line2.length / 2)), 1_700_000_000_000),
    ] },
    { name: 'metadata-title-spaces', includeInternal: false, files: [
      file('meta-1.jsonl', line1 + metadataLine({
        title: '  title  ', aiTitle: 'ai', tag: 'tag-1',
        parentSessionId: 'parent-1', forkedFromTurnId: 'fork-1',
      }, 9), 1_700_000_000_500),
    ] },
    { name: 'internal-hidden', includeInternal: false, files: [
      file('s-1.jsonl', line1, 1_700_000_000_000),
      file('always-on-discovery:1.jsonl', line2, 1_700_000_001_000),
    ] },
    { name: 'internal-included', includeInternal: true, files: [
      file('s-1.jsonl', line1, 1_700_000_000_000),
      file('always-on-discovery:1.jsonl', line2, 1_700_000_001_000),
    ] },
    { name: 'internal-compact-default', includeInternal: false, files: [
      file('normal.jsonl', line1, 1_700_000_000_000),
      file('always-on-discovery:1.jsonl', line2 + boundaryLine('always-on-discovery:1', 2), 1_700_000_001_000),
    ] },
    { name: 'internal-compact-included', includeInternal: true, files: [
      file('normal.jsonl', line1, 1_700_000_000_000),
      file('always-on-discovery:1.jsonl', line2 + boundaryLine('always-on-discovery:1', 2), 1_700_000_001_000),
    ] },
    { name: 'partial-skipped', includeInternal: false, files: [
      file('s-1.jsonl', line1, 1_700_000_000_000),
      file('partial.jsonl', line2.slice(0, Math.floor(line2.length / 2)), 1_700_000_002_000),
    ] },
    { name: 'limit-offset', includeInternal: false, limit: 1, offset: 1, files: [
      file('s-1.jsonl', line1, 1_700_000_000_000),
      file('s-2.jsonl', line2, 1_700_000_001_000),
      file('s-3.jsonl', line3, 1_700_000_002_000),
    ] },
    { name: 'limit-zero', includeInternal: false, limit: 0, offset: 0, files: [
      file('s-1.jsonl', line1, 1_700_000_000_000),
      file('s-2.jsonl', line2, 1_700_000_001_000),
    ] },
    { name: 'pagination-negative-limit', includeInternal: false, limit: -1, offset: 0, files: fiveFiles },
    { name: 'pagination-negative-end', includeInternal: false, limit: -6, offset: 1, files: fiveFiles },
    { name: 'pagination-fractional', includeInternal: false, limit: 1.5, offset: 0, files: fiveFiles },
    { name: 'same-mtime-name-order', includeInternal: false, files: [
      file('c.jsonl', line3, 1_700_000_000_000),
      file('a.jsonl', line1, 1_700_000_000_000),
      file('b.jsonl', line2, 1_700_000_000_000),
    ] },
    { name: 'same-mtime-pagination', includeInternal: false, limit: 1, offset: 1, files: [
      file('c.jsonl', line3, 1_700_000_000_000),
      file('a.jsonl', line1, 1_700_000_000_000),
      file('b.jsonl', line2, 1_700_000_000_000),
    ] },
    { name: 'mtime-submillisecond-up', includeInternal: false, files: [
      fileNs('s-1.jsonl', line1, '1793128812309687438'),
    ] },
    { name: 'mtime-submillisecond-carry', includeInternal: false, files: [
      fileNs('s-1.jsonl', line1, '1793128812299999999'),
    ] },
    { name: 'surrogate-fast-title', includeInternal: false, files: [
      file('s-1.jsonl', line1 + metadataLine({ title: '\ud800' }, 9), 1_700_000_000_000),
    ] },
    { name: 'surrogate-prompt', includeInternal: false, files: [
      file('s-1.jsonl', sessionLine('pre \ud800 post', 1), 1_700_000_000_000),
    ] },
    { name: 'surrogate-metadata-fallback', includeInternal: false, files: [
      file('s-1.jsonl', line1 + padLine + metadataLine({ title: 'mid \ud800' }, 9) + padLine, 1_700_000_000_000),
    ] },
    { name: 'bad-escape-title', includeInternal: false, files: [
      file('s-1.jsonl', line1 + rawMetadataLine('{"title":"\\q"}'), 1_700_000_000_000),
    ] },
    { name: 'bad-escape-createdAt', includeInternal: false, files: [
      file('s-1.jsonl', '{"type":"other","createdAt":"\\q"}\n' + line1, 1_700_000_000_000),
    ] },
  ];
  const matrix: any[] = [];
  for (const matrixCase of matrixCases) {
    await rm(matrixChat, { recursive: true, force: true });
    await mkdir(matrixChat, { recursive: true });
    for (const entry of matrixCase.files) {
      const path = join(matrixChat, entry.name);
      await writeFile(path, entry.content, 'utf8');
      if (entry.mtimeNs !== undefined) {
        const seconds = Number(BigInt(entry.mtimeNs)) / 1e9;
        await utimes(path, seconds, seconds);
        const actual: any = await statFile(path, { bigint: true });
        entry.mtimeNs = actual.mtimeNs.toString();
      } else {
        await utimes(path, new Date(entry.mtime), new Date(entry.mtime));
      }
    }
    try {
      const value = await listProjectSessions({
        projectRoot: matrixRoot,
        pilotHome: matrixHome,
        includeInternal: matrixCase.includeInternal,
        ...(matrixCase.limit === undefined ? {} : { limit: matrixCase.limit }),
        ...(matrixCase.offset === undefined ? {} : { offset: matrixCase.offset }),
      });
      matrix.push({
        ...matrixCase,
        outcome: JSON.parse(JSON.stringify(value).split(matrixRoot).join('<dir>')),
      });
    } catch (error: any) {
      matrix.push({
        ...matrixCase,
        error: JSON.parse(JSON.stringify(errorRecord(error)).split(matrixRoot).join('<dir>')),
      });
    }
  }

  // The absent-provider path needs a Gateway whose InProcess options truly lack
  // managerBrowsers. Run it last so the first instance can be disposed first.
  await local.dispose();
  const absent = createLocalGateway({
    projectRoot: project,
    pilotHome: project,
    sessionCatalog: catalog as never,
  });
  try {
    const operation = operations.find((row) => row.browser === 'absent')!;
    catalogMode = operation.catalog;
    const catalogStart = catalogCalls.length;
    const browserStart = browserCalls.length;
    let outcome: any;
    try {
      const value = await absent.gateway.managerBrowsers(expand(operation.params) as never);
      outcome = { ok: clean(value) };
    } catch (error: any) {
      outcome = { error: errorRecord(error) };
    }
    results.push({
      ...operation,
      params: clean(operation.params),
      catalog: clean(operation.catalog),
      browser: 'absent',
      outcome,
      catalogCalls: catalogCalls.slice(catalogStart),
      browserCalls: browserCalls.slice(browserStart),
    });
  } finally {
    await absent.dispose();
  }

  const oracle = {
    sourceCommit,
    sources,
    command: 'PDRS_BASELINE=/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/manager-gateway/generate.mts <output>',
    node: process.version,
    iso: 'HOME/XDG_*/TMPDIR and PILOT_HOME are per-run directories',
    projectRoot: '<dir>',
    operations: results,
    matrix,
  };
  await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
  assert.equal(results.length, operations.length, 'every operation is recorded');
  console.log(`PASS: ${results.length} real TS manager operations`);
} finally {
  await rm(scratch, { recursive: true, force: true });
  await rm(isolatedHome, { recursive: true, force: true });
}
