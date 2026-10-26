// Calls the fixed cc7c4e6 `core/pipeline/heartbeat.ts` directly. Every
// expected value in oracle.json comes from these calls; none is synthesized by
// the Rust side.
import assert from 'node:assert/strict';
import {existsSync, readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {dirname, join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';

export const NOW = '2026-10-22T00:00:00.000Z';
export const HOME = '/fixture/home';
const ROOT = 'src/context/memory/edgeclaw-memory-core/src';
const PRIVATE = [
  'sameMessage', 'hasNewContent', 'tokenizeSearchText',
  'buildGeneralProjectShortlist', 'buildCandidateRoutingQuery',
  'buildCandidateMemoryPreview', 'flattenBatchMessages', 'commonPrefixLength',
  'mergeSessionMessages', 'deriveFocusTurns', 'buildIndexTraceId',
  'normalizeTrigger', 'previewText', 'inferStorageKind',
  'exposeStoredRelativePath', 'textDetail', 'noteDetail', 'listDetail',
  'kvDetail', 'jsonDetail', 'createStep', 'createBatchTrace',
];

export function enc(v: any): any {
  if (v === undefined) return {$undefined: true};
  if (typeof v === 'number' && (Object.is(v, -0) || !Number.isFinite(v))) {
    return {$number: Object.is(v, -0) ? '-0' : String(v)};
  }
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

async function transformed(baseline: string) {
  const sourcePath = join(baseline, ROOT, 'core/pipeline/heartbeat.ts');
  const requireFromBaseline = createRequire(join(baseline, 'package.json'));
  const {transformSync} = createRequire(requireFromBaseline.resolve('tsx'))('esbuild');
  const source = readFileSync(sourcePath, 'utf8');
  const code = transformSync(
    `${source}\nexport { ${PRIVATE.join(', ')} };\n`,
    {loader: 'ts', format: 'esm', target: 'es2022', sourcefile: sourcePath},
  ).code;
  const executable = code.replace(
    /from (["'])(\.[^"']+)\1/g,
    (_: string, quote: string, specifier: string) => {
      let file = resolve(dirname(sourcePath), specifier);
      if (file.endsWith('.js') && existsSync(file.slice(0, -3) + '.ts')) file = file.slice(0, -3) + '.ts';
      return 'from ' + quote + pathToFileURL(file).href + quote;
    },
  );
  return import('data:text/javascript;base64,' + Buffer.from(executable).toString('base64'));
}

export async function collect(baseline: string) {
  process.env.HOME = HOME;
  freezeClock();
  const m: any = await transformed(baseline);
  const cases: any[] = [];
  const add = (group: string, fn: string, args: any[], expected: any) =>
    cases.push({group, fn, args: enc(args), expected: enc(clean(expected))});
  const call = (group: string, fn: string, ...args: any[]) =>
    add(group, fn, args, m[fn](...args));
  // `kvDetail` is a pure constructor that lets `String(value)` throw; pin both
  // the returned detail and the JS error name/message.
  const callCatching = (group: string, fn: string, ...args: any[]) => {
    const target = fn === 'kvDetailCatching' ? 'kvDetail' : fn;
    let expected: any;
    try {
      expected = {ok: m[target](...args)};
    } catch (error) {
      const thrown = error as Error;
      expected = {thrown: {name: thrown.name, message: thrown.message}};
    }
    add(group, fn, args, expected);
  };

  // -- messages -------------------------------------------------------------
  const msg = (role: string, content: string, msgId?: string) =>
    msgId === undefined ? {role, content} : {role, content, msgId};
  const u = (content: string) => msg('user', content);
  const a = (content: string) => msg('assistant', content);
  const sameCases: Array<[any, any]> = [
    [undefined, undefined], [undefined, u('x')], [u('x'), undefined],
    [u('x'), u('x')], [u('x'), u('y')], [u('x'), a('x')], [a('x'), a('x')],
    [msg('user', 'x', 'm1'), msg('user', 'x', 'm2')], [u(''), u('')],
    [u('产品🚀'), u('产品🚀')], [u('a\ud800b'), u('a\ud800b')],
    // ChatRole is any string; comparison keeps the role.
    [msg('system', 'x'), msg('system', 'x')],
    [msg('system', 'x'), msg('user', 'x')],
    [msg('system', 'x'), msg('system', 'y')],
    [msg('tool', 'x'), msg('tool', 'x')],
    [msg('developer', 'x'), msg('developer', 'x')],
  ];
  for (const [left, right] of sameCases) call('messages', 'sameMessage', left, right);

  const newCases: Array<[any[], any[]]> = [
    [[], []], [[], [u('a')]], [[u('a')], []], [[u('a')], [u('a')]],
    [[u('a')], [u('a'), a('b')]], [[u('a'), a('b')], [u('a')]],
    [[u('a')], [u('b')]], [[u('a')], [a('a')]],
    [[u('a'), a('b')], [u('a'), a('c')]],
    [[u('a'), a('b')], [u('a'), a('b'), u('c')]],
    [[u('a')], [msg('user', 'a', 'm1')]],
    [[u('产品')], [u('产品')]],
    [[msg('system', 'a')], [msg('system', 'a'), u('q')]],
    [[msg('system', 'a')], [msg('system', 'b')]],
  ];
  for (const [previous, incoming] of newCases) call('messages', 'hasNewContent', previous, incoming);

  // -- tokenizeSearchText ---------------------------------------------------
  const tokenTexts = [
    '', ' ', '\n\t',
    '项目 当前 这个 那个 现在 一下 关于 里面 这里 那里 怎么 怎样 如何 什么 哪些 进展 情况 默认 应该 需要 general',
    '产品指南', '产品指南手册', '项目产品', '项目产品指南',
    'Memory 产品指南！General 2026',
    '中文English混排', '中文 English 混排',
    'a AB A1 1A 12 １A ＡＢ', 'Straße STRASSE İI ı', 'İstanbul ısparta',
    '！？。，、；：（）【】', '\uFEFF\u00A0\u3000a\u2028b\u2029c',
    '🚀产品🚀', 'e\u0301lan café', '１２３٤٥ Ⅻ',
    '一二三四五六七八', '一二三四五六七八九', '一二三四五六七八九十',
    '𠀀𠀁𠀂', 'a\ud800b 产品\ud801指南',
    '用户需要当前项目的进展和情况',
    '产品-指南_手册.v2', 'A_B-C.D', 'a--b', '中a文',
  ];
  for (const value of tokenTexts) call('token', 'tokenizeSearchText', value);

  // -- General project shortlist -------------------------------------------
  const project = (id: string, name: string, description: string, updatedAt: string,
                   extra: any = {}) => ({
    logicalProjectId: id, projectName: name, description, status: 'in_progress',
    updatedAt, sourceType: 'general_local', summary: {}, ...extra,
  });
  const catalog = [
    project('p-alpha', 'Alpha', 'memory search engine', '2026-10-20T00:00:00.000Z',
            {summary: {latestMemoryAt: '2026-10-22T00:00:00.000Z'}}),
    project('p-beta', 'Beta', '产品指南 handbook', '2026-10-21T00:00:00.000Z'),
    project('p-gamma', 'Gamma', 'alpha alpha alpha', '2026-10-19T00:00:00.000Z',
            {sourceType: 'workspace_external'}),
    project('p-delta', 'Delta', 'unrelated', '2026-10-22T00:00:00.000Z',
            {sourceType: 'workspace_external_mirror', summary: {latestMemoryAt: ''}}),
    project('p-epsilon', 'EPSILON', 'Mixed Case', '2026-10-18T00:00:00.000Z'),
    project('p-zeta', 'Zeta', '', '2026-10-22T00:00:00.000Z'),
  ];
  // Same score + same updatedAt exercises the stable-sort tie.
  catalog.push(project('p-tie-1', 'Tie One', 'tie', '2026-10-22T00:00:00.000Z'));
  catalog.push(project('p-tie-2', 'Tie Two', 'tie', '2026-10-22T00:00:00.000Z'));
  // 25 more entries prove the 30-entry cut and updatedAt ordering.
  for (let index = 0; index < 25; index += 1) {
    const day = String(1 + (index % 9)).padStart(2, '0');
    catalog.push(project(
      `p-bulk-${index}`, `Bulk ${String(index).padStart(2, '0')}`, `bulk memory ${index}`,
      `2026-09-${day}T${String(index % 24).padStart(2, '0')}:00:00.000Z`,
    ));
  }
  const shortlistTexts = [
    'Alpha project guide', 'alpha', '产品指南', 'memory', 'bulk 07', '',
    'Nothing Matches This', 'EPSILON mixed case', 'tie', '产品',
  ];
  for (const value of shortlistTexts) {
    add('shortlist', 'buildGeneralProjectShortlist', [catalog, value],
        m.buildGeneralProjectShortlist(catalog, value));
  }
  add('shortlist', 'buildGeneralProjectShortlist', [[], 'alpha'],
      m.buildGeneralProjectShortlist([], 'alpha'));
  const minimal = [{logicalProjectId: 'x', projectName: 'X', description: '', status: 'planned', updatedAt: '2026-01-01T00:00:00.000Z', sourceType: 'general_local', summary: {}}];
  add('shortlist', 'buildGeneralProjectShortlist', [minimal, 'x'],
      m.buildGeneralProjectShortlist(minimal, 'x'));

  // -- candidate routing query / preview -----------------------------------
  const candidate = (extra: any = {}) => ({
    type: 'project', scope: 'project', name: 'Candidate', description: 'desc',
    ...extra,
  });
  const focus = u('focus turn');
  const routingCandidates = [
    candidate(),
    candidate({rule: 'r', summary: 's', stage: 'st', constraints: ['c1', 'c2'],
               decisions: ['d1'], blockers: ['b1']}),
    candidate({name: '', description: '', rule: '', summary: '', stage: '',
               constraints: [], decisions: [], blockers: []}),
    candidate({constraints: ['', 'x'], decisions: ['y', '']}),
    candidate({type: 'feedback', rule: 'prefer terse', why: 'why', howToApply: 'apply'}),
    candidate({type: 'user', body: 'body text'}),
    candidate({description: 'line1\nline2'}),
  ];
  for (const value of routingCandidates) {
    call('routing', 'buildCandidateRoutingQuery', value, focus);
    call('preview', 'buildCandidateMemoryPreview', value);
  }
  call('routing', 'buildCandidateRoutingQuery', candidate({}), u('line1\nline2'));
  call('routing', 'buildCandidateRoutingQuery', candidate({}), u(''));

  const projectPreview = candidate({
    type: 'project', name: 'P', description: 'D', stage: ' S ',
    decisions: [' d1 ', '', '  '], constraints: ['c1'], nextSteps: ['n1', 'n2'],
    blockers: ['b1'], timeline: ['t1'], notes: [' note '], summary: 'S',
    body: 'B',
  });
  call('preview', 'buildCandidateMemoryPreview', projectPreview);
  call('preview', 'buildCandidateMemoryPreview', candidate({
    type: 'project', name: 'Empty', description: '', decisions: [], constraints: [],
    nextSteps: [], blockers: [], timeline: [], notes: [], summary: '', body: '',
  }));
  call('preview', 'buildCandidateMemoryPreview', candidate({
    type: 'feedback', name: 'F', description: '', rule: '', why: '', howToApply: '',
  }));
  call('preview', 'buildCandidateMemoryPreview', candidate({
    type: 'project', name: 'Long', description: 'D', body: 'x'.repeat(4000),
  }));
  call('preview', 'buildCandidateMemoryPreview', candidate({
    type: 'feedback', name: 'LongRule', description: '', rule: 'y'.repeat(3500),
  }));

  // -- flatten / merge / prefix --------------------------------------------
  const session = (id: string, timestamp: string, messages: any[]) =>
    ({l0IndexId: id, sessionKey: 's', timestamp, messages, source: 'openclaw', indexed: false, createdAt: '2026-10-22T00:00:00.000Z'});
  const mergeCases: Array<[any[], any[]]> = [
    [[], []], [[], [u('a')]], [[u('a')], []],
    [[u('a')], [u('a')]], [[u('a')], [u('a'), a('b')]],
    [[u('a'), a('b')], [u('a')]], [[u('a')], [u('b')]],
    [[u('a'), a('b'), u('c')], [u('a'), a('b'), u('d')]],
    [[u('a'), a('b')], [u('x'), a('b')]],
    [[u('a'), a('b')], [u('a'), a('b'), u('c')]],
    [[msg('user', 'a', 'm1')], [msg('user', 'a', 'm1')]],
    [[msg('user', 'a', 'm1')], [msg('user', 'a', 'm2')]],
    // Non-user/assistant roles participate in the common prefix.
    [[msg('system', 'A')], [msg('system', 'A'), u('q')]],
    [[msg('system', 'A'), u('q')], [msg('system', 'B'), u('q')]],
    [[msg('tool', 'x')], [msg('tool', 'x')]],
    [[msg('developer', 'x')], [u('x')]],
  ];
  for (const [previous, incoming] of mergeCases) {
    call('merge', 'commonPrefixLength', previous, incoming);
    call('merge', 'mergeSessionMessages', previous, incoming);
  }
  const sessions = [
    session('l0-1', '2026-10-20T00:00:00.000Z', [u('a'), a('b')]),
    session('l0-2', '2026-10-21T00:00:00.000Z', [u('a'), a('b'), u('c')]),
    session('l0-3', '2026-10-22T00:00:00.000Z', [a('d')]),
    session('l0-4', '', [u('x'), u('y')]),
  ];
  const flattenCases: Array<[any[], any[]]> = [
    [[], []], [sessions, []], [[], [u('seed')]], [sessions, [u('seed')]],
    [[sessions[0]], [u('seed')]], [[sessions[3]], [u('a')]],
    [[session('dup', 't', [u('q')]), session('dup', 't', [a('r')])], []],
  ];
  const focusCase = (previous: any[], value: any[]) => {
    const map = m.deriveFocusTurns(previous, value);
    add('focus', 'deriveFocusTurns', [previous, value],
        Array.from(map, ([l0IndexId, newMessages]) => ({l0IndexId, newMessages})));
  };
  for (const [value, seed] of flattenCases) {
    call('flatten', 'flattenBatchMessages', value, seed);
    focusCase(seed, value);
  }
  call('flatten', 'flattenBatchMessages', sessions);
  focusCase([u('a'), a('b')], sessions);
  // P1-1: the same user text under different system prompts is not a shared
  // prefix, so the second session's user turn is still a new focus turn.
  const roleSessions = [
    session('role-1', '2026-10-20T00:00:00.000Z', [msg('system', 'A'), u('question')]),
    session('role-2', '2026-10-21T00:00:00.000Z', [msg('system', 'B'), u('question')]),
  ];
  const roleSeeds = [
    [],
    [msg('system', 'A'), u('question')],
    [msg('system', 'A'), u('question'), a('answer')],
  ];
  for (const seed of roleSeeds) {
    call('flatten', 'flattenBatchMessages', roleSessions, seed);
    focusCase(seed, roleSessions);
  }
  // Duplicate ids still use `Map.set` semantics after a role-preserving merge.
  const duplicateRoleSessions = [
    session('role-dup', '2026-10-20T00:00:00.000Z', [msg('tool', 't'), u('one')]),
    session('role-dup', '2026-10-21T00:00:00.000Z', [msg('system', 'S'), u('two')]),
  ];
  call('flatten', 'flattenBatchMessages', duplicateRoleSessions, []);
  focusCase([], duplicateRoleSessions);

  // -- trace ids / trigger / preview ---------------------------------------
  const traceIdCases: Array<[string, string, string[]]> = [
    ['session', NOW, []], ['session', NOW, ['l0-a']],
    ['s', '2026-10-22T00:00:00.000Z', ['b', 'a']],
    ['', '', ['']], ['会话🚀', 't\ud800', ['\ud801', 'x']],
  ];
  for (const [sessionKey, startedAt, ids] of traceIdCases) {
    call('trace', 'buildIndexTraceId', sessionKey, startedAt, ids);
  }
  for (const reason of [undefined, '', '   ', 'manual', ' manual ', 'SCHEDULED',
                        'scheduled run', 'not scheduled really', '安排 scheduled',
                        '\uFEFF scheduled \uFEFF', 'sched']) {
    call('trace', 'normalizeTrigger', reason);
  }
  const previewValues = [
    '', '   ', ' a \n\t b ', '\\u0041\\n\\u4e2d', 'a'.repeat(220),
    'a'.repeat(221), '产品'.repeat(120), 'x\ud800y', 'a\r\nb\u2028c',
  ];
  for (const value of previewValues) {
    call('trace', 'previewText', value);
    call('trace', 'previewText', value, 180);
  }
  call('trace', 'previewText', 'a'.repeat(30), 10);
  call('trace', 'previewText', 'a'.repeat(30), 0);

  const storageCases: Array<[string, string]> = [
    ['general_project_meta', 'GeneralProjects/x.md'],
    ['user', 'UserNotes/x.md'], ['user', '/UserNotes/x.md'],
    ['user', 'a/UserNotes/x.md'], ['user', 'UserIdentityNotes/x.md'],
    ['user', 'x/UserIdentityNotes/y.md'], ['user', 'userNotes/x.md'],
    ['user', 'x/UserNotes'], ['user', 'USER.md'],
    ['feedback', 'Feedback/x.md'], ['project', 'Project/x.md'],
    ['other', 'Other/x.md'], ['', ''],
  ];
  for (const [type, relativePath] of storageCases) {
    call('trace', 'inferStorageKind', {type, relativePath});
  }
  for (const scope of ['global', 'project', '']) {
    call('trace', 'exposeStoredRelativePath', {scope, relativePath: 'dir/UserNotes/x.md'});
    call('trace', 'exposeStoredRelativePath', {scope, relativePath: ''});
  }

  // -- detail constructors --------------------------------------------------
  const i18n = {key: 'trace.detail.example', fallback: 'Example {0}', args: [1]};
  call('detail', 'textDetail', 'k', 'L', '\\u0041\\n');
  call('detail', 'textDetail', 'k', 'L', '产品', i18n);
  call('detail', 'noteDetail', 'k', 'L', '\\n note ');
  call('detail', 'noteDetail', 'k', 'L', 'x', {key: 'x'});
  call('detail', 'listDetail', 'k', 'L', ['a', '\\u4e2d', '']);
  call('detail', 'listDetail', 'k', 'L', [], i18n);
  const kvEntries = [
    {label: 'string', value: '\\u0041'},
    {label: 'empty', value: ''},
    {label: 'null', value: null},
    {label: 'undefined', value: undefined},
    {label: 'number', value: 42},
    {label: 'float', value: 1.5},
    {label: 'zero', value: 0},
    {label: 'negzero', value: -0},
    {label: 'bool', value: true},
    {label: 'false', value: false},
    {label: 'array', value: [1, null, 'x']},
    {label: 'object', value: {a: 1}},
  ];
  call('detail', 'kvDetail', 'k', 'L', kvEntries);
  call('detail', 'kvDetail', 'k', 'L', [], i18n);
  // `String(entry.value ?? "")` throws for a wire object with an own
  // `toString` (JSON cannot hold a callable) and joins arrays through the
  // same conversion.
  for (const value of [
    {toString: null},
    [{toString: null}],
    [1, [{toString: null}]],
    {toString: 'not callable'},
  ]) {
    callCatching('detail', 'kvDetailCatching', 'k', 'L', [{label: 'throw', value}]);
  }
  callCatching('detail', 'kvDetailCatching', 'k', 'L', [
    {label: 'plain', value: {a: 1}},
    {label: 'nullish', value: null},
  ]);
  // 2001 containers are TS-success / Rust-RangeError under the already
  // registered A6-JSVALUE-DEPTH-001 shared-bound divergence. Store only the
  // depth and the real TS value so the oracle itself stays shallow.
  const nested = (depth: number) => {
    let value: any = '1';
    for (let index = 0; index < depth; index += 1) value = [value];
    return value;
  };
  for (const depth of [1999, 2000, 2001]) {
    const detail = m.kvDetail('k', 'L', [{label: 'deep', value: nested(depth)}]);
    add('detail-depth', 'kvDetailDepthProbe', [depth],
        {depth, tsValue: detail.entries[0].value});
  }
  call('detail', 'jsonDetail', 'k', 'L', {'\\u0041': '\\u0042', nested: ['\\u0043', 1, null]});
  call('detail', 'jsonDetail', 'k', 'L', undefined);
  call('detail', 'jsonDetail', 'k', 'L', null, i18n);

  // -- createStep -----------------------------------------------------------
  const startTrace = (steps: any[] = []) => ({
    indexTraceId: 'index_trace_test', sessionKey: 's', trigger: 'manual_sync',
    startedAt: NOW, status: 'running', isNoOp: false, displayStatus: 'Running',
    batchSummary: {l0Ids: [], segmentCount: 0, focusUserTurnCount: 0,
                   fromTimestamp: '', toTimestamp: ''},
    steps, storedResults: [],
  });
  const stepCases: any[][] = [
    [startTrace(), 'index_start', 'Index Started', 'info', 'in', 'out', {}],
    [startTrace(), 'batch_loaded', 'Batch Loaded', 'success', 'in', 'out',
     {refs: {a: 1}, metrics: {n: 2}, details: [m.textDetail('d', 'D', 'x')]}],
    [startTrace(), 'classification', 'Classification', 'skipped', 'in', 'out',
     {refs: {}, metrics: {}, details: [], promptDebug: {requestLabel: 'r'},
      titleI18n: i18n, inputSummaryI18n: i18n, outputSummaryI18n: i18n}],
    [startTrace(), 'persist', 'Persist', 'warning', 'in', 'out',
     {refs: undefined, metrics: null, details: undefined, promptDebug: null,
      titleI18n: {}, inputSummaryI18n: null, outputSummaryI18n: undefined}],
    [startTrace([{stepId: 'existing'}]), 'index_finished', 'Finished', 'error', 'in', 'out', {}],
  ];
  for (const [trace, kind, title, status, input, output, options] of stepCases) {
    const initial = structuredClone(trace);
    m.createStep(trace, kind, title, status, input, output, options);
    add('step', 'createStep', [initial, kind, title, status, input, output, options], trace);
  }
  const sequence = startTrace();
  const sequenceSteps = [
    ['index_start', 'One', 'info', 'i', 'o', {}],
    ['batch_loaded', 'Two', 'info', 'i', 'o', {}],
    ['index_finished', 'Three', 'success', 'i', 'o', {}],
  ];
  const sequenceInitial = startTrace();
  for (const step of sequenceSteps) m.createStep(sequence, ...step);
  add('step', 'createStepSequence', [sequenceInitial, sequenceSteps], sequence);

  // -- createBatchTrace -----------------------------------------------------
  const batchSessions = [
    session('l0-b', '2026-10-22T00:00:00.000Z', [u('b')]),
    session('l0-a', '2026-10-20T00:00:00.000Z', [u('a')]),
    session('l0-empty', '', []),
    session('l0-c', '2026-10-21T00:00:00.000Z', []),
  ];
  const batchCases: Array<[string, any[], string, number]> = [
    ['s', [], 'manual_sync', 0],
    ['session', batchSessions, 'scheduled', 3],
    ['', [session('l0-only', '', [])], 'manual_sync', 0],
    ['会话🚀', [batchSessions[0]], 'manual_sync', 1],
  ];
  for (const [sessionKey, value, trigger, count] of batchCases) {
    add('batch', 'createBatchTrace', [sessionKey, value, trigger, count],
        m.createBatchTrace(sessionKey, value, trigger, count));
  }
  return {cases, clockMs: Date.parse(NOW)};
}
