import assert from 'node:assert/strict';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {stub} from '../edgeclaw-llm/stub.mts';
export const ENTRY = 'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts';
const tick = () => new Promise<void>(resolve => setImmediate(resolve));
const ok = (api: string, text: string) => ({status: 200, body: JSON.stringify(
  api === 'responses' || api === 'openai-responses' ? {output_text: text} :
  api === 'anthropic' ? {content: [{text}]} :
  {choices: [{message: {content: text}}]},
)});
const bad = (status: number) => ({status, body: 'bad'});
const JSON_RESULT = (labels: any[], should_store: unknown = true) =>
  JSON.stringify({should_store, labels});
const LABEL = (type: string, reason = `${type} reason`, evidence = `${type} evidence`) => ({type, reason, evidence});

// Five turns; the focus is turn index 2 (u3), giving 2 previous and 2 next turns.
const BATCH = [
  {role: 'user', content: 'u1 我在做 Rust 移植'},
  {role: 'assistant', content: 'a1 好的'},
  {role: 'user', content: 'u2 请记住我是达达，一名工程师'},
  {role: 'assistant', content: 'a2 已记住'},
  {role: 'user', content: 'u3 另外回复请用中文'},
  {role: 'assistant', content: 'a3 好的'},
  {role: 'user', content: 'u4 明天继续'},
  {role: 'assistant', content: 'a4 行'},
  {role: 'user', content: 'u5 下班了'},
  {role: 'assistant', content: 'a5 拜'},
];
const META_FULL = {projectId: 'p-1', projectName: '项目一', description: 'meta 描述 😀', status: 'active', updatedAt: '2026-01-02T03:04:05.678Z'};
const CLASSIFICATION = {type: 'user', reason: 'identity', evidence: '我是达达'};
const NOTE_OK = JSON.stringify({skip: false, reason: '', name: '笔记名', description: '一行描述', markdown: '# 正文\n\n- 要点'});
// A raw unpaired surrogate: every recording layer JSON.stringify-escapes it,
// so it reaches the wire (and the oracle) as ASCII `\\ud800` and only the
// inner parse sees the code unit itself.
const SURROGATE = String.fromCharCode(0xd800);
const SURROGATE_LOW = String.fromCharCode(0xdc00);
const BACKSLASH = String.fromCharCode(0x5c);
const SUR_NOTE = JSON.stringify({name: 'X', description: 'd', markdown: 'm'}).replace('X', SURROGATE);
const SUR_LABELS = JSON.stringify({should_store: true, labels: [{type: 'user', reason: 'X', evidence: 'e'}]}).replace('"reason":"X"', `"reason":"${SURROGATE}"`);
// `JSON.stringify` would escape both the backslash and a lone surrogate, so
// the raw `\<surrogate>` sequence — an invalid JSON escape that V8 rejects
// while an even backslash run stays legal (fix-r3 ts-probe) — is spliced in
// after stringifying, exactly like the lone surrogate above.
const bsLabels = (reason: string) =>
  JSON.stringify({should_store: true, labels: [{type: 'user', reason: 'X', evidence: 'e'}]}).replace('"reason":"X"', `"reason":"${reason}"`);
const BS_HIGH_LABELS = bsLabels(BACKSLASH + SURROGATE);
const BS_LOW_LABELS = bsLabels(BACKSLASH + SURROGATE_LOW);
const BS_PAIR_LABELS = bsLabels(BACKSLASH + BACKSLASH + SURROGATE);
const bsDecision = (reason: string) =>
  JSON.stringify({decision: 'create_new', selected_project_id: '', reason: 'X'}).replace('"reason":"X"', `"reason":"${reason}"`);
const BS_HIGH_DECISION = bsDecision(BACKSLASH + SURROGATE);
const BS_PAIR_DECISION = bsDecision(BACKSLASH + BACKSLASH + SURROGATE);
// `metaDeep`/`shortlistDeep` are construction markers: the deep value is
// built on both sides from `{field, depth}` instead of being recorded, both
// because V8's own JSON.stringify cannot record it and because serde_json's
// 128-level recursion limit cannot replay it.
const deepValue = (depth: number) => { let value: any = {}; for (let i = 0; i < depth; i++) value = {x: value}; return value; };
const DEEP = {field: 'projectId', depth: 10000};

function specs() {
  const out: any[] = [];
  // -- classify -------------------------------------------------------------
  const classify = (name: string, steps: any[], extra: Record<string, any> = {}) => out.push({
    name, kind: 'classify', api: 'responses', steps, messages: BATCH,
    focus: {ref: 2}, ...extra,
  });
  classify('classify-basic', [ok('responses', JSON_RESULT([LABEL('user')]))]);
  classify('classify-chat', [ok('chat', JSON_RESULT([LABEL('user')]))], {api: 'chat'});
  classify('classify-all-three-labels', [ok('responses', JSON_RESULT([LABEL('user'), LABEL('project'), LABEL('feedback')]))]);
  classify('classify-dedupe', [ok('responses', JSON_RESULT([LABEL('user', 'first'), LABEL('user', 'second')]))]);
  classify('classify-unknown-type', [ok('responses', JSON_RESULT([LABEL('user'), LABEL('other'), LABEL('project')]))]);
  classify('classify-labels-non-array', [ok('responses', JSON.stringify({should_store: true, labels: LABEL('user')}))]);
  classify('classify-should-store-null', [ok('responses', JSON_RESULT([LABEL('user')], null))]);
  classify('classify-should-store-string', [ok('responses', JSON_RESULT([LABEL('user')], 'yes'))]);
  classify('classify-should-store-zero', [ok('responses', JSON_RESULT([LABEL('user')], 0))]);
  classify('classify-empty-labels', [ok('responses', JSON_RESULT([]))]);
  classify('classify-meta-full', [ok('responses', JSON_RESULT([LABEL('user')]))], {meta: META_FULL});
  classify('classify-meta-description-only', [ok('responses', JSON_RESULT([LABEL('user')]))], {meta: {description: '只有描述'}});
  classify('classify-meta-missing-description', [ok('responses', JSON_RESULT([LABEL('user')]))], {meta: {}});
  classify('classify-meta-description-null', [ok('responses', JSON_RESULT([LABEL('user')]))], {meta: {description: null}});
  classify('classify-meta-falsy-string', [ok('responses', JSON_RESULT([LABEL('user')]))], {meta: ''});
  classify('classify-meta-zero', [ok('responses', JSON_RESULT([LABEL('user')]))], {meta: 0});
  classify('classify-focus-by-reference', [ok('responses', JSON_RESULT([LABEL('project')]))]);
  classify('classify-focus-by-value', [ok('responses', JSON_RESULT([LABEL('project')]))], {focus: {copy: 2}});
  classify('classify-focus-absent', [ok('responses', JSON_RESULT([LABEL('user')]))], {focus: {value: {role: 'user', content: '不在批次里的消息'}}});
  classify('classify-roles-filtered', [ok('responses', JSON_RESULT([LABEL('user')]))], {messages: [
    {role: 'system', content: 'sys'},
    {role: 'user', content: 'u1'},
    {role: 'tool', content: 'tool call'},
    {role: 'assistant', content: 'a1'},
    {role: 'other', content: '别的'},
    {role: 'user', content: 'u2 focus'},
    {role: 'assistant', content: 'a2'},
  ], focus: {ref: 5}});
  classify('classify-assistant-prefix', [ok('responses', JSON_RESULT([LABEL('user')]))], {messages: [
    {role: 'assistant', content: 'leading assistant'},
    {role: 'user', content: 'u1'},
    {role: 'assistant', content: 'a1'},
    {role: 'user', content: 'u2 focus'},
  ], focus: {ref: 3}});
  classify('classify-window-first', [ok('responses', JSON_RESULT([LABEL('user')]))], {focus: {ref: 0}});
  classify('classify-window-last', [ok('responses', JSON_RESULT([LABEL('user')]))], {focus: {ref: 8}});
  classify('classify-truncate', [ok('responses', JSON_RESULT([LABEL('user')]))], {messages: [
    {role: 'user', content: `首:${'长'.repeat(350)}\n\n\t尾`},
    {role: 'assistant', content: `答:${'宽'.repeat(350)}   \t 尾`},
    {role: 'user', content: `焦:${'超'.repeat(450)}\r\n  末尾`},
    {role: 'user', content: '次轮'},
  ], focus: {ref: 2}});
  classify('classify-plain-text', [ok('responses', 'no json here')]);
  classify('classify-surrounding-text', [ok('responses', `当然！${JSON_RESULT([LABEL('project')])} 希望有帮助`)]);
  classify('classify-bad-json', [ok('responses', '{"should_store":}')]);
  classify('classify-control-chars', [ok('responses', '{"should_store":true,"labels":[{"type":"user","reason":"line1\nline2\ttab","evidence":"e"}]}')]);
  classify('classify-http-400', [bad(400)]);
  classify('classify-agent-empty', [ok('responses', JSON_RESULT([LABEL('user')]))], {agentId: ''});
  classify('classify-lone-surrogate-reason', [ok('responses', SUR_LABELS)]);
  classify('classify-backslash-surrogate-high', [ok('responses', BS_HIGH_LABELS)]);
  classify('classify-backslash-surrogate-low', [ok('responses', BS_LOW_LABELS)]);
  classify('classify-backslash-pair-surrogate', [ok('responses', BS_PAIR_LABELS)]);
  classify('classify-meta-description-number', [ok('responses', JSON_RESULT([LABEL('user')]))], {meta: {description: 5}});
  classify('classify-deep-meta', [ok('responses', JSON_RESULT([LABEL('user')]))], {metaDeep: DEEP});
  classify('classify-no-model', [ok('responses', JSON_RESULT([LABEL('user')]))], {noModel: true});

  // -- create ---------------------------------------------------------------
  const create = (name: string, steps: any[], extra: Record<string, any> = {}) => out.push({
    name, kind: 'create', api: 'responses', steps, messages: BATCH,
    focus: {ref: 2}, classification: CLASSIFICATION,
    timestamp: '2026-01-01T00:00:00.000Z', sessionKey: 'sess-1', ...extra,
  });
  create('create-user-basic', [ok('responses', NOTE_OK)]);
  create('create-project-basic', [ok('responses', NOTE_OK)], {classification: {type: 'project', reason: 'r', evidence: 'e'}});
  create('create-feedback-basic', [ok('responses', NOTE_OK)], {classification: {type: 'feedback', reason: 'r', evidence: 'e'}});
  create('create-no-session-key', [ok('responses', NOTE_OK)], {sessionKey: undefined});
  create('create-skip-true', [ok('responses', JSON.stringify({skip: true, reason: '跳过原因'}))]);
  create('create-skip-string', [ok('responses', JSON.stringify({skip: 'true', name: 'n', description: 'd', markdown: 'm'}))]);
  create('create-skip-one', [ok('responses', JSON.stringify({skip: 1, name: 'n', description: 'd', markdown: 'm'}))]);
  create('create-missing-name', [ok('responses', JSON.stringify({description: 'd', markdown: 'm'}))]);
  create('create-empty-description', [ok('responses', JSON.stringify({name: 'n', description: '', markdown: 'm'}))]);
  create('create-markdown-whitespace', [ok('responses', JSON.stringify({name: 'n', description: 'd', markdown: '  \n\t# 正文 段落 \n\t'}))]);
  create('create-generic-project-name', [ok('responses', JSON.stringify({name: 'Overview ', description: 'd', markdown: 'm'}))], {classification: {type: 'project', reason: 'r', evidence: 'e'}});
  create('create-generic-project-lowercase', [ok('responses', JSON.stringify({name: 'PROJECT', description: 'd', markdown: 'm'}))], {classification: {type: 'project', reason: 'r', evidence: 'e'}});
  create('create-generic-user-allowed', [ok('responses', JSON.stringify({name: 'Overview', description: 'd', markdown: 'm'}))]);
  create('create-name-truncated', [ok('responses', JSON.stringify({name: `${'名'.repeat(100)}x`, description: 'd', markdown: 'm'}))]);
  create('create-non-string-fields', [ok('responses', JSON.stringify({name: 5, description: true, markdown: 'm'}))]);
  create('create-loose-fallback', [ok('responses', '{"name":"line1\nline2\ttab","description":"d","markdown":"m"}')]);
  create('create-no-fallback', [ok('responses', '{"name":}')]);
  create('create-envelope-missing', [ok('responses', 'no json here')]);
  create('create-http-400', [bad(400)]);
  create('create-meta-typeerror', [ok('responses', NOTE_OK)], {meta: {projectId: 'p-1'}});
  create('create-meta-ok', [ok('responses', NOTE_OK)], {meta: META_FULL});
  create('create-lone-surrogate-name', [ok('responses', SUR_NOTE)]);
  create('create-meta-description-number', [ok('responses', NOTE_OK)], {meta: {description: 5}});
  create('create-meta-description-bool', [ok('responses', NOTE_OK)], {meta: {description: true}});
  create('create-meta-description-array', [ok('responses', NOTE_OK)], {meta: {description: []}});
  create('create-meta-description-object', [ok('responses', NOTE_OK)], {meta: {description: {}}});
  create('create-deep-meta', [ok('responses', NOTE_OK)], {metaDeep: DEEP});
  create('create-no-model', [ok('responses', NOTE_OK)], {noModel: true});
  create('create-project-no-model', [ok('responses', NOTE_OK)], {noModel: true, classification: {type: 'project', reason: 'r', evidence: 'e'}});
  create('create-feedback-no-model', [ok('responses', NOTE_OK)], {noModel: true, classification: {type: 'feedback', reason: 'r', evidence: 'e'}});

  // -- select ---------------------------------------------------------------
  const CANDIDATE_RICH = {
    type: 'project', name: '候选 项目', description: '候选描述',
    rule: '回复用中文', summary: '摘要', why: '原因', howToApply: '应用', stage: '阶段',
    decisions: ['决定一', '决定二', ...Array.from({length: 11}, (_, i) => `d${i}`)],
    constraints: ['约束一'], nextSteps: ['下一步'], blockers: ['阻塞'], timeline: ['时间线'], notes: ['备注'],
  };
  const CANDIDATE_MINIMAL = {type: 'user', name: '最小候选', description: '最小描述'};
  const SHORTLIST_RICH = [
    {projectId: 'p1', projectName: '项目一', description: '描述一', status: 'active', updatedAt: '2026-01-02T03:04:05.678Z', score: 0.9, exact: 1, source: 'query', matchedText: '匹配 文本'},
    {projectId: 'p2', projectName: '项目二', description: '描述二', status: 'archived', updatedAt: '2025-12-31T23:59:59.999Z', score: 0.5, exact: 0, source: 'recent', matchedText: 'p2 匹配'},
  ];
  const RECENT = Array.from({length: 6}, (_, i) => ({role: 'user', content: `最近消息 ${i + 1}`}));
  const select = (name: string, steps: any[], extra: Record<string, any> = {}) => out.push({
    name, kind: 'select', api: 'responses', steps, messages: BATCH, focus: {ref: 2},
    candidate: CANDIDATE_RICH, candidatePreview: '## 候选预览\n正文 😀', recentUserMessages: RECENT,
    shortlist: SHORTLIST_RICH, ...extra,
  });
  const DECISION = (decision: unknown, selected_project_id: unknown, reason: unknown) =>
    JSON.stringify({decision, selected_project_id, reason});
  select('select-attach', [ok('responses', DECISION('attach_existing', 'p1', '匹配项目一'))]);
  select('select-create-new', [ok('responses', DECISION('create_new', '', '新项目'))]);
  select('select-create-new-no-reason', [ok('responses', DECISION('create_new', ''))]);
  select('select-attach-invalid-id', [ok('responses', DECISION('attach_existing', 'pX', '理由仍在'))]);
  select('select-attach-invalid-id-no-reason', [ok('responses', DECISION('attach_existing', 'pX'))]);
  select('select-attach-id-trim', [ok('responses', DECISION('attach_existing', ' p1 ', 'trim 后命中'))]);
  select('select-attach-no-reason', [ok('responses', DECISION('attach_existing', 'p2'))]);
  select('select-reason-blank', [ok('responses', DECISION('create_new', '', '   '))]);
  select('select-reason-non-string', [ok('responses', DECISION('create_new', '', 5))]);
  select('select-decision-non-string', [ok('responses', DECISION(5, 'p1', 'r'))]);
  select('select-decision-case', [ok('responses', DECISION('ATTACH_EXISTING', 'p1', 'r'))]);
  select('select-selected-id-non-string', [ok('responses', DECISION('create_new', 5, 'r'))]);
  select('select-selected-id-absent', [ok('responses', JSON.stringify({decision: 'create_new'}))]);
  select('select-empty-shortlist', [], {shortlist: []});
  select('select-candidate-rich', [ok('responses', DECISION('attach_existing', 'p1', 'r'))]);
  select('select-candidate-minimal', [ok('responses', DECISION('create_new', '', 'r'))], {candidate: CANDIDATE_MINIMAL, candidatePreview: '最小预览'});
  select('select-candidate-null-fields', [ok('responses', DECISION('create_new', '', 'r'))], {candidate: {type: 'project', name: '名字', description: '描述', rule: null, decisions: undefined}});
  select('select-shortlist-partial', [ok('responses', DECISION('attach_existing', 'p1', 'r'))], {shortlist: [{projectId: 'p1', description: '只有描述', matchedText: '命中 文本'}]});
  select('select-shortlist-missing-description', [ok('responses', DECISION('attach_existing', 'p1', 'r'))], {shortlist: [{projectId: 'p1'}]});
  select('select-no-recent-messages', [ok('responses', DECISION('create_new', '', 'r'))], {recentUserMessages: undefined});
  select('select-http-400', [bad(400)]);
  select('select-bad-json', [ok('responses', '{"decision":}')]);
  select('select-lone-surrogate-reason', [ok('responses', DECISION('create_new', '', SURROGATE))]);
  select('select-backslash-surrogate-high', [ok('responses', BS_HIGH_DECISION)]);
  select('select-backslash-pair-surrogate', [ok('responses', BS_PAIR_DECISION)]);
  select('select-candidate-name-number', [ok('responses', DECISION('attach_existing', 'p1', 'r'))], {candidate: {type: 'project', name: 5, description: 'd'}});
  select('select-deep-shortlist', [ok('responses', DECISION('create_new', '', 'r'))], {shortlistDeep: DEEP});
  select('select-no-model', [ok('responses', DECISION('create_new', '', 'r'))], {noModel: true});
  // Non-array candidate list fields: V8 names the receiver's source
  // expression, so the warning carries `input.candidate.<field>` — never the
  // field's value (probe-truncated-list.out).
  select('select-candidate-decisions-number', [ok('responses', DECISION('attach_existing', 'p1', 'r'))], {candidate: {type: 'project', name: 'n', description: 'd', decisions: 5}});
  select('select-candidate-constraints-string', [ok('responses', DECISION('attach_existing', 'p1', 'r'))], {candidate: {type: 'project', name: 'n', description: 'd', constraints: '字符串值'}});
  select('select-candidate-notes-object', [ok('responses', DECISION('attach_existing', 'p1', 'r'))], {candidate: {type: 'project', name: 'n', description: 'd', notes: {}}});
  return out;
}

function normalized(value: any, origin: string): any {
  if (typeof value === 'string') return value.split(origin).join('$ORIGIN');
  if (Array.isArray(value)) return value.map(v => normalized(v,origin));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalized(v,origin)]));
  return value;
}

export async function collect(baseline: string) {
  assert.equal(process.version,'v22.22.0');
  const home = mkdtempSync(join(tmpdir(),'pdrs-llm-index-home-'));
  const oldHome = process.env.HOME; process.env.HOME = home;
  const oldDateNow = Date.now, oldRandom = Math.random;
  Date.now = () => 1792972800000; Math.random = () => 0.25;
  const {LlmMemoryExtractor} = await import(pathToFileURL(join(baseline,ENTRY)).href);
  const cases: any[] = [];
  try { for (const spec of specs()) {
    let now = 0;
    const debug: any[] = [], warnings: string[] = [], events: any[] = [];
    const server = await stub(spec.steps,()=>now);
    const config = spec.noModel ? {} : {agents:{defaults:{model:{primary:'p/m'}}},models:{providers:{p:{api:spec.api,baseUrl:server.origin+'/v1',apiKey:'fixture-key',models:[{id:'m'}]}}}};
    const runtime = {telemetry:{trackFeatureLoopStage:(event:any)=>events.push(event),trackError:(e:any,event:any)=>events.push({...event,error:e.message})}};
    const extractor:any=new LlmMemoryExtractor(config,runtime,{warn:(message:string)=>warnings.push(message)});
    const meta = spec.meta ?? (spec.metaDeep === undefined ? undefined
      : {description: 'd', [spec.metaDeep.field]: deepValue(spec.metaDeep.depth)});
    const batch = (spec.messages ?? []).map((message:any)=>({role:message.role,content:message.content}));
    const focus = spec.focus.ref !== undefined ? batch[spec.focus.ref]
      : spec.focus.copy !== undefined ? {...batch[spec.focus.copy]}
      : spec.focus.value;
    const input = {
      timestamp: spec.timestamp ?? '2026-01-01T00:00:00.000Z',
      ...(spec.sessionKey === undefined ? {} : {sessionKey: spec.sessionKey}),
      focusUserTurn: focus,
      batchContextMessages: batch,
      ...(meta === undefined ? {} : {currentProjectMeta: meta}),
      ...(spec.agentId === undefined ? {} : {agentId: spec.agentId}),
      debugTrace: (value:any)=>debug.push(value),
    };
    const pending = (async () => {
      if (spec.kind === 'classify') return {ok: await extractor.classifyMemoryTurn(input)};
      if (spec.kind === 'create') {
        const withKind = {...input, classification: spec.classification};
        const call = spec.classification.type === 'user' ? extractor.createUserMemoryNote.bind(extractor)
          : spec.classification.type === 'project' ? extractor.createProjectMemoryNote.bind(extractor)
          : extractor.createFeedbackMemoryNote.bind(extractor);
        return {ok: await call(withKind)};
      }
      return {ok: await extractor.selectIndexProject({
        candidate: spec.candidate,
        candidatePreview: spec.candidatePreview,
        focusTurn: focus,
        ...(spec.recentUserMessages === undefined ? {} : {recentUserMessages: spec.recentUserMessages}),
        shortlist: spec.shortlistDeep === undefined ? spec.shortlist
          : [{description: 'd', matchedText: 'm', [spec.shortlistDeep.field]: deepValue(spec.shortlistDeep.depth)}],
        ...(spec.agentId === undefined ? {} : {agentId: spec.agentId}),
        debugTrace: input.debugTrace,
      })};
    })();
    let result: any;
    const watchdog = setTimeout(() => { throw Error(`watchdog ${spec.name}`); }, 5000);
    try {
      result = await pending;
    } catch (error) {
      result = {error: error instanceof Error ? error.message : String(error), name: error instanceof Error ? error.name : 'Error'};
    } finally {
      clearTimeout(watchdog);
    }
    await tick();
    cases.push({spec, expected: normalized({result, requests: server.requests, debug, warnings, calls: server.requests.length}, server.origin)});
    // Snapshot through JSON: `undefined` spec keys drop exactly as they do in
    // oracle.json, so verify compares the same shapes it records.
    cases[cases.length-1] = JSON.parse(JSON.stringify(cases[cases.length-1]));
    await server.close();
  }} finally {Date.now=oldDateNow;Math.random=oldRandom;if(oldHome===undefined)delete process.env.HOME;else process.env.HOME=oldHome;rmSync(home,{recursive:true,force:true});}
  return {cases};
}
