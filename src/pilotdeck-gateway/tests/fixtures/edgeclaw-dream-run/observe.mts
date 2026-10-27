// Calls the fixed TS DreamRewriteRunner.run directly on a real
// MemoryRepository. Every expected value in oracle.json is produced here;
// the Rust side only replays the same seeded repository and script.
import {mkdirSync, readdirSync, readFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
import {join, resolve} from 'node:path';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';
import {removeOwned} from '../_shared/generator_guard.mjs';
import {createRunScratch, removeRunScratch} from '../_shared/run_scratch.mjs';

export const NOW = '2026-10-22T00:00:00.000Z';
const ROOT = 'src/context/memory/edgeclaw-memory-core/src';
// `run()` creates the trace with `Math.random().toString(36).slice(2, 10)`,
// so index 0 seeds the trace id and every mutation consumes the next value.
const RANDOMS = [0.25, 0.5, 0.75, 0.125, 0.375, 0.625, 0.875, 0.0625, 0.1875, 0.3125,
  0.4375, 0.5625, 0.6875, 0.8125, 0.9375, 0.03125, 0.09375, 0.15625, 0.21875, 0.28125];
const RUN_SUFFIX = (RANDOMS[0]!).toString(36).slice(2, 10);
// Direct sub-flow probes create the trace themselves; their mutation sequence
// starts at index 0 and the suffix is an independent frozen token.
const TRACE_SUFFIX = (0.5).toString(36).slice(2, 10);

function tree(root: string, dir = ''): any[] {
  return readdirSync(join(root, dir), {withFileTypes: true})
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
    .flatMap((entry) => {
      const path = dir ? `${dir}/${entry.name}` : entry.name;
      return entry.isDirectory()
        ? tree(root, path)
        : [{relativePath: path, content: readFileSync(join(root, path), 'utf8')}];
    });
}

function jsonSafe(value: any): any {
  return value === undefined ? null : JSON.parse(JSON.stringify(value));
}

class ScriptedExtractor {
  script: any[];
  index = 0;
  calls: any[] = [];
  constructor(script: any[]) {
    this.script = script;
  }
  next(method: string, input: any) {
    const step = this.script[this.index++];
    if (!step || step.method !== method) {
      throw new Error(`fixture script expected ${step?.method ?? '<end>'} but got ${method}`);
    }
    this.calls.push({method, input: jsonSafe(input)});
    if (step.debug !== undefined) input.debugTrace?.(step.debug);
    if (step.throws !== undefined) {
      const error = new Error(step.throws);
      if (step.throwsName !== undefined) error.name = step.throwsName;
      throw error;
    }
    return step.result === undefined ? undefined : jsonSafe(step.result);
  }
  async planDreamClusters(input: any) {
    return this.next('planDreamClusters', input);
  }
  async refineDreamCluster(input: any) {
    return this.next('refineDreamCluster', input);
  }
  async planGeneralProjectMetaMerges(input: any) {
    return this.next('planGeneralProjectMetaMerges', input);
  }
  async reviewDreamProjectMeta(input: any) {
    return this.next('reviewDreamProjectMeta', input);
  }
  async rewriteUserProfile(input: any) {
    return this.next('rewriteUserProfile', input);
  }
}

async function load(baseline: string) {
  const at = (relative: string) => import(pathToFileURL(resolve(baseline, ROOT, relative)).href);
  const {DreamRewriteRunner} = await at('core/review/dream-review.ts');
  const {MemoryRepository} = await at('core/storage/sqlite.ts');
  const {hashText} = await at('core/utils/id.ts');
  return {DreamRewriteRunner, MemoryRepository, hashText};
}

function makeRepository(MemoryRepository: any, mode: string, base: string, home: string, name: string) {
  const globalRootDir = join(base, 'global');
  if (mode === 'general') {
    const general = join(home, '.pilotdeck');
    return new MemoryRepository(join(general, 'workspaces', name, 'control.sqlite'), {
      memoryDir: join(general, 'workspaces', name, 'memory'),
      globalRootDir,
      workspaceDir: general,
    });
  }
  const dir = join(base, 'single');
  return new MemoryRepository(join(dir, 'control.sqlite'), {
    memoryDir: join(dir, 'memory'),
    globalRootDir,
    workspaceDir: dir,
  });
}

function candidate(type: string, name: string, description: string, body: string, projectId?: string) {
  return {
    type,
    scope: type === 'user' ? 'global' : 'project',
    ...(projectId ? {projectId} : {}),
    name,
    description,
    body,
  };
}

function seed(repository: any, setup: any) {
  const store = repository.getFileMemoryStore();
  for (const meta of setup.metas ?? []) store.upsertProjectMeta(meta);
  for (const item of setup.candidates ?? []) store.upsertCandidate(item);
  const global = repository.getGlobalUserStore();
  for (const note of setup.userNotes ?? []) global.upsertCandidate(note);
  if (setup.userProfile) global.upsertUserProfile(setup.userProfile);
  if (setup.ensureMeta) store.ensureProjectMeta({});
}

function selectionInput(selection: any) {
  return {
    kinds: selection.kinds ?? ['project', 'feedback'],
    scope: selection.scope ?? 'project',
    includeDeprecated: selection.includeDeprecated ?? false,
    limit: selection.limit ?? 5000,
    ...(selection.projectId ? {projectId: selection.projectId} : {}),
  };
}

function resolveTokens(value: any, replacements: Record<string, string>): any {
  if (typeof value === 'string' && value.startsWith('$')) {
    const replacement = replacements[value.slice(1)];
    if (replacement === undefined) throw new Error(`fixture token ${value} is unresolved`);
    return replacement;
  }
  if (Array.isArray(value)) return value.map((item) => resolveTokens(item, replacements));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, resolveTokens(item, replacements)]));
  }
  return value;
}

function clean(value: any, root: string): any {
  if (typeof value === 'string') return value.split(root).join('$ROOT');
  if (Array.isArray(value)) return value.map((item) => clean(item, root));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, clean(item, root)]));
  }
  return value;
}

function traceSeed(hashText: any, suffix: string) {
  return {
    dreamTraceId: `dream_trace_${hashText(`manual:${NOW}:${suffix}`)}`,
    trigger: 'manual',
    startedAt: NOW,
    status: 'running',
    isNoOp: false,
    displayStatus: 'Running',
    snapshotSummary: {
      projectMetaPresent: false,
      projectFileCount: 0,
      feedbackFileCount: 0,
      hasUserProfile: false,
    },
    steps: [],
    mutations: [],
    outcome: {
      rewrittenProjects: 0,
      deletedProjects: 0,
      deletedFiles: 0,
      profileUpdated: false,
      summary: '',
    },
  };
}

async function runCase(baseline: string, scratch: string, home: string, spec: any) {
  const {DreamRewriteRunner, MemoryRepository, hashText} = await load(baseline);
  const base = join(scratch, 'cases', spec.name);
  mkdirSync(base, {recursive: true});
  const repository = makeRepository(MemoryRepository, spec.mode, base, home, spec.name);
  const savedRandom = Math.random;
  try {
    seed(repository, spec.setup ?? {});
    const store = repository.getFileMemoryStore();
    const global = repository.getGlobalUserStore();
    const logs: string[] = [];
    const logger = {
      info: (message: any) => logs.push(`info: ${String(message)}`),
      warn: (message: any) => logs.push(String(message)),
      error: (message: any) => logs.push(`error: ${String(message)}`),
    };
    let randomIndex = 0;
    Math.random = () => RANDOMS[randomIndex++] ?? 0.5;

    const all = repository.listMemoryEntries({kinds: ['project', 'feedback'], scope: 'project', includeDeprecated: false, limit: 5000});
    const tokens: Record<string, string> = {};
    all.filter((entry: any) => entry.type === 'project')
      .forEach((entry: any, index: number) => { tokens[`PROJECT${index}`] = entry.relativePath; });
    all.filter((entry: any) => entry.type === 'feedback')
      .forEach((entry: any, index: number) => { tokens[`FEEDBACK${index}`] = entry.relativePath; });

    const expected: any = {};
    if (spec.method === 'run') {
      const extractor = new ScriptedExtractor(resolveTokens(spec.script, tokens));
      const runner = new DreamRewriteRunner(repository, extractor, {logger});
      const result = await runner.run(spec.trigger ?? 'manual');
      expected.result = jsonSafe(result);
      expected.trace = jsonSafe(result.trace);
      expected.calls = jsonSafe(extractor.calls);
    } else if (spec.method === 'runCategoryDreamError') {
      const selection = spec.selection ?? {type: 'project'};
      const entries = repository.listMemoryEntries(selectionInput(selection))
        .filter((entry: any) => entry.type === selection.type);
      const records = entries.length > 0
        ? repository.getMemoryRecordsByIds(entries.map((entry: any) => entry.relativePath), 5000)
        : [];
      const visible = records.slice(0, Math.max(0, records.length - (spec.dropRecords ?? 0)));
      const extractor = new ScriptedExtractor(resolveTokens(spec.script, tokens));
      const runner = new DreamRewriteRunner(repository, extractor, {logger});
      const trace = traceSeed(hashText, spec.randomSuffix ?? TRACE_SUFFIX);
      expected.error = null;
      try {
        await runner['runCategoryDream']({
          trace,
          kind: selection.type,
          entries,
          recordsByPath: new Map(visible.map((record: any) => [record.relativePath, record])),
        });
      } catch (error) {
        expected.error = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
      }
      expected.trace = jsonSafe(trace);
      expected.calls = jsonSafe(extractor.calls);
    } else {
      throw new Error(`unknown fixture method ${spec.method}`);
    }

    expected.logs = jsonSafe(logs);
    expected.tree = {
      workspace: tree(store.getRootDir()),
      global: tree(global.getRootDir()),
    };
    return JSON.parse(JSON.stringify(clean({
      name: spec.name,
      method: spec.method,
      mode: spec.mode,
      trigger: spec.trigger ?? 'manual',
      setup: spec.setup ?? {},
      selection: spec.selection,
      dropRecords: spec.dropRecords,
      randomSuffix: spec.method === 'run' ? RUN_SUFFIX : (spec.randomSuffix ?? TRACE_SUFFIX),
      script: resolveTokens(spec.script, tokens),
      startedAt: NOW,
      randoms: RANDOMS,
      expected,
    }, scratch)));
  } finally {
    Math.random = savedRandom;
    repository.close();
    await removeOwned(scratch, base, {recursive: true});
  }
}

export async function collect(baseline: string) {
  const scratch = createRunScratch(
    process.env.PDRS_RUNNER_SCRATCH || tmpdir(),
    'pdrs-dream-run-observe-',
  );
  try {
    freezeClock();
    return await collectCases(baseline, scratch.owned);
  } finally {
    await removeRunScratch(scratch);
  }
}

async function collectCases(baseline: string, scratch: string) {
  // `GENERAL_WORKSPACE_DIR` is a module-load constant, so HOME must be the
  // isolated scratch home before the first baseline import.
  const home = join(scratch, 'home');
  mkdirSync(home, {recursive: true});
  process.env.HOME = home;
  const cases: any[] = [];
  const add = async (spec: any) => cases.push(await runCase(baseline, scratch, home, spec));

  const projectA = candidate('project', 'Alpha Plan', 'alpha plan', '## Goal\nShip alpha.\n');
  const projectB = candidate('project', 'Alpha Notes', 'alpha notes', '## Goal\nShip the alpha release.\n');
  const feedbackA = candidate('feedback', 'Use tabs', 'formatting rule', 'Rule: use tabs.\n');
  const feedbackB = candidate('feedback', 'Use two-space tabs', 'formatting rule duplicate', 'Rule: indent with tabs.\n');
  const metaA = {projectId: 'p1', projectName: 'Alpha', description: 'alpha project', status: 'active'};
  const metaB = {projectId: 'p2', projectName: 'Beta', description: 'beta project', status: 'active'};
  const note = candidate('user', 'Prefers dark mode', 'ui preference', 'Prefers dark mode.\n');
  const profile = candidate('user', 'User Profile', 'user profile', '## 身份背景\n- likes rust\n');

  await add({
    name: 'run-empty-single',
    method: 'run',
    mode: 'single',
    setup: {},
    script: [],
  });

  await add({
    name: 'run-empty-general',
    method: 'run',
    mode: 'general',
    setup: {},
    script: [],
  });

  await add({
    name: 'run-single-full',
    method: 'run',
    mode: 'single',
    setup: {
      candidates: [projectA, projectB, feedbackA, feedbackB],
      userNotes: [note],
      userProfile: profile,
    },
    script: [
      {
        method: 'planDreamClusters',
        debug: {request: 'project-plan'},
        result: {
          summary: 'Found one duplicate project cluster.',
          clusters: [{memberRelativePaths: ['$PROJECT0', '$PROJECT1'], reason: 'duplicate content'}],
        },
      },
      {
        method: 'refineDreamCluster',
        debug: {request: 'project-refine'},
        result: {
          summary: 'Merged the duplicate project memories.',
          file: {name: 'Alpha Merged', description: 'merged alpha', markdown: '# Alpha\n\nMerged body.'},
        },
      },
      {
        method: 'planDreamClusters',
        result: {
          summary: 'Found one duplicate feedback cluster.',
          clusters: [{memberRelativePaths: ['$FEEDBACK0', '$FEEDBACK1'], reason: 'same rule'}],
        },
      },
      {
        method: 'refineDreamCluster',
        result: {
          summary: 'Merged the formatting feedback.',
          file: {name: 'Use tabs', description: 'formatting rule', markdown: 'Rule: use tabs.'},
        },
      },
      {
        method: 'reviewDreamProjectMeta',
        debug: {request: 'meta-review'},
        result: {
          shouldUpdate: true,
          reason: 'Project metadata refreshed.',
          projectMeta: {projectName: 'Alpha Renamed', description: 'alpha plan renamed', status: 'active'},
        },
      },
      {
        method: 'rewriteUserProfile',
        debug: {request: 'profile-rewrite'},
        result: {
          type: 'user',
          scope: 'global',
          name: 'User Profile',
          description: 'user profile',
          body: '## 身份背景\n- likes rust\n- prefers dark mode\n',
        },
      },
    ],
  });

  await add({
    name: 'run-single-null-profile',
    method: 'run',
    mode: 'single',
    setup: {userNotes: [note], userProfile: profile},
    script: [
      {method: 'rewriteUserProfile', result: null},
    ],
  });

  await add({
    name: 'run-single-kept-profile',
    method: 'run',
    mode: 'single',
    setup: {userNotes: [note], userProfile: profile},
    script: [
      {
        method: 'rewriteUserProfile',
        result: {
          type: 'user',
          scope: 'global',
          name: 'User Profile',
          description: 'user profile',
          body: '## 身份背景\n- likes rust\n',
        },
      },
    ],
  });

  await add({
    name: 'run-single-rewrite-error',
    method: 'run',
    mode: 'single',
    setup: {userNotes: [note], userProfile: profile},
    script: [
      {method: 'rewriteUserProfile', throws: 'fixture rewrite failed'},
    ],
  });

  await add({
    name: 'run-general-merge',
    method: 'run',
    mode: 'general',
    setup: {
      metas: [metaA, metaB],
      candidates: [
        candidate('project', 'Alpha Plan', 'alpha plan', '## Goal\nShip alpha.\n', 'p1'),
        candidate('feedback', 'Use tabs', 'formatting rule', 'Rule: use tabs.\n', 'p1'),
        candidate('project', 'Alpha Notes', 'alpha notes', '## Goal\nShip the alpha release.\n', 'p2'),
      ],
    },
    script: [
      {
        method: 'planGeneralProjectMetaMerges',
        debug: {request: 'merge-plan'},
        result: {
          summary: 'Found one duplicate General project node.',
          mergeGroups: [{keeperProjectId: 'p1', duplicateProjectIds: ['p2'], reason: 'duplicate'}],
        },
      },
      {
        method: 'planDreamClusters',
        result: {
          summary: 'Found one duplicate project cluster.',
          clusters: [{memberRelativePaths: ['$PROJECT0', '$PROJECT1'], reason: 'duplicate content'}],
        },
      },
      {
        method: 'refineDreamCluster',
        result: {
          summary: 'Merged the duplicate project memories.',
          file: {name: 'Alpha Merged', description: 'merged alpha', markdown: '# Alpha\n\nMerged body.'},
        },
      },
      {
        method: 'reviewDreamProjectMeta',
        result: {
          shouldUpdate: true,
          reason: 'Project metadata refreshed from General memory.',
          projectMeta: {projectName: 'Alpha Renamed', description: 'alpha project renamed', status: 'active'},
        },
      },
    ],
  });

  await add({
    name: 'category-warning-then-error',
    method: 'runCategoryDreamError',
    mode: 'single',
    setup: {candidates: [
      projectA,
      projectB,
      candidate('project', 'Alpha Extra', 'extra notes', '## Goal\nKeep extra.\n'),
      candidate('project', 'Alpha Archive', 'archive notes', '## Goal\nKeep archive.\n'),
    ]},
    selection: {type: 'project'},
    dropRecords: 1,
    randomSuffix: TRACE_SUFFIX,
    script: [
      {
        method: 'planDreamClusters',
        result: {
          summary: 'Two clusters, one unloadable.',
          clusters: [
            {memberRelativePaths: ['$PROJECT2', '$PROJECT3'], reason: 'missing record'},
            {memberRelativePaths: ['$PROJECT0', '$PROJECT1'], reason: 'duplicate'},
          ],
        },
      },
      {method: 'refineDreamCluster', throws: 'fixture refine failed'},
    ],
  });

  for (const item of cases) {
    const text = JSON.stringify(item, null, 2);
    if (text.includes(resolve(process.cwd())) || text.includes(scratch)) {
      throw new Error(`oracle case ${item.name} leaks an absolute path`);
    }
  }
  return {cases, clockMs: Date.parse(NOW), randoms: RANDOMS};
}
