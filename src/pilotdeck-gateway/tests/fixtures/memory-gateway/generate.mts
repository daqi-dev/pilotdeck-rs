import { baselineSourceHashes } from '../_shared/baseline.mts';
// Run from the repository root with the fixed baseline's tsx loader:
//   PDRS_BASELINE=/abs/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/memory-gateway/generate.mts \
//     src/pilotdeck-gateway/tests/fixtures/memory-gateway
//
// Expectations are produced by the real fixed baseline: the real
// `InProcessGateway` class runs the real wrapper validation
// (InProcessGateway.ts:2794-2808) and delegates to the unmodified
// memoryList/memoryWipe method bodies extracted from createLocalGateway.ts
// with an injected fake project registry and provider. No baseline file is
// copied or changed. Every input is JSON-representable so the committed
// oracle replays through the native wire adapter (boxed strings only exist
// in the direct-API domain and are documented in the README instead).
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture output directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);

// Shared provenance guard: pinned HEAD, clean tracked tree, and every tracked
// src byte compared against the Git blob at the pinned commit.
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
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);

const sourceFiles = [
  'src/cli/createLocalGateway.ts',
  'src/gateway/client/InProcessGateway.ts',
  'src/gateway/dialog/errors.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sourceFiles);

const { InProcessGateway } = await import(
  pathToFileURL(join(baseline, 'src/gateway/client/InProcessGateway.ts')).href
);
const { DialogGatewayError } = await import(
  pathToFileURL(join(baseline, 'src/gateway/dialog/errors.ts')).href
);

const PROJECT = '/pdrs-fixture/project';
const FALLBACK = '/pdrs-fixture/fallback';

function errorJson(error: any) {
  return {
    name: error?.name ?? 'Error',
    code: error?.code ?? null,
    message: error?.message ?? String(error),
  };
}

// The management methods are plain JS once extracted; execute the exact source
// bytes from createLocalGateway.ts with fake dialog/registry seams.
const createLocalGatewaySource = await readFile(join(baseline, 'src/cli/createLocalGateway.ts'), 'utf8');
const methodStart = createLocalGatewaySource.indexOf('    async memoryList(input) {');
const methodEnd = createLocalGatewaySource.indexOf('    async managerSessions(input) {', methodStart);
assert.ok(methodStart > 0, 'memoryList source was not found');
assert.ok(methodEnd > methodStart, 'memoryWipe source was not found');
const methodSource = createLocalGatewaySource.slice(methodStart, methodEnd);
assert.equal((methodSource.match(/async memory(List|Wipe)\(/g) ?? []).length, 2, 'expected exactly two methods');

const ITEMS = [
  { id: 'a', sourceSessionKey: 's1', title: 'one' },
  { id: 'b', title: 'no-session' },
  { id: 'c', sourceSessionKey: 's2' },
  { id: 'd', sourceSessionKey: 1 },
  { id: 'e', sourceSessionKey: 's1' },
  { id: 'f', sourceSessionKey: '' },
  { id: 'g', sourceSessionKey: 'a\ud800b' },
];
function cloneItems() {
  return JSON.parse(JSON.stringify(ITEMS));
}

// One scenario shares a single call recorder and one real InProcessGateway
// instance per capability row. `options.capability` decides whether the
// wrapper's `this.options.memoryList` / `memoryWipe` seam is present.
const gatewayCases: any[] = [];
async function gatewayCase(
  name: string,
  options: {
    capability: 'available' | 'absent';
    management: 'available' | 'unavailable';
    items?: any[];
    knownProjects?: string[];
  },
  inputs: Array<{
    op: 'memoryList' | 'memoryWipe';
    input?: any;
    params?: 'object' | 'undefined' | 'null';
  }>,
) {
  const items = options.items ?? cloneItems();
  const sharedCalls: any[] = [];
  const management = options.management === 'unavailable' ? undefined : {
    list(input: any) {
      sharedCalls.push({ op: 'list', limit: input?.limit });
      return JSON.parse(JSON.stringify(items));
    },
    clear(scope: any) {
      sharedCalls.push({ op: 'clear', scope });
    },
    clearSession(sessionKey: any) {
      sharedCalls.push({ op: 'clearSession', sessionKey });
    },
  };
  const dialog = {
    projects: {
      resolveProjectKey: async (key: string) => {
        sharedCalls.push({ op: 'resolveProjectKey', key });
        if (options.knownProjects) {
          // JS string equality is unit-exact: "\ud800" never matches "�".
          if (!options.knownProjects.includes(key)) {
            throw new DialogGatewayError('PROJECT_NOT_FOUND', `Unknown projectKey: ${key}`);
          }
          return key;
        }
        return `resolved:${key}`;
      },
    },
  };
  const registry = {
    resolve: (key: string) => {
      sharedCalls.push({ op: 'resolve', key });
      return { memoryManagement: management };
    },
  };
  const boundApi = new Function(
    'dialog',
    'registry',
    'fallbackProjectRoot',
    'DialogGatewayError',
    `return { ${methodSource} };`,
  )(dialog, registry, FALLBACK, DialogGatewayError);
  assert.equal(typeof boundApi.memoryList, 'function');
  assert.equal(typeof boundApi.memoryWipe, 'function');
  const gateway = new InProcessGateway({} as any, options.capability === 'available'
    ? {
        memoryList: (input: any) => boundApi.memoryList(input),
        memoryWipe: (input: any) => boundApi.memoryWipe(input),
      }
    : {});
  assert.equal(typeof gateway.memoryList, 'function');
  assert.equal(typeof gateway.memoryWipe, 'function');

  const steps: any[] = [];
  for (const { op, input, params } of inputs) {
    const before = sharedCalls.length;
    const omitted = params !== undefined && input === undefined;
    let step: any = {
      op,
      params: params ?? 'object',
      input: omitted ? {} : input,
    };
    try {
      const argument = params === 'undefined' ? undefined : params === 'null' ? null : input;
      const result = op === 'memoryList'
        ? await gateway.memoryList(argument as never)
        : await gateway.memoryWipe(argument as never);
      step.resultJson = JSON.stringify(result);
    } catch (error) {
      step.error = errorJson(error);
    }
    step.calls = sharedCalls.slice(before);
    steps.push(step);
  }
  gatewayCases.push({
    name,
    capability: options.capability,
    management: options.management,
    items,
    ...(options.knownProjects ? { knownProjects: options.knownProjects } : {}),
    steps,
  });
}

// Wrapper parameter validation: every PROJECT_NOT_FOUND cell.
await gatewayCase('list-project-key-validation', { capability: 'available', management: 'available' }, [
  { op: 'memoryList', input: {} },
  { op: 'memoryList', input: { projectKey: '' } },
  { op: 'memoryList', input: { projectKey: '   ' } },
  { op: 'memoryList', input: { projectKey: 42 } },
  { op: 'memoryList', input: { projectKey: true } },
  { op: 'memoryList', input: { projectKey: null } },
  { op: 'memoryList', input: { projectKey: ['p'] } },
  { op: 'memoryList', params: 'undefined' },
  { op: 'memoryList', params: 'null' },
  { op: 'memoryList', input: 42 },
  { op: 'memoryList', input: 'str' },
]);
// Wrapper parameter validation: INVALID_SESSION_KEY for every non-string or
// blank sessionKey; `undefined` (missing key) stays legal.
await gatewayCase('list-session-key-validation', { capability: 'available', management: 'available' }, [
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: '' } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: '   ' } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: null } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: 42 } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: false } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: ['s1'] } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: {} } },
  { op: 'memoryList', input: { projectKey: PROJECT } },
]);
// Happy path: filter by exact sessionKey units, including the lone surrogate.
await gatewayCase('list-filter', { capability: 'available', management: 'available' }, [
  { op: 'memoryList', input: { projectKey: PROJECT } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: 's1' } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: 'missing' } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: 'a\ud800b' } },
  { op: 'memoryList', input: { projectKey: PROJECT, sessionKey: 'a�b' } },
]);
// Wipe scope discrimination: `=== "session"` only; everything else (unknown,
// absent, null, wrong case, numeric) wipes the current project.
await gatewayCase('wipe-scope-discrimination', { capability: 'available', management: 'available' }, [
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: 's1' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: '  s1  ' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'session' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'project' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'global' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'SESSION', sessionKey: 's1' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'session ', sessionKey: 's1' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: null } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 42 } },
  { op: 'memoryWipe', input: { projectKey: PROJECT } },
  { op: 'memoryWipe', input: {} },
  { op: 'memoryWipe', input: { projectKey: '   ', scope: 'session', sessionKey: 's1' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: '   ' } },
  { op: 'memoryWipe', params: 'undefined' },
]);
// Wipe sessionKey validation follows projectKey but precedes the scope check.
await gatewayCase('wipe-validation-order', { capability: 'available', management: 'available' }, [
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: 42 } },
  { op: 'memoryWipe', input: { projectKey: null, scope: 'session', sessionKey: 's1' } },
]);
// Capability: the wrapper rejects after the parameter validation (and after
// the wipe scope check), with the same message the inner projection uses when
// memoryManagement is absent.
await gatewayCase('capability-absent', { capability: 'absent', management: 'available' }, [
  { op: 'memoryList', input: { projectKey: PROJECT } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'project' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'session' } },
  { op: 'memoryList', input: {} },
]);
// Inner capability: the wrapper passes a valid request through, and the inner
// memoryManagement lookup raises CAPABILITY_UNAVAILABLE after resolution.
await gatewayCase('management-unavailable', { capability: 'available', management: 'unavailable' }, [
  { op: 'memoryList', input: { projectKey: PROJECT } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: 's1' } },
  { op: 'memoryWipe', input: { projectKey: PROJECT, scope: 'project' } },
]);
// Project keys are JS strings compared by UTF-16 code unit: a lone surrogate
// and the literal U+FFFD project are different identities.
const FFFD_PROJECT = `${PROJECT}-�`;
const SURROGATE_PROJECT = `${PROJECT}-\ud800`;
await gatewayCase(
  'project-key-lone-surrogate',
  { capability: 'available', management: 'available', knownProjects: [PROJECT, FFFD_PROJECT] },
  [
    { op: 'memoryWipe', input: { projectKey: SURROGATE_PROJECT, scope: 'session', sessionKey: 's1' } },
    { op: 'memoryList', input: { projectKey: SURROGATE_PROJECT } },
    { op: 'memoryList', input: { projectKey: FFFD_PROJECT, sessionKey: 's1' } },
  ],
);

const oracle = {
  sourceCommit,
  sourceSha256,
  gatewayCases,
};
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(`wrote ${gatewayCases.length} gateway cases and ${gatewayCases.reduce((sum, row) => sum + row.steps.length, 0)} steps`);
