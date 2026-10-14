/** A5.2b preregistration. Commit this file ALONE before coordinator collection.
 * Written before the first B measurement. Outcomes deliberately unspecified.
 * k=5 repeats one explicit schedule; not an estimate of scheduler stability.
 */
export const REPEATS = 5;
export const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
export const RULES = Object.freeze({
  COVERAGE: '完整 case 集合、每例 k=5、每连接和每观察面显式存在；不接受隐式 skip',
  CONNECTIONS: '声明的每个连接须经过真实 hello 和 sendText；漏连接不能算双连接',
  FRAME_RAW: '每连接帧逐项等于独立 sendText 原始文本解码，全序与请求 seq 保留',
  FRAME_SEQ: '每个 submit 请求 seq 从 0 连续；final 是包装帧，不能算第二次业务终态',
  STATUS_RAW: 'session 观察值须等于独立读盘原文解析（含坏行 diagnostics）',
  STATUS_COMMITS: '成功完整 JSONL 行须有真实 appendFile 成功 journal；partial debris 单列',
  STATUS_FIFO: '真实 writer 接纳后底层 store 进入的顺序相同；同时在飞 store 不能越过 FIFO',
  REGISTRY_RAW: '完整 registry record/events/result 须等于独立读盘快照；失败读面明确记录',
  REGISTRY_COMMITS: '磁盘变化须由成功 rename 或故障发生在 rename 后的 journal 支撑',
  REGISTRY_FIFO: '真实 mutationChain 接纳和 persist 进入顺序一致；不得装置绕过',
  SESSION_ISOLATION: 'run/request 到 session 的归属须与接纳材料、transcript 和 registry 一致',
  BINDING_IDENTITY: '发到 consume 的 binding 须来自该真实连接 hello；不得把旧 binding 替成新 binding',
  ANSWER_JOURNAL: 'delivered 值须与真实 bus consume 和 resolve journal 相符，同一 pending 只能 resolve 一次',
  BARRIERS: '预登记的关键 barrier 均到达；没有用 sleep 决定交错',
  FAULT_EXERCISED: '故障必须命中登记的文件/操作；故障错误和恢复读面均留存',
  RESTART_IDENTITY: 'cold-restart 前后不同自有 PID、同一 PILOT_HOME，旧 PID 已回收；不得用对象重建冒充',
  SIDECAR_IDENTITY: '真实 sidecar spawn/握手/Module 调用和 unknown ledger 原始证据；不可用受控抛错冒充',
});

const dual = ['permission', 'question'].flatMap(kind => ['owner-first', 'other-first', 'handoff-double']
  .map(order => ({ id: `dual/${kind}/${order}`, kind, order,
    schedule: ['A hello', 'B hello', 'A submit held at pending request', 'B run_events + active_turn_snapshot',
      ...(order === 'handoff-double' ? ['rebind from A to B with exact previous binding'] : []),
      `answer order ${order}`, 'both connections query pending/snapshot + run_events', 'release loop return', 'read all three planes'] })));
export const CASES = Object.freeze([
  ...dual,
  ...['distinct-run', 'same-run', 'sdk-control'].map(order => ({ id: `dual/${order}`, order,
    schedule: ['A+B hello', 'A submit reaches held loop', 'B submit same session / SDK control answer', 'B read registry', 'release A', 'reread planes'] })),
  ...['A-first', 'B-first'].map(order => ({ id: `multi/${order}`, order,
    schedule: ['A+B hello', 'both sessions submit reach held loops', 'cross-session read/answer probes', `release ${order}`, 'read frames/status/registry by session'] })),
  ...['permission', 'question'].flatMap(kind => ['before-disconnect', 'after-disconnect'].map(order => ({
    id: `close/${kind}/${order}`, kind, order,
    schedule: ['A submit reaches pending', `A close held ${order}`, 'B reconnect exact previous binding', 'B resume_session',
      'A old answer then B answer while loop held', 'release old close', 'read active state', 'release loop return', 'reread planes'] }))),
  { id: 'close/no-pending/new-turn', schedule: ['A loop held without pending', 'A close held after disconnect',
    'complete A gateway but hold WS submit return', 'B resume + new submit reaches held loop', 'release A old close', 'record which run abort sees', 'release B', 'release A stream return', 'reread planes'] },
  ...['write-eio', 'partial-write', 'read-eio'].map(fault => ({ id: `store/session/${fault}`, fault,
    schedule: ['real writer/session/gateway', `arm transcript ${fault}`, 'record status or read transcript', 'disarm', 'append recovery status', 'fresh eventStore.read + raw disk'] })),
  ...['accept-read-eio', 'accept-write-eio', 'accept-partial-write', 'accept-rename-before', 'accept-rename-after',
    'append-write-eio', 'append-partial-write', 'append-rename-before', 'append-rename-after', 'read-eio', 'corrupt-snapshot']
    .map(fault => ({ id: `store/registry/${fault}`, fault,
      schedule: ['real FileRunRegistry + gateway', 'seed if required', `arm ${fault}`, 'real request', 'retain disk/temp bytes',
        'disarm', 'fresh get/events and recovery submit/read', 'reread all planes'] })),
]);
export const EXTERNAL_CASES = Object.freeze([
  { id: 'cold/running', schedule: ['child A held run with committed registry and transcript', 'parent observes ready barrier',
    'terminate and reap only child A', 'start child B with same PILOT_HOME', 'run_get/run_events/run_reattach/resume/read history', 'retain disk before/after'] },
  { id: 'cold/unknown', schedule: ['child A commits result_unknown to real registry', 'parent observes ready barrier',
    'terminate and reap child A', 'child B same PILOT_HOME reads registry and transcript', 'unknown event/result equality'] },
]);
export const SIDECAR_CASE = Object.freeze({ id: 'sidecar/G/no-registry', schedule: ['real stdio sidecar handshake',
  'hold Module model call at barrier', 'gateway timeout callback', 'observe abort/reconciliation outcome from real sidecar transport',
  'hold outer completion fence until late outcome', 'read frames/status/registry separately'] });
export const CONTROLS = Object.freeze([
  { name: 'apparatus-bypass-status-fifo', rule: 'STATUS_FIFO', level: 'apparatus' },
  { name: 'apparatus-bypass-registry-fifo', rule: 'REGISTRY_FIFO', level: 'apparatus' },
  { name: 'apparatus-omit-B-connection', rule: 'CONNECTIONS', level: 'apparatus' },
  { name: 'apparatus-old-binding-as-new', rule: 'BINDING_IDENTITY', level: 'apparatus' },
  { name: 'drop-frame', rule: 'FRAME_RAW', level: 'observer' },
  { name: 'omit-registry-events', rule: 'REGISTRY_RAW', level: 'observer' },
  { name: 'ignore-result-overwrite', rule: 'REGISTRY_RAW', level: 'observer' },
  { name: 'attempt-as-success', rule: 'STATUS_COMMITS', level: 'observer' },
  { name: 'both-sides-bad', rule: 'FRAME_RAW', level: 'observer' },
  { name: 'double-answer-success', rule: 'ANSWER_JOURNAL', level: 'observer' },
]);
export const OBSERVATION_FIELDS = Object.freeze({
  frames: ['connection', 'raw text', 'decoded payload', 'request id', 'seq', 'final', 'notifications', 'responses'],
  durable: ['path', 'raw bytes/text', 'entries', 'invalid lines', 'success/failure attempts', 'fresh reader diagnostics'],
  registry: ['raw snapshot', 'temp debris', 'get', 'events', 'seq', 'state', 'revision', 'lastSeq', 'result', 'recovery'],
  apparatus: ['barriers', 'real hello bindings', 'consume/resolve', 'abort target', 'owned PIDs', 'source and apparatus hashes'],
});
