import test from 'node:test';
import assert from 'node:assert/strict';
import { Readable } from 'node:stream';
import { readFile, readdir, stat, mkdtemp, rm, mkdir, symlink, rename } from 'node:fs/promises';
import { resolve } from 'node:path';
import { deflateSync } from 'node:zlib';
import { randomUUID } from 'node:crypto';
import net from 'node:net';
import path from 'node:path';
import { EventEmitter } from 'node:events';
import { pathToFileURL } from 'node:url';
import { baseline, requireUI, REAL_TEST_KEY, realPorts, realOrigin, root } from './env.mjs';
import { createMockLLM, planResponse, probeColor } from './mock-llm.mjs';
import { localFetchURL, installNetworkIsolation } from './real-network.mjs';
import { runtimeConfig, runtimeArgs, prepareRuntime } from './real-launch.mjs';
import { realExecutionEnv } from './execution-env.mjs';
import { assertRealPortsFree, validateRealRegistration, assertCanonicalAncestor, bridgeReady, realExecutionError } from './real-runner.mjs';
import { processTree, tcpRecords, forbiddenTCP, startTCPAudit, lsofExitOutput } from './real-tcp-audit.mjs';
import { PENDING_ORCHESTRATION, reconcileReal } from './accounting.mjs';
const headers = { authorization: `Bearer ${REAL_TEST_KEY}` };
const request = body => ({ method: 'POST', path: '/v1/chat/completions', headers, body });
const marker = 'E2E-CHAT-google-123456789';
const body = { model: 'gpt-4.1', max_tokens: 1024, messages: [{ role: 'system', content: 'system' },
  { role: 'user', content: 'Reply with exactly this marker: E2E-CHAT-openai-111' },
  { role: 'assistant', content: 'E2E-CHAT-openai-111' },
  { role: 'user', content: [{ type: 'text', text: `Reply with exactly this marker: ${marker}` }] }],
  tools: [{ type: 'function', function: { name: 'unused', parameters: {} } }], stream: true };

// Execute the frozen parser/probe functions to validate compatibility independently
// of the mock's own serialization. Omit imports and top-level service side effects.
async function frozenFunctions(path, dependencies = {}) {
  const ts = requireUI('typescript'), source = await readFile(resolve(baseline, path), 'utf8');
  const ast = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true);
  const functions = ast.statements.filter(node => ts.isFunctionDeclaration(node));
  const code = ts.transpileModule(functions.map(node => node.getText(ast)).join('\n'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const exports = {};
  new Function('exports', ...Object.keys(dependencies), `${code}\n${functions.map(node => `exports.${node.name.text} = ${node.name.text};`).join('\n')}`)(exports, ...Object.values(dependencies));
  return exports;
}

test('OpenAI deterministic marker: latest user, no tool calls, native parser accepts nonstream and every stream chunk', async () => {
  const usage = await frozenFunctions('../src/model/response/normalizeUsage.ts');
  const finish = await frozenFunctions('../src/model/response/normalizeFinishReason.ts');
  const dependencies = { ...usage, ...finish, randomUUID, jsonrepair: requireUI('jsonrepair').jsonrepair, ModelProviderError: Error };
  const native = await frozenFunctions('../src/model/providers/openai/stream.ts', dependencies);
  const response = await frozenFunctions('../src/model/providers/openai/response.ts', dependencies);
  const plan = planResponse(request(body));
  assert.deepEqual(plan, planResponse(request(structuredClone(body))));
  assert.equal(plan.script, 'chat-marker'); assert.equal(plan.answer, marker);
  assert.equal(plan.frames.at(-1), 'data: [DONE]\n\n');
  assert(plan.frames.length > 4);
  const state = native.createOpenAIStreamState(), events = [];
  for (const frame of plan.frames.slice(0, -1)) events.push(...native.normalizeOpenAIStreamEvent(JSON.parse(frame.slice(6)), state));
  assert.equal(events.filter(event => event.type === 'message_start').length, 1);
  assert.equal(events.filter(event => event.type === 'text_delta').map(event => event.text).join(''), marker);
  assert.equal(events.filter(event => event.type === 'message_end').length, 1);
  assert.equal(events.find(event => event.type === 'message_end').finishReason, 'stop');
  assert.equal(events.find(event => event.type === 'usage').usage.totalTokens, 16);
  assert(!events.some(event => event.type.startsWith('tool_call')));
  const nonstream = planResponse(request({ ...body, stream: false }));
  const parsed = response.parseOpenAIResponse(nonstream.body);
  assert.equal(parsed.finishReason, 'stop');
  assert.deepEqual(parsed.content, [{ type: 'text', text: marker }]);
  assert.equal(parsed.usage.totalTokens, 16);
  const title = planResponse(request({ ...body, stream: false, metadata: { purpose: 'session_title_generation' } }));
  assert.equal(title.script, 'session-title');
  const titleParser = await frozenFunctions('../src/session/title/SessionTitleGenerator.ts', { SESSION_TITLE_MAX_OUTPUT_CHARS: 80 });
  assert.equal(titleParser.parseGeneratedTitle(response.parseOpenAIResponse(title.body).content), 'Verify E2E chat marker');
});

test('baseline connection text and random solid PNG color probes return exact answers, malformed images reject', async () => {
  const probe = await frozenFunctions('server/services/modelConnectionProbe.js', { deflateSync });
  const colors = { red: [220, 48, 48], blue: [40, 96, 220], green: [36, 168, 72], yellow: [232, 196, 40] };
  for (const [color, rgb] of Object.entries(colors)) {
    const image = 'data:image/png;base64,' + probe.encodeSolidPng(rgb).toString('base64');
    assert.equal(probeColor(image), color);
    const plan = planResponse(request({ model: 'gpt-4.1', messages: [{ role: 'user', content: [
      { type: 'text', text: 'What color is the shape in this image? Reply with one English color word.' },
      { type: 'image_url', image_url: { url: image } },
    ] }] }));
    assert.equal(plan.script, 'image-color'); assert.equal(plan.body.choices[0].message.content, color);
  }
  assert.throws(() => probeColor('data:image/png;base64,YmFk'));
  const plan = planResponse(request({ model: 'gpt-4.1', messages: [{ role: 'user', content: 'Reply exactly: 1' }] }));
  assert.equal(plan.body.choices[0].message.content, '1');
});

test('HTTP handler writes distinct SSE chunks, ends stream, logs every request without credentials; rejects unknown/malformed/auth', async () => {
  const recorded = [], writes = [], pauses = [], mock = createMockLLM({ record: async row => recorded.push(row), pause: async () => pauses.push(true) });
  const bytes = Buffer.from(JSON.stringify({ ...body, messages: [{ role: 'system', content: '中文' }, ...body.messages] }));
  const req = Readable.from([...bytes].map(byte => Buffer.from([byte])));
  Object.assign(req, { url: '/v1/chat/completions', method: 'POST', headers });
  const res = { destroyed: false, writeHead(status, values) { this.status = status; this.headers = values; }, write(value) { writes.push(value); }, end(value) { this.ended = true; this.body = value; } };
  await mock.handleHTTP(req, res);
  assert.equal(res.status, 200); assert.match(res.headers['Content-Type'], /text\/event-stream/);
  assert.equal(res.ended, true); assert.deepEqual(writes, planResponse(request({ ...body, messages: [{ role: 'system', content: '中文' }, ...body.messages] })).frames);
  assert.equal(recorded[0].body.messages[0].content, '中文');
  assert.equal(pauses.length, writes.length); assert.equal(recorded.length, 1);
  assert.equal(recorded[0].authorization, 'fixed-test-key');
  assert(!JSON.stringify(recorded).includes(REAL_TEST_KEY));
  for (const [raw, expected] of [['{', 400], ['null', 400]]) {
    const invalid = Readable.from([raw]); Object.assign(invalid, { url: req.url, method: 'POST', headers });
    await mock.handleHTTP(invalid, res); assert.equal(res.status, expected);
  }
  assert.equal(recorded.length, 3); assert.equal(mock.stats.rejected, 2);
  assert.equal(planResponse({ ...request(body), headers: {} }).status, 401);
  assert.equal(planResponse({ ...request(body), path: '/v1/messages' }).status, 404);
  assert.equal(planResponse({ ...request(body), method: 'PUT' }).status, 405);
  assert.equal(planResponse(request({ ...body, messages: [{ role: 'user', content: 'unsupported tool task' }] })).status, 422);
  assert.equal(planResponse(request({ ...body, stream: 'true' })).status, 400);
  const list = planResponse({ method: 'GET', path: '/v1/models', headers });
  assert.equal(list.body.data[0].id, 'gpt-4.1');
});

test('runtime env/config isolation, exact ports, no occupied-port kill, preloaded network guard (all sockets/fetch stubbed)', async () => {
  for (const mode of ['ready', 'onboarding']) {
    const env = realExecutionEnv({ PATH: process.env.PATH, HOME: '/kept', PILOT_HOME: '/bad', WORKSPACES_ROOT: '/bad', HOST: '0.0.0.0', PILOTDECK_GATEWAY_TOKEN: 'bad', PILOTDECK_GATEWAY_TOKEN_PATH: '/bad', PILOTDECK_E2E_OPENAI_MODEL: 'wrong', OPENAI_API_KEY: 'secret', NODE_OPTIONS: '--bad', HTTP_PROXY: 'bad', npm_config_offline: 'false', TASKMASTER_SKIP_AUTO_UPDATE: '0' },
      { output: '/owned/output', runDir: '/owned/run', mode });
    assert.equal(env.HOME, '/kept'); assert.equal(env.HOST, '127.0.0.1');
    assert.equal(env.PILOTDECK_E2E_API_KEY, REAL_TEST_KEY); assert.equal(env.PILOT_HOME, '/owned/run/home');
    assert.equal(env.PILOTDECK_E2E_OPENAI_MODEL, undefined); assert.equal(env.OPENAI_API_KEY, undefined); assert.equal(env.NODE_OPTIONS, undefined);
    assert.equal(env.PILOTDECK_GATEWAY_TOKEN, undefined);
    assert.equal(env.PILOTDECK_GATEWAY_TOKEN_PATH, '/owned/run/home/server-token');
    assert.equal(env.WORKSPACES_ROOT, '/owned/run/workspace');
    assert.equal(env.PATH, '/owned/run/bin:/usr/bin:/bin:/usr/sbin:/sbin');
    assert.equal(env.TASKMASTER_SKIP_AUTO_UPDATE, '1'); assert.equal(env.npm_config_offline, 'true');
    const config = runtimeConfig(mode, '/owned/run');
    assert.equal(config.webui.runtime.serverPort, realPorts[mode].server);
    assert.equal(config.webui.runtime.databasePath, env.DATABASE_PATH);
    assert.equal(config.webui.runtime.workspacesRoot, env.WORKSPACES_ROOT);
    assert.equal(Object.keys(config.model.providers).length, mode === 'ready' ? 3 : 0);
    if (mode === 'ready') assert.equal(config.agent.model, 'e2e-openai/gpt-4.1');
  }
  const checked = [];
  await assert.rejects(assertRealPortsFree(async port => { checked.push(port); return port === 5180; }), /occupied/);
  assert(!checked.includes(3001)); assert(!checked.includes(18789));
  assert.deepEqual(Object.values(realPorts.ready), [3010, 5180, 18798]);
  assert.deepEqual(Object.values(realPorts.onboarding), [3011, 5181, 18799]);
  await mkdir(resolve(root, '_tmp/fe-parity'), { recursive: true });
  const directory = await mkdtemp(resolve(root, '_tmp/fe-parity/unit-'));
  await symlink('/private/tmp', resolve(directory, 'escape'));
  await assert.rejects(assertCanonicalAncestor(resolve(directory, 'escape', 'must-not-create')), /symlink/);
  await assertCanonicalAncestor(resolve(directory, 'new-descendant'));
  const savedConnect = net.Socket.prototype.connect, savedListen = net.Server.prototype.listen, savedFetch = globalThis.fetch, calls = [];
  try {
    net.Server.prototype.listen = function (...args) { calls.push(args); return this; };
    net.Socket.prototype.connect = function (...args) { calls.push(args); return this; };
    globalThis.fetch = async (input, init) => { calls.push([input, init]); return new Response('{}'); };
    installNetworkIsolation({ PILOTDECK_E2E_LLM_CENTER_URL: `http://127.0.0.1:${realPorts.mock}`, PILOTDECK_E2E_API_KEY: REAL_TEST_KEY, FE_NETWORK_LOG: resolve(directory, 'network.jsonl') });
    assert.throws(() => new net.Socket().connect({ host: '0.0.0.0', port: 3010 }), /blocked/);
    assert.throws(() => new net.Socket().connect({ host: 'api.openai.com', port: 443 }), /blocked/);
    assert.throws(() => new net.Socket().connect({ host: '127.0.0.1', port: 18789 }), /blocked/);
    assert.throws(() => new net.Server().listen(3001, '127.0.0.1'), /blocked/);
    assert.throws(() => new net.Server().listen(3010, '0.0.0.0'), /blocked/);
    new net.Server().listen(3010, '127.0.0.1');
    new net.Socket().connect({ host: '127.0.0.1', port: 3010 });
    await fetch('https://api.openai.com/v1/chat/completions', { method: 'POST', body: 'same' });
    assert.equal(calls.at(-1)[0], `http://127.0.0.1:${realPorts.mock}/v1/chat/completions`);
    assert.equal(calls.at(-1)[1].body, 'same');
    await fetch(new Request('https://api.openai.com/v1/chat/completions', { method: 'POST', body: 'request-body' }));
    assert.equal(calls.at(-1)[0].url, `http://127.0.0.1:${realPorts.mock}/v1/chat/completions`);
    assert.equal(await calls.at(-1)[0].text(), 'request-body');
    assert.equal(localFetchURL('https://api.openai.com/other', 'http://127.0.0.1:28379'), 'https://api.openai.com/other');
    assert.equal(localFetchURL('https://evil.test/v1/models', 'http://127.0.0.1:28379'), 'https://evil.test/v1/models');
    assert.match(await readFile(resolve(directory, 'network.jsonl'), 'utf8'), /redirect-openai-preset/);
  } finally { net.Server.prototype.listen = savedListen; net.Socket.prototype.connect = savedConnect; globalThis.fetch = savedFetch; await rm(directory, { recursive: true }); }
});

test('real partition accounting credits 28 unique executions and rejects skip, failures, omission, swapped provenance, extra identities', () => {
  const inventory = PENDING_ORCHESTRATION, origins = { ready: realOrigin('ready'), onboarding: realOrigin('onboarding') };
  const make = () => Object.fromEntries(['ready', 'onboarding'].map(mode => [mode, {
    config: { forbidOnly: true, metadata: { frontend: 'old', mode, baseURL: origins[mode], suite: 'real-D', backend: 'native-with-mock-llm' } },
    suites: [{ specs: inventory.filter(row => (row.file === 'real/onboarding.spec.mjs') === (mode === 'onboarding')).map(row => ({ ...row,
      tests: [{ expectedStatus: 'passed', annotations: [], results: [{ status: 'passed' }] }],
    })) }],
  }]));
  const account = reports => reconcileReal(inventory, reports, '', origins);
  const pass = account(make());
  assert.equal(pass.status, 'PASS'); assert.equal(pass.executed, 28); assert.equal(pass.minimumExecuted, 28); assert.equal(pass.pendingOrchestration, 0);
  assert.equal(pass.oldGate.status, 'FAIL'); // Reviewed stale PASS never satisfies the old calibration gate.
  for (const status of ['skipped', 'failed', 'interrupted']) {
    const reports = make(); reports.ready.suites[0].specs[0].tests[0].results[0].status = status;
    const result = account(reports); assert.equal(result.status, 'FAIL'); assert.equal(result.minimumExecuted, 28); assert.equal(result.explicitSkips, 0);
    if (status === 'skipped') assert.equal(result.pendingOrchestration, 1);
  }
  const missing = make(); missing.ready.suites[0].specs.pop(); assert.equal(account(missing).status, 'FAIL');
  const extra = make(); extra.ready.suites[0].specs.push(extra.onboarding.suites[0].specs[0]); assert.equal(account(extra).status, 'FAIL');
  const swapped = make(); [swapped.ready, swapped.onboarding] = [swapped.onboarding, swapped.ready]; assert.equal(account(swapped).status, 'FAIL');
  const metadata = make(); metadata.ready.config.metadata.backend = 'f2-mock'; assert.equal(account(metadata).status, 'FAIL');
  const noFlag = make(); noFlag.ready.config.forbidOnly = false; assert.equal(account(noFlag).status, 'FAIL');
  const partial = reconcileReal(inventory, make(), '', origins, ['onboarding']);
  assert.equal(partial.status, 'PASS'); assert.equal(partial.executed, 4); assert.equal(partial.denominator, 28);
  assert.equal(partial.rows.filter(row => row.status === 'NOT_RUN').length, 24); assert.equal(partial.coverage, 'D_PARTIAL_MODE_ONLY');
  const registration = inventory.filter(row => row.file !== 'real/onboarding.spec.mjs')
    .map(row => ({ ...row, line: Number(row.source.registration.split(':').at(-1)), column: 1 }));
  validateRealRegistration(registration, inventory, 'ready');
  registration[0].line++;
  assert.throws(() => validateRealRegistration(registration, inventory, 'ready'), /location drift/);
  assert.throws(() => validateRealRegistration(inventory.slice(1), inventory, 'ready'), /drift/);
});


test('frozen supervisor launches native server/client then gateway only on ready; client override and cleanup use owned children (spawn/port checks stubbed)', async () => {
  const native = await frozenFunctions('server/webRuntimeSupervisor.js', {
    path, UI_ROOT: baseline, getRestartRequestFile: () => '/unused', existsSync: () => false, unlinkSync: () => {},
    RESTART_EXIT_CODE: 75, spawn: () => { throw new Error('No real spawn allowed'); },
  });
  for (const state of ['needs_setup', 'ready']) {
    const spawned = [], exitCodes = [], hooks = new EventEmitter();
    const supervisor = native.createRuntimeSupervisor({ mode: 'dev', cwd: baseline, env: { PILOTDECK_GATEWAY_PORT: '18798' },
      requestFile: '/unused', processLike: hooks, waitForPortImpl: async (port, host) => {
        assert.equal(port, 18798); assert.equal(host, '127.0.0.1');
      }, exists: () => false, log: () => {}, error: () => {}, exit: code => exitCodes.push(code),
      spawnImpl(command, args, options) {
        const child = new EventEmitter();
        Object.assign(child, { exitCode: null, connected: true, killed: false,
          send: () => {}, kill: signal => { child.killed = true; child.exitCode = 0; child.emit('close', 0, signal); },
        });
        assert.equal(options.detached, undefined); // inherited launcher process group
        assert.equal(command, process.execPath);
        spawned.push({ args: runtimeArgs(args), options, child }); return child;
      },
    });
    supervisor.run();
    assert.equal(spawned.length, 2);
    assert.deepEqual(spawned[0].args, ['--import', 'tsx', resolve(baseline, 'server/index.js')]);
    assert.deepEqual(spawned[1].args, [resolve(root, 'scripts/fe-parity/real-vite.mjs')]);
    supervisor.child.emit('message', { type: 'pilotdeck:configuration-state', configuration: { state } });
    await Promise.resolve();
    assert.equal(spawned.length, state === 'ready' ? 3 : 2);
    if (state === 'ready') assert.deepEqual(spawned[2].args, ['--import', 'tsx', resolve(baseline, '../src/cli/pilotdeck.ts'), 'server']);
    await supervisor.stop('SIGTERM');
    assert(spawned.every(entry => entry.child.killed)); assert.equal(supervisor.children.size, 0);
    assert.deepEqual(exitCodes, [1]);
  }
});

test('round two: frozen gateway explicit-token bypass versus generated file; bridge readiness requires native handshake', async () => {
  let generated = 0;
  const native = await frozenFunctions('../src/gateway/server/GatewayServer.ts', {
    ensureGatewayAuthToken: async () => { generated++; return { token: 'generated', tokenPath: '/owned/home/server-token' }; },
    createServer: () => Object.assign(new EventEmitter(), {
      address: () => ({ port: 18798 }), listen: (_port, _host, callback) => callback(), close: callback => callback(),
    }),
  });
  const broken = await native.startGatewayServer({ token: 'explicit', port: 18798 });
  assert.equal(generated, 0); assert.equal(broken.tokenPath, undefined); await broken.close();
  const fixed = await native.startGatewayServer({ port: 18798 });
  assert.equal(generated, 1); assert.equal(fixed.tokenPath, '/owned/home/server-token'); await fixed.close();
  const env = realExecutionEnv({}, { output: '/owned/output', runDir: '/owned/run', mode: 'ready' });
  assert(!bridgeReady('[runtime-supervisor] Gateway ready on 127.0.0.1:18798', 'generated', env));
  assert(!bridgeReady(`[pilotdeck-bridge] connected → ${env.PILOTDECK_GATEWAY_URL}`, '', env));
  assert(bridgeReady(`[pilotdeck-bridge] connected → ${env.PILOTDECK_GATEWAY_URL}`, 'generated\n', env));
  await mkdir(resolve(root, '_tmp/fe-parity/real-run'), { recursive: true });
  const parent = await mkdtemp(resolve(root, '_tmp/fe-parity/real-run/unit-r2-'));
  try {
    const runDir = resolve(parent, 'ready');
    await prepareRuntime('ready', runDir);
    const nativePaths = await frozenFunctions('server/utils/generalWorkspace.js', {
      os: { homedir: () => '/must-not-use' }, path, GENERAL_WORKSPACE_DIRECTORY_NAME: 'general',
      resolvePilotHome: () => resolve(runDir, 'home'),
      isPathInsideOrEqual: (root, value) => value === root || value.startsWith(root + path.sep),
    });
    assert.equal(nativePaths.resolveGeneralWorkspaceDirectory({ WORKSPACES_ROOT: resolve(runDir, 'workspace') }), resolve(runDir, 'workspace/general'));
    assert.throws(() => nativePaths.resolveGeneralWorkspaceDirectory({ WORKSPACES_ROOT: resolve(runDir, 'home') }), /separate/);
  } finally { await rm(parent, { recursive: true }); }
});

test('round two: actual onboarding empty-key catalog headers accepted only for GET models; alternate/wrong credentials reject', async () => {
  const plans = [undefined, `Bearer ${REAL_TEST_KEY}`, 'Bearer wrong', 'Basic wrong', ''];
  assert.deepEqual(plans.map(authorization => planResponse({ method: 'GET', path: '/v1/models', headers: authorization === undefined ? {} : { authorization } }).status), [200, 200, 401, 401, 401]);
  assert.equal(planResponse({ ...request(body), headers: {} }).status, 401);
  for (const name of ['x-api-key', 'x-goog-api-key']) {
    assert.equal(planResponse({ method: 'GET', path: '/v1/models', headers: { [name]: REAL_TEST_KEY } }).status, 401);
  }
  const rows = [], mock = createMockLLM({ record: async row => rows.push(row) });
  const req = Object.assign(Readable.from([]), { method: 'GET', url: '/v1/models', headers: {} });
  await mock.handleHTTP(req, { writeHead(status) { assert.equal(status, 200); }, end() {} });
  assert.equal(rows[0].authHeaders.authorization, 'absent'); assert.equal(mock.stats.rejected, 0);
});

test('round two: ps/lsof audit owns descendants and reparented groups; IPv4/IPv6 forbidden targets abort, observation errors fail closed (no ps/lsof executed)', async () => {
  const ps = '10 1 5 node\n20 10 20 runtime\n21 20 20 server\n22 1 20 orphan\n30 10 30 playwright\n31 30 30 chrome\n99 1 99 unrelated';
  assert.deepEqual(processTree(ps, 10, [20]).map(row => row.pid), [10, 20, 21, 22, 30, 31]);
  const raw = 'p21\nf10\ntIPv4\nn127.0.0.1:40000->127.0.0.1:18798\nTST=ESTABLISHED\np31\nf11\ntIPv6\nn[::1]:40001->[::1]:18789\n';
  assert.equal(forbiddenTCP(tcpRecords(raw)).length, 1);
  assert.equal(forbiddenTCP(tcpRecords('p21\nf1\ntIPv4\nn127.0.0.1:18789->127.0.0.1:3010\n')).length, 0);
  assert.equal(forbiddenTCP(tcpRecords('p21\nf1\ntIPv4\nn127.0.0.1:3001\n')).length, 1);
  assert.throws(() => tcpRecords('p21\nf1\ntIPv4\nnbad->endpoint\n'), /Unrecognized/);
  const rows = [], killed = []; let lsof = '', deny = false;
  const run = async (file, args) => {
    if (deny) throw new Error('lsof unavailable');
    if (file === 'ps') return ps;
    assert(args.includes('-iTCP')); assert(args.includes('-a')); assert(args.includes('10,20,21,22,30,31'));
    return lsof;
  };
  const audit = await startTCPAudit({ rootPid: 10, groupRoots: () => [20, 30], run, record: async row => rows.push(row), onFatal: error => killed.push(error.message), intervalMs: 60_000 });
  await audit.checkpoint('before-test'); lsof = raw;
  await assert.rejects(audit.checkpoint('after-test'), /3001\/18789/);
  assert.equal(killed.length, 1); assert(rows.some(row => row.forbidden?.length === 1));
  await assert.rejects(audit.stop(), /3001\/18789/);
  deny = true;
  await assert.rejects(startTCPAudit({ rootPid: 10, run, record: async row => rows.push(row), onFatal: error => killed.push(error.message) }), /unavailable/);
  assert.equal(killed.length, 2);
  deny = false; lsof = '';
  const safe = await startTCPAudit({ rootPid: 10, groupRoots: () => [20], run, record: async row => rows.push(row), intervalMs: 60_000 });
  await safe.stop(); assert(rows.some(row => row.phase === 'after-cleanup'));
});

test('lsof exit 1 with empty stderr keeps stdout (PID exited between ps and lsof); other failures fail closed', () => {
  const live = 'p21\nf10\ntIPv4\nn127.0.0.1:40000->127.0.0.1:18798\n';
  assert.equal(lsofExitOutput({ code: 1, stdout: live, stderr: '' }), live);
  assert.equal(lsofExitOutput({ code: 1, stdout: '', stderr: '' }), '');
  assert.equal(forbiddenTCP(tcpRecords(lsofExitOutput({ code: 1, stdout: 'p21\nf1\ntIPv4\nn127.0.0.1:1->127.0.0.1:18789\n', stderr: '' }))).length, 1);
  assert.equal(lsofExitOutput({ code: 1, stdout: live, stderr: 'lsof: WARNING' }), undefined);
  assert.equal(lsofExitOutput({ code: null, signal: 'SIGTERM', stdout: '', stderr: '' }), undefined);
  assert.equal(lsofExitOutput({ code: 2, stdout: '', stderr: '' }), undefined);
});

test('round three: native project registry discovers seeded .cwd and loses it when registration is removed (no gateway/listen)', async () => {
  await mkdir(resolve(root, '_tmp/fe-parity/real-run'), { recursive: true });
  const parent = await mkdtemp(resolve(root, '_tmp/fe-parity/real-run/unit-r3-'));
  try {
    const runtime = await prepareRuntime('ready', resolve(parent, 'ready'));
    const { createProjectId } = await import(pathToFileURL(resolve(baseline, 'server/utils/pilotPaths.js')));
    const native = await frozenFunctions('../src/web/server/listProjects.ts', {
      readFile, readdir, stat, resolve, basename: path.basename, createProjectId,
      listProjectSessions: async () => [],
    });
    const options = { pilotHome: resolve(runtime.runDir, 'home') };
    assert.equal(await readFile(resolve(runtime.registrationPath, '.cwd'), 'utf8'), runtime.projectPath);
    assert.deepEqual(await native.listRegisteredWebProjects(options), [{ projectKey: runtime.projectPath }]);
    const projects = await native.listWebProjects(options);
    assert.equal(projects.projects.length, 1);
    assert.equal(projects.projects[0].fullPath, runtime.projectPath);
    assert.equal(projects.projects[0].name, 'e2e-project');
    // Execute the actual UI API row mapper too: regular rows omit capabilities,
    // unlike General. Requiring files=true in setup would reject a valid seed.
    const gateway = {
      listProjects: () => native.listWebProjects(options),
      listSessions: async () => ({ sessions: [] }),
      describeProject: async () => ({ sessionCount: 0 }),
    };
    const uiProjects = await frozenFunctions('server/projects.js', {
      fs: { readFile, readdir, stat }, path, directoryCache: new Map(),
      createProjectId, resolvePilotHome: () => options.pilotHome,
      withPilotDeckGatewayReadRetry: async callback => callback(gateway),
      isGatewayUnavailableError: () => false, applyCustomSessionNames() {},
      projectBindingSummary: () => undefined,
      ensureGeneralWorkspaceDirectory: async () => resolve(runtime.workspacePath, 'general'),
    });
    const apiRows = await uiProjects.getProjects();
    const seeded = apiRows.filter(row => row.fullPath === runtime.projectPath);
    assert.equal(seeded.length, 1); assert.equal(seeded[0].name, runtime.projectId);
    assert.equal(seeded[0].capabilities, undefined);
    assert.equal(seeded[0].capabilities?.files !== false, true);
    assert.equal(apiRows.find(row => row.name === 'general').capabilities.files, false);
    const lookup = native.createRegisteredWebProjectResolver(options);
    assert.equal(await lookup(runtime.projectPath), runtime.projectPath);
    // Move the registration outside home/projects; workspace + seeds remain.
    await rename(runtime.registrationPath, resolve(parent, 'removed-registration'));
    assert.equal(await lookup(runtime.projectPath), undefined);
    assert.deepEqual(await native.listRegisteredWebProjects(options), []);
    assert.deepEqual((await native.listWebProjects(options)).projects, []);
    assert.match(await readFile(resolve(runtime.projectPath, 'README.md'), 'utf8'), /E2E-FILE-SEED/);
  } finally { await rm(parent, { recursive: true }); }
});

test('round three: lsof timeout/kill or signal stays fail closed even if exit code is 1 and stdout is populated', () => {
  const stdout = 'p21\nf10\ntIPv4\nn127.0.0.1:40000->127.0.0.1:18798\n';
  assert.equal(lsofExitOutput({ code: 1, stdout, stderr: '', killed: true, signal: null }), undefined);
  assert.equal(lsofExitOutput({ code: 1, stdout, stderr: '', killed: false, signal: 'SIGTERM' }), undefined);
  assert.equal(lsofExitOutput({ code: 1, stdout, stderr: '', killed: false, signal: null }), stdout);
});

test('round four: frozen file tree resolves .cwd, reads seed entries and filters only documented directories (no HTTP/listen)', async () => {
  const parent = await mkdtemp(resolve(root, '_tmp/fe-parity/real-run/unit-r4-'));
  try {
    const runtime = await prepareRuntime('ready', resolve(parent, 'ready'));
    const cwd = await readFile(resolve(runtime.registrationPath, '.cwd'), 'utf8');
    assert.equal(cwd, runtime.projectPath);
    const native = await frozenFunctions('server/projects.js', {
      fs: { readFile, readdir, stat }, path, directoryCache: new Map(),
      resolvePilotHome: () => resolve(runtime.runDir, 'home'),
    });
    assert.equal(await native.extractProjectDirectory(runtime.projectId), cwd);
    const tree = await frozenFunctions('server/index.js', { fsPromises: { readFile, readdir, stat, access: (await import('node:fs/promises')).access }, fs: (await import('node:fs')).default, path });
    const entries = await tree.getFileTree(cwd, 10, 0, true);
    assert(entries.some(row => row.name === 'README.md' && row.type === 'file'));
    assert(entries.some(row => row.name === 'sample.ts' && row.type === 'file'));
    assert.match(await readFile(resolve(cwd, 'README.md'), 'utf8'), /E2E-FILE-SEED/);
    const source = await readFile(resolve(baseline, 'src/components/main-content/view/MainContent.tsx'), 'utf8');
    assert.match(source, /\[explorerCollapsed, setExplorerCollapsed\] = useState\(true\)/);
    assert.match(source, /isFiles && explorerVisible \? \(/);
    assert.match(source, /onClick=\{toggleExplorer\}/);
    const api = await readFile(resolve(baseline, 'src/utils/api.js'), 'utf8');
    assert(api.includes('authenticatedFetch(`/api/projects/${projectName}/files`, options)'));
  } finally { await rm(parent, { recursive: true }); }
});

test('real runner: exit 1 accepted only for original failed results', () => {
  const report = status => ({ suites: [{ specs: [{ file: 'x.spec.mjs', title: 'case', tests: [{ results: [{ status }] }] }] }] });
  assert.equal(realExecutionError({ code: 1, signal: null }, report('failed'), ''), null);
  assert.equal(realExecutionError({ code: 0, signal: null }, report('passed'), ''), null);
  for (const execution of [{ code: 2 }, { code: 1, signal: 'SIGTERM' }, { code: null }, { code: 0 }]) assert(realExecutionError(execution, report('failed'), ''));
  assert(realExecutionError({ code: 1 }, report('passed'), ''));
});
