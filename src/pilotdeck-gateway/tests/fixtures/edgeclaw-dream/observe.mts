// Calls the fixed TS baseline directly. Every expected value in oracle.json
// comes from these calls; no value is synthesized by the Rust side.
import {existsSync, readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {dirname, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';

export const NOW = '2026-10-22T00:00:00.000Z';
export const HOME = '/fixture/home';
const ROOT = 'src/context/memory/edgeclaw-memory-core/src';

export function enc(v: any): any {
  if (v === undefined) return {$undefined: true};
  if (typeof v === 'number' && (Object.is(v, -0) || !Number.isFinite(v))) {
    return {$number: Object.is(v, -0) ? '-0' : String(v)};
  }
  if (Array.isArray(v)) return v.map(enc);
  if (v && typeof v === 'object') {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, enc(x)]));
  }
  return v;
}

// Expected values cross the Rust JSON wire, so apply JSON.stringify semantics
// (an undefined object property disappears) before the sentinel encoding.
function jsonSafe(v: any): any {
  if (v === undefined) return null;
  return JSON.parse(JSON.stringify(v));
}

async function transformedDream(baseline: string) {
  const sourcePath = resolve(baseline, ROOT, 'core/review/dream-review.ts');
  const requireFromBaseline = createRequire(resolve(baseline, 'package.json'));
  const {transformSync} = createRequire(requireFromBaseline.resolve('tsx'))('esbuild');
  const code = transformSync(
    readFileSync(sourcePath, 'utf8')
      + '\nexport { createDreamTrace, pushStep, mutation, sortEntries, sortProjectMetas,'
      + ' toDreamMetaInput, toDreamRecordInput, toHeaderInput, toRefinedCandidate,'
      + ' validateExclusiveClusters, selectUserNoteWindow, validateGeneralProjectMergeGroups };\n',
    {loader: 'ts', format: 'esm', target: 'es2022', sourcefile: sourcePath},
  ).code;
  const executable = code.replace(/from (["'])(\.[^"']+)\1/g, (_: string, quote: string, specifier: string) => {
    let file = resolve(dirname(sourcePath), specifier);
    if (file.endsWith('.js') && existsSync(file.slice(0, -3) + '.ts')) file = file.slice(0, -3) + '.ts';
    return 'from ' + quote + pathToFileURL(file).href + quote;
  });
  return import('data:text/javascript;base64,' + Buffer.from(executable).toString('base64'));
}

export async function loadDream(baseline: string) {
  return transformedDream(baseline);
}

export async function collect(baseline: string) {
  process.env.HOME = HOME;
  freezeClock();
  const load = (relative: string) =>
    import(pathToFileURL(resolve(baseline, ROOT, relative)).href);
  const dream = await transformedDream(baseline);
  const {FileMemoryStore} = await load('core/file-memory.ts');
  const store = {toCandidate: (record: any) => FileMemoryStore.prototype.toCandidate(record)};
  const nowMs = Date.parse(NOW);
  const cases: any[] = [];
  const add = (fn: string, args: any[], expected: any) =>
    cases.push({fn, args: enc(args), expected: enc(jsonSafe(expected))});
  // A malformed LLM JSON shape must abort loud. Record the exact V8
  // TypeError name/message so Rust can match the throw path, not just the
  // happy-path result.
  const addThrows = (fn: string, args: any[], run: () => unknown) => {
    try {
      add(fn, args, run());
    } catch (error: any) {
      cases.push({fn, args: enc(args), throws: {name: String(error?.name), message: String(error?.message)}});
    }
  };
  const withRandom = <T>(value: number, fn: () => T): T => {
    const saved = Math.random;
    Math.random = () => value;
    try { return fn(); } finally { Math.random = saved; }
  };
  const suffix = (value: number) => value.toString(36).slice(2, 10);

  // -- trace / step / mutation ---------------------------------------------
  for (const [trigger, random] of [['manual', 0.5], ['scheduled', 0.123456789], ['manual', 0],
    ['scheduled', 0.9999999999999999], ['manual', 0.25]] as const) {
    const trace = withRandom(random, () => dream.createDreamTrace(trigger));
    add('createDreamTrace', [trigger, NOW, suffix(random)], trace);
  }
  let trace = withRandom(0.5, () => dream.createDreamTrace('manual'));
  const stepOptions = [
    {refs: {index: 1}, metrics: {files: 0},
      details: [{key: 'k', label: 'L', kind: 'kv', entries: [{label: 'stage', value: 'build'}]}],
      titleI18n: {key: 'trace.step.title', fallback: 'Title'}},
    {refs: null, metrics: undefined, details: [], promptDebug: {requestLabel: 'plan'}},
    {refs: 0, metrics: '', outputSummaryI18n: {key: 'k', args: ['x'], fallback: 'F'}},
    {details: [{key: 'd', label: 'D', kind: 'json', json: {a: 1}}], titleI18n: {key: 't', fallback: ''}},
  ];
  for (const [index, options] of stepOptions.entries()) {
    const next = structuredClone(trace);
    const kind = ['project_header_scan', 'project_cluster_plan', 'warning', 'feedback_cluster_refine'][index]!;
    const status = ['info', 'success', 'warning', 'skipped'][index]!;
    dream.pushStep(next, kind, `Title ${index}`, status, `in ${index}`, `out ${index}`, options);
    add('pushStep', [trace, kind, `Title ${index}`, status, `in ${index}`, `out ${index}`, options], next);
    trace = next;
  }
  const mutationOptions = [
    {},
    {candidateType: 'feedback', name: 'N', description: 'D', preview: 'P'},
    {candidateType: '', name: '', description: '', preview: ''},
    {candidateType: 'project', name: '  ', preview: '### H\nbody'},
  ];
  for (const [index, options] of mutationOptions.entries()) {
    const random = [0.5, 0.123456789, 0, 0.9999999999999999][index]!;
    add('mutation', ['write', `Project/file-${index}.md`, nowMs, random, options],
      withRandom(random, () => dream.mutation('write', `Project/file-${index}.md`, options)));
  }

  // -- sorts ---------------------------------------------------------------
  const manifest = [
    {relativePath: 'Z.md', updatedAt: '2026-01-01T00:00:00.000Z', name: 'z'},
    {relativePath: 'a.md', updatedAt: '2026-01-01T00:00:00.000Z', name: 'a'},
    {relativePath: 'ä.md', updatedAt: '2026-01-02T00:00:00.000Z', name: 'ae'},
    {relativePath: 'b.md', updatedAt: '2026-01-02T00:00:00.000Z', name: 'b'},
    {relativePath: 'A.md', updatedAt: '2026-01-01T00:00:00.000Z', name: 'A'},
    {relativePath: 'tie.md', updatedAt: '2026-01-03T00:00:00.000Z', name: 'first'},
    {relativePath: 'tie.md', updatedAt: '2026-01-03T00:00:00.000Z', name: 'second'},
  ];
  add('sortEntries', [manifest], dream.sortEntries(manifest));
  add('sortEntries', [[]], dream.sortEntries([]));
  const oddManifest = [
    {relativePath: '\ud800.md', updatedAt: '2026-02-01', name: 'lone-d800'},
    {relativePath: '\ud801.md', updatedAt: '2026-02-01', name: 'lone-d801'},
    {relativePath: '产品.md', updatedAt: '2026-02-02', name: 'cjk'},
    {relativePath: 'a.md', updatedAt: '', name: 'empty-time'},
    {relativePath: 'a.md', updatedAt: '', name: 'stable-second'},
  ];
  add('sortEntries', [oddManifest], dream.sortEntries(oddManifest));
  const metas = [
    {projectId: '1', projectName: 'Zeta', updatedAt: '2026-01-01T00:00:00.000Z'},
    {projectId: '2', projectName: 'alpha', updatedAt: '2026-01-01T00:00:00.000Z'},
    {projectId: '3', projectName: 'Ä', updatedAt: '2026-01-02T00:00:00.000Z'},
    {projectId: '4', projectName: 'beta', updatedAt: '2026-01-02T00:00:00.000Z'},
  ];
  add('sortProjectMetas', [metas], dream.sortProjectMetas(metas));

  // -- LLM input projections ----------------------------------------------
  for (const meta of [
    {projectId: 'p', projectName: 'P', description: 'D', status: 'active',
      updatedAt: 'U', dreamUpdatedAt: 'DU', sourceKind: 'general_local',
      sourceWorkspacePath: '/ws', sourceProjectId: 'sp'},
    {projectId: 'p', projectName: 'P', description: 'D', status: 'active',
      updatedAt: 'U', dreamUpdatedAt: '', sourceKind: '', sourceWorkspacePath: null},
    {projectId: 'p', projectName: 'P', description: 'D', status: 'active', updatedAt: 'U',
      dreamUpdatedAt: 0, sourceKind: 0, sourceProjectId: false},
  ]) {
    add('toDreamMetaInput', [meta], dream.toDreamMetaInput(meta));
  }
  const records = [
    {relativePath: 'Project/p.md', type: 'project', projectId: 'p', name: 'N',
      description: 'D', updatedAt: 'U', capturedAt: 'C', sourceSessionKey: 'S',
      content: '## Current Stage\nBuild\n\n## Decisions\n- one\n- two\n\n## Next Steps\n- go\n'},
    {relativePath: 'Feedback/f.md', type: 'feedback', projectId: 'p', name: 'F',
      description: 'D', updatedAt: 'U', capturedAt: '', sourceSessionKey: '',
      content: '## Rule\nAlways\n\n## Why\nBecause\n\n## How to Apply\nDo\n\n## Notes\n- n\n'},
    {relativePath: 'x.md', type: 'project', name: 'n', description: 'd', updatedAt: 'u',
      content: ''},
    {relativePath: 'g.md', type: 'general_project_meta', name: 'g', description: 'd',
      updatedAt: 'u', projectId: 'p', content: '## Status\nactive\n'},
    {relativePath: 'u.md', type: 'user', name: 'u', description: 'd', updatedAt: 'u',
      content: '## 身份背景\n- one\n'},
    {relativePath: 'z.md', type: 'project', projectId: 'p', name: 'z', description: 'd',
      updatedAt: 'u', capturedAt: 0, sourceSessionKey: 0,
      content: '## Decisions\n- one\n- one\n\n## Blockers\n'},
  ];
  for (const record of records) {
    add('toDreamRecordInput', [record], dream.toDreamRecordInput(store, record));
  }
  for (const entry of [
    {relativePath: 'a/b.md', name: 'N', description: 'D', updatedAt: 'U', extra: 1},
    {relativePath: 'a/b.md'},
  ]) {
    add('toHeaderInput', [entry], dream.toHeaderInput(entry));
  }
  for (const input of [
    {kind: 'project', name: ' N ', description: ' D ', markdown: '\n  body  \n',
      sourceRecord: {capturedAt: 'C', sourceSessionKey: 'S'}},
    {kind: 'feedback', name: 'N', description: 'D', markdown: '\t\n',
      sourceRecord: {capturedAt: '', sourceSessionKey: ''}},
    {kind: 'project', name: 'N', description: 'D', markdown: '  \u3000 x \u2028 ',
      sourceRecord: {}},
  ]) {
    add('toRefinedCandidate', [input], dream.toRefinedCandidate(input));
  }

  // -- exclusive clusters --------------------------------------------------
  const allowed = ['a.md', 'b.md', 'c.md', 'd.md', 'e.md', 'f.md'];
  const clusterCases: Array<[any[], number]> = [
    [[{memberRelativePaths: [' a.md ', 'a.md', 'unknown.md', 'b.md'], reason: 'overlap'}], 8],
    [[{memberRelativePaths: ['a.md', 'unknown.md'], reason: 'only one valid'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md', 'c.md'], reason: 'too many'}], 2],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'overlap'},
      {memberRelativePaths: ['b.md', 'c.md'], reason: 'second'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'Same project'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'Same project but overlapping content'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: '同一个项目'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: '同属这个项目'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md', 'c.md'], reason: 'all three overlap'}], 3],
    [[{memberRelativePaths: ['a.md', 'b.md']}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: ''}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'These are duplicates'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'same current project'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: '都属于同一个项目'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: '属于当前项目'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: '同一个工作区'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: '这些内容重复，可以合并'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'same project, duplicate'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'Same workspace\nbut different files'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'same project\noverlap'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: '同属这两个项目'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'all of these belong to same project'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'all\nsame project'}], 8],
    // `/i` without `u` folds ASCII only (dream-review.ts:342-355): U+017F LONG
    // S and U+212A KELVIN SIGN must not match `s` / `k`.
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: '\u017Fame project'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'same wor\u212Aspace'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'same project but incon\u017Fistent'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'same project, same ris\u212A'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'same project but incon\u0130stent'}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'same project but incon\u0131stent'}], 8],
  ];
  for (const [clusters, maxFiles] of clusterCases) {
    add('validateExclusiveClusters', [clusters, allowed, maxFiles],
      dream.validateExclusiveClusters(clusters, new Set(allowed), maxFiles));
  }
  // Malformed LLM JSON shapes (#826): the TS helper throws a TypeError instead
  // of silently normalizing. `addThrows` records the exact V8 name/message.
  const shapeClusterCases: Array<[any, number]> = [
    [null, 8], [1, 8], [{}, 8], [true, 8],
    ['', 8],
    ['ab', 8],
    [[null], 8],
    [[undefined], 8],
    [[1], 8],
    [['x'], 8],
    [[[]], 8],
    [[{}], 8],
    [[{memberRelativePaths: null}], 8],
    [[{memberRelativePaths: undefined}], 8],
    [[{memberRelativePaths: 1}], 8],
    [[{memberRelativePaths: {}}], 8],
    [[{memberRelativePaths: 'ab'}], 8],
    [[{memberRelativePaths: ['a.md', 1]}], 8],
    [[{memberRelativePaths: ['a.md', true, 'b.md']}], 8],
    [[{memberRelativePaths: ['a.md', {}, 'b.md']}], 8],
    [[{memberRelativePaths: ['a.md', null, 'b.md']}], 8],
    [[{memberRelativePaths: ['a.md', undefined, 'b.md']}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 1}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: true}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: {}}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: [1, 2]}], 8],
    [[{memberRelativePaths: ['a.md'], reason: 1}], 8],
    [[{memberRelativePaths: ['a.md'], reason: true}], 8],
    [[{memberRelativePaths: ['a.md'], reason: [1, 2]}], 8],
    [[{memberRelativePaths: ['a.md'], reason: false}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: null}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: ''}], 8],
    [[{memberRelativePaths: ['a.md', 'b.md', 'c.md'], reason: 1}], 2],
    [[{memberRelativePaths: ['a.md', 'b.md'], reason: 'overlap'},
      {memberRelativePaths: ['b.md', 'c.md'], reason: 1}], 8],
    [[{memberRelativePaths: null, reason: 1}], 8],
  ];
  for (const [clusters, maxFiles] of shapeClusterCases) {
    addThrows('validateExclusiveClusters', [clusters, allowed, maxFiles],
      () => dream.validateExclusiveClusters(clusters, new Set(allowed), maxFiles));
  }

  // -- user-note window ----------------------------------------------------
  const note = (relativePath: string, updatedAt: string, content: string) =>
    ({relativePath, type: 'user', name: relativePath, description: '', updatedAt, content});
  const windowCases = [
    [note('big.md', '2026-01-01', 'a'.repeat(120000))],
    [note('big.md', '2026-01-02', 'a'.repeat(120001)), note('small.md', '2026-01-01', 'b')],
    [note('big.md', '2026-01-02', 'a'.repeat(120000)), note('later.md', '2026-01-01', 'b')],
    [note('b.md', '2026-01-02', 'bb'), note('a.md', '2026-01-02', 'a'),
      note('c.md', '2026-01-03', 'ccc')],
    Array.from({length: 201}, (_, index) =>
      note(`n-${String(index).padStart(3, '0')}.md`, '2026-01-01', 'x')),
    [note('a.md', '2026-01-03', 'a'.repeat(60000)), note('b.md', '2026-01-02', 'b'.repeat(60000)),
      note('c.md', '2026-01-01', 'c')],
    Array.from({length: 200}, (_, index) =>
      note(`n-${String(index).padStart(3, '0')}.md`, '2026-01-01', 'x'.repeat(600))),
    [],
  ];
  for (const records of windowCases) {
    add('selectUserNoteWindow', [records], dream.selectUserNoteWindow(records));
  }

  // -- General project merge groups ---------------------------------------
  const mergeMetas = [
    {projectId: 'p1', projectName: 'One', description: '', status: 'active', updatedAt: 'U'},
    {projectId: 'p2', projectName: 'Two', description: '', status: 'active', updatedAt: 'U'},
    {projectId: 'p3', projectName: 'Three', description: '', status: 'active', updatedAt: 'U'},
    {projectId: 'p4', projectName: 'Four', description: '', status: 'active', updatedAt: 'U'},
  ];
  const mergeCases = [
    [{keeperProjectId: ' p1 ', duplicateProjectIds: [' p2 ', 'p2'], reason: 'duplicate'}],
    [{keeperProjectId: 'missing', duplicateProjectIds: ['p2'], reason: 'x'}],
    [{keeperProjectId: '', duplicateProjectIds: ['p2'], reason: 'x'}],
    [{keeperProjectId: 'p1', duplicateProjectIds: ['missing'], reason: 'x'}],
    [{keeperProjectId: 'p1', duplicateProjectIds: ['p1', 'p2'], reason: 'x'}],
    [{keeperProjectId: 'p1', duplicateProjectIds: [], reason: 'x'}],
    [{keeperProjectId: 'p1', duplicateProjectIds: ['p2'], reason: 'first'},
      {keeperProjectId: 'p1', duplicateProjectIds: ['p3'], reason: 'reused'}],
    [{keeperProjectId: 'p1', duplicateProjectIds: ['p2']}],
    [{keeperProjectId: 'p1', duplicateProjectIds: ['p2'], reason: ''}],
    [{keeperProjectId: 'p1', duplicateProjectIds: ['p2', ' p2 '], reason: 'same'}],
    [{keeperProjectId: ' p1 ', duplicateProjectIds: ['   '], reason: 'empty duplicate'}],
    [{keeperProjectId: 'p1', duplicateProjectIds: ['p2'], reason: 'keep'},
      {keeperProjectId: 'p3', duplicateProjectIds: ['p2'], reason: 'reused duplicate'}],
    [{keeperProjectId: 'p1', duplicateProjectIds: ['p2', 'p3']},
      {keeperProjectId: 'p4', duplicateProjectIds: ['p1'], reason: 'reused keeper'}],
  ];
  for (const groups of mergeCases) {
    const input = {metas: mergeMetas, groups};
    add('validateGeneralProjectMergeGroups', [input], dream.validateGeneralProjectMergeGroups(input));
  }
  // Malformed LLM JSON shapes (#826): eager `metas.map` / `groups.entries` and
  // per-entry property reads throw instead of being filtered out.
  const shapeMergeCases: any[] = [
    {metas: null, groups: []},
    {metas: 1, groups: []},
    {metas: {}, groups: []},
    {groups: []},
    {metas: undefined, groups: []},
    {metas: [null], groups: []},
    {metas: [undefined], groups: []},
    {metas: [1], groups: []},
    {metas: ['x'], groups: []},
    {metas: mergeMetas, groups: null},
    {metas: mergeMetas, groups: 1},
    {metas: mergeMetas, groups: {}},
    {metas: mergeMetas, groups: 'ab'},
    {metas: mergeMetas},
    {metas: mergeMetas, groups: undefined},
    {metas: mergeMetas, groups: [null]},
    {metas: mergeMetas, groups: [undefined]},
    {metas: mergeMetas, groups: [1]},
    {metas: mergeMetas, groups: [[]]},
    {metas: mergeMetas, groups: ['p1']},
    {metas: mergeMetas, groups: [{keeperProjectId: 1, duplicateProjectIds: []}]},
    {metas: mergeMetas, groups: [{keeperProjectId: {}, duplicateProjectIds: []}]},
    {metas: mergeMetas, groups: [{keeperProjectId: null, duplicateProjectIds: []}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1'}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: undefined}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: null}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: 1}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: {}}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: 'p2'}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: ['p2', 1]}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: ['p2', true]}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: ['p2', null]}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: ['p2', undefined]}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: ['p2'], reason: 1}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: ['p2'], reason: {}}]},
    {metas: mergeMetas, groups: [{keeperProjectId: 'p1', duplicateProjectIds: ['p2'], reason: null}]},
  ];
  for (const input of shapeMergeCases) {
    addThrows('validateGeneralProjectMergeGroups', [input],
      () => dream.validateGeneralProjectMergeGroups(input));
  }

  return {cases, clockMs: nowMs};
}
