// Child driver for the channel-loader fixture: runs only the import-failure
// scenario in a fresh module registry, because Node caches a module whose
// evaluation threw and the mock signal module must fail its one and only
// load. Invoked by generate.mts as `node --import <tsx loader> this.mjs OUT`.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

assert.equal(process.version, 'v22.22.0', '#1327 child Node baseline');
assert.equal(process.versions.icu, '78.2', '#1327 child ICU baseline');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output file');
const baseline = resolve(process.env.PDRS_BASELINE);
const source = 'src/adapters/channel/loadEnabledChannels.ts';
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
const compiled = transformSync(await readFile(join(baseline, source)), {
  loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
}).code;
const module = { exports: {} };
new Function('require', 'module', 'exports', compiled)((name) => {
  if (name.startsWith('node:')) return requireBaseline(name);
  throw new Error(`unexercised import accessed: ${source} -> ${name}`);
}, module, module.exports);

const constructions = [];
globalThis.__pdrsChannelLoaderProbe = {
  importFails: key => (key === 'signal' ? 'connect ECONNREFUSED 127.0.0.1:8080' : null),
  constructorFails: () => undefined,
  construct: (key, options) => constructions.push({ key, options }),
};
const errors = [];
const realError = console.error;
console.error = (...parts) => {
  errors.push(parts.map(part => (part instanceof Error ? String(part) : String(part))).join(' '));
};
const adapters={
    slack: { enabled: true, token: 'slack-token' },
    signal: { enabled: true, extra: { restUrl: 'http://127.0.0.1:8080' } },
    whatsapp: { enabled: true, extra: { bridgeUrl: 'http://127.0.0.1:8081' } },
};
const result=await module.exports.loadEnabledChannels(adapters, {});
console.error = realError;
const output = process.argv[2];
await mkdir(join(output, '..'), { recursive: true });
await writeFile(output, JSON.stringify({
  name: 'import-failure-continues', adapters, options:{}, importFailures:{signal:'connect ECONNREFUSED 127.0.0.1:8080'},
  expected:{
  result: result.map(channel => channel.channelKey),
  constructions: constructions.map(({ key, options }) => ({
    key,
    options: Object.fromEntries(Object.entries(options).map(([field, value]) =>
      [field, value === undefined ? '<undefined>' : value])),
  })),
  errors,
  },
}, null, 2) + '\n', {flag:'wx'});
