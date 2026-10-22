// Run from the repository root with the fixed baseline's tsx loader; see README.md.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/gateway/client/GatewayAgentEventProjector.ts',
  'src/gateway/client/GatewayToolResultArtifactStore.ts',
  'src/gateway/client/GatewayToolResultArtifactStorePort.ts',
  'src/model/protocol/toolResultContent.ts',
  'src/tool/protocol/result.ts',
  'src/agent/protocol/events.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');

// Transpile entire, unmodified modules. Only the projector's type imports and
// its two pure helpers plus the artifact store class are provided; any other
// import access fails closed.
async function load(source: string, imports: Record<string, any> = {}) {
  const compiled = transformSync(await readFile(join(baseline, source), 'utf8'), {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
  }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', compiled)((name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name in imports) return imports[name];
    return new Proxy({}, { get(_target, property) {
      if (property === '__esModule') return true;
      throw new Error(`unexercised import accessed: ${source} -> ${name}.${String(property)}`);
    } });
  }, module, module.exports);
  return module.exports;
}

const toolResultContent = await load('src/model/protocol/toolResultContent.ts');
const toolResult = await load('src/tool/protocol/result.ts');
const artifactStore = await load('src/gateway/client/GatewayToolResultArtifactStore.ts');
const projectorModule = await load('src/gateway/client/GatewayAgentEventProjector.ts', {
  '../../model/index.js': { flattenToolResultBlockText: toolResultContent.flattenToolResultBlockText },
  '../../tool/index.js': { contentToText: toolResult.contentToText },
  './GatewayToolResultArtifactStore.js': artifactStore,
});
const { mapAgentEvent } = projectorModule;

const THRESHOLD = 4096;
function safePart(value: string): string {
  return value.trim().replace(/[^A-Za-z0-9_.-]+/g, '-').replace(/^-+|-+$/g, '') || 'value';
}
// Deterministic stand-in for GatewayToolResultArtifactStore: mirrors its
// 4096-byte threshold but writes nothing and returns a fixed root instead of
// the host temp directory. The recorded input is replayed in Rust.
function fixtureStore(record: any[]) {
  return {
    persist(input: { sessionId: string; turnId: string; toolCallId: string; text: string }) {
      const bytes = Buffer.byteLength(input.text, 'utf8');
      const returned = bytes > THRESHOLD
        ? `/tmp/pdrs-agent-event-projector/${safePart(input.sessionId)}/${safePart(input.turnId)}/${safePart(input.toolCallId)}.txt`
        : undefined;
      record.push({ ...input, bytes, returned });
      return returned;
    },
  };
}

const cases: any[] = [];
function record(
  name: string,
  event: any,
  options: { runId?: string; forwardSubagentText?: boolean } = {},
) {
  const runId = options.runId ?? 'run-1';
  const forwardSubagentText = options.forwardSubagentText ?? false;
  const persist: any[] = [];
  // mapAgentEvent treats an options object that itself has `persist` as the
  // artifact port; the extra forwardSubagentText flag is read by the facade.
  try {
    const frames = mapAgentEvent(event, runId, Object.assign(fixtureStore(persist), {
      forwardSubagentText,
    }) as any);
    cases.push({ name, event, runId, forwardSubagentText, frames, persist });
  } catch (error) {
    // Structurally invalid known events throw in TS; record `String(error)` so
    // the Rust port's fallible API is replayed against the same message.
    cases.push({ name, event, runId, forwardSubagentText, error: String(error), persist });
  }
}

const SESSION = 'session-1';
const TURN = 'turn-1';
const tl = (order: number, extra: Record<string, unknown> = {}) => ({
  version: 1, turnId: TURN, id: `timeline-${order}`, order, revision: order, ...extra,
});
const boundary = { turnId: TURN, through: 3, revision: 3 };
const started = '2026-10-22T00:00:00.000Z';
const completed = '2026-10-22T00:00:01.000Z';
const usage = {
  inputTokens: 10, outputTokens: 20, cacheReadTokens: 0, cacheWriteTokens: 0, totalTokens: 30,
};
const message = (content: any[], metadata?: Record<string, unknown>) => ({
  role: 'assistant', content, ...(metadata ? { metadata } : {}),
});
const successResult = (content: any[], extra: Record<string, unknown> = {}) => ({
  type: 'success', toolCallId: 'call-1', toolName: 'read_file', content,
  startedAt: started, completedAt: completed, ...extra,
});
const errorResult = (content: any[], extra: Record<string, unknown> = {}) => ({
  type: 'error', toolCallId: 'call-1', toolName: 'read_file',
  error: { code: 'tool_failed', message: 'boom' }, content,
  startedAt: started, completedAt: completed, ...extra,
});

record('turn-started-with-timeline-and-boundary', {
  type: 'turn_started', sessionId: SESSION, turnId: TURN, timeline: tl(0), streamBoundary: boundary,
});
record('input-accepted', {
  type: 'input_accepted', sessionId: SESSION, turnId: TURN,
  messages: [{ role: 'user', content: [{ type: 'text', text: 'hello' }] }],
}, { runId: 'run-2' });
record('steer-applied-with-block-timeline', {
  type: 'steer_applied', sessionId: SESSION, turnId: TURN, itemId: 'item-1',
  message: { role: 'user', content: [
    { type: 'text', text: 'first' },
    { type: 'text', text: 'steer', timeline: tl(1) },
  ] },
});
record('steer-applied-without-timeline', {
  type: 'steer_applied', sessionId: SESSION, turnId: TURN, itemId: 'item-2',
  message: { role: 'user', content: [{ type: 'text', text: 'steer' }] },
  timeline: tl(2), streamBoundary: boundary,
});
record('steer-unapplied', {
  type: 'steer_unapplied', sessionId: SESSION, turnId: TURN, itemId: 'item-3', reason: 'turn_ended',
});
record('model-request-started', {
  type: 'model_request_started', sessionId: SESSION, turnId: TURN, model: 'model-x', provider: 'provider-x',
});
record('model-event-text-delta-with-block', {
  type: 'model_event', sessionId: SESSION, turnId: TURN, blockId: 'block-1',
  event: { type: 'text_delta', text: 'hello 世界' }, timeline: tl(4),
});
record('model-event-thinking-delta-with-boundary', {
  type: 'model_event', sessionId: SESSION, turnId: TURN,
  event: { type: 'thinking_delta', text: 'thinking', signature: 'sig' }, streamBoundary: boundary,
}, { runId: 'run-think' });
record('model-event-error-is-ignored', {
  type: 'model_event', sessionId: SESSION, turnId: TURN,
  event: { type: 'error', error: {
    provider: 'p', protocol: 'anthropic', code: 'overloaded_error', message: 'busy', retryable: true,
  } },
});
record('model-event-request-started-is-ignored', {
  type: 'model_event', sessionId: SESSION, turnId: TURN,
  event: { type: 'request_started', provider: 'p', model: 'm' },
});
record('assistant-message-closed-and-delta-blocks', {
  type: 'assistant_message', sessionId: SESSION, turnId: TURN,
  message: message([
    { type: 'text', text: 'closed text', blockId: 'block-1', timeline: tl(5) },
    { type: 'thinking', text: 'closed thinking', blockId: 'block-2', timeline: tl(6) },
    { type: 'text', text: 'plain text' },
    { type: 'thinking', text: 'plain thinking' },
    { type: 'image', source: 'base64', data: 'AAAA', mimeType: 'image/png' },
    { type: 'tool_call', id: 'call-9', name: 'bash', input: { command: 'ls' } },
  ], { model: 'model-x' }),
  timeline: tl(7),
});
record('assistant-message-without-metadata', {
  type: 'assistant_message', sessionId: SESSION, turnId: TURN,
  message: message([{ type: 'text', text: 'no model' }, { type: 'thinking', text: 'no model thinking' }]),
});
record('prompt-suggestion', {
  type: 'prompt_suggestion', sessionId: SESSION, turnId: TURN, suggestion: 'try again',
});
record('tool-calls-detected', {
  type: 'tool_calls_detected', sessionId: SESSION, turnId: TURN,
  calls: [
    { id: 'call-a', name: 'bash', input: { command: 'ls', args: ['a', 1, true, null] }, timeline: tl(8) },
    { id: 'call-b', name: 'read_file', input: 'plain string' },
    { id: 'call-c', name: 'no_input' },
    { id: 'call-d', name: 'nested', input: { nested: { a: [1, 2] } } },
  ],
});
record('tool-progress-with-metadata', {
  type: 'tool_progress', sessionId: SESSION, turnId: TURN, toolCallId: 'call-1', toolName: 'bash',
  message: 'running', metadata: { phase: 'start', attempt: 1 }, createdAt: started,
});
record('tool-progress-without-metadata', {
  type: 'tool_progress', sessionId: SESSION, turnId: TURN, toolCallId: 'call-1', toolName: 'bash',
  message: 'running', createdAt: started,
});
record('tool-result-small-success', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'text', text: 'ok' }]),
});
record('tool-result-exact-threshold-does-not-persist', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'text', text: 'x'.repeat(4096) }]),
});
record('tool-result-large-with-images-and-file', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN, timeline: tl(9), streamBoundary: boundary,
  result: successResult([
    { type: 'text', text: `${'x'.repeat(4100)}😀tail` },
    { type: 'image', mimeType: 'image/png', data: 'QUJD', bytes: 3, detail: 'high' },
    { type: 'file', path: '/tmp/dir/report.txt', mimeType: 'text/plain', description: 'report' },
  ], { toolName: 'screenshot', data: { answer: 42, nested: { list: [1, 'two'] } } }),
});
record('tool-result-read-file-image-has-no-attachment', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([
    { type: 'image', mimeType: 'image/jpeg', data: 'QUJD', bytes: 3, detail: 'low' },
  ], { toolName: 'read_file' }),
});
record('tool-result-error-result', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: errorResult([{ type: 'text', text: 'failed' }], { toolName: 'bash' }),
});
record('tool-result-content-text-conversions', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([
    { type: 'json', value: { k: [1, true, null], s: '中' } },
    { type: 'image', mimeType: 'image/webp', data: 'QUJDRA==', bytes: 4 },
    { type: 'pdf', mimeType: 'application/pdf', data: 'QUJD', bytes: 3, pages: 3 },
    { type: 'pdf', mimeType: 'application/pdf', data: 'QUJD', bytes: 3, pages: 0 },
    { type: 'file', path: '/tmp/x/y.bin' },
    { type: 'file', path: '/tmp/x/z.bin', mimeType: 'application/octet-stream', description: 'binary' },
  ], { toolName: 'read_file' }),
});
record('tool-result-data-string-truncation', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'text', text: 'ok' }], {
    toolName: 'bash',
    data: {
      exact: 'y'.repeat(4000),
      over: `${'z'.repeat(3999)}😀end`,
      nested: [{ deep: 'w'.repeat(4005) }],
      number: 7,
      flag: false,
      nothing: null,
    },
  }),
});
record('tool-result-data-non-record', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'text', text: 'ok' }], {
    toolName: 'bash', data: ['a', { b: 'c' }, 1],
  }),
});
record('tool-result-preview-truncation-utf16', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'text', text: `${'a'.repeat(19998)}😀${'b'.repeat(10)}` }]),
});
record('file-artifacts', {
  type: 'file_artifacts', sessionId: SESSION, turnId: TURN,
  artifacts: [{
    id: 'artifact-1', name: 'out.txt', path: 'out.txt', operation: 'created', source: 'tool',
    status: 'complete', size: 3, sha256: 'abc', mimeType: 'text/plain', createdAt: started,
  }],
});
record('mode-change-requested', {
  type: 'mode_change_requested', sessionId: SESSION, turnId: TURN, mode: 'plan',
});
record('turn-completed-structured-output', {
  type: 'turn_completed', sessionId: SESSION, turnId: TURN,
  result: {
    type: 'success', sessionId: SESSION, turnId: TURN, stopReason: 'completed', usage,
    permissionDenials: [], turns: 1, startedAt: started, completedAt: completed,
    structuredOutput: { answer: 42, nested: [1, null] },
  },
});
record('turn-completed-without-structured-output', {
  type: 'turn_completed', sessionId: SESSION, turnId: TURN,
  result: {
    type: 'success', sessionId: SESSION, turnId: TURN, stopReason: 'max_turns', usage,
    permissionDenials: [], turns: 2, startedAt: started, completedAt: completed,
  },
});
record('turn-failed-provider-error', {
  type: 'turn_failed', sessionId: SESSION, turnId: TURN,
  error: {
    code: 'agent_model_error', message: 'provider failed', userHint: 'retry later',
    details: {
      provider: 'anthropic', protocol: 'anthropic', status: 529, code: 'overloaded_error',
      message: 'busy', raw: { type: 'error', message: 'overloaded' },
    },
  },
});
record('turn-failed-details-null', {
  type: 'turn_failed', sessionId: SESSION, turnId: TURN,
  error: { code: 'agent_invalid_state', message: 'no details', details: null },
});
record('turn-failed-details-empty-record', {
  type: 'turn_failed', sessionId: SESSION, turnId: TURN,
  error: { code: 'agent_invalid_state', message: 'empty details', details: {} },
});
record('token-cap-adjusted-with-previous', {
  type: 'token_cap_adjusted', sessionId: SESSION, turnId: TURN, provider: 'p', model: 'm',
  cap: 'output', previous: 100, next: 200, reason: 'overflow',
});
record('token-cap-adjusted-without-previous', {
  type: 'token_cap_adjusted', sessionId: SESSION, turnId: TURN, provider: 'p', model: 'm',
  cap: 'context', next: 50, reason: 'shrink',
});
record('empty-output-recovery', {
  type: 'empty_output_recovery', sessionId: SESSION, turnId: TURN, provider: 'p', model: 'm',
  finishReason: 'length', previousMaxOutputTokens: 100, nextMaxOutputTokens: 200,
});
record('empty-output-recovery-without-previous', {
  type: 'empty_output_recovery', sessionId: SESSION, turnId: TURN, provider: 'p', model: 'm',
  finishReason: 'stop', nextMaxOutputTokens: 200,
});
record('model-recovery-failed-with-truncated-raw', {
  type: 'model_recovery_failed', sessionId: SESSION, turnId: TURN, provider: 'p', model: 'm',
  error: {
    provider: 'p', protocol: 'openai', status: 500, code: 'server_error', message: 'failed',
    retryable: true, raw: 'r'.repeat(1300),
  },
});
record('session-aborted-with-reason', {
  type: 'session_aborted', sessionId: SESSION, reason: 'user cancelled',
});
record('session-aborted-without-reason', { type: 'session_aborted', sessionId: SESSION });
record('session-aborted-empty-reason', { type: 'session_aborted', sessionId: SESSION, reason: '' });
record('tool-results-projected-mixed', {
  type: 'tool_results_projected', sessionId: SESSION, turnId: TURN, timeline: tl(10),
  streamBoundary: boundary,
  message: message([
    { type: 'tool_result_reference', toolCallId: 'call-r', path: '/tmp/r.txt', originalBytes: 10,
      preview: 'p', hasMore: true },
    { type: 'media_reference', toolCallId: 'call-m', path: '/tmp/dir/image.png', originalBytes: 5,
      preview: 'p', hasMore: true, mimeType: 'image/png', mediaType: 'image', pages: 2,
      reason: 'media' },
    { type: 'media_reference', toolCallId: 'call-big', path: '/tmp/dir/big.pdf', originalBytes: 9,
      preview: 'p', hasMore: true, mimeType: 'application/pdf', mediaType: 'pdf',
      reason: 'media_result_too_large' },
    { type: 'media_reference', path: '/tmp/dir/no-call.txt', originalBytes: 1,
      preview: 'p', hasMore: false, mimeType: 'text/plain', mediaType: 'audio' },
    { type: 'media_reference', toolCallId: 'call-a', path: '/tmp/dir/audio.mp3', originalBytes: 4,
      preview: 'p', hasMore: true, mimeType: 'audio/mpeg', mediaType: 'audio', reason: undefined },
    { type: 'tool_result', toolCallId: 'call-t', content: [
      { type: 'text', text: 'line one' },
      { type: 'image', source: 'base64', data: 'QUJD', mimeType: 'image/png' },
      { type: 'image', source: 'url', data: 'https://example.test/a.png', mimeType: 'image/png' },
      { type: 'pdf', source: 'base64', data: 'QUJD', mimeType: 'application/pdf', bytes: 3, pages: 0 },
      { type: 'pdf', source: 'base64', data: 'QUJD', mimeType: 'application/pdf', bytes: 3, pages: 3 },
    ] },
  ]),
});
record('compact-started', {
  type: 'compact_started', sessionId: SESSION, turnId: TURN, compactionId: 'compact-1',
  trigger: 'auto', preTokens: 1000,
});
record('compact-completed', {
  type: 'compact_completed', sessionId: SESSION, turnId: TURN, compactionId: 'compact-1',
  trigger: 'auto', status: 'completed', preTokens: 1000, postTokens: 400, messagesSummarized: 7,
});
record('context-budget-minimal', {
  type: 'context_budget', sessionId: SESSION, turnId: TURN,
  snapshot: { tokens: 10, maxContextTokens: 100, warningRatio: 0.8, blockingRatio: 0.9, state: 'ok', ratio: 0.1 },
});
record('context-budget-full', {
  type: 'context_budget', sessionId: SESSION, turnId: TURN,
  snapshot: {
    tokens: 10, localEstimateTokens: 11, displayTokens: 12, estimateSource: 'usage',
    usageTokens: 13, calibrationActualInputTokens: 14, calibrationEstimatedInputTokens: 15,
    totalContextTokens: 900, maxContextTokens: 100, effectiveContextTokens: 80,
    maxOutputTokens: 20, reservedOutputTokens: 12, warningRatio: 0.8, blockingRatio: 0.9,
    state: 'warning', ratio: 0.5, source: 'provider', exact: false,
    estimatorError: 'none', breakdown: { system: 1, tools: 2 },
  },
});
record('context-budget-null-fallbacks', {
  type: 'context_budget', sessionId: SESSION, turnId: TURN,
  snapshot: {
    tokens: 10, totalContextTokens: null, maxContextTokens: 100, maxOutputTokens: 20,
    reservedOutputTokens: null, warningRatio: 0.8, blockingRatio: 0.9, state: 'ok', ratio: 0.1,
  },
});
record('context-budget-effective-without-total', {
  type: 'context_budget', sessionId: SESSION, turnId: TURN,
  snapshot: {
    tokens: 10, maxContextTokens: 100, effectiveContextTokens: 70, maxOutputTokens: 15,
    warningRatio: 0.8, blockingRatio: 0.9, state: 'ok', ratio: 0.1,
  },
});
record('context-budget-null-reserve-both', {
  type: 'context_budget', sessionId: SESSION, turnId: TURN,
  snapshot: {
    tokens: 10, maxContextTokens: 100, maxOutputTokens: null,
    reservedOutputTokens: null, warningRatio: 0.8, blockingRatio: 0.9, state: 'ok', ratio: 0.1,
  },
});
record('warning', {
  type: 'warning', sessionId: SESSION, turnId: TURN, code: 'warn-1', message: 'careful',
  metadata: { detail: true },
});
record('agent-status-without-detail', {
  type: 'agent_status', sessionId: SESSION, turnId: TURN, event: 'custom_status', kind: 'status',
});
record('agent-status-with-detail', {
  type: 'agent_status', sessionId: SESSION, turnId: TURN, event: 'custom_status',
  detail: { text: 'x', nested: { a: 1 } },
});
record('turn-continued', {
  type: 'turn_continued', sessionId: SESSION, turnId: TURN, reason: 'continuation',
});
record('subagent-started-with-tool-call', {
  type: 'subagent_started', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', toolCallId: 'call-sub',
});
record('subagent-started-without-tool-call', {
  type: 'subagent_started', sessionId: SESSION, turnId: TURN, subagentId: 'sub-2',
  subagentType: 'explore',
});
record('subagent-completed-aborted', {
  type: 'subagent_completed', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', success: false, aborted: true, durationMs: 12,
});
record('subagent-completed-not-aborted', {
  type: 'subagent_completed', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', success: true, aborted: false, durationMs: 12,
});
record('subagent-model-event-text-forwarded', {
  type: 'subagent_model_event', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', event: { type: 'text_delta', text: 'sub text' },
}, { forwardSubagentText: true });
record('subagent-model-event-text-status', {
  type: 'subagent_model_event', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', event: { type: 'text_delta', text: 'sub text' },
});
record('subagent-model-event-thinking', {
  type: 'subagent_model_event', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', event: { type: 'thinking_delta', text: 'sub thinking' },
});
record('subagent-model-event-error', {
  type: 'subagent_model_event', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', event: { type: 'error', error: {
    provider: 'p', protocol: 'google', code: 'server_error', message: 'sub failed', retryable: true,
  } },
});
record('subagent-model-event-ignored', {
  type: 'subagent_model_event', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', event: { type: 'request_started', provider: 'p', model: 'm' },
});
record('subagent-tool-calls-detected', {
  type: 'subagent_tool_calls_detected', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', calls: [
    { id: 'call-s1', name: 'bash', input: { command: 'ls' } },
    { id: 'call-s2', name: 'noop' },
  ],
});
record('subagent-tool-result-success', {
  type: 'subagent_tool_result', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore',
  result: successResult([
    { type: 'text', text: 'line one\nline two\nline three\nline four' },
    { type: 'image', mimeType: 'image/png', data: 'QUJD', bytes: 3 },
  ], { toolName: 'bash', toolCallId: 'call-sub' }),
});
record('subagent-tool-result-error', {
  type: 'subagent_tool_result', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', result: errorResult([{ type: 'text', text: 'failed' }]),
});
record('subagent-status-full', {
  type: 'subagent_status', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', status: 'tool_completed', toolCallId: 'call-s1', toolName: 'bash',
  success: true, durationMs: 10,
});
record('subagent-status-minimal', {
  type: 'subagent_status', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1', status: 'running',
});
record('retry-progress', {
  type: 'retry_progress', sessionId: SESSION, turnId: TURN,
  detail: {
    type: 'pilotdeck_router_retry_progress', sessionId: SESSION, turnId: TURN, attempt: 2,
    maxAttempts: 5, delayMs: 250, reason: 'rate_limit', provider: 'p', model: 'm',
  },
});
record('permission-denied', {
  type: 'permission_denied', sessionId: SESSION, turnId: TURN, toolName: 'bash', reason: 'denied',
});
for (const [name, event] of [
  ['session-ended', { type: 'session_ended', sessionId: SESSION, reason: 'done' }],
  ['user-prompt-submitted', { type: 'user_prompt_submitted', sessionId: SESSION, turnId: TURN, prompt: 'hi' }],
  ['setup-completed', { type: 'setup_completed', sessionId: SESSION }],
  ['instructions-loaded', { type: 'instructions_loaded', sessionId: SESSION, turnId: TURN, hasSystemPrompt: true }],
  ['stop-requested', { type: 'stop_requested', sessionId: SESSION, turnId: TURN }],
  ['stop-failure', { type: 'stop_failure', sessionId: SESSION, turnId: TURN, error: 'nope' }],
  ['elicitation-resolved', { type: 'elicitation_resolved', sessionId: SESSION, requestId: 'e1', delivered: true }],
  ['pre-tool-execute', { type: 'pre_tool_execute', sessionId: SESSION, turnId: TURN, toolCallId: 'c', toolName: 'bash' }],
  ['post-tool-execute', { type: 'post_tool_execute', sessionId: SESSION, turnId: TURN, toolCallId: 'c', toolName: 'bash', success: true }],
  ['permission-requested', { type: 'permission_requested', sessionId: SESSION, turnId: TURN, toolCallId: 'c', toolName: 'bash' }],
  ['elicitation-requested', { type: 'elicitation_requested', sessionId: SESSION, turnId: TURN, requestId: 'e1', toolName: 'bash' }],
  ['observer-report', { type: 'observer_report', sessionId: SESSION, turnId: TURN, observedSubagentId: 's1',
    observedSubagentType: 'a', observerSubagentId: 's2', observerSubagentType: 'b', success: true, durationMs: 1 }],
  ['session-started', { type: 'session_started', sessionId: SESSION }],
  ['unknown-type', { type: 'future_event', sessionId: SESSION }],
  ['missing-type', { sessionId: SESSION }],
] as const) record(name, event);

// Attachments never receive the event timeline, but do receive streamBoundary
// and runId. The image attachment below comes from a non-read_file tool result.
record('attachment-excludes-timeline', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN, timeline: tl(11), streamBoundary: boundary,
  result: successResult([{ type: 'image', mimeType: 'image/gif', data: 'QUJD', bytes: 3 }],
    { toolName: 'screenshot', toolCallId: 'call-shot' }),
});
// safeGatewayPathPart trims, collapses disallowed runs to one '-', trims edge
// dashes, falls back to "value", and lowercases MIME before extension lookup.
record('tool-result-attachment-name-normalization', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'image', mimeType: 'IMAGE/PNG', data: 'QUJD' }],
    { toolName: '  Shot Gun/1  ', toolCallId: ' call id.. ', data: {} }),
}, { runId: 'run-空格' });
record('tool-result-unknown-image-mime-and-empty-ids', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'image', mimeType: 'application/x-thing', data: '' }],
    { toolName: '', toolCallId: '' }),
});
record('tool-result-file-with-backslash-path', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'file', path: 'C:\\dir\\out.txt' }], { toolName: 'write_file' }),
});
record('tool-result-empty-content', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([], { toolName: 'bash', data: { empty: true } }),
});
record('assistant-message-block-id-without-timeline', {
  type: 'assistant_message', sessionId: SESSION, turnId: TURN,
  message: message([
    { type: 'text', text: 'id only text', blockId: 'block-x' },
    { type: 'thinking', text: 'id only thinking', blockId: 'block-y' },
  ], { model: 'model-x' }),
});
record('model-event-block-id-empty-is-falsy', {
  type: 'model_event', sessionId: SESSION, turnId: TURN, blockId: '',
  event: { type: 'text_delta', text: 'no block' },
});
record('turn-completed-structured-output-null', {
  type: 'turn_completed', sessionId: SESSION, turnId: TURN,
  result: {
    type: 'success', sessionId: SESSION, turnId: TURN, stopReason: 'completed', usage,
    permissionDenials: [], turns: 1, startedAt: started, completedAt: completed,
    structuredOutput: null,
  },
});
// 1199 'r' + a surrogate pair + 10 'x' crosses the 1200-unit provider-raw
// limit in the middle of the pair; TS slice keeps the lone high surrogate.
record('model-recovery-raw-splits-surrogate-pair', {
  type: 'model_recovery_failed', sessionId: SESSION, turnId: TURN, provider: 'p', model: 'm',
  error: {
    provider: 'p', protocol: 'google', code: 'server_error', message: 'failed',
    retryable: true, raw: `${'r'.repeat(1199)}😀${'x'.repeat(10)}`,
  },
});
// Presence checks are `!== undefined`, so explicit null optional fields are
// emitted as null, unlike the `??` fallbacks in the total calculation.
record('context-budget-explicit-null-optionals', {
  type: 'context_budget', sessionId: SESSION, turnId: TURN,
  snapshot: {
    tokens: 10, maxContextTokens: 100, maxOutputTokens: 20,
    warningRatio: 0.8, blockingRatio: 0.9, state: 'ok', ratio: 0.1,
    source: null, exact: null, estimatorError: null, breakdown: null,
  },
});
record('token-cap-adjusted-previous-null', {
  type: 'token_cap_adjusted', sessionId: SESSION, turnId: TURN, provider: 'p', model: 'm',
  cap: 'output', previous: null, next: 200, reason: 'overflow',
});

// Structurally invalid known events: the real projector throws a TypeError.
// These lock the Rust port's error text to `String(error)`.
record('error-null-event', null);
record('error-tool-result-missing-result', { type: 'tool_result', sessionId: SESSION, turnId: TURN });
record('error-tool-result-content-null', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN, result: { content: null },
});
record('error-assistant-message-missing-content', {
  type: 'assistant_message', sessionId: SESSION, turnId: TURN, message: { role: 'assistant' },
});
record('error-assistant-message-missing-message', {
  type: 'assistant_message', sessionId: SESSION, turnId: TURN,
});
record('error-context-budget-missing-snapshot', {
  type: 'context_budget', sessionId: SESSION, turnId: TURN,
});
record('error-turn-completed-missing-result', {
  type: 'turn_completed', sessionId: SESSION, turnId: TURN,
});
record('error-steer-applied-missing-message', {
  type: 'steer_applied', sessionId: SESSION, turnId: TURN, itemId: 'item-x',
});
record('error-retry-progress-missing-detail', {
  type: 'retry_progress', sessionId: SESSION, turnId: TURN,
});
record('error-tool-calls-missing-calls', {
  type: 'tool_calls_detected', sessionId: SESSION, turnId: TURN,
});
record('error-turn-failed-missing-error', {
  type: 'turn_failed', sessionId: SESSION, turnId: TURN,
});
record('error-model-recovery-missing-error', {
  type: 'model_recovery_failed', sessionId: SESSION, turnId: TURN,
});
record('error-model-event-missing-event', {
  type: 'model_event', sessionId: SESSION, turnId: TURN,
});
record('error-subagent-model-event-missing-event', {
  type: 'subagent_model_event', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore',
});
record('error-subagent-tool-result-missing-result', {
  type: 'subagent_tool_result', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore',
});
record('error-tool-results-projected-missing-content', {
  type: 'tool_results_projected', sessionId: SESSION, turnId: TURN,
  message: { role: 'assistant' },
});
record('error-subagent-tool-calls-missing-calls', {
  type: 'subagent_tool_calls_detected', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore',
});

// TS builds the sanitized record with `output[key] = ...`, so a `__proto__`
// own property from JSON.parse hits the setter and never reaches the output.
record('tool-result-data-drops-proto-object-value', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([], { toolName: 'bash', data: JSON.parse('{"__proto__":{"foo":1},"keep":2}') }),
});
record('tool-result-data-drops-proto-primitive-value', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([], { toolName: 'bash', data: JSON.parse('{"__proto__":7,"keep":2}') }),
});
record('tool-result-data-drops-nested-proto', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([], { toolName: 'bash', data: JSON.parse('{"keep":{"__proto__":{"x":1},"k":2}}') }),
});
record('tool-result-data-only-proto-is-empty-record', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([], { toolName: 'bash', data: JSON.parse('{"__proto__":{"foo":1}}') }),
});

// Null/missing nested entries inside an array: `contentToText` reads `.type`
// off each entry, and `flattenToolResultBlockText` reads `block.content`.
record('error-tool-result-null-content-item', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([null]),
});
record('error-subagent-tool-result-null-content-item', {
  type: 'subagent_tool_result', sessionId: SESSION, turnId: TURN, subagentId: 'sub-1',
  subagentType: 'explore', result: successResult([null]),
});
record('error-tool-results-projected-null-block', {
  type: 'tool_results_projected', sessionId: SESSION, turnId: TURN,
  message: { role: 'assistant', content: [null] },
});
record('error-tool-results-projected-tool-result-missing-content', {
  type: 'tool_results_projected', sessionId: SESSION, turnId: TURN,
  message: { role: 'assistant', content: [{ type: 'tool_result', toolCallId: 'c1' }] },
});

// Attachment/media path fields are dereferenced by TS (`trim`, `split`,
// `toLowerCase`, `data.length`); a missing field throws before the
// `tool_call_finished` object literal reads `errorCode`/`data`.
record('error-tool-result-image-missing-mime-type', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'image', data: 'QUJD' }], { toolName: 'screenshot' }),
});
record('error-tool-result-image-missing-tool-name', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'image', mimeType: 'image/png', data: 'QUJD' }],
    { toolName: undefined }),
});
record('error-tool-result-file-missing-path', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'file', mimeType: 'text/plain' }], { toolName: 'write_file' }),
});
record('error-tool-results-projected-media-reference-missing-path', {
  type: 'tool_results_projected', sessionId: SESSION, turnId: TURN,
  message: message([{
    type: 'media_reference', toolCallId: 'call-m', originalBytes: 5,
    mimeType: 'image/png', mediaType: 'image',
  }]),
});
record('error-tool-result-image-missing-data', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'image', mimeType: 'image/png' }], { toolName: 'read_file' }),
});
record('error-tool-results-projected-tool-result-image-missing-data', {
  type: 'tool_results_projected', sessionId: SESSION, turnId: TURN,
  message: message([{
    type: 'tool_result', toolCallId: 'call-t',
    content: [{ type: 'image', mimeType: 'image/png' }],
  }]),
});
// Evaluation order: the attachment loop runs before the `tool_call_finished`
// object literal reads `event.result.error.code`.
record('error-tool-result-image-missing-mime-type-before-error-code', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: errorResult([{ type: 'image', data: 'QUJD' }],
    { toolName: 'screenshot', error: { message: 'boom' } }),
});

// Template interpolation of a nested array must join like
// `Array.prototype.join` (depth 600 is past the port's old 512 clamp and far
// below V8's measured ~2516 throw threshold).
const nestedArray = (depth: number, leaf: any): any => {
  let value = leaf;
  for (let index = 0; index < depth; index += 1) value = [value];
  return value;
};
record('tool-result-deep-array-content-text', {
  type: 'tool_result', sessionId: SESSION, turnId: TURN,
  result: successResult([{ type: 'text', text: nestedArray(600, 'leaf') }]),
});

// Fix round 3 (gate3 P1): fields that are present but not strings. TS calls a
// String method on some (`.trim()`, `.split()`, `.toLowerCase()` → TypeError
// for number/boolean/array/object) and only reads `.length` on others (number
// and boolean render `undefined`, arrays their element count, objects their
// own `length`). `persist` passes the three path fields through untouched
// below its 4096-byte threshold, then `safePathPart(...).trim()` throws above.
const WRONG_TYPES: [string, unknown][] = [
  ['number', 123],
  ['boolean', true],
  ['array', [1, 2]],
  ['object', { a: 1 }],
];
for (const [label, value] of WRONG_TYPES) {
  record(`type-tool-result-image-tool-name-${label}`, {
    type: 'tool_result', sessionId: SESSION, turnId: TURN,
    result: successResult([{ type: 'image', mimeType: 'image/png', data: 'QUJD' }],
      { toolName: value }),
  });
  record(`type-tool-result-image-tool-call-id-${label}`, {
    type: 'tool_result', sessionId: SESSION, turnId: TURN,
    result: successResult([{ type: 'image', mimeType: 'image/png', data: 'QUJD' }],
      { toolName: 'screenshot', toolCallId: value }),
  });
  record(`type-tool-result-image-mime-type-${label}`, {
    type: 'tool_result', sessionId: SESSION, turnId: TURN,
    result: successResult([{ type: 'image', mimeType: value, data: 'QUJD' }],
      { toolName: 'screenshot' }),
  });
  record(`type-tool-result-file-path-${label}`, {
    type: 'tool_result', sessionId: SESSION, turnId: TURN,
    result: successResult([{ type: 'file', path: value, mimeType: 'text/plain' }],
      { toolName: 'write_file' }),
  });
  record(`type-media-reference-path-${label}`, {
    type: 'tool_results_projected', sessionId: SESSION, turnId: TURN,
    message: message([{
      type: 'media_reference', toolCallId: 'call-m', originalBytes: 5,
      mimeType: 'image/png', mediaType: 'image', path: value, reason: 'ok',
    }]),
  });
  // `contentToText` reads `content.data.length`; the canonical block text reads
  // `block.data.length`. Neither expression calls a string method.
  record(`length-tool-result-image-data-${label}`, {
    type: 'tool_result', sessionId: SESSION, turnId: TURN,
    result: successResult([{ type: 'image', mimeType: 'image/png', data: value }],
      { toolName: 'read_file' }),
  });
  record(`length-tool-result-pdf-data-${label}`, {
    type: 'tool_result', sessionId: SESSION, turnId: TURN,
    result: successResult([{ type: 'pdf', mimeType: 'application/pdf', data: value }],
      { toolName: 'read_file' }),
  });
  record(`length-tool-results-projected-image-data-${label}`, {
    type: 'tool_results_projected', sessionId: SESSION, turnId: TURN,
    message: message([{
      type: 'tool_result', toolCallId: 'call-t',
      content: [{ type: 'image', mimeType: 'image/png', data: value }],
    }]),
  });
  record(`length-tool-results-projected-pdf-data-${label}`, {
    type: 'tool_results_projected', sessionId: SESSION, turnId: TURN,
    message: message([{
      type: 'tool_result', toolCallId: 'call-t',
      content: [{ type: 'pdf', mimeType: 'application/pdf', data: value }],
    }]),
  });
  // Below the store threshold `persist` records the raw field values.
  record(`persist-session-id-${label}`, {
    type: 'tool_result', sessionId: value, turnId: TURN,
    result: successResult([{ type: 'text', text: 'hello' }], { toolName: 'bash' }),
  });
  record(`persist-turn-id-${label}`, {
    type: 'tool_result', sessionId: SESSION, turnId: value,
    result: successResult([{ type: 'text', text: 'hello' }], { toolName: 'bash' }),
  });
  record(`persist-tool-call-id-${label}`, {
    type: 'tool_result', sessionId: SESSION, turnId: TURN,
    result: successResult([{ type: 'text', text: 'hello' }],
      { toolName: 'bash', toolCallId: value }),
  });
  const large = 'x'.repeat(5_000);
  record(`error-persist-session-id-${label}-above-threshold`, {
    type: 'tool_result', sessionId: value, turnId: TURN,
    result: successResult([{ type: 'text', text: large }], { toolName: 'bash' }),
  });
  record(`error-persist-turn-id-${label}-above-threshold`, {
    type: 'tool_result', sessionId: SESSION, turnId: value,
    result: successResult([{ type: 'text', text: large }], { toolName: 'bash' }),
  });
  record(`error-persist-tool-call-id-${label}-above-threshold`, {
    type: 'tool_result', sessionId: SESSION, turnId: TURN,
    result: successResult([{ type: 'text', text: large }],
      { toolName: 'bash', toolCallId: value }),
  });
}
// Object `length` is the property read (not `String(object).length`).
for (const [name, data] of [
  ['tool-result-image', { length: 5 }],
  ['tool-result-image-array', { length: [1, 2] }],
  ['tool-result-pdf', { length: 5 }],
  ['tool-results-projected-image', { length: 5 }],
  ['tool-results-projected-pdf', { length: 5 }],
] as [string, unknown][]) {
  if (name.startsWith('tool-result-')) {
    record(`length-${name}-object-length`, {
      type: 'tool_result', sessionId: SESSION, turnId: TURN,
      result: successResult([{
        type: name.includes('pdf') ? 'pdf' : 'image',
        mimeType: name.includes('pdf') ? 'application/pdf' : 'image/png',
        data,
      }], { toolName: 'read_file' }),
    });
  } else {
    record(`length-${name}-object-length`, {
      type: 'tool_results_projected', sessionId: SESSION, turnId: TURN,
      message: message([{
        type: 'tool_result', toolCallId: 'call-t',
        content: [{
          type: name.includes('pdf') ? 'pdf' : 'image',
          mimeType: name.includes('pdf') ? 'application/pdf' : 'image/png',
          data,
        }],
      }]),
    });
  }
}

const output = { sourceCommit, sourceSha256, cases };
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(resolve(process.argv[2], 'oracle.json'), `${JSON.stringify(output, null, 2)}\n`);
