import { isDeepStrictEqual as equal } from 'node:util';
import { RULES, CASES, REPEATS } from './preregistration-b.mjs';
import { EXTRA_CASES } from './preregistration-b-extra.mjs';
export function requireRule(ok, rule, detail = '') {
  if (!ok) throw Object.assign(new Error(`${rule}: ${RULES[rule]} ${detail}`), { rule });
}
export const rows = (o, kind) => o.timeline.filter(r => r.kind === kind);
export function decodeJsonl(text) {
  const entries = [], invalid = [];
  text.split(/\r?\n/).forEach((line, i) => {
    if (!line.trim()) return;
    try { entries.push(JSON.parse(line)); } catch { invalid.push({ line: i + 1, text: line }); }
  });
  return { entries, invalid };
}
export function checkFrames(o) {
  requireRule(equal(o.connections.map(c => c.name), o.expectedConnections), 'CONNECTIONS');
  for (const c of o.connections) {
    const raw = rows(o, 'wire.send').filter(r => r.connection === c.name).map(r => r.text);
    requireRule(raw.some(t => JSON.parse(t).type === 'hello_ok'), 'CONNECTIONS', c.name);
    requireRule(equal(c.frames, raw.map(JSON.parse)) && equal(c.raw, raw), 'FRAME_RAW', c.name);
    const ids = new Set(c.frames.filter(f => f.type === 'event').map(f => f.id));
    for (const id of ids) {
      const events = c.frames.filter(f => f.type === 'event' && f.id === id);
      requireRule(events.every((f, i) => f.seq === i) && events.filter(f => f.final).length === 1
        && events.at(-1).final, 'FRAME_SEQ', `${c.name}/${id}`);
    }
  }
}
export function checkStatus(o) {
  for (const d of o.durable) {
    requireRule(equal({ entries: d.entries, invalid: d.invalid }, decodeJsonl(d.rawText)), 'STATUS_RAW');
    const readerOrder = [...d.entries].sort((a, b) => a.sequence - b.sequence || a.createdAt.localeCompare(b.createdAt));
    requireRule(equal(d.fresh.entries, readerOrder), 'STATUS_RAW', 'fresh production reader disagrees with raw disk');
    const invalidLines = d.fresh.diagnostics.filter(r => r.code === 'transcript_line_invalid').map(r => r.line);
    requireRule(equal(invalidLines, d.invalid.map(r => r.line)), 'STATUS_RAW', 'reader diagnostics disagree with raw bad lines');
    const written = o.timeline.filter(r => ['fs.append.commit', 'fs.partial'].includes(r.kind) && r.path === d.path).map(r => r.text).join('');
    requireRule(d.rawText === written && equal(d.entries, decodeJsonl(written).entries), 'STATUS_COMMITS', d.path);
    const admitted = rows(o, 'status.admit').filter(r => r.session === d.session).map(r => r.key);
    const entered = rows(o, 'status.enter').filter(r => r.session === d.session).map(r => r.key);
    requireRule(equal(admitted, entered), 'STATUS_FIFO', d.session);
  }
}
export function checkRegistry(o) {
  if (!o.registry.applicable) { requireRule(Boolean(o.registry.reason), 'REGISTRY_RAW'); return; }
  const disk = JSON.parse(o.registry.rawText);
  requireRule(equal(o.registry.records, disk.records) && equal(o.registry.events, disk.events), 'REGISTRY_RAW');
  for (const read of o.registry.fresh) {
    const record = disk.records.find(r => r.sessionKey === read.query.sessionKey && r.runId === read.query.runId);
    const key = `${read.query.projectKey ?? ''}\0${read.query.sessionKey}\0${read.query.runId}`;
    requireRule(equal(read.record, record ?? null) && equal(read.events, disk.events[key] ?? []), 'REGISTRY_RAW');
  }
  const commits = rows(o, 'fs.rename.commit').filter(r => r.to === o.registry.path);
  requireRule(commits.length > 0 && equal(JSON.parse(commits.at(-1).text), disk), 'REGISTRY_COMMITS');
  const skipped = rows(o, 'registry.skipped').map(r => r.key);
  const admitted = rows(o, 'registry.admit').map(r => r.key);
  for (const key of skipped) admitted.splice(admitted.indexOf(key), 1);
  requireRule(equal(admitted, rows(o, 'registry.enter').map(r => r.key)), 'REGISTRY_FIFO');
}
export function checkBindings(o) {
  for (const a of rows(o, 'answer.consume')) {
    requireRule(equal(a.binding ?? null, a.receivedBinding ?? null), 'BINDING_IDENTITY');
  }
  for (const a of rows(o, 'answer.result')) {
    const consumed = rows(o, 'answer.consume').find(r => r.call === a.call);
    requireRule(Boolean(consumed?.found) === a.delivered, 'ANSWER_JOURNAL');
  }
  const resolved = rows(o, 'answer.resolve').map(r => `${r.session}/${r.requestId}`);
  requireRule(new Set(resolved).size === resolved.length, 'ANSWER_JOURNAL');
}
export function checkIsolation(o) {
  for (const d of o.durable) requireRule(d.entries.every(e => e.sessionId === d.session), 'SESSION_ISOLATION');
  for (const r of o.registry.records ?? []) {
    const key = `${r.projectKey ?? ''}\0${r.sessionKey}\0${r.runId}`;
    requireRule((o.registry.events[key] ?? []).every(e => !e.event.runId || e.event.runId === r.runId), 'SESSION_ISOLATION');
  }
  for (const r of rows(o, 'loop.wait')) {
    requireRule(rows(o, 'submit.input').some(i => i.sessionKey === r.session && i.runId === r.run), 'SESSION_ISOLATION');
  }
  const bindings = o.connections.map(c => c.binding).filter(Boolean);
  requireRule(new Set(bindings.map(b => `${b.connectionId}/${b.generation}`)).size === bindings.length, 'BINDING_IDENTITY');
  for (const c of o.connections) for (const frame of c.frames.filter(f => f.type === 'event')) {
    const request = rows(o, 'wire.receive').find(r => r.connection === c.name && r.id === frame.id && r.method === 'submit_turn');
    requireRule(Boolean(request) && (!frame.event.runId || frame.event.runId === request.params.runId), 'SESSION_ISOLATION');
  }
}
export function checkObservation(o) {
  checkFrames(o); checkStatus(o); checkRegistry(o); checkBindings(o); checkIsolation(o);
  requireRule(o.requiredBarriers.every(b => rows(o, 'barrier').some(r => r.name === b)), 'BARRIERS');
  if (o.fault) requireRule(rows(o, 'fs.fault').length === 1 && o.recovery !== null, 'FAULT_EXERCISED');
}
export function checkCoverage(observations) {
  requireRule(equal(observations.map(o => o.caseId), [...CASES, ...EXTRA_CASES].flatMap(c => Array(REPEATS).fill(c.id))), 'COVERAGE');
}
export function compareObservers(left, right) {
  checkObservation(left); checkObservation(right);
  return equal(left.connections, right.connections) && equal(left.durable, right.durable) && equal(left.registry, right.registry);
}
