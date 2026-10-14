/** Pre-registered evidence rules. No assumed TS terminal output or Rust score. */
import { isDeepStrictEqual } from 'node:util';

export const RULES = Object.freeze({
  FRAME_PUBLICATIONS: '实时帧须逐项等于真实 open queue 的发布记录（顺序和原始 payload）',
  STATUS_RAW: 'durable status 须逐项等于独立读盘原文筛选的 entries',
  STATUS_COMMITS: '持久 status 须等于成功 commit journal，失败尝试不能算成功',
  STATUS_FIFO: '底层 status store 进入顺序须等于真实 writer 接纳顺序',
  REGISTRY_EVENTS: 'registry events 须逐项等于独立读盘快照，不能漏读',
  REGISTRY_RESULT: 'registry result 须等于独立读盘 result，覆盖不能被忽略',
  REGISTRY_RECORD: '完整 record/state/revision/lastSeq 须等于磁盘快照',
  REGISTRY_COMMITS: 'registry events 须等于实际成功 append journal，失败尝试不能算成功',
  REGISTRY_SEQ: 'registry seq 连续且 lastSeq/revision 与 events 一致',
  REGISTRY_FIFO: 'registry persist 进入顺序须等于 mutationChain 接纳顺序',
});

function requireRule(ok, rule) {
  if (!ok) throw Object.assign(new Error(`${rule}: ${RULES[rule]}`), { rule });
}
const rows = (o, kind) => o.timeline.filter(r => r.kind === kind);
const keys = (o, kind) => rows(o, kind).map(r => r.key);
const statuses = text => text.trim() ? text.trim().split('\n').map(JSON.parse)
  .filter(e => e.type === 'agent_status_message') : [];

export function checkFrames(o) {
  requireRule(o.frames.length > 0 && isDeepStrictEqual(o.frames,
    rows(o, 'queue.enqueue').filter(r => !r.closed).map(r => r.event)), 'FRAME_PUBLICATIONS');
}
export function checkStatus(o) {
  requireRule(isDeepStrictEqual(o.durableStatus.entries, statuses(o.durableStatus.rawText)), 'STATUS_RAW');
  requireRule(isDeepStrictEqual(o.durableStatus.entries, rows(o, 'status.commit').map(r => r.entry)), 'STATUS_COMMITS');
  requireRule(isDeepStrictEqual(keys(o, 'status.store.enter'), keys(o, 'status.attempt')), 'STATUS_FIFO');
}
export function checkRegistry(o) {
  if (!o.registry.applicable) {
    requireRule(o.registry.reason === 'runRegistry was not configured', 'REGISTRY_RECORD');
    return;
  }
  const disk = JSON.parse(o.registry.rawText);
  const events = Object.values(disk.events)[0] ?? [];
  const record = disk.records[0];
  requireRule(events.length > 0 && isDeepStrictEqual(o.registry.events, events), 'REGISTRY_EVENTS');
  requireRule(isDeepStrictEqual(o.registry.record?.result, record?.result), 'REGISTRY_RESULT');
  requireRule(isDeepStrictEqual(o.registry.record, record), 'REGISTRY_RECORD');
  requireRule(isDeepStrictEqual(o.registry.events.map(e => e.event),
    rows(o, 'registry.append.commit').map(r => r.event)), 'REGISTRY_COMMITS');
  requireRule(o.registry.events.every((e, i) => e.seq === i + 1)
    && o.registry.record.lastSeq === events.length
    && o.registry.record.revision === events.length + 1, 'REGISTRY_SEQ');
  requireRule(isDeepStrictEqual(keys(o, 'registry.persist.enter').filter(k => k !== 'accept'),
    keys(o, 'registry.append.attempt')), 'REGISTRY_FIFO');
}
export function checkObservation(o) {
  checkFrames(o); checkStatus(o); checkRegistry(o);
}

// Equality alone can accept two identical damaged observers. Validate each side
// against its own independently retained raw evidence BEFORE checking equality.
export function compareObservers(left, right) {
  checkObservation(left); checkObservation(right);
  return isDeepStrictEqual(left.frames, right.frames)
    && isDeepStrictEqual(left.durableStatus.entries, right.durableStatus.entries)
    && isDeepStrictEqual(left.registry, right.registry);
}

export function runControls(frameSource, registrySource, failedStatusSource, failedRegistrySource) {
  const controls = [];
  const control = (name, rule, source, damage, pair = false) => {
    const bad = structuredClone(source);
    damage(bad);
    let hit;
    try {
      if (pair) compareObservers(bad, structuredClone(bad));
      else checkObservation(bad);
    } catch (error) { hit = error.rule; }
    if (hit !== rule) throw new Error(`${name}: expected ${rule}, observed ${hit ?? 'no rejection'}`);
    controls.push({ name, expectedRule: rule, hitRule: hit, detected: true, source: source.id,
      ...(pair ? { identicalBadSides: true } : {}) });
  };
  const dropTimeout = o => {
    const index = o.frames.findIndex(e => e.type === 'error' && e.code === 'turn_timeout');
    if (index < 0) throw new Error('timeout control requires an actually observed frame');
    o.frames.splice(index, 1);
  };
  control('drop-timeout-frame', 'FRAME_PUBLICATIONS', frameSource, dropTimeout);
  control('omit-registry-events', 'REGISTRY_EVENTS', registrySource, o => o.registry.events.pop());
  control('ignore-result-overwrite', 'REGISTRY_RESULT', registrySource, o => {
    const timeout = o.registry.events.find(e => e.event.type === 'error' && e.event.code === 'turn_timeout');
    if (!timeout || isDeepStrictEqual(timeout.event, o.registry.record.result))
      throw new Error('result control requires actual timeout -> later-result evidence');
    o.registry.record.result = timeout.event;
  });
  control('attempt-as-success', 'STATUS_COMMITS', failedStatusSource, o => {
    const failure = rows(o, 'status.failure')[0];
    if (!failure) throw new Error('attempt control requires an actual failed storage write');
    const attempted = rows(o, 'status.attempt').find(r => r.key === failure.key);
    const fake = { type: 'agent_status_message', ...attempted.status };
    o.durableStatus.entries.push(fake);
    // Corrupt both the decoded observer and its alleged raw status read. The
    // independent commit journal must still reject the forged success.
    o.durableStatus.rawText += JSON.stringify(fake) + '\n';
  });
  control('registry-attempt-as-success', 'REGISTRY_COMMITS', failedRegistrySource, o => {
    const failed = rows(o, 'registry.append.failure')[0];
    if (!failed) throw new Error('registry attempt control requires an actual failed append');
    const event = rows(o, 'registry.append.attempt').find(r => r.key === failed.key).event;
    const disk = JSON.parse(o.registry.rawText);
    const entries = Object.values(disk.events)[0];
    entries.push({ seq: entries.length + 1, event });
    const record = disk.records[0];
    record.lastSeq++; record.revision++; record.result = event;
    o.registry.events = entries; o.registry.record = record;
    o.registry.rawText = JSON.stringify(disk);
  });
  control('both-sides-bad', 'FRAME_PUBLICATIONS', frameSource, dropTimeout, true);
  control('drop-durable-status', 'STATUS_RAW', frameSource, o => o.durableStatus.entries.pop());
  for (const [kind, rule] of [['status.store.enter', 'STATUS_FIFO'], ['registry.persist.enter', 'REGISTRY_FIFO']]) {
    control(`reverse-${kind}`, rule, registrySource, o => {
      const entries = rows(o, kind).filter(r => r.key !== 'accept');
      [entries[0].key, entries[1].key] = [entries[1].key, entries[0].key];
    });
  }
  return controls;
}
