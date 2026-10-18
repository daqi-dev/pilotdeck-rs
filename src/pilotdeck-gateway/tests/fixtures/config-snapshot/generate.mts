import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

// The oracle is produced by the fixed TypeScript baseline itself. Every case
// gets its own temporary home/project tree; the generator never writes a fixed
// path and only removes the tree it created.
const FIXED_LOADED_AT = '1970-01-01T00:00:00.000Z';
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const outputDir = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sourcePaths = [
  'src/pilot/config/loadPilotConfig.ts',
  'src/pilot/config/merge.ts',
  'src/pilot/config/hash.ts',
  'src/pilot/config/redact.ts',
  'src/pilot/config/parseMemoryConfig.ts',
  'src/pilot/config/parseGatewayConfig.ts',
  'src/pilot/config/parseToolsConfig.ts',
  'src/pilot/config/parseModulesConfig.ts',
  'src/pilot/config/parseRunPolicyConfig.ts',
  'src/pilot/config/types.ts',
  'src/model/config/parseModelConfig.ts',
  'src/model/config/resolveCredentials.ts',
  'src/model/config/providerCredentialScope.ts',
  'src/model/request/sanitizeProviderBody.ts',
  'src/model/protocol/canonical.ts',
  'src/model/protocol/capabilities.ts',
  'src/model/protocol/multimodal.ts',
  'src/model/thinking/settings.ts',
  'src/router/config/parseRouterConfig.ts',
  'src/always-on/config/parseAlwaysOnConfig.ts',
  'src/cron/config/parseCronConfig.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sourcePaths);
const { loadPilotConfig } = await import(pathToFileURL(join(baseline, 'src/pilot/config/loadPilotConfig.ts')).href);
const { stableStringify, sha256 } = await import(pathToFileURL(join(baseline, 'src/pilot/config/hash.ts')).href);
const { redactConfig } = await import(pathToFileURL(join(baseline, 'src/pilot/config/redact.ts')).href);

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
const cases: any[] = [];
const add = (id: string, yaml: string, extra: Record<string, unknown> = {}) => cases.push({ id, yaml: yaml.trimEnd() + '\n', ...extra });

add('minimal-defaults', BASE);
add('missing-schema-version', BASE.replace('schemaVersion: 1\n', ''), { env: { PILOT_AGENT_MODEL: 'parity/deterministic' } });
add('empty-known-sections', `${BASE}extension: {}
memory: {}
gateway: {}
adapters: {}
router: {}
alwaysOn: {}
cron: {}
tools: {}
runPolicy: {}
telemetry: {}
`);
add('full-snapshot', `schemaVersion: 1
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
      url: http://127.0.0.1:1/v1
      apiKey: mock-only
      timeoutMs: 5000
      headers: {x-a: b}
      models:
        deterministic:
          displayName: Det
          capabilities: {supportsToolUse: true, contextWindow: 1000, maxOutputTokens: 100}
          multimodal: {input: [text, image], imageDetail: auto}
          thinking: {state: enabled, efforts: [low, high]}
extension: {builtinPluginsEnabled: {a: true, b: false, c: 7}, includeHookEvents: true}
memory: {enabled: true, captureStrategy: full_session, includeAssistant: true, maxMessageChars: 10}
gateway: {port: 1234, idleSessionTimeoutMinutes: 5}
adapters: {telegram: {enabled: true, token: t, extra: {x: 1}}, cli: {autoConnectServer: false}, feishu: {enabled: true, defaultSessionLabel: x, connectionMode: stream}}
router: {enabled: true, fallback: {default: [parity/deterministic], maxFallbacks: 3}, tokenSaver: {enabled: true, judge: parity/deterministic, tiers: {medium: {model: parity/deterministic}}, subagent: {policy: skip}, cacheAwareSwitching: {enabled: false, minSavingsRatio: 0.2}}, autoOrchestrate: {enabled: true, triggerTiers: [medium], allowedTools: [read_file], slimSystemPrompt: false}, stats: {enabled: true, retentionMs: 1000}, customRouter: {extensionId: x}}
alwaysOn: {language: zh-CN, trigger: {tickIntervalMinutes: 7, cooldownMinutes: 0, dailyBudget: 3, heartbeatStaleSeconds: 100, recentUserMsgMinutes: 1, preferChannel: cli}, projects: {/abs/project: {enabled: true}}}
cron: {enabled: false, timezone: Asia/Shanghai, maxConcurrentRuns: 2, runTimeoutMinutes: 30}
tools: {webSearch: {enabled: true, provider: tavily, apiKey: k, endpoint: https://example.com/search, customProvider: {name: n, auth: bearer, method: GET}}}
modules: {tools: {enabled: true, implementationId: x, contract: pilotdeck.tools/v1, transport: module-http-v2, endpoint: http://x, catalog: [{name: t, description: d, inputSchema: {}}]}}
runPolicy: {failureGuard: {enabled: true, modelFailureLimit: 3, toolFailureLimits: {bash: 2}, toolLabels: {bash: Bash}}}
telemetry: {enabled: true}
proxy: {url: http://p, noProxy: localhost}
`);
add('agent-warning-fallbacks', BASE.replace('agent: {model: parity/deterministic}', `agent:
  model: parity/deterministic
  sandboxMode: bogus
  runtimeContextSurface: bogus
  interactionProfile: bogus
  fallbackModel: old
  subagents: {default: unknown/x, timeoutMs: 1.9, maxDepth: 0}`));
add('agent-subagent-inherit', BASE.replace('agent: {model: parity/deterministic}', `agent:
  model: parity/deterministic
  thinking: {enabled: false, budgetTokens: 1}
  subagents: {default: inherit}`));
add('model-catalog-openai', `schemaVersion: 1
agent: {model: openai/gpt-4.1}
model:
  providers:
    openai:
      apiKey: \${OPENAI_API_KEY}
      models:
        gpt-4.1: {}
`, { env: { OPENAI_API_KEY: ' mock-env-key ' } });
add('model-catalog-anthropic-alias', `schemaVersion: 1
agent: {model: anthropic/claude-sonnet-4.5}
model:
  providers:
    anthropic:
      apiKey: literal
      models:
        claude-sonnet-4.5: {displayName: Alias}
`);
add('model-google-openai-compat', `schemaVersion: 1
agent: {model: google/gemini-2.5-pro}
model:
  providers:
    google:
      protocol: openai
      apiKey: literal
      models:
        gemini-2.5-pro: {}
`);
add('model-custom-extra-retry', `schemaVersion: 1
agent: {model: custom/model}
model:
  providers:
    custom:
      protocol: openai-responses
      url: https://example.com/v1
      apiKey: literal
      headers: {x-a: b}
      extraBody: {temperature: 0.2, generationConfig: {temperature: 0.3, topP: 0.9}, reasoning: {effort: low}}
      speedMapping: openai_service_tier
      retry: {requestMaxRetries: 0, jitter: 0.5}
      models:
        model:
          aliases: [m1, m2]
          capabilities: {supportsThinking: false, supportsSpeed: false, maxContextTokens: 100, maxOutputTokens: 20}
          multimodal: {input: [text], maxImagesPerRequest: 1}
`);
add('model-extrabody-nonrecord', BASE.replace('      apiKey: mock-only','      apiKey: mock-only\n      extraBody: [1, 2]'));
add('model-missing-apikey-default-env', `schemaVersion: 1
agent: {model: parity/deterministic}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: \${PARITY_KEY}
      models:
        deterministic: {}
`, { env: { PARITY_KEY: ' resolved ' } });
add('memory-schedule-nested', BASE + `memory: {enabled: true, schedule: {reasoningMode: accuracy_first, autoIndexIntervalMinutes: 1.9, autoDreamIntervalMinutes: 0}}\n`);
add('memory-schedule-flat', BASE + `memory: {reasoningMode: answer_first, autoIndexIntervalMinutes: 2, autoDreamIntervalMinutes: 3, rootDir: /custom/memory}\n`);
add('memory-warning-unknown', BASE + `memory: {enabled: false, unknown: 1, schedule: {unknown: 2}}\n`);
add('gateway-defaults-and-token-path', BASE + `gateway: {tokenPath: x}\n`);
add('adapters-all', BASE + `adapters:
  cli: {}
  tui: {autoConnectServer: false}
  feishu: {enabled: true, appId: a, connectionMode: webhook, domainName: lark, permissionMode: bypassPermissions}
  weixin: {enabled: true}
  qq: {enabled: true, allowGroups: [a, 7], triggerPrefixes: [], maxMessageLength: 100}
  telegram: {enabled: true, apiKey: k, webhookUrl: w}
  discord: {}
  email: {enabled: false, extra: {x: 1}}
`);
add('router-disabled', BASE + `router: {enabled: false, scenarios: 7}\n`);
add('router-partial', BASE + `router: {zeroUsageRetry: {enabled: false}, transientRetry: {maxAttempts: 3, baseDelayMs: 1.5}, stats: {baselineModel: {provider: parity, model: deterministic}}}\n`);
add('router-token-saver-disabled', BASE + `router: {tokenSaver: {enabled: false}}\n`);
add('router-token-saver-defaults', BASE + `router: {tokenSaver: {judge: parity/deterministic, tiers: {simple: {model: parity/deterministic}, medium: {model: parity/deterministic, label: M}}}}\n`);
add('router-auto-orchestrate-defaults', BASE + `router: {autoOrchestrate: {}}\n`);
add('router-warnings', BASE + `router: {fallback: {unknown: [parity/deterministic]}, zeroUsageRetry: {enabled: x}, autoOrchestrate: {mainAgentModel: x, subagentModel: y, allowedTools: [a], blockedTools: [b]}, customRouter: {extensionId: x}}\n`);
add('always-on-defaults', BASE + `alwaysOn: {}\n`);
add('always-on-projects', BASE + `alwaysOn: {trigger: {enabled: true}, projects: {'/abs/one': {enabled: true, sessionKey: x, workspace: y}}}\n`);
add('always-on-removed-and-invalid', BASE + `alwaysOn: {enabled: true, discovery: {}, language: zh, trigger: {tickIntervalMinutes: -1, preferChannel: ''}}\n`);
add('cron-defaults', BASE + `cron: {}\n`);
add('cron-invalid-values', BASE + `cron: {enabled: 1, timezone: Mars/Unknown, maxConcurrentRuns: 1.5}\n`);
add('tools-disabled', BASE + `tools: {webSearch: {enabled: false, provider: 7}}\n`);
add('tools-custom', BASE + `tools: {webSearch: {enabled: true, provider: custom, searchEngine: google, customProvider: {name: n, method: POST, queryParam: q, resultsPath: r}, region: x, tavilyApiKey: y}}\n`);
add('tools-empty-websearch', BASE + `tools: {webSearch: {}}\n`);
add('modules-core-provider', BASE + `modules: {tools: {enabled: true, provider: pilotdeck}, skills: {enabled: false}, knowledge: {enabled: false}}\n`);
add('modules-agent-loop-stdio', BASE + `modules:
  agentLoop: {enabled: true, implementationId: a, contract: pilotdeck.agent-loop/v1, transport: module-stdio-v2, command: node, args: [x], env: {A: b}, deployment: {mode: build, context: ., dockerfile: Dockerfile, port: 1234, healthPath: /health}}
  skills: {enabled: true, provider: pilotdeck}
  modelProvider: {enabled: true, provider: pilotdeck}
  tools: {enabled: true, implementationId: t, contract: pilotdeck.tools/v1, transport: module-http-v2, endpoint: http://x, timeoutMs: 5, catalog: [{name: t, description: d, inputSchema: {}, kind: k, readOnly: true}]}
  context: {enabled: true, provider: pilotdeck}
  knowledge: {enabled: false}
  sop: {enabled: true, provider: staffdeck, endpoint: https://s, definitionsPath: defs.json, defaultSopId: x, contextMode: inherit, discoveryEndpoint: https://d, discoveryAgentId: a, discoveryApiKey: k, discoveryPath: p, discoveryTimeoutMs: 3, deployment: {mode: image, image: i, port: 9}}
`);
add('modules-sop-binding', BASE + `modules: {sop: {enabled: true, implementationId: s, contract: sop.lifecycle/v2, transport: sop-http-v2, endpoint: https://s, definitionsPath: /abs/defs.json, defaultSopId: x}}\n`, { env: { STAFFDECK_PROJECT_BINDING_ENABLED: 'true' } });
add('run-policy-default', BASE + `runPolicy: {unknown: 1}\n`);
add('run-policy-values', BASE + `runPolicy: {failureGuard: {enabled: true, modelFailureLimit: 2, toolFailureLimits: {bash: 0}, toolLabels: {bash: ' Bash '}}}\n`);
add('proxy-string', BASE + `proxy: http://proxy\n`);
add('proxy-migrated', BASE + `webui: {runtime: {httpsProxy: http://legacy}}\n`);
add('proxy-invalid', BASE + `proxy: {url: ''}\n`);
add('telemetry-default', BASE + `telemetry: {enabled: 1}\n`);
add('unknown-top-level', BASE + `extra: 1\n`);
add('project-override', `schemaVersion: 1
agent: {model: parity/deterministic}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: global-key
      models:
        deterministic: {}
runtimeContextSurface: system_prompt
`, {
  projectYaml: `schemaVersion: 1
agent: {model: parity/deterministic, sandboxMode: workspace-write}
model:
  providers:
    parity:
      url: http://127.0.0.1:28795/v1
      models:
        deterministic: {displayName: Project}
`,
  project: 'project',
});
add('project-missing', BASE, { project: 'absent' });
add('agent-subagent-spaced-default', BASE.replace('agent: {model: parity/deterministic}', `agent: {model: parity/deterministic, subagents: {default: ' parity/deterministic '}}`));
add('memory-schedule-empty-flat', BASE + `memory: {schedule: {}, autoIndexIntervalMinutes: 1.9, autoDreamIntervalMinutes: 2.9, reasoningMode: answer_first}\n`);
add('model-catalog-credential-env', `schemaVersion: 1
agent: {model: openai/gpt-4.1}
model:
  providers:
    openai:
      models:
        gpt-4.1: {}
`, { env: { OPENAI_API_KEY: ' catalog-env-key ' } });
add('model-cross-provider-catalog', `schemaVersion: 1
agent: {model: parity/gpt-4.1}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:1/v1
      apiKey: literal
      models:
        gpt-4.1: {}
`);
add('modules-agent-loop-tcp', BASE + `modules: {agentLoop: {enabled: true, implementationId: a, contract: pilotdeck.agent-loop/v1, transport: module-tcp-v2, host: 127.0.0.1, port: 9000, connectTimeoutMs: 5}}\n`);
add('modules-knowledge-credential', BASE + `modules: {knowledge: {enabled: true, implementationId: staffdeck.knowledge, contract: staffdeck.knowledge/v1, transport: module-http-v2, endpoint: https://k, callPath: /api/v1/agents/agent/knowledge-module/v2/module/call, manifestPath: /api/v1/knowledge-module/module-manifest, agentId: agent, credentialEnv: KNOWLEDGE_KEY}}\n`);
add('modules-skills-enabled', BASE + `modules: {skills: {enabled: true, provider: pilotdeck}, knowledge: {enabled: false}}\n`);
add('sop-discovery-env', BASE + `modules: {sop: {enabled: true, implementationId: s, contract: sop.lifecycle/v2, transport: sop-http-v2, endpoint: https://s, definitionsPath: /abs/defs.json, defaultSopId: x, discoveryEndpoint: https://d, discoveryAgentId: a, discoveryApiKey: "\${SOP_KEY}", discoveryPath: p, discoveryTimeoutMs: 3}}\n`, { env: { SOP_KEY: ' resolved-secret ', STAFFDECK_PROJECT_BINDING_ENABLED: 'true' } });
add('proxy-empty-string', BASE + `proxy: ""\n`);
// Review round 1 (codex P1 #1-#7, Claude P1-1/P1-2) counterexamples. Every
// expectation below is produced by the fixed baseline itself.
const BASE_M = `schemaVersion: 1
agent: {model: parity/m}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: mock-only
      models:
        m: {}
        other: {}
`;
add('router-scenarios-default-aligned', BASE_M + `router: {scenarios: {default: parity/m}}\n`);
add('router-scenarios-default-conflict', BASE_M + `router: {scenarios: {default: parity/other}}\n`);
for (const key of ['${A-B}', '${1KEY}', '${ A }']) {
  add(`model-apikey-env-lookalike-${key.replace(/[^A-Za-z0-9]+/g, '-')}`,
    BASE.replace('apiKey: mock-only', `apiKey: '${key}'`));
}
add('sop-discovery-apikey-literal', BASE + `modules: {sop: {enabled: true, implementationId: s, contract: sop.lifecycle/v2, transport: sop-http-v2, endpoint: https://s, definitionsPath: /abs/defs.json, defaultSopId: x, discoveryEndpoint: https://d, discoveryAgentId: a, discoveryApiKey: '\${A-B}', discoveryPath: p, discoveryTimeoutMs: 3}}\n`);
add('gateway-sweep-15', BASE + `gateway: {idleSweepIntervalSeconds: 15}\n`);
add('gateway-sweep-zero', BASE + `gateway: {idleSweepIntervalSeconds: 0}\n`);
add('gateway-sweep-fractional', BASE + `gateway: {idleSweepIntervalSeconds: 0.25}\n`);
add('gateway-sweep-invalid', BASE + `gateway: {idleSweepIntervalSeconds: '15'}\n`);
add('router-token-saver-int-order', BASE + `router: {tokenSaver: {judge: parity/deterministic, tiers: {2: {model: parity/deterministic}, 1: {model: parity/deterministic}}}}\n`);
add('router-token-saver-int-order-mixed', BASE + `router: {tokenSaver: {judge: parity/deterministic, tiers: {10: {model: parity/deterministic}, 2: {model: parity/deterministic}, z: {model: parity/deterministic}, 1: {model: parity/deterministic}}}}\n`);
const nestedMap = (depth: number) => '{x:'.repeat(depth) + '42' + '}'.repeat(depth);
const BASE_NO_SCHEMA = BASE.slice('schemaVersion: 1\n'.length);
add('depth-object-300', BASE + `adapters:\n  telegram:\n    extra: ${nestedMap(300)}\n`);
add('depth-object-500', BASE + `adapters:\n  telegram:\n    extra: ${nestedMap(500)}\n`);
add('depth-array-schema-300', `schemaVersion: ${'['.repeat(300)}1${']'.repeat(300)}\n` + BASE_NO_SCHEMA);
add('depth-over-v8-limit', BASE + `adapters:\n  telegram:\n    extra: ${nestedMap(1200)}\n`);
add('extension-proto-own-key', BASE + `extension: {builtinPluginsEnabled: {__proto__: true, a: false}}\n`);
add('adapters-extra-proto', BASE + `adapters: {telegram: {enabled: true, extra: {__proto__: {x: 1}, y: 2}}}\n`);
add('adapters-extra-int-order', BASE + `adapters: {telegram: {enabled: true, extra: {10: a, 2: b}}}\n`);
add('always-on-project-surrogate', BASE + `alwaysOn: {projects: {"\\uD800": {enabled: true}}}\n`);
add('modules-manifest-surrogate', BASE + `modules: {tools: {enabled: true, implementationId: x, contract: pilotdeck.tools/v1, transport: module-http-v2, endpoint: http://x, manifestPath: "/x\\uD800y", catalog: [{name: t, description: d, inputSchema: {}}]}}\n`);
add('modules-args-surrogate', BASE + `modules: {agentLoop: {enabled: true, implementationId: a, contract: pilotdeck.agent-loop/v1, transport: module-stdio-v2, command: node, args: ["\\uD800"]}}\n`);
add('version-non-default', BASE, { version: 7 });
add('model-int-order', `schemaVersion: 1
agent: {model: parity/2}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: mock-only
      models:
        10: {}
        2: {}
`);
add('no-pilot-home-bootstrap', BASE, { noPilotHome: true });
add('config-path-via-env', BASE, { configPathViaEnv: true });
add('project-proto-inherited', BASE, {
  projectYaml: `schemaVersion: 1
__proto__: {telemetry: {enabled: true}}
`,
  project: 'project',
});
add('project-nested-proto-inherited', BASE.replace('agent: {model: parity/deterministic}', `agent:
  model: parity/deterministic
  subagents: {default: parity/deterministic}`), {
  projectYaml: `schemaVersion: 1
agent:
  __proto__: {thinking: {enabled: true, budgetTokens: 7}}
`,
  project: 'project',
});

// Replay successful cases from the pre-existing config fixture corpus as well.
// The corpus is diagnostic-oriented, so only loadable cases can produce a
// snapshot; this samples it deterministically instead of duplicating thousands
// of identical projections.
const existingDir = new URL('../config/', import.meta.url);
for (const [file, sample] of [['catalog.json', 1], ['diagnostics.json', 18]] as const) {
  const corpus = JSON.parse(await readFile(new URL(file, existingDir), 'utf8'));
  const usable = corpus.cases.filter((item: any) => item.load?.ok
    && !item.sourceKind
    && Object.entries(item.env ?? {}).every(([key, value]) =>
      typeof value === 'string'
      && !value.includes('<PILOT_HOME>')
      && !value.includes('<ROOT>')
      && key !== 'PILOTDECK_CONFIG_PATH'
      && key !== 'PILOT_HOME'
      && key !== 'STAFFDECK_PROJECT_BINDING_ENABLED'));
  const selected = sample === 1 ? usable : usable.filter((_: unknown, index: number) => index % sample === 0);
  for (const item of selected) {
    cases.push({
      id: `existing-${file.replace('.json', '')}-${item.id}`,
      yaml: item.yaml,
      env: item.env ?? {},
    });
  }
}

// The corpus replay above only takes loadable cases. `diag-1-empty` (input
// `{}`) is the canonical error case: its first diagnostic is the recoverable
// schemaVersion warning, while the thrown error's own code is the fatal
// missing-agent error. Recording it pins `error.code` against
// `error.diagnosticCode` instead of comparing the two by accident.
add('existing-diagnostics-diag-1-empty', '{}');

add('non-ascii-values', `schemaVersion: 1
agent: {model: 中文/模型}
model:
  providers:
    中文:
      protocol: openai
      url: http://127.0.0.1:1/v1
      apiKey: 密钥
      models:
        模型: {displayName: 显示}
alwaysOn: {language: en}
`);

function encode(value: unknown): unknown {
  if (value === undefined) return { $: 'undefined' };
  if (value instanceof Date) return FIXED_LOADED_AT;
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
function normalize(value: unknown, root: string): unknown {
  if (value instanceof Date) return FIXED_LOADED_AT;
  if (typeof value === 'string') return value.split(root).join('<ROOT>').split(process.cwd()).join('<CWD>');
  if (Array.isArray(value)) return value.map((v) => normalize(v, root));
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value as Record<string, unknown>).map(([k, v]) => [normalize(k, root), normalize(v, root)]));
  }
  return value;
}
function normalizedSnapshot(snapshot: any, root: string): any {
  const normalized = normalize(snapshot, root) as any;
  // Content hashes are recomputed after path normalization so the fixture is
  // independent of the per-run temporary root while still being produced by
  // the baseline's own stableStringify/redactConfig. loadPilotConfig hashes one
  // object literal containing every section key, so absent sections stay as
  // explicit undefined members here too.
  const config = normalized.config;
  const hashInput = Object.fromEntries([
    'agent', 'model', 'extension', 'memory', 'gateway', 'adapters', 'router',
    'alwaysOn', 'cron', 'tools', 'telemetry', 'proxy', 'modules', 'runPolicy',
  ].map((key) => [key, config[key]]));
  normalized.contentHash = sha256(stableStringify(redactConfig(hashInput)));
  return normalized;
}

const scratch = await mkdtemp(join(tmpdir(), 'pdrs-config-snapshot-'));
const recorded = [];
try {
  for (const item of cases) {
    const root = join(scratch, item.id);
    const home = join(root, 'home');
    const projectRoot = item.project ? join(root, item.project) : undefined;
    await mkdir(home, { recursive: true });
    if (!item.noGlobal) await writeFile(join(home, 'pilotdeck.yaml'), item.yaml);
    if (projectRoot && item.projectYaml) {
      await mkdir(join(projectRoot, '.pilotdeck'), { recursive: true });
      await writeFile(join(projectRoot, '.pilotdeck', 'pilotdeck.yaml'), item.projectYaml);
    }
    const env: Record<string, string> = {
      HOME: home,
      PILOTDECK_SKIP_BROWSER_OPEN: '1',
      ...(item.env ?? {}),
    };
    if (!item.noPilotHome) env.PILOT_HOME = home;
    env.HOME = home;
    if (item.configPathViaEnv) env.PILOTDECK_CONFIG_PATH = join(home, 'pilotdeck.yaml');
    let snapshot;
    try {
      snapshot = loadPilotConfig({
        env,
        ...(item.configPathViaEnv ? {} : { configPath: join(home, 'pilotdeck.yaml') }),
        ...(projectRoot ? { projectRoot } : {}),
        version: item.version ?? 1,
      });
    } catch (error) {
      const e = error as { name?: string; code?: string; message?: string; diagnostics?: any[] };
      recorded.push({
        id: item.id,
        yaml: item.yaml,
        ...(item.projectYaml ? { projectYaml: item.projectYaml } : {}),
        ...(item.project ? { project: item.project } : {}),
        ...(item.version !== undefined ? { version: item.version } : {}),
        ...(item.noPilotHome ? { noPilotHome: true } : {}),
        ...(item.configPathViaEnv ? { configPathViaEnv: true } : {}),
        env: normalize({
          ...(item.env ?? {}),
          ...(item.configPathViaEnv ? { PILOTDECK_CONFIG_PATH: join(home, 'pilotdeck.yaml') } : {}),
        }, root),
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
    const normalized = encode(normalizedSnapshot(snapshot, root));
    const normalizedCase = {
      id: item.id,
      yaml: item.yaml,
      ...(item.projectYaml ? { projectYaml: item.projectYaml } : {}),
      ...(item.project ? { project: item.project } : {}),
      ...(item.version !== undefined ? { version: item.version } : {}),
      ...(item.noPilotHome ? { noPilotHome: true } : {}),
      ...(item.configPathViaEnv ? { configPathViaEnv: true } : {}),
      env: normalize({
        ...(item.env ?? {}),
        ...(item.configPathViaEnv ? { PILOTDECK_CONFIG_PATH: join(home, 'pilotdeck.yaml') } : {}),
      }, root),
      snapshot: normalized,
    };
    recorded.push(normalizedCase);
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
await writeFile(join(outputDir, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.log(JSON.stringify({ cases: recorded.length, output: join(outputDir, 'oracle.json') }));
