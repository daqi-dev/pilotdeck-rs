import test from 'node:test';
import assert from 'node:assert/strict';
import { Readable } from 'node:stream';
import { readFile, readdir, stat, mkdtemp, rm, mkdir, symlink, rename, realpath, unlink, writeFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { deflateSync } from 'node:zlib';
import { randomUUID } from 'node:crypto';
import net from 'node:net';
import path from 'node:path';
import { EventEmitter } from 'node:events';
import { userInfo } from 'node:os';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { baseline, requireUI, REAL_TEST_KEY, realPorts, realOrigin, root } from './env.mjs';
import { createMockLLM, planResponse, probeColor } from './mock-llm.mjs';
import { localFetchURL, installNetworkIsolation } from './real-network.mjs';
import { runtimeConfig, runtimeArgs, prepareRuntime } from './real-launch.mjs';
import { realExecutionEnv, assertRealExecutionEnv, prepareRealOSDirectories, realOSPaths, realChromeEnv } from './execution-env.mjs';
import { assertRealPortsFree, validateRealRegistration, assertCanonicalAncestor, bridgeReady, realExecutionError } from './real-runner.mjs';
import { processTree, tcpRecords, forbiddenTCP, forbiddenProcesses, startTCPAudit, lsofExitOutput, processHomes, retryNodeHomes, environmentTargets, resolveUnreadableProcesses } from './real-tcp-audit.mjs';
import { PENDING_ORCHESTRATION, reconcileReal } from './accounting.mjs';
import { requiredChromeFlags, chromeDefaultsEvidence, realChromeLaunchOptions } from './real-browser.mjs';
import { startIsolatedMockLLM } from './real-mock.mjs';
import { realGitCandidates, discoverRealGit, prepareRealGitBin, assertRealGitBin } from './real-git.mjs';
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
async function frozenFunctions(path, dependencies = {}, names) {
  const ts = requireUI('typescript'), source = await readFile(resolve(baseline, path), 'utf8');
  const ast = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true);
  const functions = ast.statements.filter(node => ts.isFunctionDeclaration(node) && (!names || names.includes(node.name?.text)));
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
    assert.equal(env.HOME, '/owned/run/home-os'); assert.equal(env.HOST, '127.0.0.1');
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

test('round six: OS HOME/XDG/tmp and browser opt-out are mandatory; real OS home and missing opt-out FAIL before spawn', async () => {
  await mkdir(resolve(root, '_tmp/fe-parity/real-run'), { recursive: true });
  const parent = await mkdtemp(resolve(root, '_tmp/fe-parity/real-run/unit-home-r6-'));
  try {
    for (const mode of ['ready', 'onboarding']) {
      const env = realExecutionEnv({ ...process.env, HOME: userInfo().homedir, USERPROFILE: '/operator', XDG_CONFIG_HOME: '/operator/config', XDG_DATA_HOME: '/operator/data', XDG_CACHE_HOME: '/operator/cache', TMPDIR: '/operator/tmp', PILOTDECK_SKIP_BROWSER_OPEN: '0' },
        { output: parent, runDir: resolve(parent, mode), mode });
      assertRealExecutionEnv(env);
      assert.notEqual(env.HOME, userInfo().homedir);
      assert.notEqual(env.HOME, env.PILOT_HOME);
      for (const [name, expected] of Object.entries(realOSPaths(env.PILOTDECK_E2E_RUN_DIR))) assert.equal(env[name], expected);
      assert.equal(env.PILOTDECK_SKIP_BROWSER_OPEN, '1');
      const missing = { ...env }; delete missing.PILOTDECK_SKIP_BROWSER_OPEN;
      assert.throws(() => assertRealExecutionEnv(missing), /FAIL:.*SKIP_BROWSER_OPEN/);
      assert.throws(() => assertRealExecutionEnv({ ...env, HOME: userInfo().homedir }), /FAIL:.*operator home/);
      for (const name of Object.keys(realOSPaths(env.PILOTDECK_E2E_RUN_DIR))) assert.throws(() => assertRealExecutionEnv({ ...env, [name]: '/operator' }), /FAIL:/);
      await prepareRealOSDirectories(env);
      for (const directory of new Set(Object.values(realOSPaths(env.PILOTDECK_E2E_RUN_DIR)))) assert((await stat(directory)).isDirectory());
      // A real Node child proves os.homedir()/tmpdir(), without importing a
      // server, connecting a socket, or inspecting the operator's filesystem.
      const actual = JSON.parse(execFileSync(process.execPath, ['--input-type=module', '-e', 'import os from "node:os"; console.log(JSON.stringify({home:os.homedir(),tmp:os.tmpdir()}))'], { env, encoding: 'utf8' }));
      assert.deepEqual(actual, { home: env.HOME, tmp: env.TMPDIR });
      const runtime = await prepareRuntime(mode, env.PILOTDECK_E2E_RUN_DIR);
      assert.equal(runtime.runDir, env.PILOTDECK_E2E_RUN_DIR);
      await assert.rejects(prepareRuntime(mode, env.PILOTDECK_E2E_RUN_DIR), /EEXIST/);
    }
    const escaped = realExecutionEnv({}, { output: parent, runDir: resolve(parent, 'escaped'), mode: 'ready' });
    await mkdir(escaped.PILOTDECK_E2E_RUN_DIR);
    await symlink('/private/tmp', escaped.HOME);
    await assert.rejects(prepareRealOSDirectories(escaped), /FAIL:.*symlink escape/);
  } finally { await rm(parent, { recursive: true }); }
});

test('round seven: open/xdg-open/osascript and applications FAIL immediately, owned Playwright Chrome requires current ancestry and safe flags (injected ps only)', async () => {
  const chrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
  const base = '10 1 5 node runner\n20 10 20 node playwright\n21 20 20 node worker\n30 10 30 node runtime';
  const launch = `${chrome} ${requiredChromeFlags.join(' ')} --remote-debugging-pipe --user-data-dir=/owned/tmp/playwright_chromiumdev_profile-abc`;
  const options = { playwrightRoots: [20], browserTmpRoots: ['/owned/tmp'], processHOME: new Map([[22, userInfo().homedir], [23, userInfo().homedir]]) };
  assert.deepEqual(forbiddenProcesses(processTree(`${base}\n22 21 20 ${launch}`, 10), options), []);
  for (const command of ['open', '/usr/bin/open http://127.0.0.1:3010', 'xdg-open http://127.0.0.1:3010', '/usr/bin/osascript -e script', '/bin/sh -c open http://127.0.0.1:3010', '/Applications/LibreOffice.app/Contents/MacOS/soffice --version', '/Applications/GoogleUpdater.app/Contents/MacOS/GoogleUpdater', chrome, launch.replace('/owned/tmp/', '/operator/'), launch.replace(' --remote-debugging-pipe', ''), launch.replace('playwright_chromiumdev_profile-', 'other-')]) {
    const rows = processTree(`${base}\n22 21 20 ${command}`, 10);
    assert.equal(forbiddenProcesses(rows, options).length, 1, command);
  }
  // Even identical browser arguments from the native runtime cannot authorize it.
  assert.equal(forbiddenProcesses(processTree(`${base}\n32 30 30 ${launch}`, 10), options).length, 1);
  const helper = '/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/142.0/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer) --type=renderer';
  const browserTree = processTree(`${base}\n22 21 20 ${launch}\n23 22 20 ${helper}\n24 22 20 /Applications/GoogleUpdater.app/Contents/MacOS/GoogleUpdater`, 10);
  assert.deepEqual(forbiddenProcesses(browserTree, options).map(row => row.pid), [24]);
  assert.equal(forbiddenProcesses(processTree(`${base}\n23 30 30 ${helper}`, 10), options).length, 1);
  const remembered = { ...options, approvedChrome: new Map([[22, launch], [23, helper]]) };
  assert.equal(forbiddenProcesses(processTree(`${base}\n23 1 20 ${helper}`, 10, [20]), remembered).length, 1, 'previously attested helper loses approval when its current chain breaks');
  assert.equal(forbiddenProcesses(processTree(`${base}\n22 21 20 ${launch.replace('/owned/tmp/', '/operator/')}`, 10), remembered).length, 1, 'changed command must lose approval');
  assert.equal(forbiddenProcesses(processTree(`${base}\n24 21 20 /Users/example/Library/Google/GoogleUpdater/GoogleUpdater`, 10), options).length, 1);
  let forbidden = false, killed = 0, lsofCalls = 0;
  const records = [];
  const audit = await startTCPAudit({ rootPid: 10, groupRoots: () => [20, 30], intervalMs: 60_000,
    run: async file => {
      if (file === 'ps') return base + (forbidden ? '\n32 30 32 /usr/bin/open http://127.0.0.1:3010' : '');
      lsofCalls++; return '';
    },
    onFatal: (_error, groups) => { killed++; assert(groups.includes(32)); },
    record: async row => { if (row.status === 'FAIL') assert.equal(killed, 1); records.push(row); },
  });
  forbidden = true;
  await assert.rejects(audit.checkpoint('injected-open'), /FAIL:.*forbidden process/);
  assert.equal(killed, 1); assert.equal(lsofCalls, 1, 'fail before lsof or evidence I/O');
  assert.equal(records.at(-1).forbiddenProcesses[0].pid, 32);
  await assert.rejects(audit.stop(), /FAIL:.*forbidden process/);
});

test('round seven: Chrome alone gets account HOME; setup and baseline Node retain isolated HOME; installed default flags appear once (no browser)', () => {
  const env = realExecutionEnv({ HOME: '/inherited-wrong', XDG_CONFIG_HOME: '/operator-config',
    UserInstallation: '/operator/profile', SYSUSERCONFIG: '/operator/config', SYSUSERHOME: '/operator/home', URE_BOOTSTRAP: '/operator/bootstrap' }, { output: '/owned/out', runDir: '/owned/run', mode: 'ready' });
  const chrome = realChromeEnv(env), options = realChromeLaunchOptions(env), evidence = chromeDefaultsEvidence();
  assertRealExecutionEnv(env);
  assert.equal(env.HOME, '/owned/run/home-os');
  assert.equal(chrome.HOME, userInfo().homedir); assert.equal(chrome.USERPROFILE, chrome.HOME);
  assert.equal(chrome.TMPDIR, env.TMPDIR); assert.equal(chrome.NODE_OPTIONS, '');
  assert(!('XDG_CONFIG_HOME' in chrome));
  for (const name of ['UserInstallation', 'SYSUSERCONFIG', 'SYSUSERHOME', 'URE_BOOTSTRAP']) assert(!(name in env));
  assert.throws(() => realChromeEnv({ ...env, HOME: userInfo().homedir }), /FAIL:/);
  assert.throws(() => assertRealExecutionEnv(chrome), /FAIL:/);
  for (const flag of requiredChromeFlags) assert.equal([...evidence.defaults, ...options.args].filter(value => value === flag).length, 1);
  assert(!options.args.some(value => value.startsWith('--user-data-dir')), 'Playwright supplies the temporary profile');
  assert.equal(evidence.sourceSHA256.length, 64);
});

test('round seven: five requested negative controls, R14 updater identity and strict server ancestry plus environment positives (injected snapshots)', () => {
  const operatorHome = '/Users/operator', serviceHome = '/owned/run/home-os';
  const chrome = `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome ${requiredChromeFlags.join(' ')} --remote-debugging-pipe --user-data-dir=/owned/tmp/playwright_chromiumdev_profile-abc`;
  const userUpdater = `${operatorHome}/Library/Application Support/Google/GoogleUpdater/142.0/GoogleUpdater.app/Contents/MacOS/GoogleUpdater`;
  const helperUpdater = '/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/155.0/Helpers/GoogleUpdater.app/Contents/MacOS/GoogleUpdater';
  const server = `${process.execPath} --import tsx ${resolve(baseline, 'server/index.js')}`;
  const office = '/Applications/LibreOffice.app/Contents/MacOS/soffice --version';
  const base = `10 1 5 node runner\n20 10 20 node playwright\n21 20 20 node worker\n22 21 20 ${chrome}\n30 10 30 node supervisor\n31 30 30 ${server}`;
  const options = { playwrightRoots: [20], browserTmpRoots: ['/owned/tmp'], runtimeRoots: [{ pid: 30, home: serviceHome }], operatorHome, updaterRealpath: path => path, sofficeRealpath: path => path,
    processHOME: new Map([[22, operatorHome], [23, operatorHome], [31, serviceHome], [32, serviceHome]]) };
  const failures = (extra = '', overrides = {}) => forbiddenProcesses(processTree(base + extra, 10), { ...options, ...overrides }).map(row => row.pid);
  assert.deepEqual(failures(`\n23 22 20 ${userUpdater} --wake\n32 31 30 ${office}`), []);
  assert.deepEqual(failures('', { processHOME: new Map([[22, serviceHome], [31, serviceHome]]) }), [], 'Chrome HOME is outside the environment audit');
  assert.deepEqual(failures(`\n23 22 20 ${helperUpdater} --enable-logging`), [23], 'bundle Helpers GoogleUpdater FAIL');
  for (const flag of ['--install', '--install=true', '--register', '--uninstall', '--unregister', '--setup', '--system', '--self-update', '--INSTALL']) {
    assert.deepEqual(failures(`\n23 22 20 ${userUpdater} ${flag}`), [23], `user GoogleUpdater ${flag} FAIL`);
  }
  assert.deepEqual(failures(`\n32 30 30 ${office}`), [32], 'soffice supervisor ancestry without server FAIL');
  assert.deepEqual(failures(`\n32 31 30 ${office}`, { processHOME: new Map([...options.processHOME, [32, operatorHome]]) }), [32], 'soffice real HOME FAIL');
  assert.deepEqual(failures(`\n32 31 30 ${office}`, { processHOME: new Map([[22, operatorHome], [31, serviceHome]]) }), [32], 'missing soffice HOME FAIL');
  assert.deepEqual(failures(`\n32 31 30 ${office}`, { processHOME: new Map([...options.processHOME, [31, operatorHome]]) }), [32], 'server with real HOME cannot authorize soffice');
  assert.deepEqual(failures(`\n23 21 20 ${userUpdater} --wake`), [], 'R14: updater identity suffices with a Playwright worker parent');
  assert.deepEqual(failures(`\n23 30 30 ${userUpdater} --wake`), [], 'R14: updater identity suffices with a runtime parent');
  for (const command of [userUpdater.replace('/Users/operator/', '/Users/other/'), userUpdater.replace('/142.0/', '/../'), userUpdater.replace('/GoogleUpdater/142.0/', '/GoogleUpdater-extra/142.0/')]) {
    assert.deepEqual(failures(`\n23 22 20 ${command} --wake`), [23]);
  }
  for (const flag of requiredChromeFlags) {
    assert.equal(forbiddenProcesses(processTree(base.replace(' ' + flag, ''), 10), options)[0].pid, 22, `missing ${flag} FAIL`);
    assert.equal(forbiddenProcesses(processTree(base.replace(' ' + flag, ` ${flag} ${flag}`), 10), options)[0].pid, 22, `duplicate ${flag} FAIL`);
  }
  assert.deepEqual(forbiddenProcesses(processTree(base.replace('31 30 30', '31 21 20') + `\n32 31 20 ${office}`, 10), options).map(row => row.pid), [32], 'server-looking argv outside owned runtime FAIL');
});

test('round seven: ps -E HOME parsing binds identities/argv, redacts credentials and audit fails closed (no ps/lsof)', async () => {
  const row = { pid: 22, ppid: 21, pgid: 20, command: '/Applications/LibreOffice.app/Contents/MacOS/soffice --version' };
  const raw = `22 21 20 ${row.command} HOME=/owned/run with spaces/home-os SECRET=do-not-record TMPDIR=/owned/tmp`;
  assert.equal(processHomes(raw, [row]).get(22), '/owned/run with spaces/home-os');
  assert.equal(processHomes(`22 21 20 HOME=/owned/home TMPDIR=/owned/tmp ${row.command}`, [row]).get(22), '/owned/home');
  for (const value of [raw.replace('22 21', '22 99'), raw.replace('--version', '--headless'), raw.replace('HOME=', 'WRONG='), raw + ' HOME=/other', `22 21 20 ${row.command}`]) assert.throws(() => processHomes(value, [row]), /FAIL:/);
  const fakeArg = { ...row, command: row.command + ' HOME=/fake' };
  assert.equal(processHomes(`22 21 20 ${fakeArg.command} HOME=/owned/home TMPDIR=/owned/tmp`, [fakeArg]).get(22), '/owned/home');
  const serviceHome = '/owned/run/home-os';
  const server = `${process.execPath} --import tsx ${resolve(baseline, 'server/index.js')}`;
  const snapshot = `10 1 5 node runner\n30 10 30 node supervisor\n31 30 30 ${server}\n32 31 30 ${row.command}`;
  let wrongHOME = false, fatal = 0, lsof = 0;
  const records = [];
  const audit = await startTCPAudit({ rootPid: 10, groupRoots: () => [30], runtimeRoots: () => [{ pid: 30, home: serviceHome }], sofficeRealpath: path => path, intervalMs: 60_000,
    run: async (file, args) => {
      if (file === 'lsof') { lsof++; return ''; }
      if (!args.includes('-E')) return snapshot;
      assert.equal(args[args.indexOf('-p') + 1], '31,32');
      return `31 30 30 ${server} HOME=${serviceHome} SECRET=do-not-record\n32 31 30 ${row.command} HOME=${wrongHOME ? userInfo().homedir : serviceHome} SECRET=do-not-record`;
    }, onFatal: () => { fatal++; }, record: async record => records.push(record),
  });
  assert.equal(records[0].environmentEvidence.length, 2);
  assert(!JSON.stringify(records).includes('do-not-record'));
  wrongHOME = true;
  await assert.rejects(audit.checkpoint('soffice-real-home'), /FAIL:.*forbidden process/);
  assert.equal(fatal, 1); assert.equal(lsof, 1, 'HOME rejection precedes lsof');
  await assert.rejects(audit.stop(), /FAIL:/);
  await assert.rejects(startTCPAudit({ rootPid: 10, runtimeRoots: () => [{ pid: 30, home: serviceHome }], sofficeRealpath: path => path,
    run: async (_file, args) => { if (args.includes('-E')) throw new Error('ps -E unavailable'); return snapshot; }, record: async () => {},
  }), /ps -E unavailable/);
});

test('round seven: mock uses a separate isolated Node environment and returns final stats on shutdown (spawn/IPC injected; no listen)', async () => {
  const env = realExecutionEnv({}, { output: '/owned/out', runDir: '/owned/mock', mode: 'ready' });
  const children = new Set(); let child;
  const spawnImpl = (_command, args, options) => {
    assert.equal(args[0], resolve(root, 'scripts/fe-parity/real-mock.mjs'));
    assertRealExecutionEnv(options.env); assert.equal(options.env.HOME, '/owned/mock/home-os');
    assert.equal(options.env.FE_REAL_RUNTIME, '1'); assert(options.env.NODE_OPTIONS.endsWith('/real-network.mjs'));
    assert(options.detached); assert(options.stdio.includes('ipc'));
    child = new EventEmitter(); child.connected = true;
    child.send = message => {
      assert.equal(message.type, 'close');
      child.emit('message', { type: 'stats', stats: { requests: 7, rejected: 1, errors: ['rejected fixture'] } });
      child.connected = false; child.emit('close', 0);
    };
    queueMicrotask(() => child.emit('message', { type: 'ready' }));
    return child;
  };
  const mock = await startIsolatedMockLLM(env, children, spawnImpl);
  assert(children.has(child)); await mock.close(); assert.equal(children.size, 0);
  assert.equal(mock.stats.requests, 7); assert.equal(mock.stats.rejected, 1); assert.deepEqual(mock.stats.errors, ['rejected fixture']);
  await assert.rejects(startIsolatedMockLLM({ ...env, HOME: userInfo().homedir }, children, () => { throw new Error('must not spawn'); }), /FAIL:/);
});

test('round seven: setup refuses unisolated Node before Chromium launch; mock startup/early-exit/missing-stats cannot become PASS (no spawn/listen)', async () => {
  const { default: setup } = await import('./real-setup.mjs');
  const previous = process.env.PILOTDECK_SKIP_BROWSER_OPEN;
  process.env.PILOTDECK_SKIP_BROWSER_OPEN = '0';
  try { await assert.rejects(setup({}), /FAIL:/); }
  finally { if (previous === undefined) delete process.env.PILOTDECK_SKIP_BROWSER_OPEN; else process.env.PILOTDECK_SKIP_BROWSER_OPEN = previous; }
  const env = realExecutionEnv({}, { output: '/owned/out', runDir: '/owned/mock', mode: 'ready' });
  const children = new Set();
  await assert.rejects(startIsolatedMockLLM(env, children, () => {
    const child = new EventEmitter();
    queueMicrotask(() => child.emit('error', new Error('injected spawn failed')));
    return child;
  }), /injected spawn failed/);
  assert.equal(children.size, 0);
  for (const early of [false, true]) {
    let child;
    const mock = await startIsolatedMockLLM(env, children, () => {
      child = new EventEmitter(); child.connected = true;
      child.send = () => { child.connected = false; child.emit('close', 0); };
      queueMicrotask(() => child.emit('message', { type: 'ready' }));
      return child;
    });
    if (early) { child.connected = false; child.emit('close', 0); }
    await mock.close();
    assert(mock.stats.errors.some(error => error.includes(early ? 'unexpectedly' : 'without final stats')));
    assert.equal(children.size, 0);
  }
});

test('round eight: ordered real Git discovery rejects system/Xcode shims before executing, no missing-candidate fallback to system Git', async () => {
  const env = realExecutionEnv({ GIT_CONFIG_GLOBAL: '/operator/config', GIT_DIR: '/operator/repo', GIT_WORK_TREE: '/operator/work', GIT_EXEC_PATH: '/operator/bin' },
    { output: '/owned/out', runDir: '/owned/run', mode: 'ready' });
  for (const key of ['GIT_CONFIG_GLOBAL', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_EXEC_PATH']) assert(!(key in env));
  const probed = [], executed = [];
  const missing = () => Object.assign(new Error('missing'), { code: 'ENOENT' });
  const io = { canonical: async candidate => { probed.push(candidate); if (candidate === realGitCandidates[0]) throw missing(); return '/usr/local/Cellar/git/2.51/bin/git'; },
    executable: async () => {}, version: async target => { executed.push(target); return 'git version 2.51.0'; } };
  const evidence = await discoverRealGit(env, io);
  assert.deepEqual(probed, realGitCandidates.slice(0, 2)); assert.equal(evidence.candidate, realGitCandidates[1]);
  assert.equal(evidence.realpath, executed[0]); assert.equal(evidence.version, 'git version 2.51.0');
  probed.length = 0; executed.length = 0;
  for (const target of ['/usr/bin/git', '/Applications/Xcode.app/Contents/Developer/usr/bin/git']) {
    await assert.rejects(discoverRealGit(env, { ...io, canonical: async candidate => { probed.push(candidate); return target; } }), /FAIL:.*shim/);
    assert.equal(executed.length, 0, 'never execute a known shim, even for --version');
  }
  await assert.rejects(discoverRealGit(env, { ...io, canonical: async () => { throw missing(); } }), /FAIL: no real git/);
  await assert.rejects(discoverRealGit(env, { ...io, version: async () => 'wrong program' }), /FAIL:.*version/);
  probed.length = 0;
  await discoverRealGit(env, { ...io, canonical: async candidate => { probed.push(candidate); return '/opt/homebrew/Cellar/git/2.51/bin/git'; } });
  assert.deepEqual(probed, [realGitCandidates[0]]);
});

test('round eight: run bin contains only node/git links, link to /usr/bin/git FAIL; drift/global CLI rejected (fake discovery, no Git execution)', async () => {
  const parent = await mkdtemp(resolve(root, '_tmp/fe-parity/real-run/unit-git-r8-'));
  const env = realExecutionEnv({}, { output: parent, runDir: resolve(parent, 'ready'), mode: 'ready' });
  // Node stands in for an executable only in this injected filesystem fixture.
  const target = await realpath(process.execPath);
  const io = { canonical: async candidate => realGitCandidates.includes(candidate) ? target : realpath(candidate),
    executable: async () => {}, version: async () => 'git version fixture' };
  try {
    await prepareRealOSDirectories(env);
    const evidence = await prepareRealGitBin(env, io);
    assert.equal(evidence.realpath, target); assert.equal(evidence.version, 'git version fixture');
    assert.deepEqual((await readdir(resolve(env.PILOTDECK_E2E_RUN_DIR, 'bin'))).sort(), ['git', 'node']);
    assert.equal(await assertRealGitBin(env, io), target);
    await unlink(evidence.link); await symlink('/usr/bin/git', evidence.link);
    await assert.rejects(assertRealGitBin(env, io), /FAIL:.*shim/);
    await unlink(evidence.link); await symlink(target, evidence.link);
    await symlink(target, resolve(env.PILOTDECK_E2E_RUN_DIR, 'bin/task-master'));
    await assert.rejects(assertRealGitBin(env, io), /FAIL:.*drift/);
    await assert.rejects(assertRealGitBin({ ...env, PATH: env.PATH + ':/opt/homebrew/bin' }, io), /FAIL:.*drift/);
  } finally { await rm(parent, { recursive: true }); }
});

test('round eight: exact Xcode license check needs current owned server/git ancestry and isolated HOME; other xcodebuild/xcrun or nonservice ancestors FAIL', () => {
  const home = '/owned/run/home-os', runtime = { pid: 30, home }, license = '/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild -license check';
  const server = `${process.execPath} --import tsx ${resolve(baseline, 'server/index.js')}`;
  const base = `10 1 10 node runner\n30 10 30 node runtime\n31 30 30 ${server}\n32 31 30 git rev-parse --is-inside-work-tree`;
  const rows = processTree(`${base}\n33 32 30 ${license}`, 10), readOnlySystemChecks = [];
  const options = { runtimeRoots: [runtime], processHOME: new Map([[31, home], [32, home], [33, home]]), readOnlySystemChecks };
  assert.deepEqual(forbiddenProcesses(rows, options), []);
  assert.deepEqual(readOnlySystemChecks[0].argv, [license.split(' ')[0], '-license', 'check']);
  assert.equal(readOnlySystemChecks[0].ancestorPid, 31);
  assert(environmentTargets(rows).some(row => row.pid === 33));
  assert.deepEqual(forbiddenProcesses(processTree(`${base}\n33 31 30 ${license}`, 10), options), []);
  // Reproduce old8 gateway -> Git; the Git must still descend from this runtime.
  const gatewayBase = '10 1 10 runner\n30 10 30 runtime\n34 30 30 gateway\n32 34 30 git rev-parse --is-inside-work-tree';
  assert.deepEqual(forbiddenProcesses(processTree(`${gatewayBase}\n33 32 30 ${license}`, 10), options), []);
  for (const command of [license + ' extra', license.replace('check', 'accept'), license.replace('-license check', '-version'),
    license.replace('/Applications/Xcode.app', '/Applications/Xcode-beta.app'), '/usr/bin/xcodebuild -license check',
    '/usr/bin/xcrun --find git', 'xcrun xcodebuild -license check', `/bin/sh -c ${license}`]) {
    assert(forbiddenProcesses(processTree(`${base}\n33 32 30 ${command}`, 10), options).some(row => row.pid === 33), command);
  }
  for (const extra of [`33 10 10 ${license}`, `33 30 30 ${license}`, `33 1 30 ${license}`,
    `32 10 10 git rev-parse HEAD\n33 32 10 ${license}`]) {
    const noGitBase = `10 1 10 node runner\n30 10 30 node runtime\n31 30 30 ${server}`;
    assert(forbiddenProcesses(processTree(`${noGitBase}\n${extra}`, 10, [30]), options).some(row => row.pid === 33), extra);
  }
  for (const pid of [31, 32, 33]) {
    const homes = new Map(options.processHOME); homes.set(pid, userInfo().homedir);
    const tree = pid === 31 ? processTree(`${base}\n33 31 30 ${license}`, 10) : processTree(`${gatewayBase}\n33 32 30 ${license}`, 10);
    assert(forbiddenProcesses(tree, { ...options, processHOME: homes }).some(row => row.pid === 33));
  }
});

test('round eight: license exception is recorded after actual-HOME snapshot; invalid argv/nonservice ancestry fatal before lsof (injected ps)', async () => {
  const home = '/owned/run/home-os', license = '/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild -license check';
  const server = `${process.execPath} --import tsx ${resolve(baseline, 'server/index.js')}`;
  const base = `10 1 10 runner\n30 10 30 runtime\n31 30 30 ${server}`;
  let command = license, parent = 31, lsof = 0, killed = 0;
  const records = [];
  const audit = await startTCPAudit({ rootPid: 10, runtimeRoots: () => [{ pid: 30, home }], intervalMs: 60_000,
    run: async (file, args) => {
      if (file === 'lsof') { lsof++; return ''; }
      if (args.includes('-E')) return `31 30 30 ${server} HOME=${home}\n33 ${parent} 30 ${command} HOME=${home}`;
      return `${base}\n33 ${parent} 30 ${command}`;
    }, record: async row => records.push(row), onFatal: () => killed++ });
  assert.equal(records[0].status, 'PASS'); assert.equal(records[0].readOnlySystemChecks.length, 1);
  assert.equal(records[0].environmentEvidence.length, 2);
  command = license + ' --help';
  await assert.rejects(audit.checkpoint('wrong-argv'), /FAIL:.*forbidden process/);
  assert.equal(lsof, 1); assert.equal(killed, 1);
  parent = 10; command = license;
  await assert.rejects(audit.stop(), /FAIL:.*forbidden process/);
  assert.equal(lsof, 1); assert.equal(records.at(-1).forbiddenProcesses[0].pid, 33);
});

test('round eight: frozen telemetry Git explicitly reads gateway cwd, HOME isolation does not relocate a source checkout (no Git/server execution)', async () => {
  const calls = [], repo = resolve(baseline, '..');
  const telemetry = await frozenFunctions('../src/telemetry/context.ts', {
    process: { cwd: () => repo }, execFileSync: (command, args, options) => {
      calls.push({ command, args, cwd: options.cwd });
      return args.includes('--short') ? 'cc7c4e6\n' : 'true\n';
    },
  }, ['looksLikeSourceCheckout', 'resolveAppCommitHash']);
  assert.equal(telemetry.looksLikeSourceCheckout(), true);
  assert.equal(telemetry.resolveAppCommitHash({}), 'cc7c4e6');
  assert.deepEqual(calls, [
    { command: 'git', args: ['rev-parse', '--is-inside-work-tree'], cwd: repo },
    { command: 'git', args: ['rev-parse', '--short', 'HEAD'], cwd: repo },
  ]);
});

test('round nine: only unchanged optional application argv can be unreadable; Node, identity changes and malformed HOME fail closed', () => {
  const office = { pid: 22, ppid: 21, pgid: 20, command: '/Applications/LibreOffice.app/Contents/MacOS/soffice --version' };
  const raw = `22 21 20 ${office.command}`, unreadable = new Set();
  assert.equal(processHomes(raw, [office], { unreadable }).size, 0);
  assert.deepEqual([...unreadable], [22]);
  const node = { ...office, command: `${process.execPath} /owned/service.mjs` };
  assert.throws(() => processHomes(`22 21 20 ${node.command}`, [node], { unreadable: new Set() }), /FAIL:.*did not expose environment/);
  for (const invalid of [raw.replace('22 21', '22 99'), raw.replace('21 20', '21 99'), raw.replace('22 21', '23 21'),
    raw + ' --extra', raw + ' SECRET=do-not-record', raw + ' HOME=', raw + ' HOME=/one HOME=/two', raw + '\n' + raw]) {
    assert.throws(() => processHomes(invalid, [office], { unreadable: new Set() }), /FAIL:/, invalid);
  }
});

test('round nine/eleven: updater/Chrome use current identity only; soffice retains environment alternatives', () => {
  const operatorHome = userInfo().homedir, home = '/owned/run/home-os';
  const chrome = `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome ${requiredChromeFlags.join(' ')} --remote-debugging-pipe --user-data-dir=/owned/tmp/playwright_chromiumdev_profile-abc`;
  const helper = '/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/142.0/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer) --type=renderer';
  const updater = `${operatorHome}/Library/Application Support/Google/GoogleUpdater/142.0/GoogleUpdater.app/Contents/MacOS/GoogleUpdater --wake`;
  const office = '/Applications/LibreOffice.app/Contents/MacOS/soffice --version';
  const server = `${process.execPath} --import tsx ${resolve(baseline, 'server/index.js')}`;
  const base = `10 1 10 runner\n20 10 20 playwright\n21 20 20 worker\n22 21 20 ${chrome}\n24 23 20 ${helper}\n23 22 20 ${helper}\n25 22 20 ${updater}\n30 10 30 runtime\n31 30 30 ${server}\n32 31 30 ${office}`;
  const options = { operatorHome, playwrightRoots: [20], browserTmpRoots: ['/owned/tmp'], runtimeRoots: [{ pid: 30, home }], updaterRealpath: path => path, sofficeRealpath: path => path,
    processHOME: new Map([[31, home]]), unreadable: new Set([22, 23, 24, 32]), alternativeEvidence: [] };
  const rows = processTree(base, 10);
  assert.deepEqual(environmentTargets(rows).map(row => row.pid), [31, 32], 'only server Node and soffice require environment snapshots');
  assert.deepEqual(forbiddenProcesses(rows, options), []);
  assert.deepEqual(options.alternativeEvidence.map(row => row.pid), [32]);
  assert(options.alternativeEvidence.every(row => row.environment === 'unreadable' && row.alternative));
  assert.equal(options.alternativeEvidence.find(row => row.pid === 32).serverAncestorPid, 31);
  for (const invalid of [base.replace('32 31 30', '32 30 30'), base.replace('31 30 30', '31 21 20')]) {
    assert(forbiddenProcesses(processTree(invalid, 10), { ...options, alternativeEvidence: [] }).some(row => row.pid === 32));
  }
  for (const invalid of [base.replace('22 21 20', '22 30 30'), base.replace('/owned/tmp/', '/operator/'), base.replace(' --remote-debugging-pipe', ''), base.replace('GoogleUpdater --wake', 'GoogleUpdater --install')]) {
    assert(forbiddenProcesses(processTree(invalid, 10), { ...options, alternativeEvidence: [] }).length);
  }
  for (const updaterHOME of [undefined, home, '/wrong', operatorHome]) {
    const processHOME = new Map(options.processHOME);
    if (updaterHOME !== undefined) processHOME.set(25, updaterHOME);
    for (const unreadableUpdater of [false, true]) {
      const unreadable = new Set(options.unreadable);
      if (unreadableUpdater) unreadable.add(25);
      assert.deepEqual(forbiddenProcesses(rows, { ...options, unreadable, processHOME, alternativeEvidence: [] }), [],
        'updater missing/wrong HOME or unreadable environment does not affect identity approval');
    }
  }
  for (const officeHOME of [undefined, operatorHome, '/wrong']) {
    const unreadable = new Set(options.unreadable); unreadable.delete(32);
    const processHOME = new Map(options.processHOME);
    if (officeHOME !== undefined) processHOME.set(32, officeHOME);
    assert.deepEqual(forbiddenProcesses(rows, { ...options, unreadable, processHOME, alternativeEvidence: [] }).map(row => row.pid), [32],
      'soffice without the isolated server HOME or an unreadable-environment alternative FAIL');
  }
  const remembered = new Map([[23, helper]]);
  const orphan = `10 1 10 runner\n23 1 20 ${helper}\n24 23 20 ${helper}`;
  assert(forbiddenProcesses(processTree(orphan, 10, [20]), { ...options, approvedChrome: remembered,
    unreadable: new Set([24]), processHOME: new Map([[23, operatorHome]]), alternativeEvidence: [] }).some(row => row.pid === 24), 'unreadable helper cannot use only a cached orphan');
});

test('round nine/eleven: audit checks every spawned Node role HOME, fails before lsof on unreadable Node, records Chrome identity without secrets', async () => {
  const home = '/owned/ready/home-os', mockHome = '/owned/mock/home-os';
  const server = `${process.execPath} --import tsx ${resolve(baseline, 'server/index.js')}`;
  const chrome = `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome ${requiredChromeFlags.join(' ')} --remote-debugging-pipe --user-data-dir=/owned/tmp/playwright_chromiumdev_profile-abc`;
  const snapshot = `10 1 10 runner\n20 10 20 ${process.execPath} /owned/playwright-cli.js\n21 20 20 ${process.execPath} /owned/setup-worker.js\n22 21 20 ${chrome}\n30 10 30 ${process.execPath} /owned/real-launch.mjs\n31 30 30 ${server}\n32 30 30 ${process.execPath} --import tsx /owned/pilotdeck.ts server\n33 30 30 ${process.execPath} /owned/real-vite.mjs\n40 10 40 ${process.execPath} /owned/real-mock.mjs`;
  const processes = processTree(snapshot, 10), roots = [{ pid: 20, home }, { pid: 30, home }, { pid: 40, home: mockHome }];
  assert.deepEqual(environmentTargets(processes, { nodeRoots: roots }).map(row => row.pid), [20, 21, 30, 31, 32, 33, 40]);
  for (const badPid of [20, 21, 30, 31, 32, 33, 40]) for (const failure of ['unreadable', 'wrong-home']) {
    let lsof = 0, fatal = 0;
    await assert.rejects(startTCPAudit({ rootPid: 10, nodeRoots: () => roots, runtimeRoots: () => [{ pid: 30, home }],
      playwrightRoots: () => [20], browserTmpRoots: () => ['/owned/tmp'],
      run: async (file, args) => {
        if (file === 'lsof') { lsof++; return ''; }
        if (!args.includes('-E')) return snapshot;
        return processes.filter(row => row.pid !== 10).map(row => `${row.pid} ${row.ppid} ${row.pgid} ${row.command}`
          + (row.pid === 22 || row.pid === badPid && failure === 'unreadable' ? '' : ` HOME=${row.pid === badPid ? userInfo().homedir : row.pid === 40 ? mockHome : home} SECRET=do-not-record`)).join('\n');
      }, onFatal: () => fatal++, record: async () => {} }), /FAIL:/, `${badPid}: ${failure}`);
    assert.equal(fatal, 1); assert.equal(lsof, 0);
  }
  const records = [];
  const audit = await startTCPAudit({ rootPid: 10, nodeRoots: () => roots, runtimeRoots: () => [{ pid: 30, home }],
    playwrightRoots: () => [20], browserTmpRoots: () => ['/owned/tmp'], intervalMs: 60_000,
    run: async (file, args) => file === 'lsof' ? '' : !args.includes('-E') ? snapshot : processes.filter(row => row.pid !== 10)
      .map(row => `${row.pid} ${row.ppid} ${row.pgid} ${row.command}` + (row.pid === 22 ? '' : ` HOME=${row.pid === 40 ? mockHome : home} SECRET=do-not-record`)).join('\n'),
    record: async row => records.push(row) });
  await audit.stop();
  assert(records.every(row => row.status === 'PASS' && row.environmentEvidence.length === 7));
  assert(!records[0].environmentEvidence.some(row => row.pid === 22));
  assert.equal(records[0].browserEvidence.find(row => row.pid === 22).environment, 'not-checked');
  assert.equal(records[0].browserEvidence.find(row => row.pid === 22).playwrightAncestorPid, 20);
  assert(!JSON.stringify(records).includes('do-not-record'));
});

test('round eleven: Chrome/helper/crashpad skip HOME; changing helper argv passes only with a current verified main/profile', async () => {
  const home = '/owned/ready/home-os', profile = '/owned/tmp/playwright_chromiumdev_profile-live';
  const chrome = `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome ${requiredChromeFlags.join(' ')} --remote-debugging-pipe --user-data-dir=${profile}`;
  const bundle = '/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/142.0';
  const helper = `${bundle}/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)`;
  const crashpad = `${bundle}/chrome_crashpad_handler --monitor-self`;
  let title = `--type=renderer --user-data-dir=${profile} --renderer-client-id=1`, environmentCalls = 0;
  const snapshot = () => `10 1 10 runner\n24 23 20 ${crashpad}\n23 22 20 ${helper} ${title}\n22 20 20 ${chrome}\n20 10 20 ${process.execPath} /owned/playwright-cli.js`;
  const options = { playwrightRoots: [20], browserTmpRoots: ['/owned/tmp'], nodeRoots: [{ pid: 20, home }] };
  assert.deepEqual(environmentTargets(processTree(snapshot(), 10), options).map(row => row.pid), [20]);
  for (const processHOME of [new Map(), new Map([[22, '/wrong'], [23, '/wrong'], [24, '/wrong']])]) {
    assert.deepEqual(forbiddenProcesses(processTree(snapshot(), 10), { ...options, processHOME }), []);
  }
  const records = [];
  const audit = await startTCPAudit({ rootPid: 10, playwrightRoots: () => options.playwrightRoots,
    browserTmpRoots: () => options.browserTmpRoots, nodeRoots: () => options.nodeRoots, intervalMs: 60_000,
    run: async (file, args) => {
      if (file === 'lsof') return '';
      if (!args.includes('-E')) return snapshot();
      environmentCalls++;
      assert.equal(args[args.indexOf('-p') + 1], '20', 'Chrome series must never reach ps -E');
      // Simulate a title rewrite between ordinary ps and ps -E as in old13.
      title = `--type=renderer --user-data-dir=${profile} --renderer-client-id=2 --field-trial-handle=changed`;
      return `20 10 20 ${process.execPath} /owned/playwright-cli.js HOME=${home} SECRET=do-not-record`;
    }, record: async row => records.push(row) });
  await audit.checkpoint('changed-helper-title');
  await audit.stop();
  assert.equal(environmentCalls, 3);
  assert.notEqual(records[0].processes.find(row => row.pid === 23).command, records[1].processes.find(row => row.pid === 23).command);
  assert(records.every(row => row.status === 'PASS' && row.environmentEvidence.length === 1));
  assert(records.every(row => row.browserEvidence.length === 3 && row.browserEvidence.every(proof =>
    proof.environment === 'not-checked' && proof.chromeAncestorPid === 22 && proof.userDataDir === profile)));
  assert(!JSON.stringify(records).includes('do-not-record'));
  // Even a remembered PID/argv and readable HOME cannot replace a current chain.
  const cached = { ...options, approvedChrome: new Map([[22, chrome], [23, `${helper} ${title}`]]),
    processHOME: new Map([[22, userInfo().homedir], [23, userInfo().homedir]]) };
  for (const invalid of [snapshot().replace('23 22 20', '23 1 20'), snapshot().replace('22 20 20', '22 1 20'),
    snapshot().replace(chrome, chrome.replace(` --user-data-dir=${profile}`, '')),
    snapshot().replace(chrome, chrome.replace(profile, '/operator/profile'))]) {
    for (const deferEnvironment of [true, false]) {
      assert(forbiddenProcesses(processTree(invalid, 10, [20]), { ...cached, deferEnvironment }).some(row => row.pid === 23),
        'broken ancestry or missing/non-temporary main profile must reject helper in both audit stages');
    }
  }
  for (const main of [chrome.replace(` --user-data-dir=${profile}`, ''), chrome.replace(profile, '/operator/profile')]) {
    assert(forbiddenProcesses(processTree(snapshot().replace(chrome, main), 10), options).some(row => row.pid === 22),
      'main Chrome without a temporary profile FAIL');
  }
  assert(forbiddenProcesses(processTree(snapshot().replace(`${helper} ${title}`, `${helper} ${title.replace(profile, profile + '-other')}`), 10), options)
    .some(row => row.pid === 23), 'helper with an explicit different profile FAIL');
});

test('round ten: R14 installed updater exact wake modes require canonical identity independently of parent', () => {
  const operatorHome = userInfo().homedir;
  const executable = `${operatorHome}/Library/Application Support/Google/GoogleUpdater/157.0.8089.0/GoogleUpdater.app/Contents/MacOS/GoogleUpdater`;
  const chrome = `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome ${requiredChromeFlags.join(' ')} --remote-debugging-pipe --user-data-dir=/owned/tmp/playwright_chromiumdev_profile-new`;
  const base = `10 1 10 runner\n20 10 20 playwright\n21 20 20 worker\n22 21 22 ${chrome}`;
  const options = { playwrightRoots: [20], browserTmpRoots: ['/owned/tmp'], operatorHome, updaterRealpath: path => path,
    processHOME: new Map([[22, operatorHome], [23, operatorHome], [24, operatorHome]]) };
  const rejected = (extra, overrides = {}, snapshot = base) => forbiddenProcesses(processTree(snapshot + extra, 10, [20]), { ...options, ...overrides }).map(row => row.pid);
  assert.deepEqual(rejected(`\n23 22 23 ${executable} --wake-all`), [], 'wake-all from main Chrome PASS');
  assert.deepEqual(rejected(`\n23 22 23 ${executable} --wake`), [], 'wake from main Chrome PASS');
  const childrenFirst = `\n24 23 24 ${executable} --wake\n23 22 23 ${executable} --wake-all`;
  assert.deepEqual(rejected(childrenFirst), [], 'wake child precedes wake-all parent in snapshot; new groups PASS');
  assert.deepEqual(rejected(`\n23 21 23 ${executable} --wake`), [], 'R14: non-Chrome parent PASS');
  const withoutChrome = '10 1 10 runner\n21 10 20 worker';
  for (const deferEnvironment of [true, false]) for (const mode of ['--wake', '--wake-all']) {
    assert.deepEqual(rejected(`\n23 21 23 ${executable} ${mode}`, { playwrightRoots: [], deferEnvironment }, withoutChrome), [],
      `R14: ${mode} without Chrome or Playwright ancestry PASS in both audit stages`);
  }
  for (const args of ['--wake-all --extra', '--install', '--register', '--uninstall', '--wake --wake-all', '--wake-all=true', '--wake --extra', '--WAKE', '', ' --wake', '--wake unknown', '\t--wake', '--wake-all --wake-all', '--wake\t--extra']) {
    for (const deferEnvironment of [true, false]) {
      assert.deepEqual(rejected(`\n23 21 23 ${executable} ${args}`, { playwrightRoots: [], deferEnvironment }, withoutChrome), [23], args);
    }
  }
  assert.deepEqual(rejected(childrenFirst.replace('--wake-all', '--install')), [23], 'install parent FAIL; independently valid wake child PASS');
  assert.deepEqual(rejected(childrenFirst.replace('--wake-all', '--wake')), [], 'R14: wake parent and wake child independently PASS');
  const otherVersion = executable.replace('157.0.8089.0', '158.0');
  for (const deferEnvironment of [true, false]) {
    assert.deepEqual(rejected(childrenFirst.replace('24 23 24 ' + executable, '24 23 24 ' + otherVersion),
      { deferEnvironment, updaterRealpath: path => path === otherVersion ? executable : path }), [24], 'argv and realpath version directories differ: FAIL in both stages');
    assert.deepEqual(rejected(childrenFirst, { deferEnvironment, updaterRealpath: () => '/outside/GoogleUpdater' }), [24, 23], 'symlink escape FAIL in both stages');
    assert.deepEqual(rejected(childrenFirst, { deferEnvironment, updaterRealpath: () => { throw new Error('missing'); } }), [24, 23], 'realpath failure FAIL in both stages');
    for (const processHOME of [new Map(), new Map([[23, '/wrong'], [24, '/wrong']]), new Map([[23, '/wrong'], [24, operatorHome]]), new Map([[23, operatorHome], [24, '/wrong']])]) {
      for (const unreadable of [new Set(), new Set([23, 24])]) {
        assert.deepEqual(rejected(childrenFirst, { deferEnvironment, processHOME, unreadable }), [],
          'updater/child missing or wrong HOME and unreadable environment PASS in both stages');
      }
    }
  }
  for (const path of [executable.replace('/157.0.8089.0/', '/../'), executable.replace('/157.0.8089.0/', '/157.0.8089.0/../157.0.8089.0/'),
    executable.replace('/157.0.8089.0/', '//157.0.8089.0/'), executable.replace(operatorHome + '/', '/Users/other/'),
    executable.replace('/GoogleUpdater/157.0.8089.0/', '/GoogleUpdater-extra/157.0.8089.0/'),
    executable.replace('/157.0.8089.0/', '/'), executable.replace('/157.0.8089.0/', '/157.0.8089.0/nested/'),
    executable.replace('/157.0.8089.0/', '/157.0 8089.0/'), executable.replace('/GoogleUpdater.app/', '/GoogleUpdater-other.app/')]) {
    for (const deferEnvironment of [true, false]) for (const mode of ['--wake', '--wake-all']) {
      assert.deepEqual(rejected(`\n23 21 23 ${path} ${mode}`, { playwrightRoots: [], deferEnvironment }, withoutChrome), [23],
        `noncanonical/nonoperator updater path FAIL: ${path} ${mode}`);
    }
  }
  assert.deepEqual(rejected(childrenFirst, { approvedChrome: new Map([[22, chrome]]) }, base.replace('22 21 22', '22 1 20')), [22],
    'cached reparented Chrome still FAIL; R14 updater identity independently PASS');
  // Chrome detaches wake-all and launchd adopts it; it stays visible only via its known group.
  const adopted = extra => forbiddenProcesses(processTree(base + extra, 10, [20, 23]), options).map(row => row.pid);
  assert.deepEqual(adopted(`\n23 1 23 ${executable} --wake-all\n24 23 23 ${executable} --wake`), [], 'launchd-adopted wake-all and its wake child PASS');
  assert.deepEqual(adopted(`\n23 1 23 ${executable} --wake`), [], 'R14: adopted bare wake PASS');
  assert.deepEqual(adopted(`\n23 1 23 ${executable} --wake-all --extra`), [23], 'adopted wake-all with extra args FAIL');
  const helper = '/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/155.0/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper --type=utility';
  assert.deepEqual(rejected(`\n23 22 23 ${helper}\n24 23 24 ${executable} --wake`), [], 'R14: updater with verified helper parent PASS');
  assert.deepEqual(rejected(`\n23 22 23 ${executable.replace('/GoogleUpdater.app/Contents/MacOS', '')} --wake`), [23], 'old short path FAIL');
});

test('round ten: fresh Chrome and R14 independent updater identities pass both audit stages; detached groups remain audited', async () => {
  const operatorHome = userInfo().homedir, home = '/owned/run/home-os';
  const executable = `${operatorHome}/Library/Application Support/Google/GoogleUpdater/157.0.8089.0/GoogleUpdater.app/Contents/MacOS/GoogleUpdater`;
  const chrome = `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome ${requiredChromeFlags.join(' ')} --remote-debugging-pipe --user-data-dir=/owned/tmp/playwright_chromiumdev_profile-new`;
  let invalid = false, parent = 'chrome', fatalGroups, lsof = 0, environmentCalls = 0;
  let childMode = '--wake', parentMode = '--wake-all';
  const records = [];
  const audit = await startTCPAudit({ rootPid: 10, intervalMs: 60_000, playwrightRoots: () => [20], browserTmpRoots: () => ['/owned/tmp'],
    nodeRoots: () => [{ pid: 20, home }], updaterRealpath: path => path,
    run: async (file, args) => {
      if (file === 'lsof') { lsof++; assert(args.includes('10,24,23,22,20')); return ''; }
      const parentPid = parent === 'chrome' ? 22 : parent === 'worker' ? 20 : 1;
      const rows = [`10 1 10 runner`, `24 23 24 ${executable} ${childMode}`, `23 ${parentPid} 23 ${executable} ${parentMode}${invalid ? ' --extra' : ''}`,
        `22 20 22 ${chrome}`, `20 10 20 ${process.execPath} /owned/playwright-cli.js`];
      if (args.includes('-E')) {
        environmentCalls++;
        assert.equal(args[args.indexOf('-p') + 1], '20', 'ps -E targets only Node; updater/Chrome must not be queried');
        return rows.at(-1) + ` HOME=${home} SECRET=do-not-record`;
      }
      return rows.join('\n');
    }, onFatal: (_error, groups) => { fatalGroups = groups; }, record: async row => records.push(row) });
  try {
    assert.equal(records[0].status, 'PASS');
    assert.deepEqual(records[0].environmentEvidence.map(row => row.pid), [20], 'updater has no environment evidence');
    assert.deepEqual(records[0].processes.filter(row => [23, 24].includes(row.pid)).map(row => [row.pid, row.ppid, row.command]),
      [[24, 23, `${executable} --wake`], [23, 22, `${executable} --wake-all`]], 'current updater identity is retained in process evidence');
    for (parent of ['worker', 'launchd']) {
      [childMode, parentMode] = [parentMode, childMode];
      await audit.checkpoint(`R14-${parent}-parent`);
      assert.equal(records.at(-1).status, 'PASS');
      assert.deepEqual(records.at(-1).environmentEvidence.map(row => row.pid), [20], 'adoption and changed wake argv do not create updater environment targets');
      assert.deepEqual(records.at(-1).processes.filter(row => [23, 24].includes(row.pid)).map(row => [row.pid, row.ppid, row.command]),
        [[24, 23, `${executable} ${childMode}`], [23, parent === 'worker' ? 20 : 1, `${executable} ${parentMode}`]],
        'each audit uses current updater argv and parent identity, including detached groups');
    }
    assert(!JSON.stringify(records).includes('do-not-record'));
    invalid = true;
    await assert.rejects(audit.checkpoint('extra-updater-argument'), /FAIL:.*forbidden process/);
    assert.equal(lsof, 3, 'structural failure before lsof');
    assert.equal(environmentCalls, 3, 'structural failure before environment snapshot');
    assert(fatalGroups.includes(23) && fatalGroups.includes(24), 'both updater groups remain in fatal cleanup');
    assert.deepEqual(records.at(-1).forbiddenProcesses.map(row => row.pid), [23], 'only updater with extra argv FAILs');
    await assert.rejects(audit.stop(), /FAIL:/);
  } finally {
    // An assertion failure must also release the sampling timer.
    await audit.stop().catch(() => {});
  }
});

test('round ten: updater canonicalization checks real files and rejects a symlink outside operator installation', async () => {
  const directory = await mkdtemp(resolve(root, '_tmp/round10-updater-fixture-'));
  try {
    // mkdtemp path is canonical inside this worktree, including on macOS.
    const operatorHome = await realpath(directory);
    const executable = resolve(operatorHome, 'Library/Application Support/Google/GoogleUpdater/157.0/GoogleUpdater.app/Contents/MacOS/GoogleUpdater');
    await mkdir(dirname(executable), { recursive: true }); await writeFile(executable, 'fixture');
    const chrome = `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome ${requiredChromeFlags.join(' ')} --remote-debugging-pipe --user-data-dir=/owned/tmp/playwright_chromiumdev_profile-new`;
    const rows = processTree(`10 1 10 runner\n20 10 20 playwright\n22 20 22 ${chrome}\n23 22 23 ${executable} --wake-all`, 10);
    const options = { operatorHome, playwrightRoots: [20], browserTmpRoots: ['/owned/tmp'], unreadable: new Set([22, 23]) };
    assert.deepEqual(forbiddenProcesses(rows, options), []);
    await rm(executable); await writeFile(resolve(operatorHome, 'outside'), 'fixture');
    await symlink(resolve(operatorHome, 'outside'), executable);
    assert.deepEqual(forbiddenProcesses(rows, options).map(row => row.pid), [23]);
  } finally { await rm(directory, { recursive: true }); }
});

test('round nine: coordinator CLT Git candidate remains ordered and validated as a real binary', async () => {
  assert.equal(realGitCandidates[2], '/Library/Developer/CommandLineTools/usr/bin/git');
  const probed = [], executed = [];
  const evidence = await discoverRealGit(realExecutionEnv({}, { output: '/owned/out', runDir: '/owned/run', mode: 'ready' }), {
    canonical: async candidate => { probed.push(candidate); if (candidate !== realGitCandidates[2]) throw Object.assign(new Error('missing'), { code: 'ENOENT' }); return candidate; },
    executable: async target => executed.push(target), version: async () => 'git version 2.50.1 (Apple Git-155)' });
  assert.deepEqual(probed, realGitCandidates); assert.deepEqual(executed, [realGitCandidates[2]]);
  assert.equal(evidence.realpath, realGitCandidates[2]);
});

test('round nine: frozen LibreOffice probe and conversion execFile options inherit the tested server environment (no soffice execution)', async () => {
  const calls = [];
  const probe = await frozenFunctions('server/services/officePreview.js', {
    execFileAsync: async (command, args, options) => { calls.push({ command, args, options }); return { stdout: 'LibreOffice fixture' }; },
  }, ['probeLibreOfficeCandidate']);
  await probe.probeLibreOfficeCandidate('/Applications/LibreOffice.app/Contents/MacOS/soffice');
  assert.deepEqual(calls, [{ command: '/Applications/LibreOffice.app/Contents/MacOS/soffice', args: ['--version'], options: { timeout: 5000, windowsHide: true } }]);
  const ts = requireUI('typescript'), source = await readFile(resolve(baseline, 'server/services/officePreview.js'), 'utf8');
  const ast = ts.createSourceFile('officePreview.js', source, ts.ScriptTarget.Latest, true), options = [];
  function visit(node) {
    if (ts.isCallExpression(node) && node.expression.getText(ast) === 'execFileAsync') {
      assert(ts.isObjectLiteralExpression(node.arguments[2]));
      assert(node.arguments[2].properties.every(property => ts.isPropertyAssignment(property) && property.name.getText(ast) !== 'env'));
      options.push(node.arguments[2].getText(ast));
    }
    ts.forEachChild(node, visit);
  }
  visit(ast); assert.equal(options.length, 2, 'both --version probe and conversion inherit env');
});

test('round twelve: unreadable argv resolves current executable and applies opener/developer/application rules', async () => {
  const base = '10 1 10 runner\n20 10 20 (hidden)';
  const rejected = async (command, path) => {
    const rows = await resolveUnreadableProcesses(processTree(base.replace('(hidden)', command), 10), {
      run: async (file, args) => {
        assert.equal(file, 'ps');
        assert.deepEqual(args, ['-ww', '-p', '20', '-o', 'pid=,ppid=,pgid=,stat=,comm=']);
        return `20 10 20 S ${path}`;
      }, executableRealpath: path => path });
    assert.equal(rows[1].command, command, 'original argv is retained in evidence');
    assert.equal(rows[1].executableObservation.status, 'resolved');
    return forbiddenProcesses(rows).map(row => row.pid);
  };
  for (const [command, path] of [['(open)', '/usr/bin/open'], ['(osascript)', '/usr/bin/osascript'],
    ['(hidden)', '/usr/bin/xdg-open'], ['(hidden)', '/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild'],
    ['(hidden)', '/usr/bin/xcrun'], ['(hidden)', '/Applications/Other.app/Contents/MacOS/Other'],
    ['(open)', '/usr/bin/true']]) assert.deepEqual(await rejected(command, path), [20], `${command}: ${path}`);
  assert.deepEqual(await rejected('(profiles)', '/usr/bin/profiles'), []);
  assert.deepEqual(forbiddenProcesses(processTree(base, 10)).map(row => row.pid), [20], 'missing executable evidence fails closed');
});

test('round twelve: hidden Chrome child requires current verified ancestry and canonical bundle path', async () => {
  const chrome = `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome ${requiredChromeFlags.join(' ')} --remote-debugging-pipe --user-data-dir=/owned/tmp/playwright_chromiumdev_profile-abc`;
  const bundlePath = '/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/155.0/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper';
  const base = `10 1 10 runner\n20 10 20 playwright\n22 20 20 ${chrome}\n23 22 20 (launcher)`;
  const options = { playwrightRoots: [20], browserTmpRoots: ['/owned/tmp'] };
  const rowsFor = async (path, canonical = path, snapshot = base) => resolveUnreadableProcesses(processTree(snapshot, 10), {
    run: async () => `23 ${snapshot.includes('23 22 20') ? 22 : 20} 20 S ${path}`, executableRealpath: () => canonical });
  const rows = await rowsFor(bundlePath), browserEvidence = [];
  assert.deepEqual(forbiddenProcesses(rows, { ...options, browserEvidence }), []);
  assert.equal(browserEvidence.find(row => row.pid === 23).realpath, bundlePath);
  assert.equal(browserEvidence.find(row => row.pid === 23).chromeAncestorPid, 22);
  assert.deepEqual(environmentTargets(rows), [], 'hidden bundle child stays outside Chrome HOME checking');
  assert.deepEqual(forbiddenProcesses(await rowsFor('/Applications/Google Chrome.app/Contents/Helpers/launcher'), options), []);
  for (const [path, canonical] of [['/usr/bin/profiles', '/usr/bin/profiles'], ['/tmp/launcher', '/tmp/launcher'],
    [bundlePath, '/tmp/launcher'], [bundlePath.replace('/Google Chrome.app/', '/Google Chrome.app-evil/'), bundlePath.replace('/Google Chrome.app/', '/Google Chrome.app-evil/')]]) {
    for (const deferEnvironment of [false, true]) assert(forbiddenProcesses(await rowsFor(path, canonical), { ...options, deferEnvironment }).some(row => row.pid === 23));
  }
  assert(forbiddenProcesses(await rowsFor(bundlePath, bundlePath, base.replace('23 22 20', '23 20 20')), options).some(row => row.pid === 23));
  assert(forbiddenProcesses(await rowsFor(bundlePath, bundlePath, base.replace(' --remote-debugging-pipe', '')), options).some(row => row.pid === 23));
  const userUpdater = '/Users/operator/Library/Application Support/Google/GoogleUpdater/155.0/GoogleUpdater.app/Contents/MacOS/GoogleUpdater';
  assert(forbiddenProcesses(await rowsFor(userUpdater), { ...options, operatorHome: '/Users/operator', updaterRealpath: path => path }).some(row => row.pid === 23), 'hidden updater argv cannot establish exact wake mode');
});

test('round twelve: exited/defunct/zombie pass with evidence; live unknown, identity races and probe errors fail', async () => {
  const base = '10 1 10 runner\n20 10 20 (hidden)';
  for (const [raw, status, denied] of [['', 'exited', false], ['20 10 20 Z', 'zombie', false],
    ['20 10 20 S <defunct>', 'zombie', false], ['20 10 20 S', 'unavailable', true],
    ['20 10 20 S hidden', 'unavailable', true], ['20 99 20 S /usr/bin/true', 'unavailable', true],
    ['20 10 99 S /usr/bin/true', 'unavailable', true], ['20 10 20 S /tmp/../usr/bin/true', 'unavailable', true],
    ['20 10 20 S /usr/bin/true\n20 10 20 S /usr/bin/true', 'unavailable', true]]) {
    const rows = await resolveUnreadableProcesses(processTree(base, 10), { run: async () => raw, executableRealpath: path => path });
    assert.equal(rows[1].executableObservation.status, status, raw);
    assert.equal(forbiddenProcesses(rows).length > 0, denied, raw);
  }
  // A <defunct> marker only triggers a probe: stat Z passes, a live process does not.
  const defunct = await resolveUnreadableProcesses(processTree(base.replace('(hidden)', '(open) <defunct>'), 10), { run: async () => '20 10 20 Z', executableRealpath: path => path });
  assert.deepEqual(forbiddenProcesses(defunct), []);
  assert.equal(defunct[1].executableObservation.status, 'zombie');
  const liveDefunct = await resolveUnreadableProcesses(processTree(base.replace('(hidden)', '(open) <defunct>'), 10), { run: async () => '20 10 20 S /usr/bin/open', executableRealpath: path => path });
  assert.equal(forbiddenProcesses(liveDefunct)[0]?.pid, 20, 'live process with <defunct> in argv FAIL');
  const failed = await resolveUnreadableProcesses(processTree(base, 10), { run: async () => { throw new Error('timeout'); } });
  assert.equal(forbiddenProcesses(failed)[0].pid, 20);
  for (const live of [false, true]) {
    let calls = 0;
    const rows = await resolveUnreadableProcesses(processTree(base, 10), { run: async () => ++calls === 1 || live ? '20 10 20 S /missing/executable' : '',
      executableRealpath: () => { throw new Error('ENOENT'); } });
    assert.equal(calls, 2); assert.equal(forbiddenProcesses(rows).length > 0, live, 'canonicalization race needs fresh exit evidence');
  }
});

test('round twelve: audit records comm evidence, re-probes every sample and stops before TCP on new forbidden path', async () => {
  const records = []; let path = '/usr/bin/profiles', tcp = 0, fatal = 0;
  const audit = await startTCPAudit({ rootPid: 10, intervalMs: 60_000, executableRealpath: path => path,
    run: async (file, args) => {
      if (file === 'lsof') { tcp++; return ''; }
      return args.includes('-p') ? `20 10 20 S ${path}` : '10 1 10 runner\n20 10 20 (profiles)';
    }, record: async row => records.push(row), onFatal: () => fatal++ });
  assert.equal(records[0].processes[1].executableObservation.path, path);
  path = '/usr/bin/open';
  await assert.rejects(audit.checkpoint('changed-comm-path'), /forbidden process/);
  assert.equal(fatal, 1); assert.equal(tcp, 1);
  assert.equal(records.at(-1).forbiddenProcesses[0].executableObservation.path, path);
  await assert.rejects(audit.stop(), /forbidden process/);
  for (const [raw, denied] of [['', false], ['20 10 20 Z', false], ['20 10 20 S', true]]) {
    const evidence = []; let tcp = 0;
    const begin = () => startTCPAudit({ rootPid: 10, intervalMs: 60_000, run: async (file, args) => {
      if (file === 'lsof') { tcp++; return ''; }
      return args.includes('-p') ? raw : '10 1 10 runner\n20 10 20 (hidden)';
    }, record: async row => evidence.push(row) });
    if (denied) { await assert.rejects(begin(), /forbidden process/); assert.equal(tcp, 0); }
    else { const audit = await begin(); await audit.stop(); assert.equal(tcp, 2); }
    assert.equal(evidence[0].status, denied ? 'FAIL' : 'PASS');
    assert(evidence[0].processes[1].executableObservation);
  }
});

test('round twelve: soffice exemption pins executable and realpath in addition to isolated server/HOME', () => {
  const home = '/owned/home', office = '/Applications/LibreOffice.app/Contents/MacOS/soffice';
  const server = `${process.execPath} --import tsx ${resolve(baseline, 'server/index.js')}`;
  const base = `10 1 10 runner\n30 10 30 supervisor\n31 30 30 ${server}\n32 31 30 `;
  const options = { runtimeRoots: [{ pid: 30, home }], processHOME: new Map([[31, home], [32, home]]), sofficeRealpath: path => path };
  assert.deepEqual(forbiddenProcesses(processTree(base + office + ' --version', 10), options), []);
  for (const executable of ['soffice', '/tmp/soffice', '/Applications/Other.app/Contents/MacOS/soffice', office + '.bin']) {
    assert.equal(forbiddenProcesses(processTree(base + executable + ' --version', 10), options)[0].pid, 32);
  }
  for (const sofficeRealpath of [() => '/tmp/soffice', () => { throw new Error('unobservable'); }]) {
    assert.equal(forbiddenProcesses(processTree(base + office + ' --version', 10), { ...options, sofficeRealpath })[0].pid, 32);
  }
});

test('round thirteen: Node environment races recover, exit, exhaust or rebind title within 500ms (injected ps)', async () => {
  const row = { pid: 21, ppid: 20, pgid: 20, command: `${process.execPath} /owned/workerProcessEntry.js` };
  const line = command => `21 20 20 ${command}`;
  const home = '/owned/home-os';
  for (const scenario of ['second-readable', 'last-retry-readable', 'exited', 'exit-between-pair', 'unreadable', 'title-changed']) {
    const processes = [{ ...row }], observations = [], calls = []; let elapsed = 0, environmentCalls = 0;
    const options = { processes, observations, nodeTargets: [processes[0]], now: () => elapsed,
      delay: async ms => { elapsed += ms; }, run: async (file, args, options) => {
        calls.push(args);
        assert.equal(file, 'ps'); assert.equal(args[args.indexOf('-p') + 1], '21');
        assert(options.timeout > 0 && options.timeout <= 500 - elapsed);
        if (scenario === 'exited' || scenario === 'exit-between-pair' && args.includes('-E')) return '';
        const command = scenario === 'title-changed' ? 'playwright-worker-renamed' : row.command;
        if (args.includes('-E')) environmentCalls++;
        return line(command) + (args.includes('-E') && scenario !== 'unreadable'
          && (scenario !== 'last-retry-readable' || environmentCalls === 3) ? ` HOME=${home} SECRET=do-not-record` : '');
      } };
    const initial = scenario === 'title-changed' ? line('playwright-worker-renamed') + ` HOME=${home}`
      : scenario === 'exited' ? '' : line(row.command);
    const begin = () => retryNodeHomes(initial, [processes[0]], options);
    if (scenario === 'unreadable') {
      await assert.rejects(begin(), /did not expose environment/);
      assert.equal(calls.length, 6, 'at most three fresh args/environment pairs after the initial failure');
    } else {
      const result = await begin();
      if (scenario === 'exited' || scenario === 'exit-between-pair') {
        assert(result.exited.has(21)); assert(!result.homes.has(21));
        assert.equal(observations[0].environment, 'exited-before-environment');
        assert.equal(processes[0].executableObservation.status, 'exited'); assert.equal(calls.length, scenario === 'exited' ? 1 : 2);
      } else {
        assert.equal(result.homes.get(21), home); assert.equal(calls.length, scenario === 'last-retry-readable' ? 6 : 2);
        assert.equal(observations[0].argvRebound, scenario === 'title-changed');
        assert.equal(processes[0].command, scenario === 'title-changed' ? 'playwright-worker-renamed' : row.command);
      }
    }
    assert(elapsed <= 500); assert(!JSON.stringify(observations).includes('do-not-record'));
  }
  // Neither an identity change nor exposed bad HOME is a retryable argv race.
  for (const raw of [line(row.command).replace('21 20 20', '21 99 20'), line(row.command) + ' TMPDIR=/owned/tmp', line(row.command) + ` HOME=${home} HOME=/other`]) {
    await assert.rejects(retryNodeHomes(raw, [row], { nodeTargets: [row], run: async () => assert.fail('must fail before retry') }), /FAIL:/);
  }
  // A slow probe cannot succeed after the deadline, even with a readable HOME.
  let elapsed = 0, probes = 0;
  await assert.rejects(retryNodeHomes(line(row.command), [row], { nodeTargets: [row], now: () => elapsed,
    delay: async ms => { elapsed += ms; }, run: async () => { probes++; elapsed = 501; return line(row.command); } }), /FAIL:/);
  assert.equal(probes, 1);
});

test('round thirteen: integrated audit records Node exit, validates rebound HOME and keeps structural rules (no ps/lsof)', async () => {
  const home = '/owned/home-os', original = `${process.execPath} /owned/workerProcessEntry.js`;
  const snapshot = `10 1 10 runner\n20 10 20 ${process.execPath} /owned/cli.js\n21 20 20 ${original}`;
  for (const scenario of ['second-readable', 'exited', 'unreadable', 'title-changed', 'wrong-home', 'forbidden-title', 'identity-changed', 'query-failed']) {
    const records = []; let envCalls = 0, freshCalls = 0, tcp = 0, fatal = 0;
    const command = scenario === 'title-changed' || scenario === 'wrong-home' ? 'playwright-worker-renamed'
      : scenario === 'forbidden-title' ? '/usr/bin/open /Applications/Other.app' : original;
    const begin = () => startTCPAudit({ rootPid: 10, intervalMs: 60_000, nodeRoots: () => [{ pid: 20, home }],
      onFatal: () => fatal++, record: async row => records.push(row), run: async (file, args, options) => {
        if (file === 'lsof') {
          tcp++;
          if (scenario === 'exited') assert(!args[args.indexOf('-p') + 1].split(',').includes('21'));
          return '';
        }
        if (args.includes('-axo')) return snapshot;
        if (!args.includes('-E')) {
          freshCalls++; assert(options.timeout <= 500);
          if (scenario === 'exited') return '';
          if (scenario === 'query-failed') throw new Error('probe unavailable');
          return `21 ${scenario === 'identity-changed' ? 99 : 20} 20 ${command}`;
        }
        envCalls++;
        if (args[args.indexOf('-p') + 1] === '20,21') return `20 10 20 ${process.execPath} /owned/cli.js HOME=${home}\n21 20 20 `
          + (scenario === 'title-changed' ? `${command} HOME=${home}` : original);
        return `21 20 20 ${command}` + (scenario === 'unreadable' ? '' : ` HOME=${scenario === 'wrong-home' ? '/wrong/home' : home} SECRET=do-not-record`);
      } });
    if (['unreadable', 'wrong-home', 'forbidden-title', 'identity-changed', 'query-failed'].includes(scenario)) {
      await assert.rejects(begin(), /FAIL:|probe unavailable/);
      assert.equal(fatal, 1); assert.equal(tcp, 0);
      if (scenario === 'unreadable') { assert.equal(envCalls, 4); assert.equal(freshCalls, 3); }
    } else {
      const audit = await begin(); await audit.stop();
      assert(records.every(row => row.status === 'PASS'));
      const proof = records[0].environmentEvidence.find(row => row.pid === 21);
      assert.equal(proof.environment, scenario === 'exited' ? 'exited-before-environment' : 'readable');
      if (scenario !== 'exited') { assert.equal(proof.HOME, home); assert.equal(proof.retries, 1); }
    }
    assert(!JSON.stringify(records).includes('do-not-record'));
  }
});

test('coordinator: R14 updater with unreadable "(Google Chrome)" parent passes independently of parent identity or Playwright ancestry', () => {
  const operatorHome = userInfo().homedir;
  const updater = `${operatorHome}/Library/Application Support/Google/GoogleUpdater/157.0.8089.0/GoogleUpdater.app/Contents/MacOS/GoogleUpdater`;
  const chromeBin = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
  const row = (pid, ppid, pgid, command, path) => ({ pid, ppid, pgid, command,
    ...(path ? { executableObservation: { status: 'resolved', path, realpath: path } } : {}) });
  const base = [row(10, 1, 10, 'runner'), row(20, 10, 20, 'playwright'), row(21, 20, 20, 'worker')];
  const options = { playwrightRoots: [20], browserTmpRoots: ['/owned/tmp'], operatorHome, updaterRealpath: p => p,
    deferEnvironment: true };
  const rejected = (rows, overrides = {}) => forbiddenProcesses(rows, { ...options, ...overrides }).map(r => r.pid);
  const wake = [row(23, 22, 23, `${updater} --wake-all`), row(24, 23, 23, `${updater} --wake`)];
  const underPlaywright = [...base, row(22, 21, 22, '(Google Chrome)', chromeBin)];
  const nonChrome = [...base, row(22, 21, 22, '(Google Chrome)', '/tmp/Google Chrome')];
  const noPlaywright = [row(10, 1, 10, 'runner'), row(22, 10, 22, '(Google Chrome)', chromeBin)];
  const exited = [...base, { ...row(22, 21, 22, '(Google Chrome)'), executableObservation: { status: 'exited' } }];
  for (const deferEnvironment of [true, false]) {
    const overrides = { deferEnvironment, unreadable: new Set([23, 24]) };
    for (const [parents, parentFailures] of [[underPlaywright, [22]], [nonChrome, []], [noPlaywright, [22]]]) {
      assert.deepEqual(rejected([...parents, ...wake], overrides), parentFailures, 'R14: both updaters PASS; existing parent rules remain enforced');
      assert.deepEqual(rejected([...parents, row(23, 22, 23, `${updater} --install`)], overrides), [...parentFailures, 23], 'install still FAIL independently of parent');
    }
    assert.deepEqual(rejected([...exited, ...wake], overrides), [], 'R14: exited Chrome parent and both updaters PASS (old16/old18)');
    assert.deepEqual(rejected([...exited, row(23, 22, 23, `${updater} --install`)], overrides), [23], 'exited parent cannot authorize install');
  }
});

test('coordinator: exiting process (ps stat E) with unavailable executable is inactive, live unresolved still FAIL', () => {
  const row = (pid, ppid, pgid, command, obs) => ({ pid, ppid, pgid, command, ...(obs ? { executableObservation: obs } : {}) });
  const base = [row(10, 1, 10, 'runner'), row(20, 10, 20, 'playwright'), row(21, 20, 20, 'worker')];
  const options = { playwrightRoots: [20], browserTmpRoots: ['/owned/tmp'], deferEnvironment: true };
  const ids = rows => forbiddenProcesses(rows, options).map(r => r.pid).filter(pid => pid >= 30);
  assert.deepEqual(ids([...base, row(30, 21, 21, '(Google Chrome He)', { status: 'unavailable', stat: '?E' })]), [], 'exiting PASS');
  assert.deepEqual(ids([...base, row(30, 21, 21, '(Google Chrome He)', { status: 'unavailable', stat: 'S' })]), [30], 'live unresolved FAIL');
});

test('coordinator: only a bare "<defunct>" command is a zombie; argv containing it still passes the forbidden rules', () => {
  const row = (pid, ppid, pgid, command) => ({ pid, ppid, pgid, command });
  const base = [row(10, 1, 10, 'runner'), row(20, 10, 20, 'playwright')];
  const ids = rows => forbiddenProcesses(rows, { playwrightRoots: [20], deferEnvironment: true }).map(r => r.pid).filter(pid => pid >= 30);
  assert.deepEqual(ids([...base, { ...row(30, 20, 20, '<defunct>'), executableObservation: { status: 'zombie', stat: 'Z' } }]), [], 'probed zombie PASS');
  assert.deepEqual(ids([...base, row(30, 20, 20, 'open http://127.0.0.1:3010 <defunct>')]), [30], 'open with defunct-looking arg FAIL');
});

test('coordinator: launchd adoption between environment snapshots is the same process; other identity changes FAIL', () => {
  const home = '/owned/run/ready/home-os';
  const procs = [{ pid: 10, ppid: 1, pgid: 10, command: 'runner' }, { pid: 30, ppid: 20, pgid: 30, command: 'node server.js' }];
  const env = (ppid, pgid) => `10 1 10 runner HOME=${home}\n30 ${ppid} ${pgid} node server.js HOME=${home}`;
  assert.equal(processHomes(env(1, 30), procs).get(30), home, 'adopted (ppid 1, same pgid) PASS');
  assert.throws(() => processHomes(env(21, 30), procs), /identity changed/, 'different non-launchd parent FAIL');
  assert.throws(() => processHomes(env(1, 31), procs), /identity changed/, 'pgid changed FAIL');
});
