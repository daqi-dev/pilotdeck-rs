// #132 step-1 originals: Node/ICU collation of ill-formed UTF-16 sort keys,
// recorded through the real TS gateway for `model_catalog_list` (sorted by
// `provider/model` id) and same-sequence `session_model_get` (sorted by
// `createdAt`). A fresh child per locale keeps each recording pinned to one
// LANG; the child prints one JSON line, this parent assembles the oracle.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0');
// #132a fix-r1: the goldens are ICU-sensitive. Node 22.22.0 exists with ICU
// 77.1 (source-built ~/opt) and 78.2 (homebrew, the recording baseline,
// decision card 2026-10-31); a mismatch must BLOCK with its own message
// instead of failing the byte comparison as a vague positive-control miss.
assert.equal(process.versions.icu, '78.2',
  'BLOCKED: surrogate-collation-matrix requires ICU 78.2 (homebrew node@22 baseline, decision card 2026-10-31)');
assert.ok(process.env.PDRS_BASELINE, 'PDRS_BASELINE must point at the PilotDeck checkout');
const baseline = resolve(process.env.PDRS_BASELINE);
const sources = ['src/gateway/dialog/modelCatalog.ts', 'src/gateway/server/GatewayWsConnection.ts',
  'src/pilot/config/loadPilotConfig.ts', 'src/cli/GatewaySessionModelBundle.ts',
  'src/model/session/NativeSessionModelSelectionPort.ts', 'src/model/session/NativeSessionModelSelectionPolicy.ts',
  'src/cli/GatewayDialogBundle.ts', 'src/session/storage/ProjectSessionDataPlane.ts',
  'src/session/transcript/TranscriptReader.ts', 'src/pilot/paths.ts',
  'src/session/storage/ProjectSessionStorage.ts'];
const locales = ['en_US.UTF-8', 'zh_CN.UTF-8', 'de_DE.UTF-8', 'sv_SE.UTF-8', 'ja_JP.UTF-8'];
const resolved = new Map([['en_US.UTF-8', 'en-US'], ['zh_CN.UTF-8', 'zh-CN'], ['de_DE.UTF-8', 'de-DE'], ['sv_SE.UTF-8', 'sv-SE'], ['ja_JP.UTF-8', 'ja-JP']]);
// Key classes from #132: distinct lone highs/lows, the high/low band boundary,
// U+FFFD, valid pairs, and mixed Unicode. `locale-control` has no surrogate;
// its order differs per locale and proves the child's locale reached the sort
// (de_DE / ja_JP record the same order as en_US under ICU 78.2, so their
// arrival rests on resolvedLocale; ja_JP also diverges in mixed-uniform).
// A fold of every lone surrogate to U+FFFD (ICU4X compare_utf16) ties
// `embedded-fold` / `cjk-embedded-fold`, but the catalog inputs are already in
// the recorded order, so the stable sort keeps it and the catalog frames alone
// cannot expose a fold: the reversed session input plus w<i> tags is the fold
// detector, and both frames are recorded.
const cases: [string, string[]][] = [
  ['lone-high', ['\ud800', '\ud801', '\ud807', '\udbff']],
  ['lone-low', ['\udc00', '\udc01', '\udc07', '\udfff']],
  ['band-boundary', ['\udbff', '\udc00', '�']],
  ['fffd-vs-pair', ['�', '😀', '􏿿']],
  ['valid-pair', ['😀', '😁', '𠀀', '􏿿']],
  ['embedded-fold', ['a\ud800b', 'a\udc00b', 'a�b', 'ab']],
  ['cjk-embedded-fold', ['中\ud800', '中\udc00', '中�', '中']],
  ['pair-interpretation', ['𐀀', '\ud800\ud801', '\udc00\ud800']],
  ['mixed-uniform', ['\ud800', '\ud801', '\udc00', '\udc01', '�', '😀', '𠀀', '中', 'ä', 'a']],
  ['locale-control', ['中文', 'a', 'ä', 'Z']],
];

if (process.argv[2] === '--child') {
  const lang = process.argv[3];
  assert.ok(locales.includes(lang), `unknown locale ${lang}`);
  const root = mkdtempSync(join(tmpdir(), 'pdrs-surrogate-matrix-'));
  const isolation = join(root, 'isolation');
  mkdirSync(join(isolation, 'home'), { recursive: true });
  process.env.HOME = join(isolation, 'home');
  const source = (p: string) => import(pathToFileURL(join(baseline, 'src', p)).href);
  const { GatewaySessionModelBundle } = await source('cli/GatewaySessionModelBundle.ts');
  const { NativeSessionModelSelectionPort } = await source('model/session/NativeSessionModelSelectionPort.ts');
  const { NativeSessionModelSelectionPolicy } = await source('model/session/NativeSessionModelSelectionPolicy.ts');
  const { GatewayDialogBundle } = await source('cli/GatewayDialogBundle.ts');
  const { createProjectSessionDataPlane } = await source('session/storage/ProjectSessionDataPlane.ts');
  const { GatewayWsConnection } = await source('gateway/server/GatewayWsConnection.ts');
  const { getPilotProjectChatDir } = await source('pilot/paths.ts');
  const { sanitizeSessionIdForPath } = await source('session/storage/ProjectSessionStorage.ts');
  const { listModelCatalog } = await source('gateway/dialog/modelCatalog.ts');
  const base = 'schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: https://example.invalid/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n        alternate: {}\n';
  const catalogYaml = (models: string[]) => 'schemaVersion: 1\nagent: {model: ' + JSON.stringify('parity/' + models[0]) + '}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: https://example.invalid/v1\n      apiKey: mock-only\n      models:\n' + models.map(m => '        ' + JSON.stringify(m) + ': {}\n').join('');
  const ask = async (gateway: object, request: string) => {
    let receive: any, finish: any;
    const ws = { onMessage(fn: any) { receive = fn; }, onClose() {}, sendText(s: string) { if (JSON.parse(s).type === 'response') finish(s); }, close() { throw Error('Unexpected close'); } };
    new GatewayWsConnection(ws as any, { gateway: { describeServer: async () => ({}), ...gateway } as any, token: 'offline', serverVersion: 'offline' });
    receive(JSON.stringify({ type: 'hello', protocolVersion: '1.2', clientName: 'web', clientVersion: 'fixture', token: 'offline' }));
    await new Promise(r => setImmediate(r));
    return await new Promise<string>((done, reject) => {
      const timer = setTimeout(() => reject(Error('No response ' + request)), 5000);
      finish = (s: string) => { clearTimeout(timer); done(s); };
      receive(request);
    });
  };
  const results: any[] = [];
  try {
    for (const [id, keys] of cases) {
      const scaffold = async (kind: string) => {
        const home = join(root, kind, id, 'home'), project = join(root, kind, id, 'project');
        await mkdir(join(home, 'projects', 'registered', 'chats'), { recursive: true });
        await mkdir(project, { recursive: true });
        await writeFile(join(home, 'projects', 'registered', '.cwd'), project + '\n');
        await writeFile(join(home, 'pilotdeck.yaml'), kind === 'catalog' ? catalogYaml(keys) : base);
        return { home, project };
      };
      {
        const { home } = await scaffold('catalog');
        const env = { PILOT_HOME: home };
        const request = JSON.stringify({ type: 'request', id: `catalog-${id}`, method: 'model_catalog_list', params: {} });
        const response = await ask({ modelCatalogList: (p: any) => listModelCatalog(p, env) }, request);
        results.push({ id: `catalog-${id}`, method: 'model_catalog_list', keys, yaml: catalogYaml(keys), request, response, env: { PILOT_HOME: '/fixture/home' } });
      }
      {
        const { home, project } = await scaffold('session');
        // Reversed input order: a comparator that collapsed every key to one
        // weight would keep the reversal (stable sort) and change the winner.
        const entries = [...keys].reverse().map((key, i) => ({ type: 'session_metadata', sessionId: 'web:fixture', turnId: 'turn', sequence: 1, createdAt: key, metadata: { modelSelection: { mode: 'model', provider: 'parity', model: 'w' + i } } }));
        const transcript = join(getPilotProjectChatDir(project, home), sanitizeSessionIdForPath('web:fixture') + '.jsonl');
        await writeFile(transcript, entries.map(e => JSON.stringify(e)).join('\n') + '\n');
        const env = { PILOT_HOME: home };
        const plane = createProjectSessionDataPlane();
        const dialog = new GatewayDialogBundle({ pilotHome: home, sessionCatalog: plane.catalog });
        const bundle = new GatewaySessionModelBundle({ fallbackProjectKey: home, resolveProjectKey: dialog.projects.resolveProjectKey, router: { hasActiveTurn() { throw Error('runtime touched'); }, async close() { throw Error('runtime touched'); } }, selectionPort: new NativeSessionModelSelectionPort({ pilotHome: home, now: () => new Date(), storageProvider: plane.persistence }), policy: new NativeSessionModelSelectionPolicy(env) });
        const request = JSON.stringify({ type: 'request', id: `session-${id}`, method: 'session_model_get', params: { projectKey: project, sessionKey: 'web:fixture' } });
        const response = await ask({ sessionModelGet: (p: any) => bundle.sessionModelGet(p) }, request);
        const caseRoot = join(root, 'session', id);
        const mapped = (s: string) => s.split(caseRoot).join('/fixture');
        results.push({ id: `session-${id}`, method: 'session_model_get', keys, entries, request: mapped(request), response: mapped(response), env: { PILOT_HOME: '/fixture/home' }, home: '/fixture/home', projectKey: '/fixture/project' });
      }
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  process.stdout.write(JSON.stringify({ lang, resolvedLocale: new Intl.Collator().resolvedOptions().locale, node: process.version, icu: process.versions.icu, cases: results }) + '\n');
} else {
  assert.ok(process.argv[2], 'output directory required');
  const output = resolve(process.argv[2]);
  const sourceCommit = assertBaseline(baseline);
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const rows = locales.map(lang => {
    const childEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => key !== 'LANG' && !key.startsWith('LC_')));
    childEnv.LANG = lang;
    const raw = execFileSync(process.execPath, ['--disable-warning=ExperimentalWarning', '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'), fileURLToPath(import.meta.url), '--child', lang], { encoding: 'utf8', timeout: 120_000, env: childEnv });
    process.stdout.write(raw);
    const row = JSON.parse(raw);
    assert.equal(row.lang, lang, `child reported ${row.lang} for ${lang}`);
    assert.equal(row.resolvedLocale, resolved.get(lang), `locale ${lang} did not resolve as pinned`);
    return row;
  });
  mkdirSync(output, { recursive: true });
  writeFileSync(join(output, 'oracle.json'), JSON.stringify({ node: process.version, icu: process.versions.icu, sourceCommit, sourceSha256, locales: rows }, null, 2) + '\n', { flag: 'wx' });
}
