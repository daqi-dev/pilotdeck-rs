// Run from the repository root with the fixed baseline's tsx loader:
//   PDRS_BASELINE=/abs/PilotDeck LANG=en_US.UTF-8 \
//   node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/settings/generate.mts \
//     src/pilotdeck-gateway/tests/fixtures/settings
//
// The oracle is produced by the real TypeScript entry: every case boots the
// baseline's own `createLocalGateway` (which binds `resolveSettings` at
// `src/cli/createLocalGateway.ts:1402`) against its own temporary home/project
// tree and records `gateway.resolveSettings()` verbatim. No baseline function
// body is copied; nothing outside the per-run scratch tree is written.
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

// `loadedAt` is the one wall-clock field; the store stamps every source with
// the same Date, so its ISO string is rewritten to a fixed value.
const FIXED_LOADED_AT = '1970-01-01T00:00:00.000Z';
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const outputDir = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sourcePaths = [
  'src/pilot/config/hash.ts',
  'src/cli/createLocalGateway.ts',
  'src/cli/LocalGatewayBootConfig.ts',
  'src/pilot/config/loadPilotConfig.ts',
  'src/pilot/config/redact.ts',
  'src/pilot/config/PilotConfigStore.ts',
  'src/pilot/config/types.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sourcePaths);
const { createLocalGateway } = await import(
  pathToFileURL(join(baseline, 'src/cli/createLocalGateway.ts')).href
);
// stableStringify / redactConfig order keys via localeCompare, which follows
// the ICU default locale; Rust pins the same one.
const { stableStringify, sha256 } = await import(
  pathToFileURL(join(baseline, 'src/pilot/config/hash.ts')).href
);
const { redactConfig } = await import(pathToFileURL(join(baseline, 'src/pilot/config/redact.ts')).href);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const BASE = `schemaVersion: 1
agent: {model: parity/deterministic}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: mock-only
      models:
        deterministic: {}
`;
type Case = {
  id: string;
  yaml: string;
  env?: Record<string, string>;
  projectYaml?: string;
  noGlobal?: boolean;
};
const cases: Case[] = [];
const add = (id: string, yaml: string, extra: Omit<Case, 'id' | 'yaml'> = {}) =>
  cases.push({ id, yaml: yaml.trimEnd() + '\n', ...extra });

// Success: minimal configuration. Sources are [env/bootstrap (no path, phase),
// default file (path + contentHash, no phase)]; the provider apiKey is
// redacted in `config`, and so is `credentialSource`: the baseline's secret
// pattern is a substring match, and the key itself contains `credential`.
add('minimal', BASE);
// Success: every section, with secret-shaped keys at several nesting levels
// (provider apiKey, adapter token/webhookUrl/appSecret, tools.webSearch.apiKey
// and a secret key inside adapters.*.extra) next to non-secret values.
add('full-sections', `schemaVersion: 1
agent:
  model: parity/deterministic
  sandboxMode: read-only
  runtimeContextSurface: user_message
  interactionProfile: headless
  maxContextTokens: 1234
  maxOutputTokens: 567
  thinking: {enabled: true, budgetTokens: 99}
  subagents: {default: parity/deterministic, timeoutMs: 100, maxDepth: 2}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: mock-only
      models:
        deterministic: {}
extension: {builtinPluginsEnabled: {a: true, b: false, c: 7}, includeHookEvents: true}
memory: {enabled: true, captureStrategy: full_session, includeAssistant: true, maxMessageChars: 10}
gateway: {port: 1234, idleSessionTimeoutMinutes: 5}
adapters: {telegram: {enabled: true, token: t, apiKey: k, webhookUrl: w, extra: {x: 1, nestedSecret: s, password: p}}, cli: {autoConnectServer: false}, feishu: {enabled: true, appId: a, appSecret: sec, encryptKey: ek, verifyToken: vt, defaultSessionLabel: x, connectionMode: stream}}
router: {enabled: true, fallback: {default: [parity/deterministic], maxFallbacks: 3}, tokenSaver: {enabled: true, judge: parity/deterministic, tiers: {medium: {model: parity/deterministic}}}}
alwaysOn: {language: zh-CN}
cron: {enabled: false, timezone: Asia/Shanghai, maxConcurrentRuns: 2, runTimeoutMinutes: 30}
tools: {webSearch: {enabled: true, provider: tavily, apiKey: k, endpoint: 'https://example.com/search'}}
telemetry: {enabled: true}
proxy: {url: 'http://p', noProxy: localhost}
runPolicy: {failureGuard: {enabled: true, modelFailureLimit: 3, toolFailureLimits: {bash: 2}, toolLabels: {bash: Bash}}}
`);
// Success: extraBody keys that only Unicode-fold onto a secret name survive.
// U+212A (KELVIN SIGN) inside `toKen` folds to `token` under full Unicode
// case folding, but the baseline's non-Unicode case-insensitive pattern never
// folds a non-ASCII code unit onto an ASCII one, so the lookalike key survives
// while the ASCII-cased `TOKEN`/`token` spellings are redacted.
add('unicode-lookalike-keys', `schemaVersion: 1
agent: {model: parity/deterministic}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: mock-only
      extraBody: {toKen: visible, TOKEN: hidden-upper, token: hidden}
      models:
        deterministic: {}
`);
// Success: an unknown top-level field produces a recoverable warning whose
// `path` is present; diagnostics projection keeps `recoverable: true`.
add('unknown-field-warning', `${BASE}extra: 1\n`);
// Success: an invalid provider is excluded with a warning that carries a real
// `hint` string (the branch where hint is present and must be kept).
add('provider-excluded-hint', `schemaVersion: 1
agent: {model: parity/deterministic}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: mock-only
      models:
        deterministic: {}
    broken:
      protocol: openai
      url: '::not a url::'
      apiKey: mock-only
      models:
        m: {}
`);
// Success: an unknown router.fallback scenario warning is built without a
// `hint` member, so the snapshot diagnostic carries `hint: undefined` as an
// own key and the response must omit it (truthiness spread).
add('router-unknown-fallback', `${BASE}router: {fallback: {unknown: [parity/deterministic]}}\n`);
// Success: PILOT_AGENT_MODEL adds an env/merge source (no path, phase merge)
// and rewrites agent.model.
add('env-merge-source', BASE, { env: { PILOT_AGENT_MODEL: 'parity/deterministic' } });
// Success: a project configuration adds a priority-20 source after the
// default one and overrides sandboxMode.
add('project-config', BASE, {
  projectYaml: `schemaVersion: 1
agent: {model: parity/deterministic, sandboxMode: workspace-write}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28795/v1
      apiKey: project-key
      models:
        deterministic: {displayName: Project}
`,
});
// Success: only a project configuration exists (no global file), so the
// default source is absent entirely.
add('project-only', '', {
  noGlobal: true,
  projectYaml: `schemaVersion: 1
agent: {model: parity/deterministic}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: project-key
      models:
        deterministic: {}
`,
});
// External provider: the apiKey is an environment reference, resolved from
// env with credentialSource "environment"; the response still redacts the key.
add('external-provider-env-ref', `schemaVersion: 1
agent: {model: parity/deterministic}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: \${PARITY_KEY}
      models:
        deterministic: {}
`, { env: { PARITY_KEY: ' resolved-env-key ' } });
// External provider: a catalog provider without an explicit apiKey resolves
// it from the catalog's environment variable.
add('external-provider-catalog', `schemaVersion: 1
agent: {model: openai/gpt-4.1}
model:
  providers:
    openai:
      models:
        gpt-4.1: {}
`, { env: { OPENAI_API_KEY: ' catalog-env-key ' } });
// Unconfigured: no configuration at all; the real entry fails during gateway
// assembly and has no response body, so the boot error is recorded instead.
add('unconfigured', '', { noGlobal: true });
// Unconfigured-equivalent: a broken configuration (missing model section).
add('invalid-missing-model', `schemaVersion: 1
agent: {model: parity/deterministic}
`);

function encode(value: unknown): unknown {
  if (value === undefined) return { $: 'undefined' };
  if (typeof value === 'number') {
    if (Number.isNaN(value)) return { $: 'NaN' };
    if (value === Infinity) return { $: 'Infinity' };
    if (value === -Infinity) return { $: '-Infinity' };
    if (Object.is(value, -0)) return { $: '-0' };
    return value;
  }
  if (Array.isArray(value)) return value.map(encode);
  if (value !== null && typeof value === 'object') {
    const encoded: Record<string, unknown> = Object.create(null);
    for (const key of Object.keys(value)) {
      assert.notEqual(key, '$', 'fixture object uses the reserved "$" key');
      encoded[key] = encode((value as Record<string, unknown>)[key]);
    }
    return encoded;
  }
  return value;
}
function normalize(value: unknown, root: string, stamp: string): unknown {
  if (typeof value === 'string') {
    let out = value;
    if (stamp) out = out.split(stamp).join(FIXED_LOADED_AT);
    return out
      .split(root).join('<ROOT>')
      .split(process.cwd()).join('<CWD>');
  }
  if (Array.isArray(value)) return value.map((v) => normalize(v, root, stamp));
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([k, v]) => [
        normalize(k, root, stamp),
        normalize(v, root, stamp),
      ]),
    );
  }
  return value;
}
const HASH_SECTIONS = [
  'agent', 'model', 'extension', 'memory', 'gateway', 'adapters', 'router',
  'alwaysOn', 'cron', 'tools', 'telemetry', 'proxy', 'modules', 'runPolicy',
];
const hashOf = (config: any) =>
  sha256(stableStringify(redactConfig(Object.fromEntries(HASH_SECTIONS.map((key) => [key, config[key]])))));

// The output directory is created on demand and the oracle is written with
// 'wx' so a rerun can never silently overwrite a committed fixture.
await mkdir(outputDir, { recursive: true });
const scratch = await mkdtemp(join(tmpdir(), 'pdrs-settings-'));
const recorded = [];
try {
  for (const item of cases) {
    const root = join(scratch, item.id);
    const home = join(root, 'home');
    const project = join(root, 'project');
    await mkdir(home, { recursive: true });
    await mkdir(project, { recursive: true });
    if (!item.noGlobal) await writeFile(join(home, 'pilotdeck.yaml'), item.yaml);
    if (item.projectYaml) {
      await mkdir(join(project, '.pilotdeck'), { recursive: true });
      await writeFile(join(project, '.pilotdeck', 'pilotdeck.yaml'), item.projectYaml);
    }
    // The boot config injects PILOT_HOME from the explicit pilotHome option;
    // everything else is a closed, deterministic environment for this case.
    const env: Record<string, string> = {
      HOME: home,
      PATH: process.env.PATH ?? '',
      PILOTDECK_SKIP_BROWSER_OPEN: '1',
      PILOT_SKIP_BROWSER_OPEN: '1',
      SKIP_BROWSER_OPEN: '1',
      PILOTDECK_SKIP_BROWSER: '1',
      GP_POLL_WATCH: '1',
      ...(item.env ?? {}),
    };
    const entry = {
      id: item.id,
      yaml: item.yaml,
      ...(item.projectYaml ? { projectYaml: item.projectYaml, project: 'project' } : {}),
      ...(item.noGlobal ? { noGlobal: true } : {}),
      env: normalize(item.env ?? {}, root, ''),
    };
    let local: Awaited<ReturnType<typeof createLocalGateway>>;
    try {
      local = createLocalGateway({ pilotHome: home, projectRoot: project, env });
    } catch (error) {
      // The real entry has no response body when the configuration cannot
      // even be loaded (store creation fails inside gateway assembly).
      const e = error as { name?: string; code?: string; message?: string; diagnostics?: any[] };
      recorded.push({
        ...entry,
        error: {
          name: e.name,
          code: e.code,
          message: e.message,
          diagnosticCode: e.diagnostics?.[0]?.code,
          hint: e.diagnostics?.[0]?.hint,
        },
      });
      continue;
    }
    try {
      const snapshot = local.configStore.getSnapshot();
      const stamp = snapshot.loadedAt.toISOString();
      const response = await local.gateway.resolveSettings();
      // The transcribed section list must reproduce the hash the real entry
      // returned before any path is rewritten.
      assert.equal(hashOf(response.config), response.contentHash,
        'transcribed contentHash input diverges from the real entry');
      assert.deepEqual(Object.keys(response.config).filter((key) => !HASH_SECTIONS.includes(key)), [],
        'response config has a section outside the transcribed hash input');
      const normalized = normalize(response, root, stamp) as any;
      // Content hashes are recomputed after path normalization so the fixture
      // is independent of the per-run temporary root while still being
      // produced by the baseline's own stableStringify/redactConfig.
      normalized.contentHash = hashOf(normalized.config);
      recorded.push({ ...entry, response: encode(normalized) });
    } finally {
      await local.dispose();
    }
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
const oracle = {
  baselineCommit: sourceCommit,
  node: process.version,
  sources: sourceSha256,
  count: recorded.length,
  cases: recorded,
};
await writeFile(join(outputDir, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ cases: recorded.length, output: join(outputDir, 'oracle.json') }));
