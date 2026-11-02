// Run from the repository root with the fixed baseline's tsx loader; see README.md.
// Real loadEnabledChannels.ts from baseline cc7c4e6: the dynamic imports load
// the mock channel modules committed next to this generator.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1327 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1327 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/adapters/channel/loadEnabledChannels.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');

// Transpile the entire, unchanged loader module. Its static imports are
// type-only, so the injected require stays inert; any other import fails closed.
const loaderSource = await readFile(join(baseline, sources[0]));
const compiled = transformSync(loaderSource, {
  loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: sources[0],
}).code;
function loadEnabledChannelsModule() {
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', compiled)((name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    throw new Error(`unexercised import accessed: ${sources[0]} -> ${name}`);
  }, module, module.exports);
  return module.exports;
}

// Shared by this process and the import-failure child: records constructions
// and scriptable failures. Node caches a module whose evaluation threw, so an
// import failure can only ever be observed on a channel's first import; the
// child process owns that scenario with a fresh module registry.
type Recorder = { key: string; options: Record<string, unknown> };
const state = { constructions: [] as Recorder[], importFailures: {}, constructorFailures: {} };
globalThis.__pdrsChannelLoaderProbe = {
  importFails: (key: string) => state.importFailures[key] ?? null,
  constructorFails: (key: string) => state.constructorFailures[key],
  construct: (key: string, options: Record<string, unknown>) => {
    state.constructions.push({ key, options });
  },
};

// `console.error` is part of the observable error contract of the loader.
const errors: string[] = [];
const realError = console.error;
console.error = (...parts: unknown[]) => {
  errors.push(parts.map(part => (part instanceof Error ? String(part) : String(part))).join(' '));
};

function describeConstructions() {
  return state.constructions.map(({ key, options }) => ({
    key,
    // undefined-valued properties exist in the option object TS builds; JSON
    // would silently drop them, so they are recorded explicitly.
    options: Object.fromEntries(Object.entries(options).map(([field, value]) =>
      [field, value === undefined ? '<undefined>' : value])),
  }));
}

async function scenario(
  name: string,
  adapters: unknown,
  options: { pilotHome?: string } = {},
  constructorFailures: Record<string, unknown> = {},
) {
  state.constructions = [];
  state.importFailures = {};
  state.constructorFailures = constructorFailures;
  const before = errors.length;
  let result: unknown;
  try {
    result = await loadEnabledChannelsModule().loadEnabledChannels(adapters as any, options);
  } catch (error) {
    result = { thrown: String(error) };
  }
  return {
    name,
    adapters: adapters ?? null, adaptersUndefined: adapters === undefined, options,
    failures: Object.fromEntries(Object.entries(constructorFailures).map(([key,value]) => [key,value instanceof Error ? {error:value.message} : {plain:String(value)}])),
    expected: {
    result: (result as any[]).map(channel => channel.channelKey),
    constructions: describeConstructions(),
    errors: errors.slice(before),
    },
  };
}

const cases = [
  // L1: no adapters at all.
  await scenario('unconfigured', undefined),
  // L2: keys absent, present-but-disabled, present-without-enabled.
  await scenario('truthiness-and-nullish', {telegram:{enabled:0},discord:{enabled:''},slack:{enabled:null},matrix:{enabled:[]},bluebubbles:{enabled:'yes',token:'',extra:{password:'unused'}},email:{enabled:1,extra:{text:'line\u2028separator'}},sms:{enabled:{}},webhook:{enabled:false}}),
  await scenario('null-token-fallback', {bluebubbles:{enabled:true,token:null,extra:{password:'fallback'}}}),
  await scenario('none-enabled', {
    discord: { enabled: false, token: 'no' },
    slack: { token: 'still-no' },
  }),
  // L3+L5: every known key enabled at once; declaration order and each
  // cfg->options extraction from the real CHANNEL_LOADERS table.
  await scenario('all-keys-enabled', {
    telegram: { enabled: true, token: 'tg-token', webhookUrl: 'https://tg.example/hook' },
    discord: { enabled: true, token: 'dc-token' },
    slack: { enabled: true, token: 'slack-token', extra: { appToken: 'xapp-1' } },
    matrix: { enabled: true, token: 'mx-token', extra: { homeserver: 'https://mx.example', userId: '@bot:mx.example' } },
    mattermost: { enabled: true, token: 'mm-token', extra: { serverUrl: 'https://mm.example', teamId: 'team-9' } },
    signal: { enabled: true, extra: { restUrl: 'http://127.0.0.1:8080', account: '+15550100' } },
    whatsapp: { enabled: true, extra: { bridgePath: '/tmp/bridge', bridgeUrl: 'http://127.0.0.1:8081' } },
    bluebubbles: { enabled: true, extra: { serverUrl: 'http://127.0.0.1:1234', password: 'bb-pass' } },
    dingtalk: { enabled: true, extra: { clientId: 'dt-id', clientSecret: 'dt-secret' } },
    wecomCallback: { enabled: true, token: 'wc-token', extra: { corpId: 'ww1', agentId: '1000002', secret: 'sec', encodingAesKey: 'aes', port: 9011 } },
    email: { enabled: true, extra: { imap: { host: 'imap.example' }, smtp: { host: 'smtp.example' } } },
    sms: { enabled: true, extra: { provider: 'twilio' } },
    homeassistant: { enabled: true, token: 'ha-token', extra: { url: 'http://127.0.0.1:8123' } },
    apiServer: { enabled: true, apiKey: 'api-key-1', extra: { port: 8787 } },
    webhook: { enabled: true, extra: { port: 9919, secret: 'hook-secret' } },
  }, { pilotHome: '/pilot-home' }),
  // L5 detail: absent optionals propagate as undefined-valued properties;
  // bluebubbles falls back to token, matrix passes options.pilotHome through.
  await scenario('optionals-and-fallbacks', {
    bluebubbles: { enabled: true, token: 'bb-from-token' },
    matrix: { enabled: true },
    email: { enabled: true },
    sms: { enabled: true },
  }, { pilotHome: '/other-home' }),
  // L4: a constructor throwing an Error and one throwing a non-Error string
  // both land in the same catch; String(e) shapes the log lines.
  await scenario('constructor-failures', {
    telegram: { enabled: true, token: 'tg' },
    discord: { enabled: true, token: 'dc' },
    email: { enabled: true },
  }, {}, {
    discord: new Error('discord exploded'),
    email: 'boom',
  }),
];
console.error = realError;

// L4, import rejection: run in a child process because Node caches a module
// whose evaluation threw; the mock signal module must be loaded exactly once
// and fail that one load.
const output = process.argv[2];
const childArtifact = 'import-failure.json';
execFileSync(process.execPath, [
  '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'),
  join(import.meta.dirname, 'import-failure.mjs'), join(output, childArtifact),
], { stdio: 'inherit', env: { ...process.env }, timeout: 60000 });
cases.push(JSON.parse(await readFile(join(output, childArtifact), 'utf8')));

await mkdir(output, { recursive: true });
const command = 'PDRS_BASELINE=/absolute/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/channel-loader/generate.mts src/pilotdeck-gateway/tests/fixtures/channel-loader';
await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, command, node:process.version, icu:process.versions.icu, cases }, null, 2) + '\n', {flag:'wx'});
console.log(`PASS #1327 loader: ${cases.length} real TS scenarios`);
