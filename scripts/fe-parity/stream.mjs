// Browser wire contract: ui/server/pilotdeck-bridge.js:1206–1280.
// Native coordinates: src/agent/stream/TurnTimeline.ts:18–42,66–79;
// assistant_block -> text/isFinal: GatewayAgentEventProjector.ts:98–112.
// stream-self-check executes those frozen functions and compares wire frames.
// No frontend stores or framework hooks are involved in these fixtures.
export const SESSION_ID = 's';
export const STREAM_HZ = 25;
export const STREAM_PROTOCOLS = Object.freeze(['legacy', 'timeline']);
export const STREAM_SCENARIOS = Object.freeze({ endurance: 1500, reading: 200, search: 200 });
export const marker = index => `F2FRAME${String(index).padStart(4, '0')}END`;
export function frame(kind, fields = {}) {
  return { kind, sessionId: SESSION_ID, provider: 'pilotdeck', id: `${fields.runId || 'f2'}-${kind}-${fields.index ?? 0}`,
    timestamp: '2026-10-14T00:00:00.000Z', ...fields };
}
export function history() {
  // Like streaming-search.spec.mjs, put the search target near the tail. Keep
  // two later turns so centering still leaves >50px below it before readUp.
  // Search starts at Home, so this is still an initially offscreen result.
  return Array.from({ length: 24 }, (_, index) => [
    frame('text', { id: `question-${index}`, role: 'user', entryId: `entry-${index}`, content: `Question ${index}` }),
    frame('text', { id: `answer-${index}`, role: 'assistant', content: `Answer ${index}: historical reading paragraph.\n\nSecond paragraph for stable scrolling.${index === 21 ? '\n\nF2 searchable lighthouse. [F2 reference](https://example.invalid/f2-hidden-reference)' : ''}` }),
  ]).flat();
}
// A-class rewrites (specs/fe-parity/a-class-disposition.md): each original
// store/fixture call becomes a fixed, repository-owned wire frame batch. Tests
// only select a script and advance its steps in order; they never send content.
// Wire routing of these kinds: useChatRealtimeHandlers.ts:757-925 (text/user and
// compact_boundary -> appendRealtime, thinking -> updateStreamingThinking,
// complete -> refreshFromServer after 150 ms, :1009-1033).
const searchHistory = count => Array.from({ length: count }, (_, index) => frame('text', {
  id: `history-${index}`, role: index % 2 ? 'assistant' : 'user', content: `Historical message ${index}.\n\nAnother paragraph for reading.`,
  ...(index % 2 ? {} : { entryId: `history-entry-${index}` }) }));
// streaming-lifecycle.spec.mjs:11-16 objects, field for field.
const lifecycle = { runId: 'run', timestamp: '2026-09-11T00:00:00Z' };
const lifecycleUser = frame('text', { ...lifecycle, id: 'persisted-user', role: 'user', content: 'Review this image' });
const lifecycleThinking = frame('thinking', { ...lifecycle, id: 'persisted-thinking', content: 'Inspect the screenshot' });
const lifecycleAnswer = frame('text', { ...lifecycle, id: 'persisted-answer', role: 'assistant', content: 'The platform is too high.' });
const lifecycleCompact = frame('compact_boundary', { ...lifecycle, id: 'live-compact', compactionId: 'c1', preTokens: 120, postTokens: 40 });
// An earlier turn only gives the session a rendered conversation before the live turn.
const lifecyclePrior = [
  frame('text', { id: 'prior-user', role: 'user', entryId: 'prior-entry', content: 'Earlier question', timestamp: '2026-09-10T00:00:00Z' }),
  frame('text', { id: 'prior-answer', role: 'assistant', content: 'Earlier answer.', timestamp: '2026-09-10T00:00:01Z' }),
];
// streaming-search.spec.mjs:6-9, :17, :34.
const searchTarget = () => [...searchHistory(30),
  frame('text', { id: 'target', role: 'assistant', content: 'Visit https://example.com/unique-guide now.' })];
const searchLater = () => frame('text', { id: 'later', role: 'assistant', content: 'Another response.\n\n'.repeat(20) });
const complete = (runId, id) => frame('complete', { runId, id, exitCode: 0, success: true });
export const A_SCRIPTS = Object.freeze({
  // #1 streaming-lifecycle.spec.mjs:10
  'lifecycle-history-refresh': { runId: 'run', history: () => [...lifecyclePrior], steps: [
    // L22-27: optimistic image query, its echo, thinking, compaction, answer.
    { status: 'running', frames: () => [frame('status', { ...lifecycle, id: 'run-started', text: 'started' }),
      { ...lifecycleUser, id: 'local_user', images: ['data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7'] },
      { ...lifecycleUser, id: 'local_ws_echo' },
      frame('thinking', { ...lifecycle, id: 'live-thinking', content: 'Inspect the screenshot carefully.' }),
      lifecycleCompact, { ...lifecycleAnswer, id: 'live-answer' }] },
    // L16-17, L30: history without the compaction, shorter thinking, no image.
    { status: 'completed', history: () => [...lifecyclePrior, lifecycleUser, lifecycleThinking, lifecycleAnswer],
      frames: () => [complete('run', 'run-complete-1')] },
    // L40-41: persisted compaction and full thinking.
    { status: 'completed', history: () => [...lifecyclePrior, lifecycleUser, { ...lifecycleThinking, content: 'Inspect the screenshot carefully.' },
      { ...lifecycleCompact, id: 'persisted-compact' }, lifecycleAnswer], frames: () => [complete('run', 'run-complete-2')] },
  ] },
  // #5 streaming-lifecycle.spec.mjs:95 (cancel=true). The answer stays open (isStreaming).
  'lifecycle-search-cancel': { runId: 'a5-run', history: () => [...searchHistory(24),
    frame('text', { id: 'question', role: 'user', entryId: 'question-entry', content: 'Read a long response' })], steps: [
    { status: 'running', frames: () => [frame('status', { runId: 'a5-run', id: 'a5-started', text: 'started' }),
      frame('stream_delta', { runId: 'a5-run', id: 'a5-delta-0', content: 'Starting.' })] },
    { status: 'running', frames: () => [frame('stream_delta', { runId: 'a5-run', id: 'a5-delta-1',
      content: '\n\n' + 'A paragraph to read.\n\n'.repeat(240) + 'TAIL_NEEDLE' })] },
  ] },
  // #15 streaming-search.spec.mjs:21 (first scenario). L33-35 appends one static answer.
  'search-text-link': { history: () => searchTarget(), steps: [
    { status: 'idle', history: () => [...searchTarget(), searchLater()], frames: () => [searchLater()] },
  ] },
});
export function createStream({ send, now = () => performance.now(), schedule = setTimeout, cancel = clearTimeout }) {
  let timer, generation = 0, nextIndex = 0, state = { status: 'idle' }, transcript = history(), script;
  const stop = () => { generation++; if (timer !== undefined) cancel(timer); timer = undefined; if (state.status === 'running') state.status = 'cancelled'; };
  const reset = name => {
    if (name !== undefined && !Object.hasOwn(A_SCRIPTS, name)) throw new Error('Unknown A script');
    stop(); nextIndex = 0; script = name;
    transcript = name ? A_SCRIPTS[name].history() : history();
    state = name ? { status: 'idle', script: name, step: 0, recipients: [] } : { status: 'idle' };
  };
  function step(name, index) {
    if (!script || name !== script) throw new Error('A script not selected by reset');
    if (index !== state.step) throw new Error('A script steps advance in order');
    const entry = A_SCRIPTS[script].steps[index];
    if (!entry) throw new Error('A script exhausted');
    // Replace history before sending, so a complete-triggered refresh reads it.
    if (entry.history) transcript = entry.history();
    const recipients = entry.frames().map(send);
    state = { status: entry.status, script, step: index + 1, runId: A_SCRIPTS[script].runId, recipients: [...state.recipients, ...recipients] };
    return state;
  }
  function start(scenario, protocol = 'legacy') {
    if (script) throw new Error('A script selected; reset without one first');
    if (!STREAM_PROTOCOLS.includes(protocol)) throw new Error('Unknown stream protocol');
    if (!Object.hasOwn(STREAM_SCENARIOS, scenario)) throw new Error('Unknown stream scenario');
    if (state.status === 'running') throw new Error('Stream already running');
    const runId = `f2-run-${++generation}`, ownGeneration = generation;
    // A second response must belong to a new user turn. The baseline folds
    // earlier assistant answers within one user turn into its completed trace
    // (processGrouping.ts:142-162,909-969), even when runId differs.
    // Native timeline merging also needs the first turn's user anchor; without
    // it an unanchored first block is appended after the second turn on refresh.
    if (state.status === 'completed' || protocol === 'timeline') {
      const question = frame('text', { id: `${runId}-question`, runId, entryId: `${runId}-entry`, role: 'user', content: `Continue F2 stream ${runId}`,
        ...(protocol === 'timeline' ? { turnId: runId } : {}) });
      transcript.push(question);
      send(question);
    }
    state = { status: 'running', scenario, protocol, runId, startIndex: nextIndex, targetHz: STREAM_HZ, count: STREAM_SCENARIOS[scenario], sent: 0, startedAt: now(), times: [], recipients: [] };
    send(frame('status', { runId, text: 'started' }));
    let content = '';
    const blockId = `${runId}-text`;
    // The native allocator reserves order=0/revision=1, then updates before
    // each delta. offset is the preceding UTF-16 text length, never frame count.
    const position = revision => ({ version: 1, turnId: runId, id: blockId, order: 0, revision });
    const tick = () => {
      if (ownGeneration !== generation) return;
      // Marker identity spans runs; sent/count still measure this run's injection.
      const index = nextIndex++, delta = `${marker(index)} paragraph for scrolling.\n\n`;
      const recipients = send(frame('stream_delta', { runId, id: `${runId}-${index}`, content: delta,
        ...(protocol === 'timeline' ? { blockId, timeline: { ...position(state.sent + 2), offset: content.length } } : {}) }));
      content += delta;
      state.times.push(now()); state.recipients.push(recipients); state.sent++;
      if (state.sent === state.count) {
        // Append (never replace) before complete triggers a history refresh.
        // Baseline ui/server/routes/messages.js:208-238 projects persisted
        // assistant replies as kind=text, role=assistant, content=<full text>.
        // ui/src/components/chat/hooks/useChatRealtimeHandlers.ts:1009-1034
        // refreshes this HTTP history after each complete.
        const streamBoundary = { turnId: runId, through: 0, revision: state.count + 2 };
        const finalFields = protocol === 'timeline'
          ? { blockId, timeline: position(state.count + 3), isFinal: true } : {};
        transcript.push(frame('text', { id: `${runId}-answer`, runId, role: 'assistant', content,
          ...(protocol === 'timeline' ? { turnId: runId } : {}), ...finalFields }));
        send(frame('stream_end', { runId, ...(protocol === 'timeline' ? { streamBoundary } : {}) }));
        if (protocol === 'timeline') send(frame('text', { id: `${runId}-final`, runId,
          role: 'assistant', content, ...finalFields, streamState: 'closed' }));
        send(frame('complete', { runId, exitCode: 0, success: true }));
        state.status = 'completed'; state.finishedAt = now(); timer = undefined;
      } else timer = schedule(tick, 1000 / STREAM_HZ);
    };
    timer = schedule(tick, 1000 / STREAM_HZ);
    return state;
  }
  return { start, step, stop, reset, get state() { return state; }, get messages() { return transcript; } };
}
export function streamErrors(state) {
  const errors = [];
  if (state.status !== 'completed' || state.sent !== state.count || state.times?.length !== state.count) return ['incomplete injection'];
  const elapsed = state.times.at(-1) - state.times[0];
  const measuredHz = (state.count - 1) * 1000 / elapsed;
  if (!Number.isFinite(measuredHz) || measuredHz < 20) errors.push(`measured injection ${measuredHz}Hz < 20Hz`);
  if (state.recipients.length !== state.count || state.recipients.some(n => n !== 1)) errors.push('each delta must reach exactly one watched browser connection');
  if (state.scenario === 'endurance' && state.finishedAt - state.startedAt < 60000) errors.push('endurance shorter than 60s');
  return errors;
}
export function textErrors(text, count, startIndex = 0) {
  const tokens = [...text.matchAll(/F2FRAME(\d{4})END/g)].map(match => Number(match[1]));
  return tokens.length === count && tokens.every((value, index) => value === startIndex + index) ? [] : ['DOM delta loss, duplication or reordering'];
}
export function readingErrors(samples, readingTop) {
  return samples.length > 1 && samples.every(sample => Math.abs(sample.top - readingTop) < 2) ? [] : ['reading scrollTop moved >=2px or no samples'];
}
export function followErrors(samples) {
  if (samples.length < 2) return ['no bottom-follow observations'];
  const errors = [];
  let lastBottom = samples[0].at;
  for (let index = 0; index < samples.length; index++) {
    const sample = samples[index];
    if (index && sample.at - samples[index - 1].at > 500) errors.push('DOM sampling stalled >500ms');
    if (sample.distance < 3) lastBottom = sample.at;
    if (sample.at - lastBottom > 500) errors.push('bottom follow absent >500ms');
  }
  if (samples.at(-1).distance >= 3) errors.push('not at bottom after completion');
  return [...new Set(errors)];
}
