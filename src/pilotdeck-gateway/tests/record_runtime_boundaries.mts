// #251: execute fixed TS entry points; all writes stay in the explicit scratch root.
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { stripTypeScriptTypes } from 'node:module';
import { execFileSync } from 'node:child_process';

assert.ok(process.env.PDRS_BASELINE && process.argv[2] && process.argv[3], 'baseline, output file and scratch parent required');
const baseline = path.resolve(process.env.PDRS_BASELINE);
const output = path.resolve(process.argv[2]);
const parent = path.resolve(process.argv[3]);
for (const p of [output, parent]) assert.ok(p !== baseline && !p.startsWith(baseline + path.sep), 'baseline is read-only');
const sourceCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
assert.notEqual(process.getuid?.(), 0, 'real EACCES requires a non-root user');
const sources = [
  'src/extension/skills/SkillManager.ts', 'src/cli/createLocalGateway.ts',
  'src/gateway/client/InProcessGateway.ts', 'src/gateway/server/GatewayWsConnection.ts',
  'src/session/events/SessionEventLogValidation.ts', 'src/gateway/run/RunRegistry.ts',
];
const sourceSha256: Record<string, string> = {};
for (const p of sources) {
  const bytes = await fs.readFile(path.join(baseline, p));
  assert.deepEqual(bytes, execFileSync('git', ['-C', baseline, 'show', `${sourceCommit}:${p}`]), `dirty baseline source: ${p}`);
  sourceSha256[p] = createHash('sha256').update(bytes).digest('hex');
}
const load = (p: string) => import(pathToFileURL(path.join(baseline, p)).href);
const { SkillManager } = await load(sources[0]);
const { InProcessGateway } = await load(sources[2]);
const { GatewayWsConnection } = await load(sources[3]);
const { validateSessionEventLog } = await load(sources[4]);
const { FileRunRegistry } = await load(sources[5]);
const { DialogGatewayError } = await load('src/gateway/dialog/errors.ts');
const { isPathWithinRoot } = await load('src/tool/builtin/filesystem/pathSafety.ts');
const sourceText = await fs.readFile(path.join(baseline, sources[1]), 'utf8');
// Same anchored extraction as record_project_files.mts; no hand-written port of the callback.
assert.equal(sourceText.split('    async projectFileRead(input) {\n').length, 2);
const body = sourceText.split('    async projectFileRead(input) {\n')[1].split('\n    },')[0];
assert.ok(body.includes('Math.min(input.maxBytes') && body.includes('data.subarray'));
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const read = new AsyncFunction('input', 'dialog', 'realpath', 'resolve', 'isPathWithinRoot', 'DialogGatewayError', 'statAsync', 'readFileAsync',
  stripTypeScriptTypes('async function oracle(input) {\n' + body + '\n}').split('\n').slice(1, -1).join('\n'));
await fs.mkdir(parent, { recursive: true });
const root = await fs.realpath(await fs.mkdtemp(path.join(parent, 'runtime-')));
const blocked = path.join(root, 'bundle', 'blocked');
const normalize = (v: any): any => {
  if (typeof v === 'string') return v.split(root).join('<dir>');
  if (Array.isArray(v)) return v.map(normalize);
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, k === 'mtime' ? '<mtime>' : normalize(x)]));
  return v;
};
const errorOf = async (fn: () => any) => {
  try { await fn(); assert.fail('expected an actual TS exception'); }
  catch (e: any) { if (e.code === 'ERR_ASSERTION') throw e; return { name: e.name, code: e.code ?? null, message: e.message }; }
};
const depth = 10_000;
const nested = (leaf: string, n = depth) => '['.repeat(n) + leaf + ']'.repeat(n);
const overflow = { name: 'RangeError', code: null, message: 'Maximum call stack size exceeded' };
try {
  await fs.writeFile(path.join(root, 'a.txt'), 'inside');
  const projects = { resolveProjectKey: async () => root };
  const gateway: any = {
    describeServer: async () => ({}),
    projectFileRead: (input: any) => read(input, { projects }, fs.realpath, path.resolve, isPathWithinRoot, DialogGatewayError, fs.stat, fs.readFile),
    options: { nativeArchiveArtifact: async () => { throw new Error('unexpected archive callback'); } },
    nativeArchiveArtifact(input: any) { return InProcessGateway.prototype.nativeArchiveArtifact.call(this, input); },
  };
  const fileParams = (n: number) => `{"projectKey":"fixture","path":"a.txt","maxBytes":${nested('7', n)}}`;
  const archiveParams = (n: number) => `{"sessionKey":"fixture","artifactName":${nested('"bad/name"', n)}}`;
  const deepErrors = {
    projectFile: await errorOf(() => gateway.projectFileRead(JSON.parse(fileParams(depth)))),
    nativeArchive: await errorOf(() => gateway.nativeArchiveArtifact(JSON.parse(archiveParams(depth)))),
    transcript: await errorOf(() => validateSessionEventLog([{ sequence: JSON.parse(nested('1')) }])),
  };
  for (const e of Object.values(deepErrors)) assert.deepEqual(e, overflow);
  const shallow = {
    projectFile: await gateway.projectFileRead(JSON.parse(fileParams(2))),
    nativeArchive: await errorOf(() => gateway.nativeArchiveArtifact(JSON.parse(archiveParams(2)))),
    transcript: await errorOf(() => validateSessionEventLog([{ sequence: [[1]] }])),
  };
  assert.equal(shallow.projectFile.content, 'inside');
  assert.equal(shallow.nativeArchive.code, 'INVALID_ARCHIVE_ARTIFACT');
  assert.equal(shallow.transcript.code, 'invalid_sequence');
  async function wire(method: string, params: string) {
    let receive: any, finish: any;
    const done = new Promise<string>(r => finish = r);
    const ws = { onMessage(fn: any) { receive = fn; }, onClose() {}, sendText(text: string) {
      if (JSON.parse(text).type === 'response') finish(text);
    }, close() { throw new Error('unexpected close'); } };
    new GatewayWsConnection(ws, { gateway, token: 'offline', serverVersion: 'offline' });
    receive(JSON.stringify({ type: 'hello', protocolVersion: '1.2', clientName: 'web', clientVersion: 'offline', token: 'offline' }));
    await new Promise(r => setImmediate(r));
    receive(`{"type":"request","id":"deep","method":"${method}","params":${params}}`);
    let timer: ReturnType<typeof setTimeout> | undefined;
    try { return JSON.parse(await Promise.race([done, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error('gateway response watchdog')), 5000);
    })])); } finally { clearTimeout(timer); }
  }
  const wireErrors = {
    projectFile: await wire('project_file_read', fileParams(depth)),
    nativeArchive: await wire('native_archive_artifact', archiveParams(depth)),
  };
  for (const r of Object.values(wireErrors)) assert.deepEqual(r, {
    type: 'response', id: 'deep', ok: false, error: { code: 'gateway_request_failed', message: overflow.message },
  });
  const registryErrors: Record<string, any> = {};
  for (const key of ['projectKey', 'sessionKey', 'runId']) {
    const registryPath = path.join(root, `registry-${key}.json`);
    const text = `{"schemaVersion":1,"records":[{"state":"completed","${key}":${nested('1')}}],"events":{}}`;
    await fs.writeFile(registryPath, text);
    const registry = new FileRunRegistry(registryPath);
    registryErrors[key] = await errorOf(() => registry.get({ sessionKey: 's', runId: 'r' }));
    assert.deepEqual(registryErrors[key], overflow);
    assert.equal(await fs.readFile(registryPath, 'utf8'), text);
    await assert.rejects(fs.stat(registryPath + '.lock'), { code: 'ENOENT' });
  }
  const skillMd = '---\nname: Boundary\ndescription: Real filesystem boundary regression\n---\n';
  await fs.mkdir(blocked, { recursive: true });
  await fs.writeFile(path.join(root, 'bundle/SKILL.md'), skillMd);
  await fs.writeFile(path.join(blocked, 'data.txt'), 'hidden');
  const manager = new SkillManager({ pilotHome: path.join(root, 'home'), env: {} });
  const scan = () => manager.scan({ parentPath: path.join(root, 'bundle') });
  const before = await scan();
  assert.equal(before.folders[0].fileCount, 2);
  await fs.chmod(blocked, 0);
  let denied: any;
  try {
    await assert.rejects(fs.readdir(blocked), { code: 'EACCES' });
    denied = await scan();
    assert.deepEqual([denied.folders[0].fileCount, denied.folders[0].totalSize], [0, 0]);
  } finally { await fs.chmod(blocked, 0o755); }
  const restored = await scan();
  assert.deepEqual(normalize(restored), normalize(before));
  const copyDepth = 240;
  const relativeLeaf = Array(copyDepth).fill('d').join('/');
  const source = path.join(root, 'source');
  await fs.mkdir(path.join(source, relativeLeaf), { recursive: true });
  await fs.writeFile(path.join(source, 'SKILL.md'), skillMd);
  await fs.writeFile(path.join(source, relativeLeaf, 'leaf.txt'), 'deep leaf');
  await fs.chmod(path.join(source, 'd'), 0o551);
  const imported = await manager.import({ scope: 'user', sourcePath: source, slug: 'deep' });
  const dest = path.join(root, 'home/skills/deep');
  assert.equal(await fs.readFile(path.join(dest, relativeLeaf, 'leaf.txt'), 'utf8'), 'deep leaf');
  const copiedMode = (await fs.stat(path.join(dest, 'd'))).mode & 0o777;
  assert.equal(copiedMode, 0o551);
  const result = { sourceCommit, sourceSha256,
    node: process.version, arrayDepth: depth, deepErrors, shallow, wireErrors, registryErrors,
    skills: { skillMd, scan: { before: normalize(before), denied: normalize(denied), restored: normalize(restored) },
      copy: { depth: copyDepth, content: 'deep leaf', mode: copiedMode, result: normalize(imported) } },
  };
  await fs.writeFile(output, JSON.stringify(result, null, 2) + '\n');
  console.log(`PASS: actual TS deep arrays (3 entry errors, 2 WS errors, 3 registry keys), shallow controls, EACCES scan, ${copyDepth}-level import`);
} finally {
  for (const dir of [blocked, path.join(root, 'source/d'), path.join(root, 'home/skills/deep/d')]) {
    await fs.chmod(dir, 0o755).catch(e => { if (e.code !== 'ENOENT') throw e; });
  }
  await fs.rm(root, { recursive: true, force: true });
}
