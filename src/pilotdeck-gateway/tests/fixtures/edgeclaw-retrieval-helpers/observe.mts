// Calls the fixed TS baseline directly. Every expected value in oracle.json
// comes from these calls; no value is synthesized by the Rust side.
import {existsSync, readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {dirname, join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';

export const HOME = '/fixture/home';
const ROOT = 'src/context/memory/edgeclaw-memory-core/src';

export function enc(v: any): any {
  if (v === undefined) return {$undefined: true};
  if (typeof v === 'number' && (Object.is(v, -0) || !Number.isFinite(v))) return {$number: Object.is(v, -0) ? '-0' : String(v)};
  if (Array.isArray(v)) return v.map(enc);
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, enc(x)]));
  return v;
}

function clean(v: any): any {
  const cwd = process.cwd();
  if (typeof v === 'string') return v.split(cwd).join('$CWD');
  if (Array.isArray(v)) return v.map(clean);
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, clean(x)]));
  return v;
}

async function transformedHelpers(baseline: string) {
  const sourcePath = join(baseline, ROOT, 'core/retrieval/reasoning-loop.ts');
  const requireFromBaseline = createRequire(join(baseline, 'package.json'));
  const {transformSync} = createRequire(requireFromBaseline.resolve('tsx'))('esbuild');
  const code = transformSync(
    readFileSync(sourcePath, 'utf8')
      + '\nexport { normalizeQueryKey, buildTraceId, hasUserSummary, renderUserSummaryBlock,'
      + ' renderProjectMetaBlock, renderSelectedRecordsBlock, renderContext, recentUserMessages,'
      + ' expandCjkSearchToken, tokenizeSearchText, buildProjectShortlist, createTrace, pushStep,'
      + ' fallbackSelection };\n',
    {loader: 'ts', format: 'esm', target: 'es2022', sourcefile: sourcePath},
  ).code;
  const executable = code.replace(/from (["'])(\.[^"']+)\1/g, (_: string, quote: string, specifier: string) => {
    let file = resolve(dirname(sourcePath), specifier);
    if (file.endsWith('.js') && existsSync(file.slice(0, -3) + '.ts')) file = file.slice(0, -3) + '.ts';
    return 'from ' + quote + pathToFileURL(file).href + quote;
  });
  return import('data:text/javascript;base64,' + Buffer.from(executable).toString('base64'));
}

const tokenOf = (value: number) => value.toString(36).slice(2, 10);
const withRandom = <T>(value: number, run: () => T): T => {
  const original = Math.random;
  Math.random = () => value;
  try {
    return run();
  } finally {
    Math.random = original;
  }
};

export async function collect(baseline: string) {
  process.env.HOME = HOME;
  freezeClock();
  const helpers = await transformedHelpers(baseline);
  const clockMs = Date.now();
  const locale = new Intl.Collator().resolvedOptions().locale;
  if (!locale.startsWith('en')) throw new Error(`oracle needs the en-US default locale, got ${locale}`);
  const cases: any[] = [];
  const add = (fn: string, args: any[], expected: any) =>
    cases.push({fn, args: enc(args), expected: enc(clean(expected))});
  const userSummary = (identityBackground: string[], files: any[]) => ({identityBackground, files});
  const userFile = (relativePath: string, updatedAt: string) =>
    ({relativePath, type: 'user', updatedAt, content: 'profile body'});
  const projectMeta = (overrides: any = {}) => ({
    projectId: 'p1', projectName: 'Alpha', description: 'Alpha description', status: 'active',
    createdAt: '2026-01-01T00:00:00.000Z', updatedAt: '2026-02-01T00:00:00.000Z',
    relativePath: 'GeneralProjects/p1/project.md', absolutePath: '/fixture/home/GeneralProjects/p1/project.md',
    ...overrides,
  });
  const record = (relativePath: string, type: string, updatedAt: string, content: string) =>
    ({relativePath, type, updatedAt, content});
  const catalogEntry = (overrides: any = {}) => ({
    projectId: 'p1', projectName: 'Alpha', description: 'Alpha description', status: 'active',
    createdAt: '2026-01-01T00:00:00.000Z', updatedAt: '2026-02-01T00:00:00.000Z',
    relativePath: 'GeneralProjects/p1/project.md', absolutePath: '/abs/project.md',
    workspaceMode: 'general', workspacePath: '/ws', workspaceName: 'ws',
    sourceType: 'general_local', logicalProjectId: 'local:p1', readOnly: false,
    hasLocalMirror: true, summary: {totalEntries: 2, projectEntries: 2, feedbackEntries: 0},
    ...overrides,
  });
  const message = (role: string, content: string) => ({role, content});

  // -- normalizeQueryKey ----------------------------------------------------
  for (const query of [
    '', '  A  B  ', 'A\tB\nC', '\u00a0\u3000\ufeff x \u2028 y', 'STRASSE Straße',
    'İSTANBUL', '产品  项目', '\ud800  x  \ud801', '\r\n', 'a\rb', '  ', '\u2028\u2029',
  ]) {
    add('normalizeQueryKey', [query], helpers.normalizeQueryKey(query));
  }

  // -- buildTraceId ---------------------------------------------------------
  for (const [prefix, seed, random] of [
    ['trace', 'seed', 0.123456789],
    ['', '', 0.5],
    ['产品', '会话\ud800', 0.000001],
    ['trace', '', 0],
    ['l1', 'x'.repeat(300), 0.999999999],
  ] as const) {
    const token = tokenOf(random);
    add('buildTraceId', [prefix, seed, token],
      withRandom(random, () => helpers.buildTraceId(prefix, seed)));
  }

  // -- context rendering ----------------------------------------------------
  const summaries = [
    userSummary([], []),
    userSummary(['one'], [userFile('global/UserIdentity/user-profile.md', '2026-02-01T00:00:00.000Z')]),
    userSummary(['one', 'two'], [userFile('custom/profile.md', '')]),
    userSummary(['\\u4e2d\\u6587'], []),
  ];
  for (const summary of summaries) {
    add('renderUserSummaryBlock', [summary], helpers.renderUserSummaryBlock(summary));
  }
  for (const meta of [
    null,
    projectMeta(),
    projectMeta({description: '', projectName: '产品', relativePath: 'r/p.md', updatedAt: 't'}),
  ]) {
    add('renderProjectMetaBlock', [meta], helpers.renderProjectMetaBlock(meta));
  }
  for (const records of [
    [],
    [record('a.md', 'user', 'u1', '  body  '), record('b.md', 'project', 'u2', '\nline\n\n')],
    [record('c.md', 'feedback', 'u3', '')],
  ]) {
    add('renderSelectedRecordsBlock', [records], helpers.renderSelectedRecordsBlock(records));
  }
  const userForContext = userSummary(['background'], [userFile('global/UserIdentity/user-profile.md', '2026-02-01T00:00:00.000Z')]);
  const contextRecords = [
    record('global/UserIdentity/user-profile.md', 'user', '2026-02-01T00:00:00.000Z', 'personal'),
    record('GeneralProjects/p1/project.md', 'project', '2026-02-02T00:00:00.000Z', 'project body'),
    record('GeneralProjects/p1/feedback.md', 'feedback', '2026-02-03T00:00:00.000Z', 'feedback'),
  ];
  for (const [route, summary, meta, records] of [
    ['user', userForContext, projectMeta(), contextRecords],
    ['user', userSummary([], []), projectMeta(), contextRecords],
    ['project', userSummary([], []), projectMeta(), contextRecords],
    ['project', userSummary([], []), null, []],
    ['project', userSummary([], []), null, [contextRecords[1]!]],
    ['mix', userForContext, projectMeta(), contextRecords],
    ['mix', userSummary([], []), null, []],
    ['mix', userSummary([], []), null, [contextRecords[1]!]],
    ['none', userForContext, projectMeta(), contextRecords],
    ['auto', userForContext, projectMeta(), contextRecords],
    ['user', userSummary(['\\u4e2d'], [userFile('x.md', 't')]), null, [record('x.md', 'user', 't', 'dup')]],
  ] as const) {
    add('renderContext', [route, summary, meta, records],
      helpers.renderContext(route, summary, meta, records));
  }

  // -- recentUserMessages ------------------------------------------------
  const messages = [
    {role: 'user', content: 'u1'}, {role: 'assistant', content: 'a1'},
    {role: 'user', content: 'u2'}, {role: 'system', content: 's1'},
    {role: 'user', content: 'u3'}, {role: 'user', content: 'u4'}, {role: 'user', content: 'u5'},
    {role: 'other', content: 'o1'},
  ];
  for (const input of [undefined, [], messages]) {
    add('recentUserMessages', [input], helpers.recentUserMessages(input));
  }

  // -- CJK tokenization -----------------------------------------------------
  for (const token of [
    '', 'abc', '项目', '项目进展', '中文English混合', '𠀀𠀁项目', 'a项目b', '😀项目',
    '\ud800项目\ud801', 'カタカナ', 'C++', '12345', '項目', '中文',
    '中华人民共和国人', '中华人民共和国人人',
  ]) {
    add('expandCjkSearchToken', [token], helpers.expandCjkSearchToken(token));
  }
  for (const value of [
    '', '项目进展如何', '当前项目', '中文English混合', '𠀀𠀁项目', 'a项目b 项目',
    'C++ 项目', 'GENERAL general GENERAL', '产品🚀x', '\ud800项目\ud801',
    'カタカナ 项目', '12345 67', '项目，进展！情况？', '  \t\n  ',
    '中华人民共和国人', '中华人民共和国人人 项目',
  ]) {
    add('tokenizeSearchText', [value], helpers.tokenizeSearchText(value));
  }

  // -- buildProjectShortlist ------------------------------------------------
  const allCatalog = Array.from({length: 35}, (_, index) => catalogEntry({
    projectId: `p${index}`, logicalProjectId: `local:p${index}`,
    projectName: `Project${index}`, description: `description ${index}`,
    updatedAt: `2026-01-${String(index + 1).padStart(2, '0')}T00:00:00.000Z`,
    summary: {totalEntries: 1, projectEntries: 1, feedbackEntries: 0},
  }));
  const shortlistCases: Array<[any[], string, any[]]> = [
    [[], 'anything', []],
    [[
      catalogEntry({projectName: 'Alpha', description: 'alpha project', summary: {totalEntries: 1, projectEntries: 1, feedbackEntries: 0, latestMemoryAt: '2026-03-01T00:00:00.000Z'}}),
      catalogEntry({projectId: 'p2', logicalProjectId: 'local:p2', projectName: 'Beta', description: 'beta project', updatedAt: '2025-01-01T00:00:00.000Z'}),
      catalogEntry({projectId: 'p3', logicalProjectId: 'local:p3', projectName: 'Gamma', description: '', sourceType: 'workspace_external', summary: {totalEntries: 0, projectEntries: 0, feedbackEntries: 0, latestMemoryAt: ''}}),
    ], 'alpha', [message('assistant', 'beta gamma'), message('user', 'alpha')]],
    [[
      catalogEntry({projectName: 'One', description: 'x', updatedAt: 'a', summary: {totalEntries: 0, projectEntries: 0, feedbackEntries: 0}}),
      catalogEntry({projectId: 'p2', logicalProjectId: 'local:p2', projectName: 'Two', description: 'x', updatedAt: 'B', summary: {totalEntries: 0, projectEntries: 0, feedbackEntries: 0}}),
    ], 'zzz', []],
    [[
      catalogEntry({projectName: 'Same', description: 'x', updatedAt: 'same', summary: {totalEntries: 0, projectEntries: 0, feedbackEntries: 0}}),
      catalogEntry({projectId: 'p2', logicalProjectId: 'local:p2', projectName: 'Same2', description: 'x', updatedAt: 'same', summary: {totalEntries: 0, projectEntries: 0, feedbackEntries: 0}}),
    ], '项目', [message('user', '当前项目')]],
    [[
      catalogEntry({projectName: 'Accent', description: 'x', updatedAt: 'ä', summary: {totalEntries: 0, projectEntries: 0, feedbackEntries: 0}}),
      catalogEntry({projectId: 'p2', logicalProjectId: 'local:p2', projectName: 'Ascii', description: 'x', updatedAt: 'z', summary: {totalEntries: 0, projectEntries: 0, feedbackEntries: 0}}),
    ], 'zzz', []],
    [allCatalog, 'project', [message('user', 'description')]],
    [[catalogEntry({projectName: '产品', description: '中文 项目', summary: {totalEntries: 0, projectEntries: 0, feedbackEntries: 0}})], '产品 项目', []],
  ];
  for (const [catalog, query, recent] of shortlistCases) {
    add('buildProjectShortlist', [catalog, query, recent], helpers.buildProjectShortlist(catalog, query, recent));
  }

  // -- createTrace / pushStep ----------------------------------------------
  for (const [query, mode, random] of [
    ['q', 'auto', 0.123456789],
    ['', 'explicit', 0.5],
    ['产品', 'auto', 0.000001],
  ] as const) {
    const token = tokenOf(random);
    add('createTrace', [query, mode, token],
      withRandom(random, () => helpers.createTrace(query, mode)));
    const steps = [
      {kind: 'recall', title: 'Recall', status: 'info', inputSummary: 'in', outputSummary: 'out', options: {}},
      {kind: 'select', title: 'Select', status: 'success', inputSummary: 'in2', outputSummary: 'out2',
        options: {
          refs: {a: 1, b: '\\u4e2d'},
          metrics: {ms: 2.5},
          details: [{key: 'd1', label: 'L1', kind: 'list', items: ['\\u4e2d']}],
          promptDebug: {requestLabel: 'r', systemPrompt: 's', userPrompt: 'u', rawResponse: 'raw'},
          titleI18n: {key: 't', fallback: 'T'},
          inputSummaryI18n: {key: 'i', fallback: 'I', args: ['a']},
          outputSummaryI18n: {key: 'o', fallback: 'O'},
        }},
      {kind: 'render', title: 'Render', status: 'skipped', inputSummary: '', outputSummary: '\\n', options: {refs: {}}},
    ];
    add('traceScenario', [{query, mode, token, steps}],
      withRandom(random, () => {
        const trace = helpers.createTrace(query, mode);
        for (const step of steps) helpers.pushStep(trace, step.kind, step.title, step.status, step.inputSummary, step.outputSummary, step.options);
        return trace;
      }));
  }

  // -- fallbackSelection ----------------------------------------------------
  const manifest = [
    {relativePath: 'a.md'}, {relativePath: 'b.md'}, {relativePath: 'c.md'}, {relativePath: 'd.md'},
  ];
  for (const [route, entries] of [
    ['user', []], ['user', manifest], ['project', manifest], ['mix', manifest.slice(0, 2)],
    ['none', manifest],
  ] as const) {
    add('fallbackSelection', [route, entries], helpers.fallbackSelection(route, entries));
  }

  return {cases, clockMs, locale};
}
