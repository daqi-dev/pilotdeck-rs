// #1350 golden fixture: raw-socket behavior of the TS GatewayServer's
// module-host approvals transport (POST /api/module-host/approvals/status and
// POST /api/module-host/approvals/resume) over the real startGatewayServer +
// real publicApprovalHttpTransport + real publicApprovalBridge + real
// StaffDeckSopControlPlane/SopStateStore over seeded state files, plus a
// scripted stub bridge for the transport-level result/failure cells, recorded
// byte-exactly per cell with the Date value masked.
//
// Six servers: `main` (static binding, real bridge over the seeded store),
// `dynamic` (+ a scripted resolveBinding), `nomap` (no resolveSession port),
// `bare` (no publicApprovals option at all), `stub` (a plain scripted bridge
// object) and `nobind` (empty binding strings). Every mutating cell is
// self-contained on its own seeded session, so cell order carries no state.
//
// The 64 KiB budget cells do not inline their padding: the recorded request
// carries the __PDRS_PAD__ marker and a pad.size the replayer substitutes
// 'a'.repeat(size) for, so the oracle stays small.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdir, writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {connect} from 'node:net';
import {once} from 'node:events';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#1350 pinned Node');
assert.equal(process.versions.icu, '78.2', '#1350 pinned ICU');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, ['src/gateway/server/GatewayServer.ts', 'src/composition/publicApprovalHttpTransport.ts', 'src/composition/publicApprovalBridge.ts', 'src/composition/publicHostHttpTransport.ts', 'src/sop/staffdeck/StaffDeckSopControlPlane.ts', 'src/sop/staffdeck/SopStateStore.ts']);
const deadline = setTimeout(() => { console.error('#1350 probe deadline'); process.exit(1); }, 240000);
const req = createRequire(join(baseline, 'package.json'));
const {transformSync} = createRequire(req.resolve('tsx'))('esbuild');
async function load(source:string, imports:Record<string,any>={}) {
  const code=transformSync(await (await import('node:fs/promises')).readFile(join(baseline,source),'utf8'),{loader:'ts',format:'cjs',target:'es2022',sourcefile:source}).code;
  const module={exports:{} as any};
  new Function('require','module','exports',code)((name:string)=>{
    if(name in imports)return imports[name];
    if(name.startsWith('node:'))return req(name);
    return new Proxy({}, {get(_t,p){if(p==='__esModule')return true;throw new Error(`unexercised import ${source} -> ${name}.${String(p)}`);}});
  },module,module.exports);return module.exports;
}
const version=await load('src/gateway/protocol/version.ts');
const errors=await load('src/gateway/dialog/errors.ts');
const skills=await load('src/extension/skills/SkillManager.ts');
const connection=await load('src/gateway/server/GatewayWsConnection.ts',{'../protocol/version.js':version,'../dialog/errors.js':errors,'../../extension/skills/index.js':skills});
const websocket=await load('src/gateway/server/websocket.ts');
const staticAssets=await load('src/gateway/server/staticAssets.ts');
const approval=await load('src/composition/publicApprovalHttpTransport.ts');
const publicHostWire=await load('src/composition/publicHostWire.ts');
const hostTransport=await load('src/composition/publicHostHttpTransport.ts',{'./publicHostWire.js':publicHostWire});
const webRouter=await load('src/adapters/web/httpRouter.ts');
const sopStateStore=await load('src/sop/staffdeck/SopStateStore.ts');
const controlPlane=await load('src/sop/staffdeck/StaffDeckSopControlPlane.ts',{'./SopStateStore.js':sopStateStore});
const {createPublicApprovalBridge}=await load('src/composition/publicApprovalBridge.ts');
const {startGatewayServer}=await load('src/gateway/server/GatewayServer.ts',{
  './websocket.js':websocket,'./GatewayWsConnection.js':connection,'./staticAssets.js':staticAssets,
  '../../composition/publicApprovalHttpTransport.js':approval,
  '../../composition/publicHostHttpTransport.js':hostTransport,
  '../../adapters/web/httpRouter.js':webRouter,
});

const token = 'fixture-token';
const binding = {tenantId: 't1', agentId: 'a1', pilotDeckUserId: 'u1'};
const otherBinding = {tenantId: 't2', agentId: 'a2', pilotDeckUserId: 'u2'};
// The scripted authenticate port: one subject per bearer; unknown bearers are
// the formal authority's own rejection.
const subjects: Record<string, Record<string, unknown>> = {
  'staff-admin': {tenantId: 't1', source: 'web', disabled: false, userId: 'staff-admin-id', role: 'admin'},
  'staff-member': {tenantId: 't1', source: 'web', disabled: false, userId: 'staff-member-id', role: 'member'},
  'staff-other': {tenantId: 't1', source: 'web', disabled: false, userId: 'someone-else', role: 'member'},
  'staff-tenant2': {tenantId: 't2', source: 'web', disabled: false, userId: 'staff-admin-id', role: 'admin'},
  'staff-disabled': {tenantId: 't1', source: 'web', disabled: true, userId: 'staff-admin-id', role: 'admin'},
  'staff-source': {tenantId: 't1', source: 'cli', disabled: false, userId: 'staff-admin-id', role: 'admin'},
  'staff-guest': {tenantId: 't1', source: 'web', disabled: false, userId: 'staff-admin-id', role: 'guest'},
  'staff-emptyuser': {tenantId: 't1', source: 'web', disabled: false, userId: '', role: 'admin'},
};
const authenticate = async (input: {bearer: string}) => {
  const subject = subjects[input.bearer];
  if (!subject) throw Object.assign(new Error('approver rejected'), {status: 401, code: 'APPROVAL_AUTH_REJECTED'});
  return structuredClone(subject);
};
// The scripted resolveSession port: the registered session catalog.
const sessionProjects: Record<string, string> = {
  'sess-handoff': 'proj1', 'sess-handoff2': 'proj1', 'sess-handoff3': 'proj1', 'sess-active': 'proj1', 'sess-ext': 'proj1',
  'sess-waitstale': 'proj1', 'sess-rev': 'proj1', 'sess-badpin': 'proj1', 'sess-unassigned': 'proj1',
  'sess-gone': 'proj1', 'sess-proj2': 'proj2', 'sess-done': 'proj1', 'sess-mismatch': 'proj1',
  'sess-dyn2': 'proj1', 'sess-dynfail': 'proj1',
};
const resolveSession = async (input: {sessionKey: string; binding: typeof binding}) => {
  const projectKey = sessionProjects[input.sessionKey];
  if (projectKey === undefined) return undefined;
  if (input.sessionKey === 'sess-mismatch') return {...input.binding, sessionKey: 'other-key', projectKey};
  return {...input.binding, sessionKey: input.sessionKey, projectKey};
};

// The seeded persisted states, one JSON file per session under the profile's
// stateRoot/sessions directory (the store's own base64url naming). The Rust
// replay seeds the same bytes through its in-memory store. Two state roots:
// the `main`/`nomap`/`nobind` bridges share one (only `main` mutates it, and
// only after every non-mutating cell of its own), the `dynamic` bridge its
// own, so the dynamic cells never see another server's resume.
const stateRoot = join(tmpdir(), `pdrs-1350-${process.pid}`);
const dynamicStateRoot = join(tmpdir(), `pdrs-1350-dyn-${process.pid}`);
const HANDOFF_WAIT = '{"id":"wait-1","kind":"handoff","skillId":"skill-1","stepId":"step-1","createdAt":"2026-01-01T00:00:00.000Z"}';
const HANDOFF_STATE = '{"version":1,"selected_skill_id":"skill-1","slots_json":{},"skill_stack_json":[],"successful_tool_names":[],"status":"handoff","active_skill_id":"skill-1","active_step_id":"step-1","awaiting_input_json":{"wait_id":"wait-1"}}';
const BUNDLE = '{"sops":[{"id":"sop-1","skill_id":"skill-1","version":"1.0.0","content":{"nodes":[{"node_id":"step-1","assignee_user_id":"staff-member-id"}]}}]}';
const BUNDLE_UNASSIGNED = '{"sops":[{"id":"sop-1","skill_id":"skill-1","version":"1.0.0","content":{"nodes":[{"node_id":"step-1","assignee_user_id":"  "}]}}]}';
const ACTIVE_STATE = '{"version":1,"selected_skill_id":"skill-1","slots_json":{"existing":"1"},"skill_stack_json":[],"successful_tool_names":[],"status":"active"}';
const EXT_WAIT = '{"id":"wait-ext","kind":"external_task","skillId":"skill-1","stepId":"step-2","createdAt":"2026-01-01T00:00:00.000Z"}';
const EXT_STATE = '{"version":1,"selected_skill_id":"skill-1","slots_json":{},"skill_stack_json":[],"successful_tool_names":[],"status":"waiting_external_task","active_skill_id":"skill-1","active_step_id":"step-2","awaiting_input_json":{"wait_id":"wait-ext"}}';
const STALE_WAIT = '{"id":"wait-old","kind":"handoff","skillId":"skill-1","stepId":"step-1","createdAt":"2026-01-01T00:00:00.000Z"}';
const BADPIN_WAIT = '{"id":"wait-1","kind":"handoff","skillId":"skill-x","stepId":"step-1","createdAt":"2026-01-01T00:00:00.000Z"}';
const seeds: Array<[string, number, string | null, string, string]> = [
  ['sess-handoff', 1, HANDOFF_WAIT, HANDOFF_STATE, BUNDLE],
  ['sess-handoff2', 1, HANDOFF_WAIT, HANDOFF_STATE, BUNDLE],
  ['sess-handoff3', 1, HANDOFF_WAIT, HANDOFF_STATE, BUNDLE],
  ['sess-done', 1, HANDOFF_WAIT, HANDOFF_STATE, BUNDLE],
  ['sess-active', 3, null, ACTIVE_STATE, BUNDLE],
  ['sess-ext', 2, EXT_WAIT, EXT_STATE, BUNDLE],
  ['sess-waitstale', 1, STALE_WAIT, HANDOFF_STATE, BUNDLE],
  ['sess-rev', 7, HANDOFF_WAIT, HANDOFF_STATE, BUNDLE],
  ['sess-badpin', 1, BADPIN_WAIT, HANDOFF_STATE, BUNDLE],
  ['sess-unassigned', 1, HANDOFF_WAIT, HANDOFF_STATE, BUNDLE_UNASSIGNED],
];
const makeControlPlane = (root: string) => new controlPlane.StaffDeckSopControlPlane(
  (projectKey?: string) => (projectKey === undefined || projectKey === 'proj1') ? {stateRoot: root} as any : undefined,
);
const writeSeeds = async (root: string) => {
  const dir = join(root, 'sessions');
  await mkdir(dir, {recursive: true});
  for (const [sessionId, revision, wait, state, bundle] of seeds) {
    const persisted = `{"schemaVersion":3,"sessionId":${JSON.stringify(sessionId)},"bundle":${bundle},"state":${state},"revision":${revision}${wait ? `,"wait":${wait}` : ''},"resumeRequests":{},"updatedAt":"2026-01-01T00:00:00.000Z"}`;
    await writeFile(join(dir, `${Buffer.from(sessionId, 'utf8').toString('base64url')}.json`), `${persisted}\n`, 'utf8');
  }
};
await writeSeeds(stateRoot);
await writeSeeds(dynamicStateRoot);
const sopControl = makeControlPlane(stateRoot);
const dynamicSopControl = makeControlPlane(dynamicStateRoot);
// The already-resumed session: resume it once through the real control plane
// (the WS lane's own call shape) so the duplicate-replay and
// status-after-resume cells replay against a receipt the real store wrote.
await sopControl.resume({projectKey: 'proj1', sessionKey: 'sess-done', requestId: 'req-dup', waitId: 'wait-1',
  source: 'human', message: 'done', expectedRevision: 1,
  authority: {tenantId: 't1', sessionId: 'sess-done', subject: structuredClone(subjects['staff-member'])}});

const bridgeOptions = {binding, authenticate, resolveSession, status: (input: any) => sopControl.status(input), resume: (input: any) => sopControl.resume(input)};
const dynamicResolveBinding = async (input: {sessionKey: string}) => {
  if (input.sessionKey === 'sess-dyn2') return otherBinding;
  if (input.sessionKey === 'sess-dynfail') throw Object.assign(new Error('SOP_APPROVAL_SESSION_FORBIDDEN'), {status: 403});
  return binding;
};
// The stub bridge: a plain scripted object, transport semantics only.
const stubBridge = {
  binding,
  async status(input: {sessionKey: string; approverAuthorization?: string; tenantId: string}) {
    switch (input.sessionKey) {
      case 'stub-snapshot': return {sessionId: 'stub-snapshot', revision: 7, state: {}};
      case 'stub-null': return null;
      case 'stub-undef': return undefined;
      case 'stub-echo': return {auth: input.approverAuthorization, tenant: input.tenantId};
      case 'stub-fail-status': throw Object.assign(new Error('x'), {status: 400, code: 'X_CUSTOM'});
      case 'stub-fail-teapot': throw Object.assign(new Error('x'), {status: 418, code: 123});
      case 'stub-fail-unknown': throw Object.assign(new Error('x'), {status: 499, code: 'X_499'});
      case 'stub-fail-plain': throw new Error('boom');
      case 'stub-fail-codeonly': throw Object.assign(new Error('x'), {code: 'SOME_CODE'});
      default: return {stub: 'default'};
    }
  },
  async resume(input: {sessionKey: string}) {
    switch (input.sessionKey) {
      case 'stub-resume-ok': return {accepted: true, marker: 'ok'};
      case 'stub-resume-undef': return undefined;
      case 'stub-resume-conflict': throw Object.assign(new Error('x'), {code: 'SOP_RESUME_REQUEST_CONFLICT'});
      case 'stub-resume-413': throw Object.assign(new Error('x'), {status: 413, code: 'X_BRIDGE'});
      default: return {stub: 'resumed'};
    }
  },
};
const gateway = { async describeServer(){ return {name:'probe',capabilities:[]}; } };
const serverMain = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe', publicApprovals: createPublicApprovalBridge(bridgeOptions) as any});
const serverDynamic = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe', publicApprovals: createPublicApprovalBridge({...bridgeOptions, status: (input: any) => dynamicSopControl.status(input), resume: (input: any) => dynamicSopControl.resume(input), resolveBinding: dynamicResolveBinding}) as any});
const serverNomap = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe', publicApprovals: createPublicApprovalBridge({...bridgeOptions, resolveSession: undefined}) as any});
const serverBare = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe'});
const serverStub = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe', publicApprovals: stubBridge as any});
const serverNobind = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe', publicApprovals: createPublicApprovalBridge({...bridgeOptions, binding: {tenantId: '', agentId: '', pilotDeckUserId: ''}}) as any});
const servers: Record<string, {url: string}> = {main: serverMain, dynamic: serverDynamic, nomap: serverNomap, bare: serverBare, stub: serverStub, nobind: serverNobind};

const maskDate = (bytes: string) => bytes.replace(/^date:.*$/gim, 'date: <masked>');
async function raw(server: string, request: string, quietMs = 250, halfClose = false, pad?: {marker: string, size: number}): Promise<{response: string, closed: boolean}> {
  const socket = connect(Number(new URL(servers[server].url).port), new URL(servers[server].url).hostname);
  await once(socket, 'connect');
  let bytes = '';
  let last = Date.now();
  let closed = false;
  socket.on('data', (b: Buffer) => { bytes += b.toString('latin1'); last = Date.now(); });
  socket.on('close', () => { closed = true; last = 0; });
  socket.on('error', () => {});
  const written = pad ? request.replace(pad.marker, 'a'.repeat(pad.size)) : request;
  await new Promise<void>(resolve => socket.write(Buffer.from(written, 'utf8'), () => resolve()));
  if (halfClose) socket.end();
  const start = Date.now();
  while (Date.now() - start < 10000) {
    await new Promise(r => setTimeout(r, 25));
    if (closed || socket.destroyed) break;
    if (bytes && Date.now() - last > quietMs) break;
  }
  if (!closed && !socket.destroyed) socket.destroy();
  return {response: maskDate(bytes), closed};
}

const STATUS = '/api/module-host/approvals/status';
const RESUME = '/api/module-host/approvals/resume';
const A = `Authorization: Bearer ${token}\r\n`;
const APPROVER = (bearer: string) => `x-staffdeck-approver-authorization: Bearer ${bearer}\r\n`;
const post = (target: string, body: string, headers = '', authorized = true) =>
  `POST ${target} HTTP/1.1\r\nHost: 127.0.0.1\r\n${headers}${authorized ? A : ''}Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`;
const statusBody = (sessionKey: string, extra: Record<string, unknown> = {}) => JSON.stringify({tenantId: 't1', agentId: 'a1', sessionKey, ...extra});
const resumeBody = (sessionKey: string, requestId: string, extra: Record<string, unknown> = {}) =>
  JSON.stringify({tenantId: 't1', agentId: 'a1', sessionKey, source: 'human', requestId, waitId: 'wait-1', expectedRevision: 1, message: 'approved', ...extra});
// A status call that reaches the store: admin approver over the handoff
// session, the body the main server's static binding demands.
const S = (sessionKey: string, bearer = 'staff-admin', extra: Record<string, unknown> = {}, headers = '') =>
  post(STATUS, statusBody(sessionKey, extra), `${APPROVER(bearer)}${headers}`);
const R = (sessionKey: string, requestId: string, bearer = 'staff-admin', extra: Record<string, unknown> = {}) =>
  post(RESUME, resumeBody(sessionKey, requestId, extra), APPROVER(bearer));

const MARKER = '__PDRS_PAD__';
const budgetBase = `{"tenantId":"t1","agentId":"a1","sessionKey":"${MARKER}"}`;
const EXACT = 64 * 1024;
const exactPad = EXACT - (budgetBase.length - MARKER.length);
const budgetRequest = (total: number) =>
  `POST ${STATUS} HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}Content-Length: ${total}\r\n\r\n${budgetBase}`;

// Each cell: server, the exact request bytes (utf8; the pad cells carry the
// marker plus a size), and flags the replayer must reproduce.
const cells: Array<{id: string, server: string, request: string, pad?: {marker: string, size: number}, halfClose?: boolean}> = [
  // authorization: byte-exact value, checked before the method.
  {id: 'auth-missing', server: 'main', request: post(STATUS, statusBody('sess-handoff'), '', false)},
  {id: 'auth-wrong', server: 'main', request: post(STATUS, statusBody('sess-handoff'), 'Authorization: Bearer wrong\r\n', false)},
  {id: 'auth-lower', server: 'main', request: post(STATUS, statusBody('sess-handoff'), 'Authorization: bearer fixture-token\r\n', false)},
  {id: 'auth-dblspace', server: 'main', request: post(STATUS, statusBody('sess-handoff'), 'Authorization: Bearer  fixture-token\r\n', false)},
  {id: 'auth-get-wrong', server: 'main', request: `GET ${STATUS} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer wrong\r\n\r\n`},
  // method: anything but POST, with the body unread like TS.
  {id: 'method-get', server: 'main', request: `GET ${STATUS} HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}\r\n`},
  {id: 'method-delete', server: 'main', request: `DELETE ${RESUME} HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}\r\n`},
  {id: 'method-head', server: 'main', request: `HEAD ${RESUME} HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}\r\n`},
  // routing: the WHATWG path parse decides the two endpoints; a miss falls
  // through to the TS server's own 404.
  {id: 'route-trailing', server: 'main', request: post(`${STATUS}/`, statusBody('sess-active'))},
  {id: 'route-statuses', server: 'main', request: post('/api/module-host/approvals/statuses', statusBody('sess-active'))},
  {id: 'route-pct', server: 'main', request: post('/api/module-host/approvals/%73tatus', statusBody('sess-active'))},
  {id: 'route-dot', server: 'main', request: post('/api/module-host/approvals/x/../status', statusBody('sess-active'))},
  {id: 'route-upper', server: 'main', request: post('/API/MODULE-HOST/APPROVALS/STATUS', statusBody('sess-active'))},
  {id: 'route-query', server: 'main', request: post(`${STATUS}?x=1`, statusBody('sess-active'))},
  // the JSON.parse gate and the object-shape gate.
  {id: 'body-bad-json', server: 'main', request: post(STATUS, '{oops')},
  {id: 'body-empty', server: 'main', request: post(STATUS, '')},
  {id: 'body-number', server: 'main', request: post(STATUS, '5')},
  {id: 'body-array', server: 'main', request: post(STATUS, '[]')},
  {id: 'body-null', server: 'main', request: post(STATUS, 'null')},
  {id: 'body-string', server: 'main', request: post(STATUS, '"x"')},
  // the field whitelist (status carries only its four fields).
  {id: 'whitelist-extra', server: 'main', request: post(STATUS, JSON.stringify({tenantId: 't1', agentId: 'a1', sessionKey: 'sess-handoff', requestId: 'x'}))},
  {id: 'whitelist-approver', server: 'main', request: post(STATUS, JSON.stringify({tenantId: 't1', agentId: 'a1', sessionKey: 'sess-handoff', approverAuthorization: 'Bearer x'}))},
  // the transport-level scope gate: a static binding demands present and
  // equal tenantId/agentId.
  {id: 'scope-tenant', server: 'main', request: post(STATUS, statusBody('sess-handoff', {tenantId: 't9'}))},
  {id: 'scope-agent', server: 'main', request: post(STATUS, statusBody('sess-handoff', {agentId: 'a9'}))},
  {id: 'scope-missing', server: 'main', request: post(STATUS, JSON.stringify({sessionKey: 'sess-handoff'}))},
  // the approver authorization gate.
  {id: 'approver-missing', server: 'main', request: post(STATUS, statusBody('sess-handoff'))},
  {id: 'approver-malformed', server: 'main', request: post(STATUS, statusBody('sess-handoff'), 'x-staffdeck-approver-authorization: Token abc\r\n')},
  {id: 'approver-empty-rest', server: 'main', request: post(STATUS, statusBody('sess-handoff'), 'x-staffdeck-approver-authorization: Bearer  \r\n')},
  {id: 'approver-mixed-case', server: 'main', request: post(STATUS, statusBody('sess-handoff'), 'x-staffdeck-approver-authorization: BEARER staff-member\r\n')},
  {id: 'approver-wrong-bearer', server: 'main', request: S('sess-handoff', 'nope')},
  // the authenticated subject checks.
  {id: 'subject-tenant', server: 'main', request: S('sess-handoff', 'staff-tenant2')},
  {id: 'subject-disabled', server: 'main', request: S('sess-handoff', 'staff-disabled')},
  {id: 'subject-source', server: 'main', request: S('sess-handoff', 'staff-source')},
  {id: 'subject-role', server: 'main', request: S('sess-handoff', 'staff-guest')},
  {id: 'subject-emptyuser', server: 'main', request: S('sess-handoff', 'staff-emptyuser')},
  // the session resolution.
  {id: 'session-unknown', server: 'main', request: S('no-such')},
  {id: 'session-project-mismatch', server: 'main', request: S('sess-handoff', 'staff-admin', {projectKey: 'proj-wrong'})},
  {id: 'session-key-mismatch', server: 'main', request: S('sess-mismatch')},
  {id: 'nomap-status', server: 'nomap', request: S('sess-handoff')},
  {id: 'bare-status', server: 'bare', request: S('sess-handoff')},
  {id: 'bare-resume', server: 'bare', request: R('sess-handoff', 'req-x')},
  {id: 'nobind-status', server: 'nobind', request: post(STATUS, JSON.stringify({tenantId: '', agentId: '', sessionKey: 'sess-handoff'}), APPROVER('staff-admin'))},
  // the store's status snapshots.
  {id: 'status-handoff-admin', server: 'main', request: S('sess-handoff', 'staff-admin')},
  {id: 'status-handoff-member', server: 'main', request: S('sess-handoff', 'staff-member')},
  {id: 'status-handoff-other', server: 'main', request: S('sess-handoff', 'staff-other')},
  {id: 'status-active', server: 'main', request: S('sess-active', 'staff-admin')},
  {id: 'status-missing', server: 'main', request: S('sess-gone', 'staff-admin')},
  {id: 'status-badpin', server: 'main', request: S('sess-badpin', 'staff-admin')},
  {id: 'status-unassigned', server: 'main', request: S('sess-unassigned', 'staff-admin')},
  {id: 'status-module-disabled', server: 'main', request: S('sess-proj2', 'staff-admin')},
  {id: 'status-after-resume', server: 'main', request: S('sess-done', 'staff-member')},
  // the store's resume paths.
  {id: 'resume-success-member', server: 'main', request: R('sess-handoff', 'req-r1', 'staff-member', {slotUpdates: {k: 'v'}})},
  {id: 'resume-success-admin', server: 'main', request: R('sess-handoff2', 'req-r2', 'staff-admin', {message: 'ok-admin'})},
  {id: 'resume-duplicate', server: 'main', request: R('sess-done', 'req-dup', 'staff-member', {message: 'done'})},
  {id: 'resume-duplicate-message', server: 'main', request: R('sess-done', 'req-dup', 'staff-member', {message: 'different'})},
  {id: 'resume-duplicate-revision', server: 'main', request: R('sess-done', 'req-dup', 'staff-member', {message: 'done', expectedRevision: 5})},
  {id: 'resume-ext-source', server: 'main', request: R('sess-ext', 'req-e1', 'staff-admin', {waitId: 'wait-ext'})},
  {id: 'resume-wait-stale', server: 'main', request: R('sess-waitstale', 'req-s1', 'staff-member')},
  {id: 'resume-revision-conflict', server: 'main', request: R('sess-rev', 'req-c1', 'staff-member', {expectedRevision: 6})},
  {id: 'resume-not-found', server: 'main', request: R('sess-gone', 'req-n1', 'staff-admin')},
  {id: 'resume-invalid-source', server: 'main', request: R('sess-handoff', 'req-i1', 'staff-admin', {source: 'external_task'})},
  {id: 'resume-invalid-message', server: 'main', request: R('sess-handoff', 'req-i2', 'staff-admin', {message: '  '})},
  {id: 'resume-invalid-revision', server: 'main', request: R('sess-handoff', 'req-i3', 'staff-admin', {expectedRevision: 1.5})},
  {id: 'resume-invalid-negative', server: 'main', request: R('sess-handoff', 'req-i4', 'staff-admin', {expectedRevision: -1})},
  {id: 'resume-invalid-wait', server: 'main', request: R('sess-handoff', 'req-i5', 'staff-admin', {waitId: ''})},
  {id: 'resume-member-forbidden', server: 'main', request: R('sess-handoff3', 'req-f1', 'staff-other')},
  // the dynamic binding mode.
  {id: 'dynamic-scope-skipped', server: 'dynamic', request: post(STATUS, JSON.stringify({sessionKey: 'sess-handoff'}), APPROVER('staff-admin'))},
  {id: 'dynamic-other-binding', server: 'dynamic', request: post(STATUS, JSON.stringify({tenantId: 't1', agentId: 'a1', sessionKey: 'sess-dyn2'}), APPROVER('staff-admin'))},
  {id: 'dynamic-resolver-fail', server: 'dynamic', request: post(STATUS, JSON.stringify({tenantId: 't1', agentId: 'a1', sessionKey: 'sess-dynfail'}), APPROVER('staff-admin'))},
  // the stub bridge: transport-level result and failure serialization.
  {id: 'stub-snapshot', server: 'stub', request: post(STATUS, statusBody('stub-snapshot'))},
  {id: 'stub-null', server: 'stub', request: post(STATUS, statusBody('stub-null'))},
  {id: 'stub-undef', server: 'stub', request: post(STATUS, statusBody('stub-undef'))},
  {id: 'stub-scope', server: 'stub', request: post(STATUS, statusBody('stub-snapshot', {tenantId: 't9'}))},
  {id: 'stub-echo-approver', server: 'stub', request: post(STATUS, statusBody('stub-echo'), 'x-staffdeck-approver-authorization: Bearer a\r\nx-staffdeck-approver-authorization: Bearer b\r\n')},
  {id: 'stub-fail-status', server: 'stub', request: post(STATUS, statusBody('stub-fail-status'))},
  {id: 'stub-fail-teapot', server: 'stub', request: post(STATUS, statusBody('stub-fail-teapot'))},
  {id: 'stub-fail-unknown', server: 'stub', request: post(STATUS, statusBody('stub-fail-unknown'))},
  {id: 'stub-fail-plain', server: 'stub', request: post(STATUS, statusBody('stub-fail-plain'))},
  {id: 'stub-fail-codeonly', server: 'stub', request: post(STATUS, statusBody('stub-fail-codeonly'))},
  {id: 'stub-resume-ok', server: 'stub', request: post(RESUME, resumeBody('stub-resume-ok', 'req-s'))},
  {id: 'stub-resume-undef', server: 'stub', request: post(RESUME, resumeBody('stub-resume-undef', 'req-s'))},
  {id: 'stub-resume-conflict', server: 'stub', request: post(RESUME, resumeBody('stub-resume-conflict', 'req-s'))},
  {id: 'stub-resume-413', server: 'stub', request: post(RESUME, resumeBody('stub-resume-413', 'req-s'))},
  // the 64 KiB chunk-summed budget: exactly the budget is served through to
  // the bridge; one byte more throws out of the `for await`, which destroys
  // the request stream before the catch's send — the connection is torn down
  // with zero response bytes (measured deterministic, see the Rust replay's
  // OverBudget branch), so the transport's own 413 frame never reaches the
  // wire. The request carries the pad marker, not the padding.
  {id: 'budget-exact', server: 'stub', request: budgetRequest(EXACT), pad: {marker: MARKER, size: exactPad}},
  {id: 'budget-over', server: 'stub', request: budgetRequest(EXACT + 1), pad: {marker: MARKER, size: exactPad + 1}},
  // connection framing.
  {id: 'conn-close', server: 'main', request: post(STATUS, statusBody('sess-active'), 'Connection: close\r\n')},
  // Node answers an HTTP/1.0 request with an HTTP/1.1 status line and its own
  // close decision; hyper's version flip is the registered #1209 diff.
  {id: 'http10', server: 'main', request: `POST ${STATUS} HTTP/1.0\r\nHost: 127.0.0.1\r\n${A}${APPROVER('staff-admin')}Content-Length: ${Buffer.byteLength(statusBody('sess-active'))}\r\n\r\n${statusBody('sess-active')}`},
  {id: 'keep-alive-reuse', server: 'main', request: S('sess-active') + S('sess-active')},
  {id: 'premature-close', server: 'main', request: `POST ${RESUME} HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}Content-Length: 100\r\n\r\n`, halfClose: true},
];

const observations: any[] = [];
try {
  for (const cell of cells) {
    const {response, closed} = await raw(cell.server, cell.request, cell.id === 'budget-over' ? 400 : 250, cell.halfClose === true, cell.pad);
    if (!response && !closed) { console.error(`#1350 cell ${cell.id} produced no bytes and stayed open`); process.exit(1); }
    observations.push({id: cell.id, server: cell.server, request: cell.request, pad: cell.pad ?? null, halfClose: cell.halfClose === true, response, closed});
  }
  const output = {sourceCommit, sourceSha256, node: process.version, icu: process.versions.icu, cells: observations};
  await mkdir(process.argv[2], {recursive: true});
  await writeFile(join(process.argv[2], 'oracle.json'), JSON.stringify(output, null, 2) + '\n', {flag: 'wx'});
  console.log(JSON.stringify(output, null, 2));
} finally {
  for (const server of Object.values(servers)) await server.close().catch(() => {});
  clearTimeout(deadline);
}
