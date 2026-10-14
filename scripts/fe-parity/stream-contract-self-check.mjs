import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { baseline, requireUI } from './env.mjs';
import { createStream, STREAM_PROTOCOLS, textErrors, streamErrors } from './stream.mjs';

// Execute frozen declarations, not a second hand-written protocol mapper.
// Imports/top-level service construction are excluded; no server or store hooks.
async function declarations(file, names, dependencies = {}) {
  const ts = requireUI('typescript');
  const source = await readFile(resolve(baseline, file), 'utf8');
  const ast = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
  const selected = ast.statements.filter(node =>
    (ts.isFunctionDeclaration(node) || ts.isClassDeclaration(node)) && names.includes(node.name?.text));
  assert.equal(selected.length, names.length, `frozen declarations drifted: ${file}`);
  const code = ts.transpileModule(selected.map(node => node.getText(ast)).join('\n'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const exports = {};
  new Function('exports', ...Object.keys(dependencies), `${code}\n${names.map(name => `exports.${name} = ${name};`).join('\n')}`)(exports, ...Object.values(dependencies));
  return exports;
}

export async function streamContractSelfCheck(check) {
  await check('both protocols match frozen native allocator/projector/bridge/history; timeline reducer consumes every live delta', async () => {
    const { createNormalizedMessage } = await import(pathToFileURL(resolve(baseline, 'server/pilotdeck-message.js')));
    const { TurnTimeline } = await declarations('../src/agent/stream/TurnTimeline.ts', ['TurnTimeline']);
    const { GatewayAgentEventProjector } = await declarations('../src/gateway/client/GatewayAgentEventProjector.ts', [
      'GatewayAgentEventProjector', 'projectAgentEventForTurn', 'mapModelEvent', 'mapTurnCompleted', 'withGatewayRunId', 'getGatewayEventRunId',
    ]);
    const { gatewayEventToFrames } = await declarations('server/pilotdeck-bridge.js', ['gatewayEventToFrames'], { createNormalizedMessage });
    const { mapWebMessageToNormalized } = await declarations('server/routes/messages.js', ['mapWebMessageToNormalized'], { createNormalizedMessage });
    // This module has only type imports; execute all declarations/const helpers.
    const ts = requireUI('typescript');
    const reducerSource = await readFile(resolve(baseline, 'src/stores/sessionTimeline.ts'), 'utf8');
    const reducerCode = ts.transpileModule(reducerSource, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    const reducer = {}; new Function('exports', reducerCode)(reducer);
    const projector = new GatewayAgentEventProjector({ toolResultArtifacts: {
      persist() { throw new Error('text fixture must not persist tool artifacts'); },
    } });
    // Ignore only nondeterministic transport identity/time. JSON drops optional
    // undefined fields exactly as the real WebSocket serialization does.
    const comparable = message => {
      const { id, timestamp, ...rest } = JSON.parse(JSON.stringify(message));
      return rest;
    };
    const equalWire = (actual, expected) => assert.deepEqual(comparable(actual), comparable(expected));
    for (const protocol of STREAM_PROTOCOLS) {
      let pending, time = 0;
      const sent = [];
      const controller = createStream({ send: message => { sent.push(message); return 1; }, now: () => time,
        schedule: (callback, delay) => { pending = () => { time += delay; callback(); }; return 1; },
        cancel: () => { pending = undefined; } });
      assert.throws(() => controller.start('reading', 'unknown'), /Unknown stream protocol/);
      const timelineView = new reducer.SessionTimeline();
      for (const scenario of ['endurance', 'reading']) {
        const first = sent.length;
        controller.start(scenario, protocol);
        while (pending) { const callback = pending; pending = undefined; callback(); }
        assert.deepEqual(streamErrors(controller.state), []);
        assert.equal(controller.state.protocol, protocol);
        const run = sent.slice(first), runId = controller.state.runId, blockId = `${runId}-text`;
        const allocator = new TurnTimeline(runId);
        const event = value => protocol === 'timeline' ? allocator.event(value) : value;
        const project = value => projector.project({ event: value, runId })
          .flatMap(gateway => gatewayEventToFrames(gateway, 's', 'pilotdeck'));
        equalWire(run.find(message => message.kind === 'status'), project({ type: 'turn_started' })[0]);
        let content = '';
        for (const delta of run.filter(message => message.kind === 'stream_delta')) {
          const agentEvent = event({ type: 'model_event', sessionId: 's', turnId: runId,
            ...(protocol === 'timeline' ? { blockId } : {}), event: { type: 'text_delta', text: delta.content } });
          const expected = project(agentEvent)[0];
          equalWire(delta, expected);
          content += delta.content;
          assert.equal(reducer.isTimelineMessage(delta), protocol === 'timeline');
          if (protocol === 'timeline') {
            assert.equal(timelineView.apply(delta), false, 'no offset/predecessor gap');
            assert.equal(timelineView.values(null).find(row => row.runId === runId).content, content,
              'every live prefix must render before a final snapshot can repair loss');
            for (const mutate of [
              value => { delete value.timeline; }, value => { value.timeline.offset++; },
              value => { value.timeline.revision--; }, value => { value.timeline.id = 'wrong-block'; },
              value => { delete value.blockId; }, value => { value.timeline.turnId = 'wrong-turn'; },
            ]) {
              const bad = structuredClone(delta); mutate(bad);
              assert.throws(() => equalWire(bad, expected), assert.AssertionError);
            }
          }
        }
        const boundaryEvent = event({ type: 'model_event', sessionId: 's', turnId: runId, event: { type: 'message_end' } });
        // Frozen mapModelEvent drops message_end (returns []); do not claim an
        // end-to-end emitted boundary. The bridge supports assistant_stream_end;
        // exercise that wire using the allocator's real boundary coordinates.
        assert.deepEqual(project(boundaryEvent), []);
        const boundaryGateway = { type: 'assistant_stream_end', runId,
          ...(boundaryEvent.streamBoundary ? { streamBoundary: boundaryEvent.streamBoundary } : {}) };
        const end = run.find(message => message.kind === 'stream_end');
        equalWire(end, gatewayEventToFrames(boundaryGateway, 's', 'pilotdeck')[0]);
        if (protocol === 'timeline') {
          timelineView.close(runId, false, undefined, end.streamBoundary);
          const finalEvent = allocator.event({ type: 'assistant_message', sessionId: 's', turnId: runId,
            message: { role: 'assistant', content: [{ type: 'text', blockId, text: content }] } });
          const final = run.find(message => message.kind === 'text' && message.isFinal);
          const expectedFinal = project(finalEvent)[0];
          equalWire(final, expectedFinal);
          assert.equal(final.streamState, 'closed'); assert.equal(final.timeline.offset, undefined);
          assert.equal(timelineView.apply(final), false);
          assert.equal(timelineView.values(null).find(row => row.runId === runId).content, content);
          const durable = mapWebMessageToNormalized({ id: `${runId}-answer`, kind: 'text', role: 'assistant',
            turnId: runId, blockId, timeline: finalEvent.message.content[0].timeline,
            text: content, createdAt: final.timestamp }, 's');
          assert.deepEqual(controller.messages.at(-1), durable);
          // Replaying the HTTP snapshot must not duplicate the live final block.
          timelineView.apply(durable);
          assert.equal(timelineView.values(null).filter(row => row.runId === runId).length, 1);
        } else {
          assert.equal(run.some(message => message.isFinal || message.timeline || message.streamBoundary), false);
        }
        equalWire(run.at(-1), project({ type: 'turn_completed', result: {} })[0]);
        timelineView.close(runId, true);
        assert.deepEqual(textErrors(controller.messages.at(-1).content, controller.state.count, controller.state.startIndex), []);
        if (protocol === 'timeline') {
          const legacy = controller.messages.filter(message => !reducer.isTimelineMessage(message));
          const merged = reducer.mergeTimeline(legacy, timelineView.values(null));
          const allText = messages => messages.map(message => message.content || '').join('\n');
          const count = controller.state.startIndex + controller.state.count;
          assert.deepEqual(textErrors(allText(merged), count), [], 'HTTP refresh must retain turn order');
          if (controller.state.startIndex) {
            // Without the first user anchor the native merger appends that
            // unanchored block after turn two, even though all text still exists.
            const broken = reducer.mergeTimeline(legacy.filter(message => message.runId !== 'f2-run-1'), timelineView.values(null));
            assert(textErrors(allText(broken), count).length);
          }
        }
      }
      controller.reset();
      assert.equal(controller.messages.length, 48);
    }
  });
}
