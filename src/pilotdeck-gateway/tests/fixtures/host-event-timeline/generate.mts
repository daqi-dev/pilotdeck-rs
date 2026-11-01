// #1220: the real cc7c4e6 sidecar runner, protocol, and TurnTimeline
// projection against a scripted module connection and host event queue.
// Everything the consumer sees is recorded unmodified: no clean, no filter.
// A wire step pushes one module event frame; a host step queues host events
// that only a pump tick or the final drains (wire payloads never drain the
// host buffer in Rust, so every host step is followed by a tick before the
// next wire step; the final drains in both).
import assert from 'node:assert/strict';
import {mock} from 'node:test';
import {createRequire, syncBuiltinESMExports} from 'node:module';
import {existsSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, join, relative, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0');
const root = resolve(process.env.PDRS_BASELINE!), output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(root);
// Hash the complete local runtime import closure of the sidecar runner.
// Transpilation removes type-only imports; package/node imports are pinned
// by the baseline installation. The closure must contain both projection
// sources this fixture observes.
const ts = createRequire(join(root, 'package.json'))('typescript');
function runtimeSources(entries: string[]): string[] {
  const seen = new Set<string>();
  const visit = (path: string) => {
    if (seen.has(path)) return;
    assert.ok(path.startsWith('src/'), `source outside baseline src: ${path}`);
    seen.add(path);
    const code = ts.transpileModule(readFileSync(join(root, path), 'utf8'), {
      compilerOptions: {target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext}
    }).outputText;
    const tree = ts.createSourceFile(path, code, ts.ScriptTarget.Latest, true);
    const scan = (node: any) => {
      const specifier = (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) ? node.moduleSpecifier
        : ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword || node.expression.getText(tree) === 'require')
          ? node.arguments[0] : undefined;
      if (specifier && ts.isStringLiteral(specifier) && specifier.text.startsWith('.')) {
        const base = resolve(root, dirname(path), specifier.text);
        const candidates = [base.replace(/\.js$/, '.ts'), base, `${base}.ts`, join(base, 'index.ts')];
        const found = candidates.find(p => existsSync(p) && !p.endsWith('/'));
        assert.ok(found, `unresolved import ${path}: ${specifier.text}`);
        visit(relative(root, found));
      }
      ts.forEachChild(node, scan);
    };
    scan(tree);
  };
  entries.forEach(visit);
  return [...seen].sort();
}
const closure = runtimeSources(['src/agent/modules/transport/agentLoopSidecarClient.ts']);
for (const required of ['src/agent/stream/TurnTimeline.ts', 'src/agent/modules/protocol.ts']) {
  assert.ok(closure.includes(required), `#1220 closure must contain ${required}`);
}
const sourceSha256 = await baselineSourceHashes(root, closure);
const {createAgentLoopSidecarRuntimeFactory} =
  await import(pathToFileURL(join(root, 'src/agent/modules/transport/agentLoopSidecarClient.ts')).href);

mkdirSync(output, {recursive: true});
const flush = async (rounds = 200) => { for (let i = 0; i < rounds; i++) await Promise.resolve(); };
// A child coordinate a sidecar child might attach before the runner strips it.
const CHILD = {version: 1, turnId: 'child-turn', id: 'child-id', order: 0, revision: 0};
const result = () => ({type: 'success', sessionId: 's', turnId: 't', stopReason: 'completed',
  usage: {}, permissionDenials: [], turns: 1,
  startedAt: '1970-01-01T00:00:00.000Z', completedAt: '1970-01-01T00:00:00.000Z'});
const finalPayload = (messages: any[] = [], finalMessage?: any) => ({
  result: finalMessage ? {...result(), finalMessage} : result(), messages,
});

type WireStep = {kind: 'wire'; payload: any};
type HostStep = {kind: 'host'; events: any[]};
type TickStep = {kind: 'tick'; ms: number};
type FinalStep = {kind: 'final'; payload: any};
type Step = WireStep | HostStep | TickStep | FinalStep;
const wire = (payload: any): WireStep => ({kind: 'wire', payload});
const host = (...events: any[]): HostStep => ({kind: 'host', events});
const tick = (ms: number): TickStep => ({kind: 'tick', ms});

interface Script {
  name: string;
  steps: Step[];
  // Light generation-time guards; the oracle itself records actuals.
  check?: (events: any[], returned: any) => void;
}

const scripts: Script[] = [
  {
    name: 'wire-deltas',
    steps: [
      wire({type: 'session_started', sessionId: 's'}),
      wire({type: 'model_request_started', sessionId: 's', turnId: 't', model: 'm', provider: 'p'}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b1', event: {type: 'text_delta', text: 'hello'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b1', event: {type: 'text_delta', text: '😀'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b1', event: {type: 'text_delta', text: 'a\ud800b'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b2', event: {type: 'thinking_delta', text: 'think'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b1', event: {type: 'text_delta', text: ''}}),
    ],
    check(events) {
      const deltas = events.filter(e => e.type === 'model_event');
      // UTF-16 accumulation: 5 + 2 + 3, then the empty delta reports 10.
      assert.deepEqual(deltas.map(e => e.timeline.offset), [0, 5, 7, 0, 10], '#1220 delta offsets');
      assert.deepEqual(deltas.map(e => e.timeline.id), ['b1', 'b1', 'b1', 'b2', 'b1'], '#1220 delta block ids');
      assert.deepEqual(deltas.map(e => e.timeline.order), [0, 0, 0, 1, 0], '#1220 delta orders');
      assert.ok(deltas.every(e => e.moduleId === 'fixture'), '#1220 moduleId on every wire event');
      assert.ok(deltas.every(e => e.timeline.turnId === 't'), '#1220 timeline carries the turn id');
    },
  },
  {
    name: 'wire-boundaries',
    steps: [
      // message_end before any delta: no lastModelPosition, no boundary.
      wire({type: 'model_event', sessionId: 's', turnId: 't', event: {type: 'message_end', finishReason: 'stop'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b1', event: {type: 'text_delta', text: 'x'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', event: {type: 'message_end', finishReason: 'stop'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b2', event: {type: 'text_delta', text: 'y'}}),
      // tool_call_start reserves a provisional slot, then carries the boundary.
      wire({type: 'model_event', sessionId: 's', turnId: 't', event: {type: 'tool_call_start', id: 'tool-1', name: 'read'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', event: {type: 'error', error: {code: 'provider', message: 'boom'}}}),
    ],
    check(events) {
      const [early, , end, , toolStart, error] = events;
      assert.equal(early.streamBoundary, undefined, '#1220 no boundary before the first delta');
      assert.deepEqual(end.streamBoundary, {turnId: 't', through: 0, revision: 3}, '#1220 message_end boundary');
      assert.deepEqual(toolStart.streamBoundary, {turnId: 't', through: 1, revision: 7}, '#1220 tool_call_start boundary');
      assert.deepEqual(error.streamBoundary, {turnId: 't', through: 1, revision: 8}, '#1220 error boundary');
      assert.equal(toolStart.timeline, undefined, '#1220 boundary event carries no timeline');
    },
  },
  {
    name: 'wire-tool-cycle',
    steps: [
      wire({type: 'model_event', sessionId: 's', turnId: 't', event: {type: 'tool_call_start', id: 'tool-1', name: 'read'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', event: {type: 'tool_call_end',
        toolCall: {id: 'tool-1', name: 'read', input: {}, timeline: CHILD, raw: {}}, wasRepaired: false}}),
      wire({type: 'tool_calls_detected', sessionId: 's', turnId: 't',
        calls: [{id: 'tool-1', name: 'read', input: {}, timeline: CHILD}]}),
      wire({type: 'tool_result', sessionId: 's', turnId: 't',
        result: {toolCallId: 'tool-1', content: [{type: 'text', text: 'out', timeline: CHILD}], isError: false}}),
    ],
    check(events) {
      const [, end, detected, toolResult] = events;
      assert.equal(end.event.toolCall.timeline, undefined, '#1220 toolCall child timeline stripped');
      // Promotion rewrites previousId to undefined; JSON drops the key, strict
      // deepEqual keeps it, so the literal pins the in-memory shape.
      assert.deepEqual(detected.calls[0].timeline,
        {version: 1, turnId: 't', id: 'tool:tool-1', order: 0, revision: 1, previousId: undefined},
        '#1220 promotion keeps the reserved position with previousId erased');
      assert.deepEqual(toolResult.timeline, {version: 1, turnId: 't', id: 'result:tool-1', previousId: 'tool:tool-1', order: 1, revision: 2},
        '#1220 tool_result position chains to the published tool');
      // tool_result is not stripped: the child coordinate inside result content survives.
      assert.deepEqual(toolResult.result.content[0].timeline, CHILD, '#1220 tool_result content child timeline survives');
    },
  },
  {
    name: 'wire-provisional-reset',
    steps: [
      wire({type: 'model_event', sessionId: 's', turnId: 't', event: {type: 'tool_call_start', id: 'tool-1', name: 'read'}}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b1', event: {type: 'text_delta', text: 'x'}}),
      // model_request_started drops every provisional position, then passes through.
      wire({type: 'model_request_started', sessionId: 's', turnId: 't', model: 'm', provider: 'p'}),
      wire({type: 'tool_calls_detected', sessionId: 's', turnId: 't',
        calls: [{id: 'tool-1', name: 'read', input: {}}]}),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b1', event: {type: 'text_delta', text: 'y'}}),
    ],
    check(events) {
      const [, , reset, detected] = events;
      assert.equal(reset.timeline, undefined, '#1220 model_request_started stays coordinate-free');
      assert.deepEqual(detected.calls[0].timeline, {version: 1, turnId: 't', id: 'tool:tool-1', previousId: 'b1', order: 2, revision: 4},
        '#1220 after the reset the tool gets a fresh position, not the reserved one');
    },
  },
  {
    name: 'wire-assistant',
    steps: [
      wire({type: 'assistant_message', sessionId: 's', turnId: 't', message: {role: 'assistant',
        content: [
          {type: 'text', text: 'hi', blockId: 'b1', timeline: CHILD},
          {type: 'thinking', text: 't', blockId: 'b2'},
          {type: 'tool_call', id: 'tool-1', name: 'read', input: {}},
        ],
        metadata: {model: 'm', moduleId: 'child'}}}),
    ],
    check(events) {
      const [message] = events;
      const {content, metadata} = message.message;
      assert.deepEqual(content[0].timeline.id, 'b1', '#1220 text block coordinate');
      assert.deepEqual(content[1].timeline.id, 'b2', '#1220 thinking block coordinate');
      assert.deepEqual(content[2].timeline.id, 'tool:tool-1', '#1220 tool_call block coordinate');
      assert.equal(metadata.moduleId, 'fixture', '#1220 module-owned assistant message marked');
      assert.equal(message.moduleId, 'fixture', '#1220 top-level moduleId');
      assert.deepEqual(Object.keys(metadata), ['model', 'moduleId'], '#1220 metadata moduleId replaced in place');
    },
  },
  {
    name: 'wire-tool-results',
    steps: [
      wire({type: 'tool_results_projected', sessionId: 's', turnId: 't', message: {role: 'user',
        content: [{type: 'tool_result', toolCallId: 'tool-1',
          content: [{type: 'text', text: 'out', timeline: CHILD}], isError: false}],
        metadata: {}}}),
      wire({type: 'tool_results_projected', sessionId: 's', turnId: 't', message: {role: 'user',
        content: [{type: 'tool_result_reference', toolCallId: 'tool-2', path: '/p',
          originalBytes: 1, preview: 'pv', hasMore: false}],
        metadata: {}}}),
    ],
    check(events) {
      const [toolResult, reference] = events;
      assert.equal(toolResult.message.content[0].content[0].timeline, undefined, '#1220 nested child timeline stripped');
      assert.equal(toolResult.message.metadata.moduleId, 'fixture', '#1220 tool_result content is module-owned');
      assert.deepEqual(toolResult.message.content[0].timeline.id, 'result:tool-1', '#1220 tool_result block coordinate');
      assert.equal(reference.message.metadata.moduleId, undefined, '#1220 reference-only content is not module-owned');
      assert.deepEqual(reference.message.content[0].timeline.id, 'result:tool-2', '#1220 reference block coordinate');
    },
  },
  {
    name: 'wire-steer',
    steps: [
      wire({type: 'steer_applied', sessionId: 's', turnId: 't', itemId: 'q1', message: {role: 'user',
        content: [{type: 'text', text: 'steer'}], metadata: {queueItemId: 'q1'}}}),
    ],
    check(events) {
      const [steer] = events;
      assert.deepEqual(steer.message.content[0].timeline, {version: 1, turnId: 't', id: 'steer:q1', order: 0, revision: 1},
        '#1220 steer block coordinate');
      assert.equal(steer.message.metadata.moduleId, undefined, '#1220 steer is not module-owned');
      assert.equal(steer.moduleId, 'fixture', '#1220 top-level moduleId');
    },
  },
  {
    name: 'wire-compact',
    steps: [
      wire({type: 'compact_started', sessionId: 's', turnId: 't', compactionId: 'c1', trigger: 'auto', preTokens: 10}),
      wire({type: 'compact_completed', sessionId: 's', turnId: 't', compactionId: 'c1', trigger: 'auto',
        status: 'completed', preTokens: 10, postTokens: 5}),
    ],
    check(events) {
      assert.deepEqual(events[0].timeline, {version: 1, turnId: 't', id: 'compact:c1', order: 0, revision: 2}, '#1220 compact started');
      assert.deepEqual(events[1].timeline, {version: 1, turnId: 't', id: 'compact:c1', order: 0, revision: 3}, '#1220 compact revision bump');
    },
  },
  {
    name: 'subagent-derived',
    steps: [
      wire({type: 'agent_status', sessionId: 's', turnId: 't', event: 'thinking', kind: 'status', text: 'busy'}),
      host({type: 'subagent_started', sessionId: 's', turnId: 't', subagentId: 'child', subagentType: 'task'}),
      tick(500),
      host({type: 'pre_tool_execute', sessionId: 's::sub::child', turnId: 't', toolCallId: 'tool-1', toolName: 'read'}),
      tick(500),
      host({type: 'post_tool_execute', sessionId: 's::sub::child', turnId: 't', toolCallId: 'tool-1', toolName: 'read', success: true}),
      tick(500),
      host({type: 'subagent_completed', sessionId: 's', turnId: 't', subagentId: 'child', subagentType: 'task',
        success: true, durationMs: 900}),
      tick(500),
    ],
    check(events) {
      const [started, pre, derivedStarted, post, derivedDone, completed] = events.slice(1);
      assert.equal(started.moduleId, 'fixture', '#1220 host subagent event marked');
      assert.deepEqual(derivedStarted, {type: 'subagent_status', sessionId: 's', turnId: 't', subagentId: 'child',
        subagentType: 'task', status: 'tool_started', toolCallId: 'tool-1', toolName: 'read', durationMs: 500, moduleId: 'fixture'},
        '#1220 derived tool_started');
      assert.deepEqual(derivedDone, {type: 'subagent_status', sessionId: 's', turnId: 't', subagentId: 'child',
        subagentType: 'task', status: 'tool_completed', toolCallId: 'tool-1', toolName: 'read', success: true, durationMs: 1000, moduleId: 'fixture'},
        '#1220 derived tool_completed');
      assert.equal(pre.moduleId, 'fixture', '#1220 host pre_tool_execute marked');
      assert.equal(completed.durationMs, 900, '#1220 subagent_completed keeps its own duration');
      assert.ok(events.every(e => e.timeline === undefined && e.streamBoundary === undefined), '#1220 no coordinates');
    },
  },
  {
    name: 'heartbeat-unmarked',
    steps: [
      wire({type: 'agent_status', sessionId: 's', turnId: 't', event: 'thinking', kind: 'status', text: 'busy'}),
      host({type: 'subagent_started', sessionId: 's', turnId: 't', subagentId: 'child', subagentType: 'task'}),
      tick(500),
      tick(500), // 1000: 500 since the register, below the 2000 threshold.
      tick(500), // 1500
      tick(500), // 2000: still 1500 since the register — no heartbeat.
      tick(500), // 2500: first heartbeat.
      host({type: 'pre_tool_execute', sessionId: 's::sub::child', turnId: 't', toolCallId: 'tool-1', toolName: 'read'}),
      tick(500), // 3000: drain + derived tool_started.
      tick(500), tick(500), tick(500), tick(499), // 4999: no heartbeat.
      tick(1),   // 5000: second heartbeat, running.
      host({type: 'post_tool_execute', sessionId: 's::sub::child', turnId: 't', toolCallId: 'tool-1', toolName: 'read', success: true}),
      tick(500), // 5500: drain + derived tool_completed.
      tick(500), // 6000: no heartbeat.
      tick(500), tick(500), tick(500), // 7500: third heartbeat, waiting_model.
      host({type: 'subagent_completed', sessionId: 's', turnId: 't', subagentId: 'child', subagentType: 'task',
        success: true, durationMs: 7000}),
      tick(500), // 8000: completion, nothing pending afterwards.
      tick(500), tick(500), tick(500), tick(500), // 10000: no heartbeat from an empty registry.
    ],
    check(events) {
      const heartbeats = events.filter(e => e.type === 'subagent_status' && e.status !== 'tool_started' && e.status !== 'tool_completed');
      assert.deepEqual(heartbeats.map(h => h.status), ['waiting_model', 'running', 'waiting_model'], '#1220 heartbeat statuses');
      assert.deepEqual(heartbeats.map(h => h.durationMs), [2000, 4500, 7000], '#1220 heartbeat durations');
      assert.ok(heartbeats.every(h => h.moduleId === undefined && h.timeline === undefined), '#1220 heartbeats carry neither moduleId nor coordinates');
      assert.deepEqual(heartbeats[1].toolCallId, 'tool-1', '#1220 running heartbeat names the tool');
    },
  },
  {
    name: 'drain-before-wire-order',
    steps: [
      host({type: 'assistant_message', sessionId: 's', turnId: 't', message: {role: 'assistant',
        content: [{type: 'text', text: 'host first', blockId: 'hb1', timeline: CHILD}],
        metadata: {model: 'm'}}}),
      tick(500),
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b1', event: {type: 'text_delta', text: 'x'}}),
      // This host event stays pending at the final; the final drains it in
      // both implementations and it must be yielded before the terminal.
      host({type: 'subagent_started', sessionId: 's', turnId: 't', subagentId: 'child', subagentType: 'task'}),
    ],
    final: finalPayload(
      [{role: 'user', content: [{type: 'text', text: 'legacy'}]}],
      {role: 'assistant', content: [{type: 'text', text: 'final', blockId: 'b1', timeline: CHILD}]}),
    check(events, returned) {
      const [hostMessage, delta] = events.slice(0, 2);
      assert.deepEqual(hostMessage.message.content[0].timeline, {version: 1, turnId: 't', id: 'hb1', order: 0, revision: 2},
        '#1220 the drained host block is positioned before the later wire block');
      assert.deepEqual(delta.timeline, {version: 1, turnId: 't', id: 'b1', previousId: 'hb1', order: 1, revision: 4, offset: 0},
        '#1220 the wire block follows the host block');
      const pending = events.slice(2, -1);
      assert.deepEqual(pending.map(e => e.type), ['subagent_started'], '#1220 the final drain precedes the terminal');
      assert.deepEqual(returned.result.finalMessage.content[0].timeline,
        {version: 1, turnId: 't', id: 'b1', previousId: 'hb1', order: 1, revision: 4},
        '#1220 terminal block reuses the wire position without a revision bump');
      assert.equal(returned.messages[0].content[0].timeline, undefined, '#1220 legacy input stays untouched');
    },
  },
  {
    name: 'subagent-model-event-intact',
    steps: [
      wire({type: 'subagent_model_event', sessionId: 's', turnId: 't', subagentId: 'child', subagentType: 'task',
        blockId: 'sb1', event: {type: 'text_delta', text: 'x'},
        timeline: CHILD, streamBoundary: {turnId: 'child-turn', through: 3, revision: 4}, moduleId: 'child'}),
      wire({type: 'subagent_model_event', sessionId: 's', turnId: 't', subagentId: 'child', subagentType: 'task',
        event: {type: 'tool_call_end', toolCall: {id: 'tool-9', name: 'read', input: {}, timeline: CHILD}}}),
    ],
    check(events) {
      const [delta, end] = events;
      assert.deepEqual(delta.timeline, CHILD, '#1220 child timeline survives');
      assert.deepEqual(delta.streamBoundary, {turnId: 'child-turn', through: 3, revision: 4}, '#1220 child boundary survives');
      assert.equal(delta.moduleId, 'child', '#1220 child moduleId survives');
      assert.deepEqual(end.event.toolCall.timeline, CHILD, '#1220 subagent toolCall child timeline survives');
      assert.equal(end.moduleId, undefined, '#1220 no top-level moduleId is appended');
    },
  },
  {
    name: 'terminal-positions',
    steps: [
      wire({type: 'model_event', sessionId: 's', turnId: 't', blockId: 'b1', event: {type: 'text_delta', text: 'x'}}),
      wire({type: 'tool_calls_detected', sessionId: 's', turnId: 't',
        calls: [{id: 'tool-1', name: 'read', input: {}}]}),
    ],
    final: finalPayload(
      [
        {role: 'user', content: [{type: 'text', text: 'q'}]},
        {role: 'assistant', content: [{type: 'thinking', text: 't', blockId: 'b1', timeline: CHILD}]},
      ],
      {role: 'assistant', content: [
        {type: 'text', text: 'final', blockId: 'b1', timeline: CHILD},
        {type: 'tool_call', id: 'tool-1', name: 'read', input: {}, timeline: CHILD},
      ]}),
    check(events, returned) {
      const finalMessage = returned.result.finalMessage;
      assert.deepEqual(finalMessage.content[0].timeline, {version: 1, turnId: 't', id: 'b1', order: 0, revision: 2},
        '#1220 terminal text reuses the stream position');
      assert.deepEqual(finalMessage.content[1].timeline, {version: 1, turnId: 't', id: 'tool:tool-1', previousId: 'b1', order: 1, revision: 3},
        '#1220 terminal tool_call reuses the published position');
      assert.equal(finalMessage.metadata.moduleId, 'fixture', '#1220 terminal finalMessage is marked');
      assert.equal(returned.messages[1].content[0].timeline.id, 'b1', '#1220 history message block reuses the position');
      assert.equal(returned.messages[1].metadata.moduleId, 'fixture', '#1220 history assistant message is marked');
      assert.equal(returned.messages[0].metadata, undefined, '#1220 plain user history is not marked');
    },
  },
  {
    name: 'terminal-steer',
    steps: [
      wire({type: 'agent_status', sessionId: 's', turnId: 't', event: 'thinking', kind: 'status', text: 'busy'}),
    ],
    final: finalPayload([
      {role: 'user', content: [{type: 'text', text: 'steer'}], metadata: {queueItemId: 'q1'}},
    ]),
    check(events, returned) {
      assert.deepEqual(returned.messages[0].content[0].timeline, {version: 1, turnId: 't', id: 'steer:q1', order: 0, revision: 1},
        '#1220 terminal steer block coordinate');
      assert.equal(returned.messages[0].metadata.moduleId, undefined, '#1220 terminal steer is not module-owned');
    },
  },
  {
    name: 'module-id-child-overwrite',
    steps: [
      wire({type: 'agent_status', sessionId: 's', turnId: 't', event: 'thinking', kind: 'status', text: 'busy',
        moduleId: 'child', timeline: CHILD, streamBoundary: {turnId: 'child-turn', through: 1, revision: 1}}),
      wire({type: 'session_started', sessionId: 's', moduleId: 'child'}),
    ],
    check(events) {
      const [status, started] = events;
      assert.equal(status.moduleId, 'fixture', '#1220 child moduleId overwritten');
      assert.equal(status.timeline, undefined, '#1220 child top-level timeline stripped');
      assert.equal(status.streamBoundary, undefined, '#1220 child top-level boundary stripped');
      assert.deepEqual(Object.keys(status), ['type', 'sessionId', 'turnId', 'event', 'kind', 'text', 'moduleId'],
        '#1220 moduleId replaced in place, not appended');
      assert.equal(started.moduleId, 'fixture', '#1220 session_started moduleId overwritten');
    },
  },
];

const cases: any[] = [];
for (const script of scripts) {
  mock.timers.enable({apis: ['Date', 'setTimeout'], now: 0});
  syncBuiltinESMExports();
  const steps: Step[] = [];
  const hostQueue: any[] = [];
  const sent: any[] = [];
  const incoming: any[] = [];
  let waiter: any;
  let executeIds: any;
  let sequence = 0;
  let closed = 0;
  const push = (value: any) => {
    if (waiter) { const receive = waiter; waiter = undefined; receive({value, done: false}); }
    else incoming.push(value);
  };
  const connection = {
    send(m: any) {
      sent.push(m);
      const reply = {kind: 'response', messageId: 'response-' + m.messageId, inReplyTo: m.messageId, ok: true};
      if (m.method === 'hello') push({...reply, protocolVersion: '2.0', moduleId: 'fixture', moduleInstanceId: 'instance',
        connectionGeneration: 'current', capabilitiesVersion: 'v1'});
      if (m.method === 'capabilities') push({...reply, protocolVersion: '2.0', moduleId: 'fixture', moduleInstanceId: 'instance',
        connectionGeneration: 'current', capabilitiesVersion: 'v1',
        payload: {capabilitiesVersion: 'v1', methods: [
          {name: 'execute', profiles: ['streaming'], resumeSupport: 'streaming'}, {name: 'resume'}, {name: 'ack'}]}});
      if (m.method === 'execute') {
        executeIds = {runId: m.runId, operationId: m.operationId, requestId: m.requestId, streamId: 'stream'};
        push({...reply, requestId: m.requestId, streamId: 'stream'});
      }
    },
    receive() {
      return {
        [Symbol.asyncIterator]() { return this; },
        next(): Promise<any> {
          return incoming.length ? Promise.resolve({value: incoming.shift(), done: false}) : new Promise(r => { waiter = r; });
        },
      };
    },
    close() { closed++; },
  };
  const context = {permissionContext: () => ({}), toolRuntimeContext: () => ({}), executionContext: () => ({})};
  const modules = {event: {drain: () => hostQueue.splice(0)}, model: {execution: {}},
    capability: {runtimeContext: {bindTurn: () => context}, execution: {list: () => []}},
    permission: {requestContext: {bindTurn: () => context}},
    context: {requestIdentity: {bindTurn: () => ({contextIdentity: () => ({})})}}};
  const factory = createAgentLoopSidecarRuntimeFactory({connect: async () => connection, uuid: () => 'id',
    turnComposition: () => ({forTurn: () => ({moduleHandlers: {}})})});
  const runner = factory({config: {cwd: output, permissionMode: 'default'},
    capabilities: {transport: {}}, sidecarModules: modules} as any);
  const iterator = runner.run({sessionId: 's', turnId: 't', messages: [],
    execution: {runId: 'run', operationId: 'operation'}});
  const events: any[] = [];
  let armed: any;
  let settled = false;
  let done: any;
  const arm = () => {
    settled = false;
    armed = iterator.next().then(next => { settled = true; return next; });
  };
  const drain = async (rounds = 200) => {
    await flush(rounds);
    while (settled && done === undefined) {
      const next = await armed;
      if (next.done) { done = next.value; return; }
      events.push(next.value);
      arm();
      await flush(rounds);
    }
  };
  arm();
  for (let guard = 0; !executeIds && guard < 400; guard++) await flush(50);
  assert.ok(executeIds, `#1220 ${script.name}: the execute request never arrived`);
  const frame = (payload: any, final = false) => ({kind: 'event', eventType: 'agent_event',
    streamId: 'stream', runId: executeIds.runId, operationId: executeIds.operationId,
    requestId: executeIds.requestId, sequence: sequence++, final,
    ...(final ? {outcome: 'completed'} : {}), payload});
  for (const step of script.steps) {
    if (step.kind === 'wire') {
      steps.push({kind: 'wire', payload: step.payload});
      push(frame(step.payload));
      await drain();
    } else if (step.kind === 'host') {
      steps.push({kind: 'host', events: step.events});
      hostQueue.push(...step.events);
    } else if (step.kind === 'tick') {
      let remaining = step.ms;
      while (remaining > 0) {
        const chunk = Math.min(500, remaining);
        steps.push({kind: 'tick', ms: chunk});
        mock.timers.tick(chunk);
        remaining -= chunk;
        await drain();
      }
    }
  }
  const finalPayloadValue = (script as any).final ?? finalPayload();
  steps.push({kind: 'final', payload: finalPayloadValue});
  // The pushed final frame resolves the protocol's pending next() without any
  // timer: recordKnownTerminal positions the terminal inside that resolution,
  // then the runner's wire branch drains every pending host event before it
  // yields turn_completed and returns {result, messages}.
  push(frame(finalPayloadValue, true));
  await drain(1000);
  for (let guard = 0; done === undefined && guard < 100; guard++) await drain(200);
  assert.notEqual(done, undefined, `#1220 ${script.name}: the runner never returned`);
  // protocol.close() closes the connection and resolves void, so the runner's
  // `await protocol?.close(...) ?? connection?.close?.(...)` fallback closes a
  // second time — a deterministic baseline quirk, not a harness artifact.
  assert.equal(closed, 2, `#1220 ${script.name}: the connection closes twice`);
  script.check?.(events, done);
  cases.push({name: script.name, steps, events, returned: done});
  mock.timers.reset();
}
writeFileSync(join(output, 'oracle.json'), JSON.stringify({sourceCommit, sourceSha256, cases}) + '\n', {flag: 'wx'});
console.log(JSON.stringify({sourceCommit, sources: closure.length, cases: cases.map(c => ({
  name: c.name, events: c.events.length, returned: c.returned !== undefined}))}, null, 2));
