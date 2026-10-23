// Real baseline composition for the host model module port (A6.9-pr1). The
// chain under test is the one AgentSessionRuntimeBundle.ts:188-208 builds:
//
//   createRouterModelInvokerPort(router, options)
//     -> createDurableModelInvokerPort(port, recorder)
//     -> createSidecarModuleComposition({model, routing, metadata, ...}).model
//     -> .bindTurn({sessionId}) per turn
//
// driven the way sidecarDefaultModuleDispatcher.ts:69-158 drives it
// (prepare / iterator next() pulls / iterator return() / metadata lookups /
// materializeRequest). No implementation is copied into this fixture: the
// router, the durable wrapper, the bindTurn sticky projection, the recorder
// and the transcript writer are all the fixed baseline's own code. HTTP cases
// point the providers at an owned loopback mock and replay through the real
// streamModel, like tests/fixtures/router-execute.
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const SESSION = 's1';
const NOW = '2026-10-18T00:00:00.000Z';
const AGENT_MODEL = { provider: 'a', model: 'm' };

export async function record(baselineDir: string, scratchDir: string) {
  const load = (file: string) => import(pathToFileURL(join(baselineDir, file)).href);
  const { createRouterRuntime } = await load('src/router/RouterRuntime.ts');
  const { createModelRuntime } = await load('src/model/ModelRuntime.ts');
  const { createRouterModelInvokerPort } = await load('src/agent/modules/llm/routerModelInvokerAdapter.ts');
  const { createDurableModelInvokerPort } = await load('src/agent/modules/llm/durableModelInvokerPort.ts');
  const { createSidecarModuleComposition } = await load('src/agent/modules/transport/sidecarHostModulePorts.ts');
  const { AgentSessionEventRecorder } = await load('src/agent/session/AgentSessionEventRecorder.ts');
  const { JsonlTranscriptWriter } = await load('src/session/transcript/JsonlTranscriptWriter.ts');
  const { SessionRuntime } = await load('src/session/events/SessionRuntime.ts');
  const { JsonlSessionPersistence } = await load('src/session/persistence/JsonlSessionPersistence.ts');
  const { resolveRoutedModelMaxContextTokens } = await load('src/agent/runtime/modelContextWindow.ts');
  const { streamModel } = await load('src/model/streaming/streamModel.ts');
  const { createNativeRouterProviderHealthPort } = await load('src/router/health/RouterProviderHealthPort.ts');

  // Provider config shared by every case. Distinct capability fields so the
  // metadata lookups and the router's own admission see real boundaries.
  const providers = Object.fromEntries(['a', 'b', 'c'].map((id) => [id, {
    id,
    protocol: 'anthropic',
    url: `http://${id}.test/v1`,
    apiKey: 'fixture-key',
    retry: { streamMaxRetries: 0 },
    models: {
      m: {
        id: 'm',
        capabilities: id === 'a'
          ? {
            supportsStreaming: true, supportsToolUse: true, supportsSystemPrompt: true,
            maxOutputTokens: 64, maxContextTokens: 1024, supportsPromptCache: true,
          }
          : id === 'b'
            ? { supportsStreaming: true, supportsToolUse: true, supportsSystemPrompt: true, maxOutputTokens: 64 }
            : { supportsStreaming: true, supportsToolUse: true, supportsSystemPrompt: true, maxOutputTokens: 64, maxContextTokens: 512 },
        multimodal: { input: ['text', 'image'] },
      },
    },
  }]));

  const frame = { type: 'message_start', messageId: 'id' };
  const usage = { type: 'usage', usage: { inputTokens: 3, outputTokens: 2 } };
  const end = { type: 'message_end', finishReason: 'stop' };
  const text = (value: string) => ({ type: 'text_delta', text: value });
  const toolCall = [
    { type: 'tool_call_start', id: 't', name: 'tool' },
    { type: 'tool_call_delta', id: 't', delta: '{"x":' },
    { type: 'tool_call_delta', id: 't', delta: '1}' },
    { type: 'tool_call_end', id: 't' },
  ];
  const failure = (code = 'server_error', retryable = true) => ({
    type: 'error',
    error: { provider: 'a', protocol: 'anthropic', code, message: 'failed', retryable },
  });
  const ok = [frame, text('ok'), usage, end];

  // Anthropic wire frames for the HTTP cases (same shape the router-execute
  // fixture replays): each reply's chunks are pre-rendered SSE strings.
  const sse = (event: any) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`;
  const wireStart = sse({ type: 'message_start', message: { id: 'msg_1', type: 'message', role: 'assistant', content: [], model: 'm', usage: { input_tokens: 11, output_tokens: 1 } } });
  const wireText = (value: string) => sse({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } })
    + sse({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: value } });
  const wireTool = sse({ type: 'content_block_start', index: 0, content_block: { type: 'tool_use', id: 't', name: 'tool', input: {} } })
    + sse({ type: 'content_block_delta', index: 0, delta: { type: 'input_delta', partial_json: '{"x":' } })
    + sse({ type: 'content_block_delta', index: 0, delta: { type: 'input_delta', partial_json: '1}' } });
  const wireUsage = sse({ type: 'message_delta', delta: { stop_reason: 'tool_use' }, usage: { output_tokens: 7 } });
  const wireEnd = sse({ type: 'message_stop' });
  const httpError = (status: number) => ({
    status,
    headers: { 'content-type': 'application/json' },
    chunks: [JSON.stringify({ error: { type: status === 429 ? 'rate_limit_error' : 'api_error', message: status === 429 ? 'rate limit' : 'server fault' } })],
  });

  const transportHeaders = ['host', 'connection', 'content-length', 'user-agent', 'accept', 'accept-language', 'sec-fetch-mode', 'accept-encoding', 'transfer-encoding', 'keep-alive'];

  const specs: any[] = [
    {
      name: 'text-stream',
      scripts: [{ events: ok }],
      metadata: true,
      materialize: true,
    },
    {
      name: 'tool-call-delta',
      scripts: [{ events: [frame, ...toolCall, usage, end] }],
    },
    {
      name: 'model-override',
      context: { modelOverride: { provider: 'b', model: 'm' } },
      scripts: [{ events: ok }],
      metadata: true,
    },
    {
      name: 'provider-error-fallback',
      scripts: [{ events: [frame, failure()] }, { events: ok }],
    },
    {
      name: 'thrown-network-fallback',
      scripts: [{ throw: { name: 'TypeError', message: 'fetch failed' } }, { events: ok }],
    },
    {
      name: 'thrown-terminal',
      config: { fallback: {} },
      scripts: [{ throw: { name: 'TypeError', message: 'fetch failed' } }],
    },
    {
      name: 'policy-denied',
      managedModelPolicy: { allow: [], deny: ['a/*'] },
      scripts: [],
    },
    {
      name: 'mid-stream-cancel',
      scripts: [{ events: [frame, text('ok'), usage, end] }],
      pulls: 2,
    },
    {
      name: 'no-pull-close',
      scripts: [{ events: ok }],
      pulls: 0,
    },
    {
      name: 'second-stream',
      scripts: [{ events: ok }, { events: ok }],
      repeat: 2,
    },
    {
      name: 'sticky-consumed-per-turn',
      scripts: [{ events: ok }, { events: ok }],
      turns: 2,
      invalidateProbe: true,
    },
    {
      name: 'agent-max-context-override',
      scripts: [{ events: ok }],
      agentMaxContextTokens: 777,
      metadata: true,
    },
    {
      name: 'subagent-override',
      isMainAgent: false,
      context: { modelOverride: { provider: 'b', model: 'm' } },
      scripts: [{ events: ok }],
    },
    {
      name: 'http-text-stream',
      http: true,
      replies: [{ chunks: [wireStart, wireText('ok'), wireUsage, wireEnd] }],
      metadata: true,
    },
    {
      name: 'http-tool-call-delta',
      http: true,
      replies: [{ chunks: [wireStart, wireTool, wireUsage, wireEnd] }],
    },
    {
      name: 'http-cancel-mid-stream',
      http: true,
      pulls: 2,
      replies: [{ chunks: [wireStart, wireText('ok')], end: 'hold' }],
    },
    {
      name: 'http-error-fallback',
      http: true,
      replies: [httpError(429), { chunks: [wireStart, wireText('ok'), wireUsage, wireEnd] }],
    },
    // Fix round 1: the TS yield boundaries and the turn abortSignal the
    // dispatcher wires (sidecarDefaultModuleDispatcher.ts:81,441 ->
    // routerModelInvokerAdapter.ts:68).
    {
      // Content, then a provider error event, fully drained: execute_failed
      // and the provider-health failure only fire behind the pulls that
      // consumed the already-delivered events.
      name: 'content-error-drain',
      scripts: [{ events: [frame, text('ok'), failure()] }],
    },
    {
      // Pull only the first event, then return(): the read-ahead stops at the
      // pull boundary and no failure is published.
      name: 'content-error-partial',
      scripts: [{ events: [frame, text('ok'), failure()] }],
      pulls: 1,
    },
    {
      // abortSignal already aborted: the first pull records the request and
      // answers done with no provider call at all (RouterRuntime.ts:685).
      name: 'pre-aborted',
      scripts: [],
      abortPre: true,
    },
    {
      // Turn abort after the first pull: the buffered read-ahead event still
      // yields, the next provider read rethrows the raw AbortError.
      name: 'sync-abort-mid-stream',
      scripts: [{ events: ok }],
      abortAfterPulls: 1,
    },
    {
      // Turn abort while the first pull waits on the held response body: the
      // wait interrupts and the provider connection drops (clientClosed).
      name: 'http-abort-while-waiting',
      http: true,
      replies: [{ chunks: [], end: 'hold' }],
      abortAtMs: 100,
    },
    {
      // Turn abort mid-stream: every buffered event yields first, then the
      // held-body wait interrupts with the raw AbortError.
      name: 'http-abort-mid-stream',
      http: true,
      replies: [{ chunks: [wireStart, wireText('ok')], end: 'hold' }],
      abortAtMs: 150,
    },
    {
      // Pre-content framing plus a non-fallback-eligible terminal error:
      // the drained pending events are TS yields (RouterRuntime.ts:873-877),
      // so execute_failed, the replayed buffered events and the enriched
      // error all stay behind the pulls that consume them.
      name: 'pre-content-terminal-error',
      scripts: [{ events: [frame, failure('server_error', false)] }],
    },
    {
      // Fix round 3: the same contentless terminal failure, but the consumer
      // pulls only the first queued event and then cancels the turn. The
      // cancel tears the provider connection down (streamModel.ts's
      // cancelReader); it never advances the generator past its yield, so
      // execute_failed / observeRequest never publish (RouterRuntime.ts:962
      // stays behind the yields of :873-877), and the following return()
      // discards the rest.
      name: 'pre-content-error-abort-partial',
      scripts: [{ events: [frame, failure('server_error', false)] }],
      pulls: 1,
      abortAfterPulls: 1,
    },
  ];

  const cases = [];
  for (const spec of specs) {
    const trace: any[] = [];
    const calls: any[] = [];
    const requests: any[] = [];
    let server: any;
    let origin = '';
    const caseProviders = spec.http ? structuredClone(providers) : providers;
    if (spec.http) {
      let replyIndex = 0;
      server = createServer(async (req: any, res: any) => {
        let body = '';
        for await (const chunk of req) body += chunk;
        const reply = spec.replies[replyIndex++];
        if (!reply) throw new Error(`unexpected HTTP request ${spec.name}`);
        const received: any = {
          method: req.method,
          path: req.url,
          headers: Object.entries(req.headers)
            .filter(([key]) => !transportHeaders.includes(key))
            .sort(([a], [b]) => a.localeCompare(b)),
          body,
        };
        requests.push(received);
        if (reply.end === 'hold') {
          received.clientClosed = false;
          res.once('close', () => { received.clientClosed = true; });
        }
        res.writeHead(reply.status ?? 200, { 'content-type': reply.headers?.['content-type'] ?? 'text/event-stream' });
        for (const chunk of reply.chunks ?? []) {
          res.write(chunk);
          await new Promise((resolve) => setImmediate(resolve));
        }
        if (reply.end === 'destroy') {
          res.flushHeaders();
          setTimeout(() => res.destroy(), 10);
        } else if (reply.end !== 'hold') {
          res.end();
        }
      });
      await new Promise<void>((resolve, reject) => {
        server.once('error', reject);
        server.listen(0, '127.0.0.1', resolve);
      });
      origin = `http://127.0.0.1:${server.address().port}`;
      for (const id of ['a', 'b', 'c']) caseProviders[id].url = `${origin}/${id}/v1`;
    }

    const modelRuntime = createModelRuntime({ providers: caseProviders });
    // The scripted invoker the router executes against (RouterModelInvocation
    // shape): capabilities come from the real ModelRuntime, the stream replays
    // canonical events or delegates to the real streamModel for HTTP cases.
    const invoker = {
      getCapabilities: (provider: string, model: string) => modelRuntime.getCapabilities(provider, model),
      getMultimodal: (provider: string) => ({ input: spec.media?.[provider] ?? ['text', 'image'] }),
      getProviderProtocol: () => 'anthropic',
      getProviderGeneration: () => undefined,
      async *stream(request: any, options: any) {
        calls.push(request);
        if (spec.http) {
          yield* streamModel(request, { providers: caseProviders }, options);
          return;
        }
        const script = spec.scripts[calls.length - 1];
        if (!script) throw new Error(`unexpected invocation ${spec.name}`);
        for (const event of script.events ?? []) yield event;
        if (script.throw) {
          throw Object.assign(new Error(script.throw.message), { name: script.throw.name });
        }
      },
    };
    const events = { emit: (value: any) => trace.push({ kind: 'router', value }) };
    const router = createRouterRuntime(
      {
        enabled: true,
        scenarios: { default: { id: 'a/m', provider: 'a', model: 'm' } },
        fallback: { default: [{ id: 'b/m', provider: 'b', model: 'm' }] },
        transientRetry: { enabled: false },
        zeroUsageRetry: { enabled: false },
        ...spec.config,
      },
      {
        modelInvoker: invoker as any,
        providerHealth: createNativeRouterProviderHealthPort({ now: () => 0 }),
        now: () => new Date(0),
        events,
        usageObserver: {
          stats: {},
          getSessionUsage: () => undefined,
          observeSessionUsage: () => {},
          observeRequest: () => {},
        } as any,
      },
    );

    // Session-scoped recorder + transcript, like recorder/generate.mts open():
    // deterministic clock and ids so the bytes reproduce.
    const path = join(scratchDir, `${spec.name}-${SESSION}.jsonl`);
    let ids = 0;
    const runtime = new SessionRuntime({
      now: () => new Date(NOW),
      uuid: () => `id-${++ids}`,
    });
    const persistence = new JsonlSessionPersistence({ path });
    runtime.subscribe((entry) => persistence.append(entry), { failureMode: 'propagate', flush: () => persistence.flush() });
    const writer = new JsonlTranscriptWriter({ path, eventStore: runtime });
    const recorder = new AgentSessionEventRecorder(writer);

    const modelPort = createRouterModelInvokerPort(router, {
      isMainAgent: spec.isMainAgent ?? true,
      fallbackModels: spec.fallbackModels,
      managedModelPolicy: spec.managedModelPolicy,
    });
    const durablePort = createDurableModelInvokerPort(modelPort, recorder);
    const metadata = {
      getModelMaxContextTokens: (provider: string, model: string) => resolveRoutedModelMaxContextTokens({
        modelRuntime,
        agentModel: AGENT_MODEL,
        agentMaxContextTokens: spec.agentMaxContextTokens,
        provider,
        model,
      }),
      getModelMaxOutputTokens: (provider: string, model: string) => {
        try {
          return modelRuntime.getCapabilities(provider, model).maxOutputTokens;
        } catch {
          return undefined;
        }
      },
      getModelTokenLimits: (provider: string, model: string) => {
        try {
          const caps = modelRuntime.getCapabilities(provider, model);
          return { maxContextTokens: caps.maxContextTokens, maxOutputTokens: caps.maxOutputTokens };
        } catch {
          return undefined;
        }
      },
      getModelProtocol: (provider: string) => modelRuntime.getProviderProtocol(provider),
      getModelSupportsPromptCache: (provider: string, model: string) => {
        try {
          return modelRuntime.getCapabilities(provider, model).supportsPromptCache;
        } catch {
          return undefined;
        }
      },
    };
    const composition = createSidecarModuleComposition({
      model: durablePort,
      routing: {
        invalidateSticky: router.invalidateSticky.bind(router),
        materializeRequest: (prepared: any, request: any) => router.materializeRequest(prepared.opaque, request),
      },
      metadata: metadata as any,
      toolExecution: { list: () => [], executeAll: async () => [] } as any,
    } as any);

    // The module wire face of an error is `{message, code?}` with a STRING
    // code (SDK_MANAGED_MODEL_DENIED etc.); AbortError's numeric `code: 20`
    // never crosses either real module wire, so it stays out of the oracle.
    const errorOf = (error: any) => ({
      message: error.message,
      ...(typeof error.code === 'string' ? { code: error.code } : {}),
    });
    const baseRequest = {
      provider: 'a',
      model: 'm',
      messages: [{ role: 'user', content: [{ type: 'text', text: 'hello' }] }],
      maxOutputTokens: 100,
    };
    const turns = spec.turns ?? 1;
    for (let turn = 1; turn <= turns; turn += 1) {
      const turnId = `t${turn}`;
      await recorder.startTurn(SESSION, turnId);
      const bound = composition.model.bindTurn({ sessionId: SESSION });
      if (spec.invalidateProbe && turn === 2) {
        trace.push({ kind: 'invalidate', value: router.invalidateSticky(SESSION) });
      }
      const repeats = spec.repeat ?? 1;
      for (let repeat = 1; repeat <= repeats; repeat += 1) {
        const context = { sessionId: SESSION, turnId, runId: turnId, ...spec.context };
        let prepared: any;
        try {
          prepared = await bound.execution.prepare({ request: structuredClone(baseRequest), context });
        } catch (error: any) {
          trace.push({ kind: 'error', value: errorOf(error) });
          continue;
        }
        trace.push({
          kind: 'prepared',
          value: {
            request: prepared.request,
            provider: prepared.provider,
            model: prepared.model,
            opaque: prepared.opaque,
          },
        });
        if (spec.metadata) {
          for (const [provider, model] of [['a', 'm'], ['b', 'm'], ['x', 'm']]) {
            trace.push({ kind: 'metadata', field: 'maxContextTokens', provider, model, value: bound.metadata.getModelMaxContextTokens(provider, model) ?? null });
            trace.push({ kind: 'metadata', field: 'maxOutputTokens', provider, model, value: bound.metadata.getModelMaxOutputTokens(provider, model) ?? null });
            trace.push({ kind: 'metadata', field: 'tokenLimits', provider, model, value: bound.metadata.getModelTokenLimits(provider, model) ?? null });
            trace.push({ kind: 'metadata', field: 'protocol', provider, model, value: bound.metadata.getModelProtocol(provider) ?? null });
            trace.push({ kind: 'metadata', field: 'supportsPromptCache', provider, model, value: bound.metadata.getModelSupportsPromptCache(provider, model) ?? null });
          }
        }
        if (spec.materialize) {
          const candidate = { ...structuredClone(baseRequest), messages: [{ role: 'user', content: [{ type: 'text', text: 'candidate' }] }] };
          const materialized = await bound.materializeRequest(prepared, candidate);
          trace.push({ kind: 'materialized', value: materialized });
        }
        // The turn abort the dispatcher wires as context.abortSignal
        // (sidecarDefaultModuleDispatcher.ts:81,441), driven per spec: aborted
        // before the first pull, after N pulled events, or at a fixed delay
        // while a pull waits on a held provider response.
        const aborts = spec.abortPre === true
          || spec.abortAfterPulls !== undefined
          || spec.abortAtMs !== undefined;
        const controller = aborts ? new AbortController() : undefined;
        const streamContext = controller ? { ...context, abortSignal: controller.signal } : context;
        const iterator = bound.execution.stream({ prepared, context: streamContext })[Symbol.asyncIterator]();
        if (spec.abortPre) {
          controller!.abort();
        }
        if (spec.abortAtMs !== undefined) {
          setTimeout(() => controller!.abort(), spec.abortAtMs);
        }
        const pulls = spec.pulls ?? Number.POSITIVE_INFINITY;
        let pulled = 0;
        let drained = false;
        while (pulled < pulls) {
          let next: any;
          try {
            next = await iterator.next();
          } catch (error: any) {
            trace.push({ kind: 'error', value: errorOf(error) });
            break;
          }
          if (next.done) {
            trace.push({ kind: 'done' });
            drained = true;
            break;
          }
          trace.push({ kind: 'event', value: next.value });
          pulled += 1;
          if (spec.abortAfterPulls !== undefined && pulled >= spec.abortAfterPulls) {
            controller!.abort();
          }
        }
        // Only an explicitly limited pull count closes the iterator early,
        // like the dispatcher's close_stream after partial stream_next calls.
        if (spec.pulls !== undefined && !drained) {
          try {
            await iterator.return();
            trace.push({ kind: 'closed' });
          } catch (error: any) {
            trace.push({ kind: 'error', value: errorOf(error) });
          }
        }
      }
    }

    let transcript = '';
    try {
      transcript = await readFile(path, 'utf8');
    } catch {
      transcript = '';
    }
    if (spec.http) {
      // A held response is closed by the cancelled stream; wait for the
      // server to observe it so `clientClosed` is final in the oracle.
      const deadline = Date.now() + 5000;
      while (requests.some((request) => request.clientClosed === false) && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 5));
      }
    }
    const serialized = JSON.stringify({
      spec,
      trace,
      calls,
      ...(spec.http ? { requests } : {}),
      transcript,
    });
    cases.push(JSON.parse(spec.http ? serialized.replaceAll(origin, '$MOCK') : serialized));
    if (server) {
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) => server.close((error: any) => (error ? reject(error) : resolve())));
    }
  }

  return {
    node: process.version,
    transportHeaders,
    providers,
    cases,
  };
}
