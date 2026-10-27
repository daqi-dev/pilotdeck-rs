// Records the fixed TypeScript baseline's SDK session configuration and
// validation behaviour. Run from the repo root with Node 22 and the baseline's
// tsx loader; see README.md. Every run uses one exclusive mkdtemp root and
// only removes that root.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { chmod, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture output directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const outputDir = resolve(process.argv[2]);

const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
);
const git = (...args: string[]) =>
  execFileSync(gitExecutable, ['--no-replace-objects', '-C', baseline, ...args], {
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
const sourceCommit = bindBaseline({ baseline, commit: BASELINE_COMMIT, gitExecutable }).provenance.ref;
const trackedChanges = git('status', '--porcelain', '--untracked-files=no').toString();
assert.equal(
  trackedChanges,
  '',
  `baseline has uncommitted changes to tracked files:\n${trackedChanges}`,
);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const ENTRIES = [
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/pilot/config/sdkSessionSettings.ts',
];
const sourceSha256: Record<string, string> = sourceClosureSha256(
  baseline,
  BASELINE_COMMIT,
  gitExecutable,
  ENTRIES,
);

// ── real settings module, real DialogGatewayError, private registry functions ─
const settingsModule = await import(
  pathToFileURL(join(baseline, 'src/pilot/config/sdkSessionSettings.ts')).href
);
const {
  PilotSdkSessionSettingsError,
  resolvePilotSdkSessionSettings,
  validatePilotSdkSessionSettings,
  validatePilotSdkSettingSources,
} = settingsModule;
const errorsModule = await import(
  pathToFileURL(join(baseline, 'src/gateway/dialog/errors.ts')).href
);

const { createRequire } = await import('node:module');
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
const registrySource = 'src/cli/ProjectRuntimeRegistry.ts';
let compiled = transformSync(await readFile(join(baseline, registrySource), 'utf8'), {
  loader: 'ts',
  format: 'cjs',
  target: 'es2022',
  sourcefile: registrySource,
}).code;
// The baseline is an ES module; esbuild's CJS output drops strict mode, which
// would silently ignore `config.taskBudget = ...` on a primitive. Keep the
// harness in the same strict mode as the real module.
compiled = `'use strict';\n${compiled}`;
compiled += `
module.exports.__internals = {
  validateSdkManagedPermissions,
  validateSdkSelectorPolicy,
  applyOrganizationTaskBudgetCap,
  assertOrganizationSettingSourcesAllowed,
  resolveSdkConfiguredModel,
  assertSdkManagedModelAllowed,
  isOrganizationModelAllowed,
  organizationProviderPolicyRejection,
  hasOrganizationSdkSessionDefaults,
  resolveSdkSessionSkills,
};
`;
const pilotModule = {
  PilotSdkSessionSettingsError,
  resolvePilotSdkSessionSettings,
  validatePilotSdkSettingSources,
};
let loadPluginFromPath = async (path: string, source: string) => ({
  name: basename(path),
  source,
  path,
  skills: [],
});
const compiledModule = { exports: {} as any };
new Function('require', 'module', 'exports', compiled)(
  (name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name === '../pilot/index.js') return pilotModule;
    if (name === '../gateway/dialog/errors.js') return errorsModule;
    if (name === '../extension/index.js') return { loadPluginFromPath: (...args: any[]) => loadPluginFromPath(...args) };
    return {};
  },
  compiledModule,
  compiledModule.exports,
);
const Registry = compiledModule.exports.ProjectRuntimeRegistry;
const internals = compiledModule.exports.__internals;
assert.equal(typeof Registry.prototype.setSdkSessionConfig, 'function');

const scratch = await mkdtemp(join(tmpdir(), 'pdrs-sdk-session-'));
const ROOT = scratch;

type ErrorShape = { name: unknown; code: unknown; message: unknown };
function errorShape(error: any): ErrorShape {
  return {
    name: error?.name,
    code: error?.code,
    message: error?.message === undefined ? String(error) : String(error.message),
  };
}
function encode(value: any): any {
  if (value === undefined) return { $: 'undefined' };
  if (typeof value === 'number') {
    if (Number.isNaN(value)) return { $: 'NaN' };
    if (value === Infinity) return { $: 'Infinity' };
    if (value === -Infinity) return { $: '-Infinity' };
    if (Object.is(value, -0)) return { $: '-0' };
    return value;
  }
  if (typeof value === 'string') return value.split(ROOT).join('<ROOT>');
  if (Array.isArray(value)) return value.map(encode);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, encode(item)]));
  }
  return value;
}
function capture(fn: () => any): any {
  try {
    return { ok: true, value: encode(fn()) };
  } catch (error) {
    return { ok: false, error: encode(errorShape(error)) };
  }
}
async function captureAsync(fn: () => Promise<any>): Promise<any> {
  try {
    return { ok: true, value: encode(await fn()) };
  } catch (error) {
    return { ok: false, error: encode(errorShape(error)) };
  }
}
function writeCaseFile(caseDir: string, path: string, content: string): Promise<void> {
  const absolute = join(caseDir, path);
  return mkdir(dirname(absolute), { recursive: true }).then(() => writeFile(absolute, content));
}

// ── shared snapshot ─────────────────────────────────────────────────────────
const SNAPSHOT = {
  config: {
    agent: { model: { id: 'main', provider: 'p1', model: 'm1' } },
    model: {
      providers: {
        p1: {
          url: 'https://api.p1.example/v1',
          credentialSource: 'environment',
          models: {
            m1: { id: 'm1' },
            alt: { id: 'alias-1' },
          },
        },
        p2: {
          url: 'http://api.p2.example/v1',
          models: { m1: { id: 'm1' } },
        },
        p3: {
          url: 'https://api.p3.example/v1',
          credentialSource: 'literal',
          models: { m3: { id: 'm3' } },
        },
      },
    },
  },
};

// ── settingSources validation ───────────────────────────────────────────────
const settingsSources: any[] = [];
const sourceValues: [string, any][] = [
  ['undefined', undefined],
  ['empty-array', []],
  ['null', null],
  ['string', 'user'],
  ['unknown', ['bogus']],
  ['duplicate', ['managed', 'managed']],
  ['numeric-entry', ['user', 1]],
  ['reversed', ['local', 'user', 'managed', 'project']],
  ['subset', ['project', 'local']],
  ['managed-only', ['managed']],
];
for (const [name, value] of sourceValues) {
  settingsSources.push({
    name,
    value: encode(value),
    result: capture(() => validatePilotSdkSettingSources(value)),
  });
}

// ── settings validation ─────────────────────────────────────────────────────
const settingsValidate: any[] = [];
const settingsValues: [string, any][] = [
  ['undefined', undefined],
  ['null', null],
  ['array', []],
  ['string', 'x'],
  ['empty-object', {}],
  ['agent-undefined', { agent: undefined }],
  ['agent-null', { agent: null }],
  ['agent-array', { agent: [] }],
  ['agent-empty', { agent: {} }],
  ['unknown-root', { foo: 1 }],
  ['unknown-agent', { agent: { bogus: 1 } }],
  ['unknown-root-integer-key', { agent: {}, zz: 1, '5': 2 }],
  ['unknown-agent-integer-key', { agent: { zz: 1, '7': 2 } }],
  [
    'unknown-thinking-integer-key',
    { agent: { thinking: { enabled: true, zz: 1, '3': 2 } } },
  ],
  ['model-null', { agent: { model: null } }],
  ['model-string', { agent: { model: 'm1' } }],
  ['model-whitespace', { agent: { model: '  m1  ' } }],
  ['model-empty', { agent: { model: '' } }],
  ['model-spaces', { agent: { model: '   ' } }],
  ['model-number', { agent: { model: 3 } }],
  ['model-undefined', { agent: { model: undefined } }],
  ['fallback-null', { agent: { fallbackModel: null } }],
  ['fallback-empty', { agent: { fallbackModel: ' ' } }],
  ['fallback-number', { agent: { fallbackModel: 4 } }],
  ['maxContext-ok', { agent: { maxContextTokens: 1 } }],
  ['maxContext-zero', { agent: { maxContextTokens: 0 } }],
  ['maxContext-negative', { agent: { maxContextTokens: -1 } }],
  ['maxContext-fraction', { agent: { maxContextTokens: 1.5 } }],
  ['maxContext-string', { agent: { maxContextTokens: '3' } }],
  ['maxContext-null', { agent: { maxContextTokens: null } }],
  ['maxContext-nan', { agent: { maxContextTokens: NaN } }],
  ['maxContext-infinity', { agent: { maxContextTokens: Infinity } }],
  ['maxOutput-ok', { agent: { maxOutputTokens: 5 } }],
  ['maxOutput-zero', { agent: { maxOutputTokens: 0 } }],
  ['thinking-null', { agent: { thinking: null } }],
  ['thinking-missing-enabled', { agent: { thinking: {} } }],
  ['thinking-enabled-true', { agent: { thinking: { enabled: true } } }],
  ['thinking-enabled-string', { agent: { thinking: { enabled: 'yes' } } }],
  ['thinking-budget-zero', { agent: { thinking: { enabled: true, budgetTokens: 0 } } }],
  ['thinking-budget-negative', { agent: { thinking: { enabled: true, budgetTokens: -1 } } }],
  ['thinking-budget-fraction', { agent: { thinking: { enabled: true, budgetTokens: 1.5 } } }],
  ['thinking-budget-string', { agent: { thinking: { enabled: true, budgetTokens: '3' } } }],
  ['thinking-budget-null', { agent: { thinking: { enabled: true, budgetTokens: null } } }],
  ['thinking-unknown', { agent: { thinking: { enabled: true, foo: 1 } } }],
  ['subagents-null', { agent: { subagents: null } }],
  ['subagents-array', { agent: { subagents: [] } }],
  ['subagents-empty', { agent: { subagents: {} } }],
  ['subagents-unknown', { agent: { subagents: { bogus: 1 } } }],
  ['subagent-default-null', { agent: { subagents: { default: null } } }],
  ['subagent-default-inherit', { agent: { subagents: { default: 'inherit' } } }],
  ['subagent-default-inherit-space', { agent: { subagents: { default: ' inherit ' } } }],
  ['subagent-default-empty', { agent: { subagents: { default: '' } } }],
  ['subagent-default-space', { agent: { subagents: { default: ' ' } } }],
  ['subagent-default-string', { agent: { subagents: { default: ' m1 ' } } }],
  ['subagent-default-number', { agent: { subagents: { default: 3 } } }],
  ['subagent-timeout-ok', { agent: { subagents: { timeoutMs: 5 } } }],
  ['subagent-timeout-zero', { agent: { subagents: { timeoutMs: 0 } } }],
  ['subagent-timeout-fraction', { agent: { subagents: { timeoutMs: 1.5 } } }],
  ['subagent-timeout-string', { agent: { subagents: { timeoutMs: '5' } } }],
  ['subagent-depth-zero', { agent: { subagents: { maxDepth: 0 } } }],
  ['subagent-depth-negative', { agent: { subagents: { maxDepth: -1 } } }],
  ['subagent-depth-fraction', { agent: { subagents: { maxDepth: 1.5 } } }],
  ['subagent-depth-unsafe', { agent: { subagents: { maxDepth: 2 ** 53 } } }],
  ['subagent-depth-string', { agent: { subagents: { maxDepth: '0' } } }],
  [
    'subagent-undefined-values',
    { agent: { subagents: { default: undefined, timeoutMs: undefined, maxDepth: undefined } } },
  ],
  [
    'combined',
    {
      agent: {
        model: 'm1',
        fallbackModel: 'p3/m3',
        maxContextTokens: 2,
        maxOutputTokens: 3,
        thinking: { enabled: false, budgetTokens: 7 },
        subagents: { default: 'inherit', timeoutMs: 9, maxDepth: 2 },
      },
    },
  ],
];
for (const [name, value] of settingsValues) {
  settingsValidate.push({
    name,
    value: encode(value),
    result: capture(() => validatePilotSdkSessionSettings(value)),
  });
}

// ── Object.keys traversal order ─────────────────────────────────────────────
// `encode`/`JSON.stringify` enumerate integer keys first, which would erase the
// source insertion order Rust must reproduce. Record explicit entry pairs so
// the replay can build the same JsObject order the SDK wire value had.
type OrderedNode =
  | string
  | number
  | boolean
  | null
  | undefined
  | { entries: [string, OrderedNode][] };
function orderedValue(node: OrderedNode): any {
  if (node && typeof node === 'object' && 'entries' in node) {
    return Object.fromEntries(
      node.entries.map(([key, value]) => [key, orderedValue(value)]),
    );
  }
  return node;
}
const settingsEntryOrder: any[] = [];
const entryOrderValues: [string, OrderedNode][] = [
  ['unknown-root-integer-key', { entries: [['agent', {}], ['zz', 1], ['5', 2]] }],
  [
    'unknown-agent-integer-key',
    { entries: [['agent', { entries: [['zz', 1], ['7', 2]] }]] },
  ],
  [
    'unknown-thinking-integer-key',
    {
      entries: [
        [
          'agent',
          {
            entries: [
              ['thinking', { entries: [['enabled', true], ['zz', 1], ['3', 2]] }],
            ],
          },
        ],
      ],
    },
  ],
];
for (const [name, node] of entryOrderValues) {
  settingsEntryOrder.push({
    name,
    entries: encode(node),
    result: capture(() => validatePilotSdkSessionSettings(orderedValue(node))),
  });
}

// ── settings resolution ─────────────────────────────────────────────────────
type ResolveCase = {
  name: string;
  files?: { path: string; content: string }[];
  dirs?: string[];
  modes?: { path: string; mode: number }[];
  symlinks?: { path: string; target: string }[];
  env?: Record<string, string>;
  options: Record<string, any>;
};
/** yaml@2.9.0 throws ReferenceError while toJSON expands this alias graph. */
function aliasBombSource(): string {
  let source = 'a: &a [x,x,x,x,x,x,x,x,x,x]\n';
  let previous = 'a';
  for (let index = 0; index < 8; index += 1) {
    const name = `b${index}`;
    source += `${name}: &${name} [${Array<string>(10).fill(`*${previous}`).join(',')}]\n`;
    previous = name;
  }
  return `${source}agent: {model: m}\n`;
}
const settingsResolveCases: ResolveCase[] = [
  { name: 'empty', options: {} },
  { name: 'host-defaults', options: { hostDefaults: { agent: { model: 'd' } } } },
  { name: 'settings-only', options: { settings: { agent: { model: 's' } } } },
  { name: 'enforced-only', options: { hostEnforcedSettings: { agent: { model: 'e' } } } },
  {
    name: 'precedence-layers',
    files: [
      { path: 'project/.pilotdeck/pilotdeck.yaml', content: 'agent: {model: project-model}\n' },
    ],
    options: {
      hostSettingSources: ['project'],
      hostDefaults: { agent: { model: 'default-model', maxContextTokens: 10 } },
      settings: { agent: { fallbackModel: 'settings-fallback', maxContextTokens: 20 } },
      hostEnforcedSettings: { agent: { model: 'enforced-model' } },
    },
  },
  {
    name: 'subagent-merge',
    options: {
      hostDefaults: { agent: { subagents: { default: 'd', timeoutMs: 1 } } },
      settings: { agent: { subagents: { timeoutMs: 2, maxDepth: 3 } } },
      hostEnforcedSettings: { agent: { subagents: { default: 'e' } } },
    },
  },
  {
    name: 'managed-not-selected',
    options: {
      hostManagedSettings: { agent: { model: 'managed-model' } },
      settings: { agent: { model: 'sdk-model' } },
    },
  },
  {
    name: 'managed-selected',
    options: {
      hostManagedSettings: { agent: { model: 'managed-model' } },
      hostSettingSources: ['managed'],
      settings: { agent: { fallbackModel: 'sdk-fallback' } },
    },
  },
  {
    name: 'user-source-overlay',
    files: [
      {
        path: 'home/pilotdeck.yaml',
        content: [
          'model: {providers: {ignored: {}}}',
          'agent:',
          '  model: user-model',
          '  fallbackModel: user-fallback',
          '  maxContextTokens: 11',
          '  maxOutputTokens: 12',
          '  thinking: {enabled: true, budgetTokens: 13, extra: ignored}',
          '  subagents: {default: user-sub, timeoutMs: 14, maxDepth: 15, extra: ignored}',
          '  unknownAgentKey: ignored',
          'unknownRootKey: ignored',
          '',
        ].join('\n'),
      },
    ],
    env: { PILOT_HOME: '<CASE>/home' },
    options: { hostSettingSources: ['user'] },
  },
  {
    name: 'project-and-local',
    files: [
      { path: 'project/.pilotdeck/pilotdeck.yaml', content: 'agent: {model: project-model}\n' },
      { path: 'project/.pilotdeck/pilotdeck.local.yaml', content: 'agent: {model: local-model}\n' },
    ],
    options: { hostSettingSources: ['local', 'project'] },
  },
  {
    name: 'missing-file',
    options: { hostSettingSources: ['project', 'user', 'local'] },
  },
  {
    name: 'empty-yaml',
    files: [{ path: 'project/.pilotdeck/pilotdeck.yaml', content: '' }],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'null-root',
    files: [{ path: 'project/.pilotdeck/pilotdeck.yaml', content: 'null\n' }],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'array-root',
    files: [{ path: 'project/.pilotdeck/pilotdeck.yaml', content: '- a\n- b\n' }],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'scalar-root',
    files: [{ path: 'project/.pilotdeck/pilotdeck.yaml', content: 'hello\n' }],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'invalid-yaml',
    files: [{ path: 'project/.pilotdeck/pilotdeck.yaml', content: 'agent: [\n' }],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'read-failed-directory',
    dirs: ['project/.pilotdeck/pilotdeck.yaml'],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'read-failed-permission',
    files: [
      { path: 'project/.pilotdeck/pilotdeck.yaml', content: 'agent: {model: unreadable}\n' },
    ],
    modes: [{ path: 'project/.pilotdeck/pilotdeck.yaml', mode: 0o000 }],
    options: { hostSettingSources: ['project'] },
  },
  {
    // existsSync follows the dangling link and reports false, so TS returns
    // undefined before readFileSync; ENOENT can only surface through a race.
    name: 'missing-symlink-target',
    symlinks: [{ path: 'project/.pilotdeck/pilotdeck.yaml', target: 'missing-target' }],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'yaml-tojson-alias-bomb',
    files: [
      { path: 'project/.pilotdeck/pilotdeck.yaml', content: aliasBombSource() },
    ],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'agent-not-record',
    files: [{ path: 'project/.pilotdeck/pilotdeck.yaml', content: 'agent: [1, 2]\n' }],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'source-agent-unknown-key',
    files: [{ path: 'project/.pilotdeck/pilotdeck.yaml', content: 'agent: {bogus: 1}\n' }],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'source-invalid-model',
    files: [{ path: 'project/.pilotdeck/pilotdeck.yaml', content: 'agent: {model: ""}\n' }],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'source-thinking-bad-budget',
    files: [
      {
        path: 'project/.pilotdeck/pilotdeck.yaml',
        content: 'agent: {thinking: {enabled: true, budgetTokens: -1}}\n',
      },
    ],
    options: { hostSettingSources: ['project'] },
  },
  {
    name: 'invalid-host-defaults',
    options: { hostDefaults: { agent: { maxContextTokens: -1 } } },
  },
  {
    name: 'invalid-host-sources',
    options: { hostSettingSources: ['bogus'] },
  },
  {
    name: 'empty-overlay-is-none',
    options: { settings: { agent: {} } },
  },
];

const settingsNormalizeCases: { name: string; value: any }[] = settingsValues
  .filter(([name]) => name !== 'undefined')
  .map(([name, value]) => ({ name: `normalize-${name}`, value }));
for (const [index, testCase] of settingsNormalizeCases.entries()) {
  settingsResolveCases.push({
    name: testCase.name,
    options: { settings: testCase.value },
  });
  void index;
}
const settingsResolve: any[] = [];
for (const [index, testCase] of settingsResolveCases.entries()) {
  const dir = `resolve-${index}-${testCase.name}`;
  const caseDir = join(scratch, dir);
  const projectRoot = join(caseDir, 'project');
  await mkdir(projectRoot, { recursive: true });
  for (const dir of testCase.dirs ?? []) {
    await mkdir(join(caseDir, dir), { recursive: true });
  }
  for (const file of testCase.files ?? []) {
    await writeCaseFile(caseDir, file.path, file.content);
  }
  for (const link of testCase.symlinks ?? []) {
    const absolute = join(caseDir, link.path);
    await mkdir(dirname(absolute), { recursive: true });
    await symlink(link.target, absolute);
  }
  for (const entry of testCase.modes ?? []) {
    await chmod(join(caseDir, entry.path), entry.mode);
  }
  const env: Record<string, string> = { HOME: caseDir, PILOT_HOME: join(caseDir, 'home') };
  for (const [key, value] of Object.entries(testCase.env ?? {})) {
    env[key] = value.split('<CASE>').join(caseDir);
  }
  settingsResolve.push({
    name: testCase.name,
    dir,
    files: encode(testCase.files ?? []),
    dirs: encode(testCase.dirs ?? []),
    modes: encode(testCase.modes ?? []),
    symlinks: encode(testCase.symlinks ?? []),
    projectRoot: encode(projectRoot),
    env: encode(env),
    hostDefaults: encode(testCase.options.hostDefaults),
    hostManagedSettings: encode(testCase.options.hostManagedSettings),
    hostSettingSources: encode(testCase.options.hostSettingSources),
    settings: encode(testCase.options.settings),
    settingSources: encode(testCase.options.settingSources),
    hostEnforcedSettings: encode(testCase.options.hostEnforcedSettings),
    result: capture(() =>
      resolvePilotSdkSessionSettings({
        projectRoot,
        env,
        hostDefaults: testCase.options.hostDefaults,
        hostManagedSettings: testCase.options.hostManagedSettings,
        hostSettingSources: testCase.options.hostSettingSources,
        settings: testCase.options.settings,
        settingSources: testCase.options.settingSources,
        hostEnforcedSettings: testCase.options.hostEnforcedSettings,
      }),
    ),
  });
}

// ── private validators ─────────────────────────────────────────────────────
const validators: any[] = [];
function validatorCase(
  kind: string,
  label: string,
  args: any[],
  run: () => any,
): void {
  validators.push({
    kind,
    name: `${kind}:${label}`,
    args: encode(args),
    result: capture(run),
  });
}
const managedPermissionValues: [string, any][] = [
  ['undefined', undefined],
  ['null', null],
  ['empty-object', {}],
  ['empty-arrays', { deny: [], ask: [] }],
  ['deny-not-array', { deny: 'x', ask: [] }],
  ['ask-not-array', { deny: [], ask: 1 }],
  ['non-string-entry', { deny: [1], ask: [] }],
  ['empty-entry', { deny: [''], ask: [] }],
  ['space-entry', { deny: [' '], ask: [] }],
  ['default-plan', { deny: [], ask: [], defaultMode: 'plan' }],
  ['default-auto', { deny: [], ask: [], defaultMode: 'auto' }],
  ['default-null', { deny: [], ask: [], defaultMode: null }],
  ['can-prompt-false', { deny: [], ask: [], canPrompt: false }],
  ['can-prompt-true', { deny: [], ask: [], canPrompt: true }],
  ['can-prompt-zero', { deny: [], ask: [], canPrompt: 0 }],
];
for (const [label, value] of managedPermissionValues) {
  validatorCase('managedPermissions', label, [value], () =>
    internals.validateSdkManagedPermissions(value),
  );
}
const toolSelectorValues: [string, any][] = [
  ['undefined', undefined],
  ['null', null],
  ['empty-object', {}],
  ['not-array', { allow: 'x', deny: [] }],
  ['non-string', { allow: [1], deny: [] }],
  ['empty', { allow: [''], deny: [] }],
  ['star', { allow: ['*'], deny: [] }],
  ['simple', { allow: ['read_file'], deny: [] }],
  ['dotted', { allow: ['a.b-c_d:e'], deny: [] }],
  ['trailing-star', { allow: ['tool*'], deny: [] }],
  ['leading-star', { allow: ['*tool'], deny: [] }],
  ['leading-dash', { allow: ['-tool'], deny: [] }],
  ['slash', { allow: ['a/b'], deny: [] }],
  ['space', { allow: ['a b'], deny: [] }],
  ['duplicate', { allow: ['a', ' a '], deny: [] }],
];
for (const [label, value] of toolSelectorValues) {
  validatorCase('managedTools', label, [value, 'tools'], () =>
    internals.validateSdkSelectorPolicy(value, 'tools'),
  );
}
const modelSelectorValues: [string, any][] = [
  ['undefined', undefined],
  ['star', { allow: ['*'], deny: [] }],
  ['provider-model', { allow: ['p1/m1'], deny: [] }],
  ['provider-star', { allow: ['p1/*'], deny: [] }],
  ['provider-only', { allow: ['p1'], deny: [] }],
  ['too-many', { allow: ['p1/m1/x'], deny: [] }],
  ['space', { allow: ['p1/ m1'], deny: [] }],
  ['empty-model', { allow: ['p1/'], deny: [] }],
  ['duplicate', { deny: ['p1/m1', ' p1/m1 '], allow: [] }],
  // ECMAScript \s includes U+FEFF/U+00A0/U+3000 and excludes U+0085; the Rust
  // regex Unicode class is the opposite on the first two.
  ['feff-interior', { allow: ['p\ufeffx/m'], deny: [] }],
  ['nel-interior', { allow: ['p\u0085x/m'], deny: [] }],
  ['nbsp-interior', { allow: ['p\u00a0x/m'], deny: [] }],
  ['ideographic-space-interior', { allow: ['p\u3000x/m'], deny: [] }],
];
for (const [label, value] of modelSelectorValues) {
  validatorCase('managedModels', label, [value, 'models'], () =>
    internals.validateSdkSelectorPolicy(value, 'models'),
  );
}
const taskBudgetCases: [string, any, number | undefined][] = [
  ['no-cap', { taskBudget: { total: 5 } }, undefined],
  ['cap-no-budget', {}, 3],
  ['cap-over', { taskBudget: { total: 9 } }, 3],
  ['cap-under', { taskBudget: { total: 1 } }, 3],
  ['cap-equal', { taskBudget: { total: 3 } }, 3],
  ['cap-project', { taskBudget: { total: 9, scope: 'project', projectRetentionMs: 100 } }, 3],
  ['cap-session', { taskBudget: { total: 9, scope: 'session', projectRetentionMs: 100 } }, 3],
  ['cap-null-budget', { taskBudget: null }, 3],
  ['cap-nan-total', { taskBudget: { total: NaN } }, 3],
  ['cap-nan-cap', { taskBudget: { total: 5 } }, NaN],
];
for (const [label, config, cap] of taskBudgetCases) {
  validatorCase('taskBudget', label, [config, cap], () => {
    const value = structuredClone(config);
    internals.applyOrganizationTaskBudgetCap(value, cap);
    return value;
  });
}
const sourcePolicyCases: [string, any, any][] = [
  ['no-policy', undefined, ['user']],
  ['no-sources', { allow: [], deny: [] }, undefined],
  ['empty-allow', { allow: [], deny: [] }, ['user']],
  ['allowed', { allow: ['user'], deny: [] }, ['user']],
  ['not-in-allow', { allow: ['project'], deny: [] }, ['user']],
  ['denied', { allow: [], deny: ['local'] }, ['local']],
  ['first-denied', { allow: [], deny: ['project'] }, ['user', 'project', 'local']],
];
for (const [label, policy, sources] of sourcePolicyCases) {
  const wrapped = policy === undefined ? undefined : { settingSources: policy };
  validatorCase('settingSources', label, [wrapped, sources], () =>
    internals.assertOrganizationSettingSourcesAllowed(wrapped, sources),
  );
}
const modelResolutionCases: [string, any, string][] = [
  ['by-key', 'm3', 'model'],
  ['by-provider-model', 'p3/m3', 'model'],
  ['by-definition-id', 'alias-1', 'model'],
  ['ambiguous', 'm1', 'model'],
  ['not-found', 'missing', 'model'],
  ['agent-field', 'missing', 'agentModel'],
  ['fallback-field', 'missing', 'fallbackModel'],
  ['subagent-field', 'missing', 'subagentModel'],
];
for (const [label, requested, field] of modelResolutionCases) {
  validatorCase('resolveModel', label, [requested, field], () =>
    internals.resolveSdkConfiguredModel(SNAPSHOT, requested, field),
  );
}
const managedModelCases: [string, any, any][] = [
  ['no-policy', undefined, { provider: 'p1', model: 'm1' }],
  ['allow-empty', { allow: [], deny: [] }, { provider: 'p1', model: 'm1' }],
  ['allow-match', { allow: ['p1/m1'], deny: [] }, { provider: 'p1', model: 'm1' }],
  ['allow-provider-match', { allow: ['p1/*'], deny: [] }, { provider: 'p1', model: 'm1' }],
  ['allow-no-match', { allow: ['p2/*'], deny: [] }, { provider: 'p1', model: 'm1' }],
  ['deny-match', { allow: [], deny: ['p1/m1'] }, { provider: 'p1', model: 'm1' }],
  ['deny-star', { allow: [], deny: ['*'] }, { provider: 'p1', model: 'm1' }],
  ['deny-provider', { allow: ['p1/m1'], deny: ['p1/*'] }, { provider: 'p1', model: 'm1' }],
];
for (const [label, policy, model] of managedModelCases) {
  validatorCase('managedModel', label, [policy, model], () =>
    internals.assertSdkManagedModelAllowed(policy, model),
  );
}
const orgModelCases: [string, any, string, string][] = [
  ['no-policy', undefined, 'p1', 'm1'],
  ['allow-empty', { allow: [], deny: [] }, 'p1', 'm1'],
  ['allow-match', { allow: ['p1/m1'], deny: [] }, 'p1', 'm1'],
  ['allow-provider-match', { allow: ['p1/*'], deny: [] }, 'p1', 'm1'],
  ['allow-no-match', { allow: ['p2/*'], deny: [] }, 'p1', 'm1'],
  ['deny-match', { allow: [], deny: ['p1/m1'] }, 'p1', 'm1'],
  ['deny-star', { allow: [], deny: ['*'] }, 'p1', 'm1'],
];
for (const [label, policy, provider, model] of orgModelCases) {
  const wrapped = policy === undefined ? undefined : { models: policy };
  validatorCase('orgModel', label, [wrapped, provider, model], () =>
    internals.isOrganizationModelAllowed(wrapped, provider, model),
  );
}
const providerPolicyCases: [string, any, string, any][] = [
  ['no-policy', undefined, 'p1', { url: 'https://api.p1.example/v1', credentialSource: 'environment' }],
  ['provider-allowed', { allow: ['p1'], deny: [], origins: { allow: [], deny: [] }, credentials: { allow: [], deny: [] } }, 'p1', { url: 'https://api.p1.example/v1', credentialSource: 'environment' }],
  ['provider-denied', { allow: [], deny: ['p1'], origins: { allow: [], deny: [] }, credentials: { allow: [], deny: [] } }, 'p1', { url: 'https://api.p1.example/v1', credentialSource: 'environment' }],
  ['provider-not-in-allow', { allow: ['p2'], deny: [], origins: { allow: [], deny: [] }, credentials: { allow: [], deny: [] } }, 'p1', { url: 'https://api.p1.example/v1', credentialSource: 'environment' }],
  ['origin-denied', { allow: [], deny: [], origins: { allow: [], deny: ['https://api.p1.example'] }, credentials: { allow: [], deny: [] } }, 'p1', { url: 'https://api.p1.example/v1', credentialSource: 'environment' }],
  ['origin-not-in-allow', { allow: [], deny: [], origins: { allow: ['https://other.example'], deny: [] }, credentials: { allow: [], deny: [] } }, 'p1', { url: 'https://api.p1.example/v1', credentialSource: 'environment' }],
  ['origin-unknown-with-allow', { allow: [], deny: [], origins: { allow: ['https://api.p1.example'], deny: [] }, credentials: { allow: [], deny: [] } }, 'p1', { credentialSource: 'environment' }],
  ['origin-unknown-denied-empty', { allow: [], deny: [], origins: { allow: [], deny: [''] }, credentials: { allow: [], deny: [] } }, 'p1', { credentialSource: 'environment' }],
  ['credential-denied', { allow: [], deny: [], origins: { allow: [], deny: [] }, credentials: { allow: [], deny: ['environment'] } }, 'p1', { url: 'https://api.p1.example/v1', credentialSource: 'environment' }],
  ['credential-not-in-allow', { allow: [], deny: [], origins: { allow: [], deny: [] }, credentials: { allow: ['literal'], deny: [] } }, 'p1', { url: 'https://api.p1.example/v1', credentialSource: 'environment' }],
  ['credential-unknown-with-allow', { allow: [], deny: [], origins: { allow: [], deny: [] }, credentials: { allow: ['literal'], deny: [] } }, 'p1', { url: 'https://api.p1.example/v1' }],
  ['credential-unknown-no-allow', { allow: [], deny: [], origins: { allow: [], deny: [] }, credentials: { allow: [], deny: [] } }, 'p1', { url: 'https://api.p1.example/v1' }],
];
for (const [label, policy, providerId, provider] of providerPolicyCases) {
  const wrapped = policy === undefined ? undefined : { providers: policy };
  validatorCase('providerRejection', label, [wrapped, providerId, provider], () =>
    internals.organizationProviderPolicyRejection(wrapped, providerId, provider),
  );
}
const defaultsCases: [string, any][] = [
  ['none', undefined],
  ['empty', {}],
  ['session-defaults', { settings: { sessionDefaults: { agent: { model: 'm' } } } }],
  ['managed-settings', { settings: { managedSessionSettings: {} } }],
  ['default-sources', { settings: { sessionDefaultSources: ['user'] } }],
  ['enforced', { settings: { enforcedSessionSettings: {} } }],
  ['budget-cap', { limits: { maxTaskBudgetUsd: 1 } }],
];
for (const [label, policy] of defaultsCases) {
  validatorCase('hasDefaults', label, [policy], () =>
    internals.hasOrganizationSdkSessionDefaults(policy),
  );
}
const skillCases: [string, any, any][] = [
  ['exact', ['a'], [{ name: 'a' }]],
  ['namespaced', ['skill'], [{ name: 'plugin:skill' }]],
  ['exact-beats-suffix', ['skill'], [{ name: 'skill' }, { name: 'plugin:skill' }]],
  ['not-found', ['missing'], [{ name: 'a' }]],
  ['ambiguous', ['skill'], [{ name: 'p1:skill' }, { name: 'p2:skill' }]],
  ['deduplicate', ['a', 'a'], [{ name: 'a' }]],
  ['empty-request', [], [{ name: 'a' }]],
];
for (const [label, requested, available] of skillCases) {
  validatorCase('skills', label, [requested, available], () =>
    internals.resolveSdkSessionSkills(requested, available),
  );
}

// ── registry operations ────────────────────────────────────────────────────
class FakeOverrides {
  map = new Map<string, any>();
  get(key: string) {
    return this.map.get(key);
  }
  set(key: string, value: any) {
    this.map.set(key, value);
  }
  delete(key: string) {
    this.map.delete(key);
  }
  entries() {
    return [...this.map.entries()];
  }
}
class FakePluginRuntime {
  names: string[];
  skills: { name: string }[];
  events: string[];
  constructor(names: string[], skills: { name: string }[], events: string[]) {
    this.names = names;
    this.skills = skills;
    this.events = events;
  }
  async refresh() {
    this.events.push('refresh');
    return [];
  }
  snapshot() {
    return this.names.map((name) => ({ name }));
  }
  createView(plugins: { name: string }[]) {
    this.events.push(`createView:${plugins.map((plugin) => plugin.name).join(',')}`);
    return { getAllSkills: () => this.skills.map((skill) => ({ ...skill })) };
  }
}
type RegistryCase = {
  name: string;
  organizationPolicy?: any;
  pluginNames?: string[];
  pluginSkills?: { name: string }[];
  files?: { path: string; content: string }[];
  steps: any[];
};
const registryCases: RegistryCase[] = [];
function pushRegistryCase(testCase: RegistryCase): void {
  registryCases.push(testCase);
}

pushRegistryCase({ name: 'set-minimal', steps: [{ op: 'set', config: {} }] });
pushRegistryCase({
  name: 'set-same-twice',
  steps: [
    { op: 'set', config: { fallbackModel: 'p3/m3' } },
    { op: 'set', config: { fallbackModel: 'p3/m3' } },
  ],
});
pushRegistryCase({
  name: 'set-host-defaults',
  organizationPolicy: { settings: { sessionDefaults: { agent: { model: 'm3' } } } },
  steps: [{ op: 'set', config: {} }],
});
for (const [label, managedPermissions] of [
  ['managed-permissions-not-array', { deny: 'x', ask: [] }],
  ['managed-permissions-empty', { deny: [''], ask: [] }],
  ['managed-permissions-mode', { deny: [], ask: [], defaultMode: 'auto' }],
] as [string, any][]) {
  pushRegistryCase({
    name: label,
    steps: [{ op: 'set', config: { managedPermissions } }],
  });
}
pushRegistryCase({
  name: 'managed-tools-invalid',
  steps: [{ op: 'set', config: { managedTools: { allow: ['-bad'], deny: [] } } }],
});
pushRegistryCase({
  name: 'managed-models-invalid',
  steps: [{ op: 'set', config: { managedModels: { allow: ['p1'], deny: [] } } }],
});
pushRegistryCase({
  name: 'setting-sources-invalid',
  steps: [{ op: 'set', config: { settingSources: [] } }],
});
pushRegistryCase({
  name: 'setting-sources-org-denied',
  organizationPolicy: { settingSources: { allow: ['user'], deny: ['local'] } },
  steps: [{ op: 'set', config: { settingSources: ['local'] } }],
});
pushRegistryCase({
  name: 'settings-invalid',
  steps: [{ op: 'set', config: { settings: { agent: { maxContextTokens: 0 } } } }],
});
pushRegistryCase({
  name: 'settings-model-invalid',
  steps: [{ op: 'set', config: { settings: { agent: { model: 'missing' } } } }],
});
pushRegistryCase({
  name: 'settings-model-org-denied',
  organizationPolicy: { models: { allow: ['p2/*'], deny: [] } },
  steps: [{ op: 'set', config: { settings: { agent: { model: 'm3' } } } }],
});
pushRegistryCase({
  name: 'provider-org-denied',
  organizationPolicy: {
    providers: {
      allow: ['p1'],
      deny: [],
      origins: { allow: [], deny: [] },
      credentials: { allow: [], deny: [] },
    },
  },
  steps: [{ op: 'set', config: { settings: { agent: { model: 'p3/m3' } } } }],
});
pushRegistryCase({
  name: 'managed-model-denied',
  steps: [
    {
      op: 'set',
      config: {
        settings: { agent: { model: 'm3' } },
        managedModels: { allow: ['p1/*'], deny: [] },
      },
    },
  ],
});
pushRegistryCase({
  name: 'fallback-model-invalid',
  steps: [{ op: 'set', config: { fallbackModel: 'missing' } }],
});
pushRegistryCase({
  name: 'fallback-model-org-denied',
  organizationPolicy: { models: { allow: ['p1/*'], deny: [] } },
  steps: [{ op: 'set', config: { fallbackModel: 'p3/m3' } }],
});
pushRegistryCase({
  name: 'settings-model-valid-and-fallback',
  steps: [
    {
      op: 'set',
      config: {
        settings: {
          agent: {
            model: 'p3/m3',
            fallbackModel: 'alias-1',
            subagents: { default: 'p3/m3' },
          },
        },
        fallbackModel: 'p1/m1',
      },
    },
  ],
});
pushRegistryCase({
  name: 'task-budget-cap',
  organizationPolicy: { limits: { maxTaskBudgetUsd: 3 } },
  steps: [
    {
      op: 'set',
      config: { taskBudget: { total: 9, scope: 'project', projectRetentionMs: 100 } },
    },
  ],
});
pushRegistryCase({
  name: 'task-budget-session',
  steps: [{ op: 'set', config: { taskBudget: { total: 9, scope: 'session' } } }],
});
// The real TS registry method (the generator no longer stubs it) fixes the
// project ledger on the first call, so the second call surfaces the exact
// conflict text; the abort case keeps the ledger write while dropping config.
pushRegistryCase({
  name: 'task-budget-total-conflict',
  steps: [
    { op: 'set', config: { taskBudget: { total: 5, scope: 'project' } } },
    { op: 'set', config: { taskBudget: { total: 6, scope: 'project' } } },
  ],
});
pushRegistryCase({
  name: 'task-budget-retention-conflict',
  steps: [
    {
      op: 'set',
      config: { taskBudget: { total: 5, scope: 'project', projectRetentionMs: 100 } },
    },
    {
      op: 'set',
      config: { taskBudget: { total: 5, scope: 'project', projectRetentionMs: 200 } },
    },
  ],
});
pushRegistryCase({
  name: 'task-budget-abort-after-configure',
  steps: [
    {
      op: 'set',
      config: { taskBudget: { total: 5, scope: 'project', projectRetentionMs: 100 } },
      abort: 'fourth',
    },
    { op: 'set', config: { taskBudget: { total: 6, scope: 'project' } } },
  ],
});
for (const [label, config, organizationPolicy] of [
  ['config-null', null, undefined],
  ['config-undefined', undefined, undefined],
  ['config-null-with-cap', null, { limits: { maxTaskBudgetUsd: 3 } }],
  ['config-number-with-cap', 5, { limits: { maxTaskBudgetUsd: 3 } }],
  ['config-array', [], undefined],
  ['config-string', 'x', undefined],
] as [string, any, any][]) {
  pushRegistryCase({
    name: label,
    organizationPolicy,
    steps: [{ op: 'set', config }],
  });
}
pushRegistryCase({
  name: 'plugins-set-and-clear',
  pluginNames: ['existing'],
  pluginSkills: [{ name: 'plugin:skill' }],
  steps: [
    { op: 'set', config: { plugins: [{ type: 'local', path: '<CASE>/plugin-a' }], skills: ['skill'] } },
    { op: 'set', config: { skills: ['plugin:skill'] } },
  ],
});
pushRegistryCase({
  name: 'plugin-missing',
  steps: [{ op: 'set', config: { plugins: [{ type: 'local', path: '<CASE>/missing' }] } }],
});
pushRegistryCase({
  name: 'plugin-not-directory',
  files: [{ path: 'not-a-dir', content: 'x' }],
  steps: [
    { op: 'set', config: { plugins: [{ type: 'local', path: '<CASE>/not-a-dir' }] } },
  ],
});
pushRegistryCase({
  name: 'plugin-duplicate',
  steps: [
    {
      op: 'set',
      config: {
        plugins: [
          { type: 'local', path: '<CASE>/plugin-a' },
          { type: 'local', path: '<CASE>/plugin-a/.' },
        ],
      },
    },
  ],
});
pushRegistryCase({
  name: 'plugin-name-conflict',
  pluginNames: ['existing'],
  steps: [
    { op: 'set', config: { plugins: [{ type: 'local', path: '<CASE>/plugin-conflict' }] } },
  ],
});
pushRegistryCase({
  name: 'plugin-load-failure',
  steps: [
    { op: 'set', config: { plugins: [{ type: 'local', path: '<CASE>/plugin-fail' }] } },
  ],
});
pushRegistryCase({
  name: 'skills-ambiguous',
  pluginSkills: [{ name: 'p1:skill' }, { name: 'p2:skill' }],
  steps: [{ op: 'set', config: { skills: ['skill'] } }],
});
pushRegistryCase({
  name: 'skills-not-found',
  steps: [{ op: 'set', config: { skills: ['missing'] } }],
});
pushRegistryCase({
  name: 'skills-all',
  steps: [{ op: 'set', config: { skills: 'all' } }],
});
pushRegistryCase({
  name: 'abort-before',
  steps: [{ op: 'set', config: {}, abort: 'first' }],
});
pushRegistryCase({
  name: 'abort-after-validation',
  steps: [
    { op: 'set', config: { settings: { agent: { model: 'm3' } } }, abort: 'second' },
  ],
});
pushRegistryCase({
  name: 'null-undefined-missing',
  steps: [
    { op: 'set', config: { settings: null, fallbackModel: null } },
    { op: 'set', config: { settings: undefined } },
    { op: 'set', config: {} },
  ],
});
pushRegistryCase({
  name: 'clear-state',
  steps: [
    { op: 'set', config: { taskBudget: { total: 1, scope: 'project' } } },
    { op: 'registerConfig' },
    { op: 'registerHook' },
    { op: 'clear' },
  ],
});
pushRegistryCase({
  name: 'ephemeral',
  steps: [{ op: 'set', config: { persistSession: false } }, { op: 'isEphemeral' }],
});
pushRegistryCase({
  name: 'assert-model-default',
  steps: [{ op: 'set', config: {} }, { op: 'assertModel' }],
});
pushRegistryCase({
  name: 'assert-model-session-setting',
  steps: [
    { op: 'set', config: { settings: { agent: { model: 'p3/m3' } } } },
    { op: 'assertModel' },
  ],
});
pushRegistryCase({
  name: 'assert-model-explicit',
  steps: [
    { op: 'set', config: { managedModels: { allow: ['p1/*'], deny: [] } } },
    { op: 'assertModel', model: { provider: 'p3', model: 'm3' } },
  ],
});
pushRegistryCase({
  name: 'assert-model-project-key',
  steps: [{ op: 'assertModel', projectKey: '<CASE>/project' }],
});
pushRegistryCase({
  name: 'flags-empty',
  steps: [{ op: 'flag', settings: {} }],
});
for (const [label, settings] of [
  ['effort-undefined', { effortLevel: undefined }],
  ['effort-null', { effortLevel: null }],
  ['effort-high', { effortLevel: 'high' }],
  ['permissions-null', { permissions: null }],
  ['permissions-default-null', { permissions: { defaultMode: null } }],
  ['permissions-default-undefined', { permissions: { defaultMode: undefined } }],
  ['permissions-default-plan', { permissions: { defaultMode: 'plan' } }],
  ['permissions-empty', { permissions: {} }],
  ['permissions-undefined', { permissions: undefined }],
  ['both', { effortLevel: 'low', permissions: { defaultMode: 'plan' } }],
] as [string, any][]) {
  pushRegistryCase({ name: `flag-${label}`, steps: [{ op: 'flag', settings }] });
}
pushRegistryCase({
  name: 'flag-permissions-removes-existing',
  steps: [
    { op: 'flag', settings: { permissions: { defaultMode: 'plan', keep: 1 } } },
    { op: 'flag', settings: { permissions: { defaultMode: null } } },
  ],
});
pushRegistryCase({ name: 'thinking-missing', steps: [{ op: 'thinking' }] });
pushRegistryCase({ name: 'thinking-null', steps: [{ op: 'thinking', value: null }] });
pushRegistryCase({
  name: 'thinking-object',
  steps: [
    { op: 'thinking', value: { enabled: true, mode: 'high', budgetTokens: 5 } },
    { op: 'thinking' },
  ],
});
pushRegistryCase({
  name: 'handlers-register-unregister',
  steps: [
    { op: 'registerConfig' },
    { op: 'registerHook' },
    { op: 'unregisterConfig', token: 0 },
    { op: 'unregisterHook', token: 0 },
  ],
});
pushRegistryCase({
  name: 'handlers-replace-and-stale-token',
  steps: [
    { op: 'registerConfig' },
    { op: 'registerConfig' },
    { op: 'unregisterConfig', token: 0 },
    { op: 'unregisterConfig', token: 1 },
  ],
});
pushRegistryCase({
  name: 'handlers-clear',
  steps: [{ op: 'registerConfig' }, { op: 'registerHook' }, { op: 'clear' }],
});

// Materialize per-case directories, files, and plugin paths before running.
for (const [index, testCase] of registryCases.entries()) {
  const dir = `registry-${index}-${testCase.name}`;
  const caseDir = join(scratch, dir);
  await mkdir(join(caseDir, 'project'), { recursive: true });
  await mkdir(join(caseDir, 'fallback'), { recursive: true });
  for (const name of ['plugin-a', 'plugin-conflict', 'plugin-fail']) {
    await mkdir(join(caseDir, name), { recursive: true });
  }
  for (const file of testCase.files ?? []) {
    await writeCaseFile(caseDir, file.path, file.content);
  }
  materialize(testCase, caseDir);
  (testCase as any).dir = dir;
}
function materialize(value: any, caseDir: string): void {
  if (Array.isArray(value)) {
    for (const item of value) materialize(item, caseDir);
    return;
  }
  if (!value || typeof value !== 'object') return;
  for (const [key, item] of Object.entries(value)) {
    if (typeof item === 'string' && item.includes('<CASE>')) {
      value[key] = item.split('<CASE>').join(caseDir);
    } else {
      materialize(item, caseDir);
    }
  }
}
loadPluginFromPath = async (path: string, source: string) => {
  const name = basename(path);
  if (name === 'plugin-fail') throw new Error('boom');
  if (name === 'plugin-conflict') return { name: 'existing', source, path, skills: [] };
  return { name, source, path, skills: [] };
};

function registryState(registry: any) {
  return {
    configs: [...registry.sdkSessionConfigs.entries()],
    plugins: [...registry.sdkSessionPlugins.entries()].map(
      ([key, plugins]: [string, any[]]) => [key, plugins.map((plugin) => plugin.name)],
    ),
    projects: [...registry.sdkSessionProjects.entries()],
    thinking: [...registry.sdkThinkingOverrides.entries()],
    overrides: registry._sessionOverrides.entries(),
    handlers: {
      config: [...registry.sdkConfigChangeHandlers.keys()],
      hook: [...registry.sessionHookHandlers.keys()],
    },
  };
}
function makeRegistry(options: {
  fallbackRoot: string;
  caseDir: string;
  organizationPolicy: any;
  overrides: FakeOverrides;
  runtime: any;
}) {
  const registry = Object.create(Registry.prototype);
  registry.options = {
    fallbackProjectRoot: options.fallbackRoot,
    pilotHome: join(options.caseDir, 'home'),
    env: {
      HOME: options.caseDir,
      PILOT_HOME: join(options.caseDir, 'home'),
    },
    organizationPolicy: options.organizationPolicy,
    now: () => new Date(0),
  };
  registry.sdkSessionConfigs = new Map();
  registry.sdkConfigChangeHandlers = new Map();
  registry.sessionHookHandlers = new Map();
  registry.sdkSessionPlugins = new Map();
  registry.sdkSessionMcpServers = new Map();
  registry.sdkMcpPermissionOverrides = new Map();
  registry.sdkMcpPermissionRules = new Map();
  registry.sdkThinkingOverrides = new Map();
  registry.sdkSessionProjects = new Map();
  registry.sdkTaskBudgetLedgers = new Map();
  registry.taskBudgetLedgerRecordCount = 0;
  registry.taskBudgetLedgerCompactAfterRecords = 512;
  registry._sessionOverrides = options.overrides;
  registry.resolve = (key?: string) => {
    // The oracle's runtime is the only generation; both the fallback root and
    // the project key resolve to it, matching the TS cache contract.
    void key;
    return options.runtime;
  };
  return registry;
}
function sequencedSignal(kind: string) {
  let calls = 0;
  return {
    throwIfAborted() {
      calls += 1;
      if ((kind === 'first' && calls >= 1) || (kind === 'second' && calls >= 2) || (kind === 'third' && calls >= 3) || (kind === 'fourth' && calls >= 4)) {
        throw new Error(`aborted-${kind}`);
      }
    },
  };
}

const registrySection: any[] = [];
for (const testCase of registryCases) {
  const caseDir = join(scratch, (testCase as any).dir);
  const projectRoot = join(caseDir, 'project');
  const fallbackRoot = join(caseDir, 'fallback');
  const events: string[] = [];
  const pluginRuntime = new FakePluginRuntime(
    testCase.pluginNames ?? [],
    testCase.pluginSkills ?? [],
    events,
  );
  const runtime = { projectRoot, snapshot: SNAPSHOT, pluginRuntime };
  const overrides = new FakeOverrides();
  const registry = makeRegistry({
    fallbackRoot,
    caseDir,
    organizationPolicy: testCase.organizationPolicy,
    overrides,
    runtime,
  });
  const configTokens: any[] = [];
  const hookTokens: any[] = [];
  const steps: any[] = [];
  for (const step of testCase.steps) {
    let result: any;
    let input: any;
    if (step.op === 'set') {
      const config = structuredClone(step.config);
      input = {
        config,
        projectKey: step.projectKey,
        abort: step.abort,
        sessionKey: step.sessionKey,
      };
      result = await captureAsync(() =>
        registry.setSdkSessionConfig(
          step.sessionKey ?? 's',
          config,
          step.projectKey,
          step.abort === undefined ? undefined : sequencedSignal(step.abort),
        ),
      );
    } else if (step.op === 'assertModel') {
      input = {
        model: step.model,
        projectKey: step.projectKey,
        sessionKey: step.sessionKey,
      };
      result = capture(() =>
        registry.assertSdkModelAllowed(step.sessionKey ?? 's', step.model, step.projectKey),
      );
    } else if (step.op === 'thinking') {
      input = { value: step.value, sessionKey: step.sessionKey };
      result = capture(() =>
        registry.setSessionThinkingForSdk(step.sessionKey ?? 's', step.value),
      );
    } else if (step.op === 'flag') {
      input = { settings: step.settings, sessionKey: step.sessionKey };
      result = await captureAsync(() =>
        registry.applyFlagSettingsForSdk({
          sessionKey: step.sessionKey ?? 's',
          settings: step.settings,
        }),
      );
    } else if (step.op === 'clear') {
      input = { sessionKey: step.sessionKey };
      result = capture(() => registry.clearSdkSessionState(step.sessionKey ?? 's'));
    } else if (step.op === 'isEphemeral') {
      input = { sessionKey: step.sessionKey };
      result = capture(() => registry.isEphemeralSession(step.sessionKey ?? 's'));
    } else if (step.op === 'registerConfig') {
      input = { sessionKey: step.sessionKey };
      const token = registry.registerSdkConfigChangeHandler(step.sessionKey ?? 's', () => {});
      configTokens.push(token);
      result = { ok: true, value: true };
    } else if (step.op === 'registerHook') {
      input = { sessionKey: step.sessionKey };
      const token = registry.registerSessionHookHandler(step.sessionKey ?? 's', () => {});
      hookTokens.push(token);
      result = { ok: true, value: true };
    } else if (step.op === 'unregisterConfig') {
      input = { token: step.token, sessionKey: step.sessionKey };
      result = capture(() => configTokens[step.token]?.());
    } else if (step.op === 'unregisterHook') {
      input = { token: step.token, sessionKey: step.sessionKey };
      result = capture(() => hookTokens[step.token]?.());
    } else {
      throw new Error(`unknown step ${step.op}`);
    }
    steps.push({
      op: step.op,
      input: encode(input),
      result,
      state: encode(registryState(registry)),
    });
  }
  registrySection.push({
    name: testCase.name,
    dir: (testCase as any).dir,
    files: encode(testCase.files ?? []),
    organizationPolicy: encode(testCase.organizationPolicy),
    pluginNames: encode(testCase.pluginNames ?? []),
    pluginSkills: encode(testCase.pluginSkills ?? []),
    steps,
  });
}

const oracle = {
  sourceCommit,
  sourceSha256,
  snapshot: encode(SNAPSHOT),
  count:
    settingsSources.length +
    settingsValidate.length +
    settingsEntryOrder.length +
    settingsResolve.length +
    validators.length +
    registrySection.length,
  settingsSources: { count: settingsSources.length, cases: settingsSources },
  settingsValidate: { count: settingsValidate.length, cases: settingsValidate },
  settingsEntryOrder: { count: settingsEntryOrder.length, cases: settingsEntryOrder },
  settingsResolve: { count: settingsResolve.length, cases: settingsResolve },
  validators: { count: validators.length, cases: validators },
  registry: { count: registrySection.length, cases: registrySection },
};

await mkdir(outputDir, { recursive: true });
await writeFile(join(outputDir, 'oracle.json'), `${JSON.stringify(oracle, null, 2)}\n`, {
  flag: 'wx',
});
console.log(
  `SDK_SESSION_ORACLE_PASS sources=${settingsSources.length} validate=${settingsValidate.length} resolve=${settingsResolve.length} validators=${validators.length} registry=${registrySection.length}`,
);
await rm(scratch, { recursive: true, force: true });
