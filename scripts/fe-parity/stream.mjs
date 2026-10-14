// Browser wire contract: pilotdeck-message.js:22, pilotdeck-bridge.js:1206–1278.
// No frontend stores or framework hooks are involved in these fixtures.
export const SESSION_ID = 's';
export const STREAM_HZ = 25;
export const STREAM_SCENARIOS = Object.freeze({ endurance: 1500, reading: 200, search: 200 });
export const marker = index => `F2FRAME${String(index).padStart(4, '0')}END`;
export function frame(kind, fields = {}) {
  return { kind, sessionId: SESSION_ID, provider: 'pilotdeck', id: `${fields.runId || 'f2'}-${kind}-${fields.index ?? 0}`,
    timestamp: '2026-10-14T00:00:00.000Z', ...fields };
}
export function history() {
  return Array.from({ length: 24 }, (_, index) => [
    frame('text', { id: `question-${index}`, role: 'user', entryId: `entry-${index}`, content: `Question ${index}` }),
    frame('text', { id: `answer-${index}`, role: 'assistant', content: `Answer ${index}: historical reading paragraph.\n\nSecond paragraph for stable scrolling.${index === 10 ? '\n\nF2 searchable lighthouse. [F2 reference](https://example.invalid/f2-hidden-reference)' : ''}` }),
  ]).flat();
}
export function createStream({ send, now = () => performance.now(), schedule = setTimeout, cancel = clearTimeout }) {
  let timer, generation = 0, nextIndex = 0, state = { status: 'idle' }, transcript = history();
  const stop = () => { generation++; if (timer !== undefined) cancel(timer); timer = undefined; if (state.status === 'running') state.status = 'cancelled'; };
  const reset = () => { stop(); nextIndex = 0; transcript = history(); state = { status: 'idle' }; };
  function start(scenario) {
    if (!Object.hasOwn(STREAM_SCENARIOS, scenario)) throw new Error('Unknown stream scenario');
    if (state.status === 'running') throw new Error('Stream already running');
    const runId = `f2-run-${++generation}`, ownGeneration = generation;
    // A second response must belong to a new user turn. The baseline folds
    // earlier assistant answers within one user turn into its completed trace
    // (processGrouping.ts:142-162,909-969), even when runId differs.
    if (state.status === 'completed') {
      const question = frame('text', { id: `${runId}-question`, runId, entryId: `${runId}-entry`, role: 'user', content: `Continue F2 stream ${runId}` });
      transcript.push(question);
      send(question);
    }
    state = { status: 'running', scenario, runId, startIndex: nextIndex, targetHz: STREAM_HZ, count: STREAM_SCENARIOS[scenario], sent: 0, startedAt: now(), times: [], recipients: [] };
    send(frame('status', { runId, text: 'started' }));
    let content = '';
    const tick = () => {
      if (ownGeneration !== generation) return;
      // Marker identity spans runs; sent/count still measure this run's injection.
      const index = nextIndex++, delta = `${marker(index)} paragraph for scrolling.\n\n`;
      const recipients = send(frame('stream_delta', { runId, id: `${runId}-${index}`, content: delta }));
      content += delta;
      state.times.push(now()); state.recipients.push(recipients); state.sent++;
      if (state.sent === state.count) {
        // Append (never replace) before complete triggers a history refresh.
        // Baseline ui/server/routes/messages.js:208-238 projects persisted
        // assistant replies as kind=text, role=assistant, content=<full text>.
        // ui/src/components/chat/hooks/useChatRealtimeHandlers.ts:1009-1034
        // refreshes this HTTP history after each complete.
        transcript.push(frame('text', { id: `${runId}-answer`, runId, role: 'assistant', content }));
        send(frame('stream_end', { runId }));
        send(frame('complete', { runId, exitCode: 0, success: true }));
        state.status = 'completed'; state.finishedAt = now(); timer = undefined;
      } else timer = schedule(tick, 1000 / STREAM_HZ);
    };
    timer = schedule(tick, 1000 / STREAM_HZ);
    return state;
  }
  return { start, stop, reset, get state() { return state; }, get messages() { return transcript; } };
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
export function textErrors(text, count) {
  const tokens = [...text.matchAll(/F2FRAME(\d{4})END/g)].map(match => Number(match[1]));
  return tokens.length === count && tokens.every((value, index) => value === index) ? [] : ['DOM delta loss, duplication or reordering'];
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
