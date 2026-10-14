import assert from 'node:assert/strict';
import { promisify } from 'node:util';
import { execFile } from 'node:child_process';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { STREAM_PROTOCOLS, createStream, frame, marker, history, streamErrors, textErrors, readingErrors, followErrors } from './stream.mjs';
import { executionEnv, PROFILES } from './execution-env.mjs';
import { root, baseline, requireUI, origin, historicalInventory } from './env.mjs';
import { reportCases, checkInventory } from './accounting.mjs';
import { createMock, TOKEN } from './mock.mjs';
import { streamContractSelfCheck } from './stream-contract-self-check.mjs';
const exec = promisify(execFile);
export async function streamSelfCheck(check) {
  await streamContractSelfCheck(check);
  await check('final textContent and innerText validators: missing, hidden, duplicate or reordered rounds must FAIL', () => {
    // Offline inputs for the same validator used by STREAM-002's textContent poll.
    // This tests completeness, not browser rendering or content-visibility behavior.
    const paragraphs = Array.from({ length: 400 }, (_, index) => `${marker(index)} paragraph.\n`);
    const textContent = paragraphs.join('');
    assert.deepEqual(textErrors(textContent, 400), []);
    for (const missingIndex of [0, 137, 199, 200, 399]) {
      const missing = paragraphs.filter((_, index) => index !== missingIndex).join('');
      assert.deepEqual(textErrors(missing, 400), ['DOM delta loss, duplication or reordering']);
    }
    assert.deepEqual(textErrors(paragraphs.slice(200).join(''), 400), ['DOM delta loss, duplication or reordering']);
    const rendered = paragraphs.slice(200);
    assert.deepEqual(textErrors(rendered.join(''), 200, 200), []);
    // These retain a perfect textContent but must fail the separate innerText gate.
    for (const bad of ['', paragraphs.join(''), paragraphs.slice(0, 200).join(''),
      rendered.slice(1).join(''), [...rendered, rendered[0]].join(''),
      [rendered[1], rendered[0], ...rendered.slice(2)].join('')]) {
      assert(textErrors(bad, 200, 200).length);
    }
  });
  for (const protocol of STREAM_PROTOCOLS) {
    await check(`${protocol}: HTTP history retains both completed streaming replies, including at complete; missing first reply must FAIL`, async () => {
      let time = 0, pending;
      const mock = createMock({ streamClock: {
        now: () => time,
        schedule: (callback, delay) => { pending = () => { time += delay; callback(); }; return 1; },
        cancel: () => { pending = undefined; },
      } });
      const request = async (url, method = 'GET') => {
        const response = { writeHead(status) { this.status = status; }, end(body) { this.body = JSON.parse(body); } };
        await mock.handleHTTP({ url, method, headers: { authorization: `Bearer ${TOKEN}` } }, response);
        assert.equal(response.status, method === 'POST' && url.includes('action=start') ? 202 : 200);
        return response.body;
      };
      const endpoint = '/api/sessions/s/messages?projectName=probe&projectPath=%2Fprobe';
      const sent = [], completionReads = [];
      const socket = { path: '/ws', readyState: 1, send: raw => {
        const message = JSON.parse(raw); sent.push(message);
        // Read synchronously through handleHTTP at the earliest possible refresh.
        if (message.kind === 'complete') completionReads.push(request(endpoint));
      } };
      const validate = (body, runs) => {
        assert.equal(body.total, body.messages.length);
        assert.equal(body.hasMore, false);
        const answers = body.messages.filter(message => message.role === 'assistant' && runs.includes(message.runId));
        assert.deepEqual(answers.map(message => message.runId), runs);
        assert.equal(new Set(answers.map(message => message.id)).size, runs.length);
        for (const answer of answers) {
          assert.equal(answer.kind, 'text'); assert.equal(answer.sessionId, 's');
          assert.equal(answer.provider, 'pilotdeck');
          assert.equal(answer.content, sent.filter(message => message.kind === 'stream_delta' && message.runId === answer.runId)
            .map(message => message.content).join(''));
        }
        assert.deepEqual(textErrors(answers.map(message => message.content).join('\n'), 200 * runs.length), []);
        if (runs.length === 2) {
          const first = body.messages.indexOf(answers[0]), second = body.messages.indexOf(answers[1]);
          assert(body.messages.slice(first + 1, second).some(message => message.role === 'user' && message.runId === runs[1]),
            'separate user turn must survive the HTTP history projection');
        }
      };
      try {
        mock.handleMessage(socket, JSON.stringify({ type: 'watch-session', sessionId: 's' }));
        const runs = [];
        for (let run = 0; run < 2; run++) {
          runs.push((await request(`/__parity/stream?action=start&scenario=reading&protocol=${protocol}`, 'POST')).runId);
          while (pending) { const callback = pending; pending = undefined; callback(); }
          validate(await completionReads[run], runs);
          validate(await request(endpoint), runs);
        }
        const final = await request(endpoint);
        assert.equal(final.total, protocol === 'timeline' ? 52 : 51); // Native first turn also has its own user anchor.
        const firstReply = final.messages.find(message => message.role === 'assistant' && message.runId === runs[0]);
        const missing = structuredClone(final);
        missing.messages = missing.messages.filter(message => message.id !== firstReply.id);
        missing.total = missing.messages.length;
        assert.throws(() => validate(missing, runs), assert.AssertionError);
        const overwritten = structuredClone(final);
        overwritten.messages.find(message => message.id === firstReply.id).content = '';
        assert.throws(() => validate(overwritten, runs), assert.AssertionError);
        const captured = structuredClone(mock.stats.historyResponses);
        assert.equal(captured.length, 5);
        assert.deepEqual(captured.at(-1).body, final);
        for (const response of captured) {
          assert.equal(mock.stats.requests[response.requestIndex].path, '/api/sessions/s/messages');
          assert.equal(response.url, endpoint);
        }
        await request('/__parity/stream?action=reset', 'POST');
        assert.deepEqual(mock.stats.historyResponses, captured);
        assert.deepEqual((await request(endpoint)).messages, history());
      } finally { mock.close(); }
    });
  }
  await check('two runs emit 0000-0399; overlapping run numbering must FAIL the final DOM validator; reset restarts at 0000', () => {
    let time = 0, pending;
    const deltas = [];
    const controller = createStream({ send: message => { if (message.kind === 'stream_delta') deltas.push(message); return 1; },
      now: () => time, schedule: (fn, delay) => { pending = () => { time += delay; fn(); }; return 1; },
      cancel: () => { pending = undefined; } });
    const drain = () => { while (pending) { const callback = pending; pending = undefined; callback(); } };
    for (const startIndex of [0, 200]) {
      controller.start('reading'); drain();
      assert.equal(controller.state.startIndex, startIndex);
      assert.equal(controller.state.count, 200); assert.equal(controller.state.sent, 200);
      assert.deepEqual(streamErrors(controller.state), []);
      const emitted = deltas.slice(startIndex).map(message => message.content).join('');
      assert(emitted.startsWith(marker(startIndex)));
      assert.equal(controller.messages.at(-1).content, emitted);
    }
    const text = deltas.map(message => message.content).join('');
    assert.deepEqual(textErrors(text, 400), []);
    assert.deepEqual(textErrors(controller.messages.map(message => message.content || '').join('\n'), 400), []);
    assert.equal(new Set(deltas.map(message => message.id)).size, 400);
    // Same 400 deltas, but the second run restarts at 0000 as in old4.
    // A raw regex count would pass; the browser's shared validator must fail.
    const overlapping = text.replace(/F2FRAME(\d{4})END/g, (_, index) => marker(Number(index) % 200));
    assert.equal([...overlapping.matchAll(/F2FRAME\d{4}END/g)].length, 400);
    assert.equal(new Set(overlapping.match(/F2FRAME\d{4}END/g)).size, 200);
    assert.deepEqual(textErrors(overlapping, 400), ['DOM delta loss, duplication or reordering']);
    controller.reset();
    deltas.length = 0;
    controller.start('reading'); drain();
    assert.equal(controller.state.startIndex, 0);
    assert.deepEqual(textErrors(deltas.map(message => message.content).join(''), 200), []);
  });
  await check('frozen grouping only: missing user yields 200, restart boundary yields 400 (not browser innerText)', async () => {
    const ts = requireUI('typescript');
    const source = await readFile(resolve(baseline, 'src/components/chat-v2/processGrouping.ts'), 'utf8');
    // Execute original pure function declarations for these prose-only inputs.
    // No React/store, tool rendering, browser geometry or mocked grouping logic.
    const ast = ts.createSourceFile('processGrouping.ts', source, ts.ScriptTarget.Latest, true);
    const code = ts.transpileModule(ast.statements.filter(ts.isFunctionDeclaration).map(node => node.getText(ast)).join('\n'), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    const grouping = {}; new Function('exports', code)(grouping);
    const fold = messages => {
      const converted = messages.map(message => ({ ...message, type: message.role }));
      const items = converted.map((message, originalIndex) => ({ message, originalIndex,
        beforeRunAttachment: null, afterRunAttachment: null, beforeProcessAttachments: [], afterProcessAttachments: [] }));
      return grouping.foldCompletedTurns(converted, items, false);
    };
    const count = items => [...items.map(item => item.message.content).join('\n').matchAll(/F2FRAME\d{4}END/g)].length;
    const searchTarget = fold(history()).find(item => item.message.content?.includes('F2 searchable lighthouse'));
    assert.equal(searchTarget.message.id, 'answer-21');
    assert.equal(searchTarget.turnTrace, undefined);
    let pending;
    const sent = [];
    const controller = createStream({ send: message => { sent.push(message); return 1; },
      schedule: callback => { pending = callback; return 1; }, cancel: () => { pending = undefined; } });
    const drain = () => { while (pending) { const callback = pending; pending = undefined; callback(); } };
    controller.start('reading'); drain();
    assert.equal(count(fold(controller.messages)), 200);
    const firstAnswer = controller.messages.at(-1);
    const startIndex = sent.length;
    controller.start('reading'); drain();
    const question = sent[startIndex];
    assert.equal(question.kind, 'text'); assert.equal(question.role, 'user');
    assert.equal(sent[startIndex + 1].kind, 'status');
    assert.equal(question.runId, controller.state.runId);
    assert.equal(question.entryId, `${question.runId}-entry`);
    assert.equal(controller.messages.at(-2), question);
    assert.equal(count(fold(controller.messages)), 400);
    const broken = [...history(), firstAnswer, controller.messages.at(-1)];
    const folded = fold(broken);
    assert.equal(count(folded), 200);
    assert(folded.some(item => item.turnTrace?.items.some(child => child.message.id === firstAnswer.id)));
    controller.reset(); assert.deepEqual(controller.messages, history());
  });
  await check('mock runtime projection passes frozen reader/validator; absent endpoint and binding drift fail (no UI render)', async () => {
    const ts = requireUI('typescript');
    const generated = await readFile(resolve(baseline, 'src/composition/generated/frontend-modules.ts'), 'utf8');
    const profile = JSON.parse(generated.match(/export const generatedFrontendProfile = ([\s\S]+?) as const;/)[1]);
    const source = await readFile(resolve(baseline, 'src/composition/runtime.ts'), 'utf8');
    // Execute only the original pure HTTP reader/validator, with no hooks,
    // component imports, Vite server or copied implementation of the gate.
    const ast = ts.createSourceFile('runtime.ts', source, ts.ScriptTarget.Latest, true);
    const names = ['readRuntimeCapabilities', 'validateRuntimeCapabilities', 'assertRuntimeCapabilities', 'isRecord'];
    const functions = ast.statements.filter(node => ts.isFunctionDeclaration(node) && names.includes(node.name?.text));
    assert.equal(functions.length, names.length);
    const code = ts.transpileModule(functions.map(node => node.getText(ast)).join('\n'), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    const gate = {};
    new Function('exports', 'slots', code)(gate, Object.keys(profile.modules));
    const mock = createMock();
    const fetchMock = async endpoint => {
      const response = { writeHead(status) { this.status = status; }, end(body) { this.body = JSON.parse(body); } };
      await mock.handleHTTP({ url: endpoint, method: 'GET', headers: { authorization: `Bearer ${TOKEN}` } }, response);
      return { ok: response.status === 200, status: response.status, json: async () => response.body };
    };
    try {
      const runtime = await gate.readRuntimeCapabilities(fetchMock);
      assert.equal(mock.stats.requests.at(-1).path, '/api/modules/runtime');
      assert.deepEqual(runtime.modules, profile.modules);
      assert.equal(runtime.gatewayState, 'ready'); assert.deepEqual(runtime.unavailableSlots, []);
      const assembly = { bindings: profile.modules, selections: Object.entries(profile.modules)
        .filter(([, binding]) => binding.enabled).map(([slot, binding]) => ({ slot, binding, frontend: {} })) };
      gate.assertRuntimeCapabilities(assembly, runtime);
      await assert.rejects(() => gate.readRuntimeCapabilities(fetchMock, '/api/missing'), /HTTP 404/);
      for (const mutate of [
        value => { delete value.modules.agentLoop; },
        value => { value.modules.agentLoop.provider = 'wrong'; },
        value => { value.modules.skills.enabled = false; },
      ]) {
        const changed = structuredClone(runtime); mutate(changed);
        assert.throws(() => gate.assertRuntimeCapabilities(assembly, changed), /composition mismatch/);
      }
    } finally { mock.close(); }
  });
  await check('stream wire shape equals baseline normalized messages; 25Hz/60s, cancel and restart', async () => {
    const { createNormalizedMessage } = await import(pathToFileURL(resolve(baseline, 'server/pilotdeck-message.js')));
    let time = 0, pending, sent = [];
    const controller = createStream({ send: message => { sent.push(message); return 1; }, now: () => time,
      schedule: (fn, delay) => { pending = () => { time += delay; fn(); }; return 1; }, cancel: () => { pending = undefined; } });
    for (const scenario of ['invalid', 'toString', '__proto__', null]) assert.throws(() => controller.start(scenario), /Unknown/);
    controller.start('endurance'); assert.throws(() => controller.start('reading'), /already running/);
    while (pending) { const callback = pending; pending = undefined; callback(); }
    assert.equal(time, 60000); assert.equal(controller.state.sent, 1500);
    assert.deepEqual(streamErrors(controller.state), []);
    for (const message of sent) {
      assert.deepEqual(message, createNormalizedMessage(message));
      assert.equal(message.type, undefined);
    }
    assert.deepEqual(sent.slice(-2).map(message => message.kind), ['stream_end', 'complete']);
    assert.equal(new Set(sent.map(message => message.id)).size, sent.length);
    assert.deepEqual(textErrors(controller.messages.at(-1).content, 1500), []);
    for (const mutation of [
      state => { state.status = 'cancelled'; },
      state => { state.times = state.times.map(t => t * 2); },
      state => { state.sent--; }, state => { state.recipients[5] = 0; }, state => { state.recipients[5] = 2; },
    ]) { const state = structuredClone(controller.state); mutation(state); assert(streamErrors(state).length); }
    controller.start('reading'); pending(); const previous = sent.length;
    controller.stop(); assert.equal(pending, undefined); assert.equal(controller.state.status, 'cancelled');
    assert.equal(sent.length, previous); controller.reset(); assert.equal(controller.messages.length, 48);
    assert.equal(controller.state.status, 'idle');
    const initialRun = sent[0].runId; controller.start('reading');
    assert.notEqual(controller.state.runId, initialRun); controller.stop();
    assert.equal(frame('stream_delta').sessionId, 's');
  });
  await check('DOM evidence controls reject missing/duplicate/reordered text and scrolling jumps', () => {
    const text = [0, 1, 2].map(marker).join('\n');
    assert.deepEqual(textErrors(text, 3), []);
    for (const bad of [[0, 2], [0, 1, 1, 2], [0, 2, 1], []]) assert(textErrors(bad.map(marker).join(' '), 3).length);
    assert.deepEqual(readingErrors([{ top: 100 }, { top: 100.5 }], 100), []);
    assert.deepEqual(followErrors([{ at: 0, distance: 0 }, { at: 30, distance: 40 }, { at: 60, distance: 0 }]), []);
    const noFollow = Array.from({ length: 10 }, (_, index) => ({ at: index * 100, distance: index === 9 ? 0 : 40 }));
    assert(followErrors(noFollow).includes('bottom follow absent >500ms'));
    assert(followErrors([{ at: 0, distance: 0 }, { at: 1000, distance: 0 }]).includes('DOM sampling stalled >500ms'));
    assert(followErrors([]).length);
    for (const samples of [[], [{ top: 100 }], [{ top: 100 }, { top: 102 }], [{ top: 150 }, { top: 100 }]]) assert(readingErrors(samples, 100).length);
  });
  await check('execution env isolates absolute URL/env consumers without changing route identity', () => {
    const hostile = { FORMAL_ROUTE_PROFILE: 'native', REAL_NATIVE_URL: 'http://127.0.0.1:3001', PILOTDECK_API_URL: 'http://127.0.0.1:18789', PLAYWRIGHT_JSON_OUTPUT_NAME: '/tmp/unowned', FE_SUITE: 'wrong' };
    assert.deepEqual(executionEnv(hostile), { PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD: '1' });
    for (const frontend of ['old', 'new']) for (const profile of PROFILES) {
      const env = executionEnv(hostile, { frontend, profile, suite: 'baseline', output: resolve(root, '_tmp/fe-parity/control') });
      assert.equal(env.FORMAL_ROUTE_PROFILE, undefined);
      for (const name of ['REAL_NATIVE_URL', 'REAL_COMPOSITION_URL', 'FORMAL_COMPOSITION_URL', 'PILOTDECK_API_URL']) assert.equal(env[name], origin(frontend));
      assert(env.TMPDIR.startsWith(root)); assert.equal(env.REAL_COMPOSITION_PROFILE, profile);
    }
  });
  await check('repository configs discover full 72 in all six side/profile combinations; blackbox 8 (4 per protocol), skeleton 3', async () => {
    const output = await mkdtemp(resolve(root, '_tmp/fe-parity/config-control-'));
    const historical = JSON.parse(await readFile(historicalInventory, 'utf8'));
    try {
      for (const frontend of ['old', 'new']) for (const suite of ['baseline', 'streaming-scroll', 'skeleton']) {
        for (const profile of suite === 'baseline' ? PROFILES : [undefined]) {
          const config = resolve(root, `scripts/fe-parity/${suite === 'baseline' ? 'baseline' : 'browser'}.config.mjs`);
          const env = executionEnv(process.env, { frontend, output, profile, suite });
          const { stdout } = await exec(process.execPath, [requireUI.resolve('@playwright/test/cli'), 'test', '--config', config, '--list', '--forbid-only', '--reporter=json'], { cwd: root, env, maxBuffer: 4 * 1024 * 1024 });
          const report = JSON.parse(stdout);
          assert.deepEqual(report.errors, []); assert.equal(report.config.forbidOnly, true);
          assert.equal(report.config.metadata.frontend, frontend); assert.equal(report.config.metadata.baseURL, origin(frontend));
          assert.equal(report.config.webServer, null); assert.equal(report.config.globalSetup, null); assert.equal(report.config.globalTeardown, null);
          assert.equal(report.config.projects.length, 1); assert.equal(report.config.projects[0].repeatEach, 1); assert.equal(report.config.projects[0].retries, 0);
          const rows = reportCases(report, suite === 'baseline' ? resolve(baseline, 'e2e') : resolve(root, 'scripts/fe-parity/tests'));
          if (suite === 'baseline') assert.deepEqual(checkInventory(rows, historical), []);
          else {
            assert.equal(rows.length, suite === 'skeleton' ? 3 : 8);
            if (suite === 'streaming-scroll') for (const protocol of STREAM_PROTOCOLS) {
              assert.equal(rows.filter(row => row.title.includes(protocol)).length, 4);
            }
          }
        }
      }
    } finally { await rm(output, { recursive: true }); }
  });
}
