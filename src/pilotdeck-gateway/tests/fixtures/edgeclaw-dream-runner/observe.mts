// Calls the fixed TS DreamRewriteRunner directly on a real MemoryRepository.
// Every expected value in oracle.json comes from these calls; no value is
// synthesized by the Rust side.
import {mkdirSync, readdirSync, readFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
import {join, resolve} from 'node:path';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';
import {removeOwned} from '../_shared/generator_guard.mjs';
import {createRunScratch, removeRunScratch} from '../_shared/run_scratch.mjs';

export const NOW = '2026-10-22T00:00:00.000Z';
const ROOT = 'src/context/memory/edgeclaw-memory-core/src';
const SUFFIX = (0.5).toString(36).slice(2, 10);
// Every runner mutation consumes one Math.random(); a longer script makes a
// port that calls it a different number of times drift rather than run dry.
const RANDOMS = [0.25, 0.5, 0.75, 0.125, 0.375, 0.625, 0.875, 0.0625, 0.1875, 0.3125,
  0.4375, 0.5625, 0.6875, 0.8125, 0.9375, 0.03125, 0.09375, 0.15625, 0.21875, 0.28125];

function tree(root, dir = '') {
  return readdirSync(join(root, dir), {withFileTypes: true})
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
    .flatMap((entry) => {
      const path = dir ? `${dir}/${entry.name}` : entry.name;
      return entry.isDirectory()
        ? tree(root, path)
        : [{relativePath: path, content: readFileSync(join(root, path), 'utf8')}];
    });
}

function jsonSafe(value) {
  return value === undefined ? null : JSON.parse(JSON.stringify(value));
}

class ScriptedExtractor {
  constructor(script) {
    this.script = script;
    this.index = 0;
    this.calls = [];
  }

  next(method, input) {
    const step = this.script[this.index++];
    if (!step || step.method !== method) {
      throw new Error(`fixture script expected ${step?.method ?? '<end>'} but got ${method}`);
    }
    const recorded = jsonSafe(input);
    this.calls.push({method, input: recorded});
    if (step.debug !== undefined) input.debugTrace?.(step.debug);
    if (step.throws !== undefined) {
      const error = new Error(step.throws);
      // `Object.assign(new Error('x'), {name: ''})` retains a custom empty
      // name through the catch; `String(error)` then omits the separator.
      if (step.throwsName !== undefined) error.name = step.throwsName;
      throw error;
    }
    return jsonSafe(step.result);
  }

  async planDreamClusters(input) {
    return this.next('planDreamClusters', input);
  }

  async refineDreamCluster(input) {
    return this.next('refineDreamCluster', input);
  }

  async planGeneralProjectMetaMerges(input) {
    return this.next('planGeneralProjectMetaMerges', input);
  }

  async reviewDreamProjectMeta(input) {
    return this.next('reviewDreamProjectMeta', input);
  }
}

async function load(baseline) {
  const at = (relative) => import(pathToFileURL(resolve(baseline, ROOT, relative)).href);
  const {DreamRewriteRunner} = await at('core/review/dream-review.ts');
  const {MemoryRepository} = await at('core/storage/sqlite.ts');
  const {hashText} = await at('core/utils/id.ts');
  return {DreamRewriteRunner, MemoryRepository, hashText};
}

function makeRepository(MemoryRepository, mode, dir, home, name) {
  if (mode === 'general') {
    const general = join(home, '.pilotdeck');
    return new MemoryRepository(join(general, 'workspaces', name, 'control.sqlite'), {
      memoryDir: join(general, 'workspaces', name, 'memory'),
      globalRootDir: join(home, 'global'),
      workspaceDir: general,
    });
  }
  return new MemoryRepository(join(dir, 'control.sqlite'), {
    memoryDir: join(dir, 'memory'),
    globalRootDir: join(dir, 'global'),
    workspaceDir: dir,
  });
}

function seed(repository, setup) {
  const store = repository.getFileMemoryStore();
  for (const meta of setup.metas ?? []) store.upsertProjectMeta(meta);
  for (const candidate of setup.candidates ?? []) store.upsertCandidate(candidate);
}

function selectionInput(selection) {
  return {
    kinds: selection.kinds ?? ['project', 'feedback'],
    scope: selection.scope ?? 'project',
    includeDeprecated: selection.includeDeprecated ?? false,
    limit: selection.limit ?? 5000,
    ...(selection.projectId ? {projectId: selection.projectId} : {}),
  };
}

function resolveTokens(value, replacements) {
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

function clean(value, root) {
  if (typeof value === 'string') return value.split(root).join('$ROOT');
  if (Array.isArray(value)) return value.map((item) => clean(item, root));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, clean(item, root)]));
  }
  return value;
}

function traceSeed(hashText) {
  const trace = {
    dreamTraceId: `dream_trace_${hashText(`manual:${NOW}:${SUFFIX}`)}`,
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
  return {trace, startedAt: NOW, suffix: SUFFIX, randomSuffix: SUFFIX};
}

async function runCase(baseline, scratch, home, spec) {
  const {DreamRewriteRunner, MemoryRepository, hashText} = await load(baseline);
  const dir = join(scratch, spec.name);
  const external = spec.setup?.external;
  if (external) {
    // Mirror cases need a real external workspace snapshot under
    // `~/.pilotdeck/workspaces/<name>` whose pipeline state points at the
    // mirrored workspace path. Create and close it before the General store
    // scans, so `listReadableProjectEntries` sees a real snapshot.
    const extDir = join(home, '.pilotdeck', 'workspaces', external.name);
    const extRepository = new MemoryRepository(join(extDir, 'control.sqlite'), {
      memoryDir: join(extDir, 'memory'),
      globalRootDir: join(home, 'global-external'),
      workspaceDir: external.workspace,
    });
    try {
      extRepository.setPipelineState('workspaceDir', external.workspace);
      seed(extRepository, external);
    } finally {
      extRepository.close();
    }
  }
  const repository = makeRepository(MemoryRepository, spec.mode, dir, home, spec.name);
  const savedRandom = Math.random;
  try {
    seed(repository, spec.setup);
    const store = repository.getFileMemoryStore();
    if (spec.ensureMeta) store.ensureProjectMeta({});
    const expected = {};
    // `DreamReviewRunnerOptions.logger` (`dream-review.ts:528-541`): the
    // merge-planning catch forwards `logger.warn` at line 838. Record every
    // level so an extra call is a fixture difference, not silently dropped.
    const logs = [];
    const logger = {
      info: (message) => logs.push(`info: ${String(message)}`),
      warn: (message) => logs.push(String(message)),
      error: (message) => logs.push(`error: ${String(message)}`),
    };
    let randomIndex = 0;
    Math.random = () => RANDOMS[randomIndex++] ?? 0.5;
    let script = spec.script;

    if (spec.method === 'runCategoryDream') {
      const entries = repository.listMemoryEntries(selectionInput(spec.selection));
      const wanted = spec.selection.type;
      const filtered = entries.filter((entry) => entry.type === wanted);
      const records = filtered.length > 0
        ? repository.getMemoryRecordsByIds(filtered.map((entry) => entry.relativePath), 5000)
        : [];
      // The entries snapshot can outlive a record: model "the file vanished
      // before recordsByPath was read" by dropping the last N records.
      const dropRecords = spec.dropRecords ?? 0;
      const visibleRecords = dropRecords > 0
        ? records.slice(0, Math.max(0, records.length - dropRecords))
        : records;
      const projectMeta = spec.projectMeta
        ? (spec.projectMeta === 'current' ? store.getProjectMeta() : store.getProjectMeta(spec.projectMeta)) ?? undefined
        : undefined;
      const pool = wanted === 'feedback' ? 'FEEDBACK' : 'PROJECT';
      script = resolveTokens(script, Object.fromEntries(filtered.map((entry, index) => [`${pool}${index}`, entry.relativePath])));
      expected.input = jsonSafe({kind: spec.kind, entries: filtered, records: visibleRecords, projectMeta});
      const extractor = new ScriptedExtractor(script);
      const runner = new DreamRewriteRunner(repository, extractor, {logger});
      const {trace} = traceSeed(hashText);
      const result = await runner['runCategoryDream']({
        trace,
        kind: spec.kind,
        entries: filtered,
        recordsByPath: new Map(visibleRecords.map((record) => [record.relativePath, record])),
        ...(projectMeta ? {projectMeta} : {}),
      });
      expected.result = jsonSafe(result);
      expected.trace = jsonSafe(trace);
      expected.calls = jsonSafe(extractor.calls);
    } else if (spec.method === 'mergeGeneralProjectMetas') {
      const extractor = new ScriptedExtractor(script);
      const runner = new DreamRewriteRunner(repository, extractor, {logger});
      const {trace} = traceSeed(hashText);
      expected.result = jsonSafe(await runner['mergeGeneralProjectMetas'](trace, store));
      expected.trace = jsonSafe(trace);
      expected.calls = jsonSafe(extractor.calls);
    } else if (spec.method === 'runProjectMetaReview') {
      const projectMeta = spec.projectMeta === 'current'
        ? store.getProjectMeta()
        : store.getProjectMeta(spec.projectMeta);
      if (!projectMeta) throw new Error(`fixture project meta ${spec.projectMeta} missing`);
      const extractor = new ScriptedExtractor(script);
      const runner = new DreamRewriteRunner(repository, extractor, {logger});
      const {trace} = traceSeed(hashText);
      expected.result = jsonSafe(await runner['runProjectMetaReview']({trace, workspaceStore: store, projectMeta}));
      expected.trace = jsonSafe(trace);
      expected.calls = jsonSafe(extractor.calls);
    } else {
      throw new Error(`unknown fixture method ${spec.method}`);
    }

    expected.logs = jsonSafe(logs);
    expected.tree = tree(repository.getFileMemoryStore().getRootDir());
    return JSON.parse(JSON.stringify(clean({
      name: spec.name,
      method: spec.method,
      mode: spec.mode,
      setup: spec.setup,
      selection: spec.selection,
      kind: spec.kind,
      projectMeta: spec.projectMeta,
      ensureMeta: spec.ensureMeta ?? false,
      ...(spec.dropRecords ? {dropRecords: spec.dropRecords} : {}),
      script,
      startedAt: NOW,
      randomSuffix: SUFFIX,
      randoms: RANDOMS,
      expected,
    }, repository.getFileMemoryStore().getRootDir())));
  } finally {
    Math.random = savedRandom;
    repository.close();
    await removeOwned(scratch, dir, {recursive: true});
  }
}

function candidate(kind, name, description, body, projectId) {
  return {
    type: kind,
    scope: 'project',
    ...(projectId ? {projectId} : {}),
    name,
    description,
    body,
  };
}

export async function collect(baseline) {
  // A fresh mkdtemp root per collect() call: concurrent generators must not
  // share HOME / SQLite paths, and this run only removes the root it owns.
  const scratch = createRunScratch(
    process.env.PDRS_RUNNER_SCRATCH || tmpdir(),
    'pdrs-dream-runner-observe-',
  );
  try {
    return await collectCases(baseline, scratch.owned);
  } finally {
    await removeRunScratch(scratch);
  }
}

async function collectCases(baseline, scratch) {
  const home = join(scratch, 'home');
  mkdirSync(home, {recursive: true});
  process.env.HOME = home;
  freezeClock();
  const cases = [];
  const add = async (spec) => cases.push(await runCase(baseline, scratch, home, spec));

  const projectA = candidate('project', 'Alpha Plan', 'alpha plan', '## Goal\nShip alpha.\n', undefined);
  const projectB = candidate('project', 'Alpha Notes', 'alpha notes', '## Goal\nShip the alpha release.\n', undefined);
  const feedbackA = candidate('feedback', 'Use tabs', 'formatting rule', 'Rule: use tabs.\n', undefined);
  const feedbackB = candidate('feedback', 'Use two-space tabs', 'formatting rule duplicate', 'Rule: indent with tabs.\n', undefined);
  const metaA = {projectId: 'p1', projectName: 'Alpha', description: 'alpha project', status: 'active'};
  const metaB = {projectId: 'p2', projectName: 'Beta', description: 'beta project', status: 'active'};
  const metaC = {projectId: 'p3', projectName: 'Gamma', description: 'gamma project', status: 'paused'};

  await add({
    name: 'category-project-cluster',
    method: 'runCategoryDream',
    mode: 'single',
    kind: 'project',
    setup: {candidates: [projectA, projectB]},
    selection: {type: 'project'},
    script: [
      {
        method: 'planDreamClusters',
        debug: {request: 'cluster-plan'},
        result: {
          summary: 'Found one duplicate project cluster.',
          clusters: [{memberRelativePaths: ['$PROJECT0', '$PROJECT1'], reason: 'duplicate content'}],
        },
      },
      {
        method: 'refineDreamCluster',
        debug: {request: 'cluster-refine'},
        result: {
          summary: 'Merged the duplicate project memories.',
          file: {name: 'Alpha Merged', description: 'merged alpha', markdown: '# Alpha\n\nMerged body.'},
        },
      },
    ],
  });

  await add({
    name: 'category-feedback-cluster',
    method: 'runCategoryDream',
    mode: 'single',
    kind: 'feedback',
    setup: {candidates: [feedbackA, feedbackB]},
    selection: {type: 'feedback'},
    script: [
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
    ],
  });

  await add({
    name: 'category-empty',
    method: 'runCategoryDream',
    mode: 'single',
    kind: 'project',
    setup: {},
    selection: {type: 'project'},
    script: [],
  });

  await add({
    name: 'category-skip-single',
    method: 'runCategoryDream',
    mode: 'single',
    kind: 'project',
    setup: {candidates: [projectA]},
    selection: {type: 'project'},
    script: [],
  });

  await add({
    name: 'category-general-scope',
    method: 'runCategoryDream',
    mode: 'general',
    kind: 'project',
    projectMeta: 'p1',
    setup: {
      metas: [metaA],
      candidates: [
        candidate('project', 'Alpha Plan', 'alpha plan', '## Goal\nShip alpha.\n', 'p1'),
        candidate('project', 'Alpha Notes', 'alpha notes', '## Goal\nShip the alpha release.\n', 'p1'),
      ],
    },
    selection: {type: 'project', projectId: 'p1'},
    script: [
      {
        method: 'planDreamClusters',
        result: {
          summary: 'Found one duplicate project cluster.',
          clusters: [{memberRelativePaths: ['$PROJECT0', '$PROJECT1'], reason: 'overlapping goals'}],
        },
      },
      {
        method: 'refineDreamCluster',
        result: {
          summary: 'Merged the duplicate project memories.',
          file: {name: 'Alpha Merged', description: 'merged alpha', markdown: '# Alpha\n\nMerged body.'},
        },
      },
    ],
  });

  await add({
    name: 'category-dropped-clusters',
    method: 'runCategoryDream',
    mode: 'single',
    kind: 'project',
    setup: {candidates: [projectA, projectB]},
    selection: {type: 'project'},
    script: [
      {
        method: 'planDreamClusters',
        result: {
          summary: 'Planner returned overlapping and low-quality clusters.',
          clusters: [
            {memberRelativePaths: ['Project/alpha-plan.md'], reason: 'only one file'},
            {memberRelativePaths: ['$PROJECT0', '$PROJECT1'], reason: 'same project'},
            {memberRelativePaths: ['$PROJECT0', '$PROJECT1'], reason: 'overlap'},
          ],
        },
      },
      {
        method: 'refineDreamCluster',
        result: {summary: 'No refined file.', file: null},
      },
    ],
  });

  await add({
    name: 'category-missing-record',
    method: 'runCategoryDream',
    mode: 'single',
    kind: 'project',
    setup: {candidates: [projectA, projectB]},
    selection: {type: 'project'},
    // Both headers form a valid cluster, but only one record is visible to
    // the runner: the plan-output detail must gain the same skip warning as
    // the result (TS stores one live droppedWarnings array).
    dropRecords: 1,
    script: [
      {
        method: 'planDreamClusters',
        result: {
          summary: 'Found one duplicate project cluster.',
          clusters: [{memberRelativePaths: ['$PROJECT0', '$PROJECT1'], reason: 'duplicate content'}],
        },
      },
    ],
  });

  await add({
    name: 'category-refine-null',
    method: 'runCategoryDream',
    mode: 'single',
    kind: 'project',
    setup: {candidates: [projectA, projectB]},
    selection: {type: 'project'},
    script: [
      {
        method: 'planDreamClusters',
        result: {
          summary: 'Found one duplicate project cluster.',
          clusters: [{memberRelativePaths: ['$PROJECT0', '$PROJECT1'], reason: 'duplicate content'}],
        },
      },
      {
        method: 'refineDreamCluster',
        result: {summary: 'Model could not synthesize a file.', file: null},
      },
    ],
  });

  await add({
    name: 'merge-basic',
    method: 'mergeGeneralProjectMetas',
    mode: 'general',
    setup: {
      metas: [metaA, metaB, metaC],
      candidates: [candidate('project', 'Beta Plan', 'beta plan', '## Goal\nShip beta.\n', 'p2')],
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
    ],
  });

  await add({
    name: 'merge-skip-single',
    method: 'mergeGeneralProjectMetas',
    mode: 'general',
    setup: {metas: [metaA]},
    script: [],
  });

  await add({
    name: 'merge-plan-error',
    method: 'mergeGeneralProjectMetas',
    mode: 'general',
    setup: {metas: [metaA, metaB]},
    script: [
      {
        method: 'planGeneralProjectMetaMerges',
        throws: 'fixture model failed',
      },
    ],
  });

  // Three `Error.prototype.toString()` / raw-UTF-16 boundaries from review
  // fix-r2: an empty message drops `name + ": "`, an empty name drops the
  // separator, and a lone surrogate stays a lone surrogate.
  await add({
    name: 'merge-plan-error-empty-message',
    method: 'mergeGeneralProjectMetas',
    mode: 'general',
    setup: {metas: [metaA, metaB]},
    script: [
      {
        method: 'planGeneralProjectMetaMerges',
        throws: '',
      },
    ],
  });

  await add({
    name: 'merge-plan-error-empty-name',
    method: 'mergeGeneralProjectMetas',
    mode: 'general',
    setup: {metas: [metaA, metaB]},
    script: [
      {
        method: 'planGeneralProjectMetaMerges',
        throws: 'x',
        throwsName: '',
      },
    ],
  });

  await add({
    name: 'merge-plan-error-lone-surrogate',
    method: 'mergeGeneralProjectMetas',
    mode: 'general',
    setup: {metas: [metaA, metaB]},
    script: [
      {
        method: 'planGeneralProjectMetaMerges',
        throws: '\ud800',
      },
    ],
  });

  await add({
    name: 'merge-invalid-groups',
    method: 'mergeGeneralProjectMetas',
    mode: 'general',
    setup: {
      metas: [metaA, metaB, metaC],
      candidates: [candidate('project', 'Beta Plan', 'beta plan', '## Goal\nShip beta.\n', 'p2')],
    },
    script: [
      {
        method: 'planGeneralProjectMetaMerges',
        result: {
          summary: 'Planner returned invalid groups.',
          mergeGroups: [
            {keeperProjectId: 'missing', duplicateProjectIds: ['p2']},
            {keeperProjectId: 'p1', duplicateProjectIds: ['missing']},
            {keeperProjectId: 'p1', duplicateProjectIds: ['p1']},
            {keeperProjectId: 'p1', duplicateProjectIds: []},
            {keeperProjectId: 'p1', duplicateProjectIds: ['p2']},
            {keeperProjectId: 'p1', duplicateProjectIds: ['p3']},
          ],
        },
      },
    ],
  });

  const metaProjectA = candidate('project', 'Alpha Plan', 'alpha plan', '## Goal\nShip alpha.\n');
  const metaProjectB = candidate('project', 'Alpha Notes', 'alpha notes', '## Goal\nShip alpha faster.\n');
  const metaFeedback = candidate('feedback', 'Prefer Rust', 'language rule', 'Rule: prefer Rust.\n');

  await add({
    name: 'meta-review-update',
    method: 'runProjectMetaReview',
    mode: 'single',
    projectMeta: 'current',
    setup: {candidates: [metaProjectA, metaProjectB, metaFeedback]},
    script: [
      {
        method: 'reviewDreamProjectMeta',
        debug: {request: 'meta-review'},
        result: {
          shouldUpdate: true,
          projectMeta: {projectName: 'Renamed Alpha', description: 'new alpha description', status: 'paused'},
          reason: 'Reviewed two project files and one feedback file.',
        },
      },
    ],
  });

  await add({
    name: 'meta-review-keep',
    method: 'runProjectMetaReview',
    mode: 'single',
    projectMeta: 'current',
    setup: {candidates: [metaProjectA, metaProjectB]},
    script: [
      {
        method: 'reviewDreamProjectMeta',
        result: {
          shouldUpdate: false,
          projectMeta: {},
          reason: 'Reviewed the project files and kept the metadata.',
        },
      },
    ],
  });

  await add({
    name: 'meta-review-empty',
    method: 'runProjectMetaReview',
    mode: 'single',
    projectMeta: 'current',
    ensureMeta: true,
    setup: {},
    script: [],
  });

  await add({
    name: 'meta-review-general',
    method: 'runProjectMetaReview',
    mode: 'general',
    projectMeta: 'p1',
    setup: {
      metas: [metaA],
      candidates: [candidate('project', 'Alpha Plan', 'alpha plan', '## Goal\nShip alpha.\n', 'p1')],
    },
    script: [
      {
        method: 'reviewDreamProjectMeta',
        result: {
          shouldUpdate: false,
          projectMeta: {projectName: 'Alpha', description: 'alpha project', status: 'active'},
          reason: 'Reviewed the project file and kept the metadata.',
        },
      },
    ],
  });

  await add({
    name: 'meta-review-mirror',
    method: 'runProjectMetaReview',
    mode: 'general',
    projectMeta: 'mirror',
    setup: {
      metas: [{
        projectId: 'mirror',
        projectName: 'Alpha mirror',
        description: 'mirrored alpha',
        status: 'active',
        sourceKind: 'workspace_external_mirror',
        sourceWorkspacePath: '/fixture/Mirror',
        sourceProjectId: 'current_project',
      }],
      candidates: [candidate('feedback', 'Local mirror feedback', 'local rule', 'Rule: local.\n', 'mirror')],
      external: {
        name: 'mirror-ext',
        workspace: '/fixture/Mirror',
        candidates: [
          candidate('project', 'Mirrored plan', 'mirrored plan', '## Goal\nShip mirror.\n'),
          candidate('feedback', 'Mirrored feedback', 'mirrored rule', 'Rule: mirror.\n'),
        ],
      },
    },
    script: [
      {
        method: 'reviewDreamProjectMeta',
        result: {
          shouldUpdate: false,
          projectMeta: {projectName: 'Alpha mirror', description: 'mirrored alpha', status: 'active'},
          reason: 'Reviewed mirrored project files.',
        },
      },
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

