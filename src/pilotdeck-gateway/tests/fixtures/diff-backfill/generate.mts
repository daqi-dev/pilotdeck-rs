// Read-only fixed baseline; all writes belong to this invocation's scratch tree.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an existing output parent');
assert.match(process.version, /^v22\./);
const baseline = resolve(process.env.PDRS_BASELINE);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const sourceCommit = bindSourceTree(baseline, commit, git).ref;
const entries = ['src/context/budget/TokenAccountingRuntime.ts',
  'src/context/instructions/InstructionDiscovery.ts', 'src/model/request/validateModelRequest.ts',
  'src/model/providers/anthropic/stream.ts', 'src/session/transcript/TranscriptReader.ts',
  'src/gateway/run/RunRegistry.ts'];
const sourceSha256 = sourceClosureSha256(baseline, commit, git, entries);
const load = (file: string) => import(pathToFileURL(join(baseline, file)).href);
const { TokenAccountingRuntime } = await load(entries[0]);
const { InstructionDiscovery } = await load(entries[1]);
const { validateModelRequest } = await load(entries[2]);
const { readTranscript } = await load(entries[4]);
const { FileRunRegistry } = await load(entries[5]);
const output = mkdtempSync(join(resolve(process.argv[2]), 'diff-backfill-'));
const tree = join(output, 'tree');
for (const name of ['project', 'home', 'a', 'b']) mkdirSync(join(tree, name), { recursive: true });
for (const name of ['a', 'b']) writeFileSync(join(tree, name, 'PILOTDECK.md'), `  managed ${name}\n`, { flag: 'wx' });
const request = { provider: 'p', model: 'm', messages: [] };
const model = { id: 'm', capabilities: { maxOutputTokens: 10 } };
const urlCases: any[] = [];
for (const protocol of ['anthropic', 'openai-responses', 'openai']) {
  for (const raw of ['123', '{}', 'null', 'true', '[]', '"https://api.openai.com/v1/"', '""', 'undefined']) {
    const url = raw === 'undefined' ? undefined : JSON.parse(raw);
    for (const badTools of [false, true]) {
      const input = { ...request, ...(badTools ? { tools: 'x' } : {}) };
      const config = { providers: { p: { id: 'p', protocol, url, apiKey: 'fixture', models: { m: model } } } };
      const sent: string[] = [];
      const runtime = new TokenAccountingRuntime({ modelConfig: config, fetch: async (url: any) => {
        sent.push(String(url)); return new Response('{"input_tokens":7}');
      } });
      let error: string | undefined;
      try { await (runtime as any).countWithProvider(input); } catch (e: any) { error = e.message; }
      urlCases.push({ protocol, raw, badTools, ...(error === undefined ? {} : { error }), sent });
    }
  }
}
const discovery = new InstructionDiscovery(join(tree, 'project'), join(tree, 'project'), join(tree, 'home'));
const managed: any[] = [];
const previous = process.env.PILOTDECK_MANAGED_CONFIG;
try {
  for (const value of ['a', 'b', '', null, 'a']) {
    if (value === null) delete process.env.PILOTDECK_MANAGED_CONFIG;
    else process.env.PILOTDECK_MANAGED_CONFIG = value ? join(tree, value) : '';
    const layers = await discovery.discover();
    managed.push({ value, layers: layers.map((layer: any) => ({ ...layer, path: layer.path.replace(tree, '<tree>') })) });
  }
} finally {
  if (previous === undefined) delete process.env.PILOTDECK_MANAGED_CONFIG;
  else process.env.PILOTDECK_MANAGED_CONFIG = previous;
}
const validation: any[] = [];
for (const kind of ['provider', 'model']) for (const key of ['toString', '__proto__', 'constructor']) {
  const input = { ...request, [kind]: key };
  let outcome: any;
  try { validateModelRequest(input, { providers: { p: { models: { m: model } } } }); outcome = { ok: true }; }
  catch (e: any) { outcome = { name: e.name, message: e.message }; }
  validation.push({ kind, key, outcome });
}
const arrays = [2000, 2001, 2100, 3000].map(depth => {
  let value: any = 7;
  for (let i = 0; i < depth; i++) value = [value];
  try { return { depth, text: String(value) }; }
  catch (e: any) { return { depth, error: e.name, message: e.message }; }
});
const resolved = createRequire(join(baseline, entries[3])).resolve('jsonrepair');
const packageDir = resolve(resolved, '../../..');
const packageFiles = ['package.json', 'lib/esm/regular/jsonrepair.js', 'lib/esm/utils/stringUtils.js'];
const jsonrepairSha256 = Object.fromEntries(packageFiles.map(file => [file,
  createHash('sha256').update(readFileSync(join(packageDir, file))).digest('hex')]));
// Bind dependency bytes to the already reviewed corpus, not merely this run.
const trusted = JSON.parse(readFileSync(new URL('../jsonrepair/oracle.json', import.meta.url), 'utf8'));
for (const file of packageFiles.slice(1)) assert.equal(jsonrepairSha256[file], trusted.packageSha256[file]);
const { jsonrepair } = await import(pathToFileURL(join(packageDir, 'lib/esm/index.js')).href);
const repairs = [199, 200, 201, 205, 250].map(depth => {
  const input = '['.repeat(depth);
  try { return { depth, repaired: jsonrepair(input) }; }
  catch (e: any) { return { depth, error: e.name, message: e.message }; }
});
const dates = ['NaN', 'Infinity', '-Infinity', '-0.9', '-1.9', '253402300800000', '-62167219200001', '8640000000000000', '8640000000000001']
  .map(input => { try { return { input, iso: new Date(Number(input)).toISOString() }; }
    catch (e: any) { return { input, error: e.name, message: e.message }; } });
const collation = [[0xd800, 0xe000], [0xd801, 0xe001], [0xd800, 0xd801]].map(([left, right]) => ({
  left, right, sign: Math.sign(String.fromCharCode(left).localeCompare(String.fromCharCode(right), 'en-US')),
}));
const syntaxCases: any[] = [];
for (const input of ['{', '[1,]', '{"x":}', 'undefined', '"bad\\q"', '01', '[] trailing', '{\n"x":1,}']) {
  const file = join(output, 'syntax.json');
  writeFileSync(file, input);
  const transcript = await readTranscript(file);
  let registryError: string | undefined;
  try { await new FileRunRegistry(file).get({ sessionKey: 's', runId: 'r' }); }
  catch (e: any) { registryError = e.message; }
  assert.ok(registryError, 'malformed registry must reject');
  syntaxCases.push({ input, diagnostics: transcript.diagnostics, registryError });
}
const oracle = { sourceCommit, sourceSha256, node: process.version, jsonrepairSha256, urlCases, managed, validation, arrays, repairs, dates, collation, syntaxCases };
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(join(output, 'oracle.json'));
