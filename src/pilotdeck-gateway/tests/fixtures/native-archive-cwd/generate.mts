import assert from 'node:assert/strict';
import {mkdir, mkdtemp, realpath, rm, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1154 archive pinned Node');
assert.ok(process.env.PDRS_BASELINE, '#1154 archive explicit baseline');
const baseline = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]);
await mkdir(output, {recursive: true});
const root = await realpath(await mkdtemp(join(output, 'work-')));
const old = process.cwd();
const saved = {...process.env};
const cases: any[] = [];
const sources = ['src/cli/createLocalGateway.ts', 'src/gateway/client/InProcessGateway.ts'];
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, sources);
try {
  for (const key of ['HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'TMPDIR', 'PILOT_HOME']) {
    process.env[key] = join(root, key.toLowerCase());
    await mkdir(process.env[key]!, {recursive: true});
  }
  for (const key of ['PILOTDECK_SKIP_BROWSER_OPEN', 'PILOT_SKIP_BROWSER_OPEN', 'SKIP_BROWSER_OPEN', 'PILOTDECK_SKIP_BROWSER', 'GP_POLL_WATCH']) process.env[key] = '1';
  // Reuse the frozen shallow-polling fixture for sandbox fs.watch; no server is started.
  await import(new URL('../../../../../scripts/gateway-parity/watcher.mjs', import.meta.url).href);
  const {createLocalGateway} = await import(pathToFileURL(join(baseline, sources[0])).href);
  const {createProjectId} = await import(pathToFileURL(join(baseline, 'src/pilot/paths.ts')).href);
  for (const cwd of ['project', 'other', 'deleted']) {
    const base = join(root, cwd), home = join(base, 'home'), project = join(base, 'project');
    const chat = join(home, 'projects', createProjectId(project), 'chats');
    const artifacts = join(project, '.pilotdeck', 'tool-results', 'fixture');
    for (const dir of [home, project, chat, artifacts, join(base, 'other'), join(base, 'deleted')]) await mkdir(dir, {recursive: true});
    await writeFile(join(home, 'pilotdeck.yaml'), `schemaVersion: 1
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
`, {flag: 'wx'});
    await writeFile(join(chat, '..', '.cwd'), project, {flag: 'wx'});
    await writeFile(join(chat, 'fixture.jsonl'), JSON.stringify({type: 'message', sessionId: 'fixture', turnId: '', sequence: 0, createdAt: '2026-10-15T00:00:00Z', payload: {text: 'hello'}}) + '\n', {flag: 'wx'});
    await writeFile(join(artifacts, 'fixture.txt'), 'fixture artifact', {flag: 'wx'});
    process.env.PILOT_HOME = home;
    process.chdir(project);
    const local = createLocalGateway({projectRoot: project, pilotHome: home, env: {...process.env}});
    try {
      process.chdir(join(base, cwd));
      if (cwd === 'deleted') await rm(join(base, cwd), {recursive: true});
      let cwdReadable = true;
      try {process.cwd();} catch {cwdReadable = false;}
      for (const key of ['missing', 'null', 'empty', 'absolute']) {
        const params: any = {sessionKey: 'fixture'};
        if (key !== 'missing') params.projectKey = key === 'null' ? null : key === 'empty' ? '' : project;
        for (const [method, call] of [
          ['manifest', () => local.gateway.nativeArchiveManifest(params)],
          ['entries', () => local.gateway.nativeArchiveEntries(params)],
          ['artifact', () => local.gateway.nativeArchiveArtifact({...params, artifactName: 'fixture.txt'})],
        ] as const) {
          let observation;
          try {observation = {ok: true, result: await call()};}
          catch (e: any) {observation = {ok: false, code: e.code ?? 'gateway_request_failed', message: e.message};}
          cases.push({name: `${method}_${key}_${cwd}`, method, key, cwd, cwdReadable, ...observation});
        }
      }
    } finally {
      process.chdir(root);
      await local.dispose();
    }
  }
} finally {
  process.chdir(old);
  for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key];
  Object.assign(process.env, saved);
  await rm(root, {recursive: true, force: true});
}
const record = {sourceCommit, sourceSha256, node: process.version, cases};
const raw = JSON.stringify(record, null, 2).replaceAll(root, '{root}') + '\n';
await writeFile(join(output, 'oracle.json'), raw, {flag: 'wx'});
console.log(raw);
