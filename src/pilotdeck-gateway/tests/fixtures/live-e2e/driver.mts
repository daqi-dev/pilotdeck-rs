// #969 live-e2e apparatus self-test driver.
//
// Drives the REAL TS sidecar client (agentLoopSidecarClient) through the
// Python stdio middleman into the REAL sidecar CLI. The client believes it
// spawned the sidecar directly; stdio_mitm.py records both directions and
// applies the declarative injection plan. Model streaming goes to the local
// mock provider over HTTP; the `read` tool executes in the case sandbox.
//
// Usage: node --import "<baseline>/node_modules/tsx/dist/loader.mjs" driver.mts \
//          <scenario: text|tool|cancel> <injection: passthrough|drop|duplicate|early-final> <caseDir>
// Env: PDRS_BASELINE (read-only TS baseline root, required),
//      LIVE_E2E_MOCK_PORT (mock provider port, required),
//      LIVE_E2E_MITM (stdio_mitm.py path, required),
//      LIVE_E2E_PYTHON (python3 executable, default "python3").
import assert from 'node:assert/strict';
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

assert.equal(process.version, 'v22.22.0', '#969 driver: requires Node v22.22.0');
assert.ok(process.env.PDRS_BASELINE, '#969 driver: set PDRS_BASELINE');
assert.ok(process.env.LIVE_E2E_MOCK_PORT, '#969 driver: set LIVE_E2E_MOCK_PORT');
assert.ok(process.env.LIVE_E2E_MITM, '#969 driver: set LIVE_E2E_MITM');
const [scenario, injection, caseDirArg] = process.argv.slice(2);
assert.ok(['text', 'tool', 'cancel'].includes(scenario), '#969 driver: scenario must be text|tool|cancel');
assert.ok(['passthrough', 'drop', 'duplicate', 'early-final'].includes(injection), '#969 driver: injection must be one of the four modes');
assert.ok(caseDirArg, '#969 driver: provide caseDir');

const root = process.env.PDRS_BASELINE!;
const caseDir = caseDirArg!;
const sandbox = join(caseDir, 'sandbox');
const home = join(caseDir, 'home');
mkdirSync(sandbox, { recursive: true });
mkdirSync(home, { recursive: true });
const toolTarget = join(sandbox, 'tool-target.txt');
const toolTargetText = 'live-e2e read tool payload\n';
writeFileSync(toolTarget, toolTargetText, { flag: 'wx' });

const load = (path: string) => import(pathToFileURL(join(root, 'src', path)).href);
const { createAgentLoopSidecarRuntimeFactory } = await load('agent/modules/transport/agentLoopSidecarClient.ts');
const { createStdioAgentLoopSidecarConnectionFactory } = await load('agent/modules/transport/stdioAgentLoopSidecarConnection.ts');
const { createSidecarModuleComposition } = await load('agent/modules/transport/sidecarHostModulePorts.ts');
const { createDefaultPermissionContext } = await load('permission/index.ts');

const mockPort = Number(process.env.LIVE_E2E_MOCK_PORT);
const mitmPath = process.env.LIVE_E2E_MITM!;
const python = process.env.LIVE_E2E_PYTHON ?? 'python3';
const loader = join(root, 'node_modules/tsx/dist/loader.mjs');
const sidecarCli = join(root, 'src/cli/pilotdeck-agent-loop-sidecar.ts');

// Evidence collected by the host side of the apparatus.
const evidence: any = {
  scenario,
  injection,
  caseDir,
  events: [],
  eventDetails: [],
  terminal: null,
  error: null,
  modelCalls: 0,
  modelEventCounts: [],
  modelInterrupted: false,
  toolCalls: [],
  toolResults: [],
  abortAfterEvents: null,
  abortReason: null,
  startedAtMs: Date.now(),
};
const now = () => new Date('2026-10-26T00:00:00.000Z').toISOString();

// The host model port: one mock HTTP request per model stream, canonical
// events yielded as NDJSON lines arrive. An abort (sidecar cancel or early
// client teardown) interrupts the provider connection on purpose; the mock
// records that as the observable interruption.
function mockModelPort() {
  return {
    async prepare({ request }: any) {
      return { request, provider: request.provider, model: request.model };
    },
    async *stream({ prepared, context }: any) {
      evidence.modelCalls++;
      const callIndex = evidence.modelCalls;
      let received = 0;
      const controller = new AbortController();
      const onAbort = () => controller.abort(new Error('live_e2e_model_interrupted'));
      if (context?.abortSignal?.aborted) onAbort();
      else context?.abortSignal?.addEventListener('abort', onAbort, { once: true });
      try {
        const response = await fetch(`http://127.0.0.1:${mockPort}/model`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ request: prepared.request }),
          signal: controller.signal,
        });
        assert.equal(response.status, 200, `#969 driver: mock provider responded ${response.status}`);
        const reader = response.body!.getReader();
        const decoder = new TextDecoder();
        let buffer = '';
        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });
          let newline;
          while ((newline = buffer.indexOf('\n')) >= 0) {
            const line = buffer.slice(0, newline);
            buffer = buffer.slice(newline + 1);
            if (!line.trim()) continue;
            const parsed = JSON.parse(line);
            if (parsed.done === true) {
              evidence.modelEventCounts[callIndex] = received;
              return;
            }
            received++;
            yield parsed.event;
          }
        }
        evidence.modelEventCounts[callIndex] = received;
        return;
      } catch (error) {
        if (context?.abortSignal?.aborted || controller.signal.aborted) {
          evidence.modelInterrupted = true;
          evidence.modelEventCounts[callIndex] = received;
          throw new Error('live_e2e_model_interrupted');
        }
        throw error;
      } finally {
        context?.abortSignal?.removeEventListener('abort', onAbort);
        controller.abort();
      }
    },
  };
}

// The host capability port: the `read` tool, confined to the case sandbox.
function readToolPort() {
  const readTool = {
    name: 'read',
    description: 'Read one file from the live-e2e case sandbox.',
    kind: 'filesystem',
    inputSchema: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] },
    isReadOnly: () => true,
    isConcurrencySafe: () => true,
    requiresUserInteraction: () => false,
    requiredRuntimeCapabilities: [],
  } as any;
  const executeRead = async (call: any) => {
    evidence.toolCalls.push({ id: call.id, name: call.name, input: call.input });
    const failure = (message: string) => ({
      type: 'error',
      toolCallId: call.id,
      toolName: call.name,
      error: { code: 'live_e2e_tool_error', message },
      content: [{ type: 'text', text: message }],
      startedAt: now(),
      completedAt: now(),
    });
    if (call.name !== 'read') return failure(`unknown tool ${call.name}`);
    const path = String(call.input?.path ?? '');
    if (path !== toolTarget) return failure(`read target must be ${toolTarget}`);
    const text = readFileSync(path, 'utf8');
    const result = {
      type: 'success',
      toolCallId: call.id,
      toolName: call.name,
      content: [{ type: 'text', text }],
      startedAt: now(),
      completedAt: now(),
    };
    evidence.toolResults.push({ id: call.id, text });
    return result;
  };
  return {
    list: () => [readTool],
    executeAll: async (calls: any[]) => {
      const results = [];
      for (const call of calls) results.push(await executeRead(call));
      return results;
    },
  } as any;
}

const modules = createSidecarModuleComposition({
  model: mockModelPort(),
  toolExecution: readToolPort(),
});

// Deterministic client-side ids keep the recorded h2s wire reproducible.
let serial = 0;
const uuid = () => `fixed-${++serial}`;
const connect = createStdioAgentLoopSidecarConnectionFactory({
  command: python,
  args: [
    mitmPath,
    '--log', join(caseDir, 'frames.jsonl'),
    '--summary', join(caseDir, 'mitm-summary.json'),
    ...(injection === 'passthrough' ? [] : ['--plan', join(caseDir, 'plan.json')]),
    '--',
    process.execPath, '--import', loader, sidecarCli,
  ],
  cwd: sandbox,
  env: { PATH: process.env.PATH!, HOME: home },
  killTimeoutMs: 3_000,
});

const config = {
  provider: 'live-e2e',
  model: 'mock-model',
  cwd: sandbox,
  permissionMode: 'default',
  permissionContext: createDefaultPermissionContext({ cwd: sandbox, mode: 'default', canPrompt: false }),
} as any;

const sessionId = 'live-e2e-session';
const turnId = `turn-${scenario}-${injection}`;
const prompt = scenario === 'text'
  ? 'LIVE_E2E_TEXT run the text stream window'
  : scenario === 'tool'
    ? `LIVE_E2E_TOOL ${toolTarget}`
    : 'LIVE_E2E_HANG run the cancel window';
const abortController = new AbortController();
const input = {
  sessionId,
  turnId,
  messages: [{ role: 'user', content: [{ type: 'text', text: prompt }] }],
  execution: { runId: `run-${scenario}-${injection}`, operationId: `operation-${scenario}-${injection}` },
  abortSignal: abortController.signal,
} as any;

const factory = createAgentLoopSidecarRuntimeFactory({ connect, uuid });
// sidecarModules takes precedence, but the factory still reads the legacy
// capabilities.transport fallback chain, so a minimal shape is required.
const runner = factory({ config, capabilities: { transport: {} } as any, sidecarModules: modules, seedState: undefined });

const watchdog = setTimeout(() => {
  evidence.timeout = true;
  writeFileSync(join(caseDir, 'driver-result.json'), JSON.stringify(evidence, null, 2) + '\n');
  console.error('#969 driver: turn did not finish within 60s');
  process.exit(1);
}, 60_000);
watchdog.unref?.();

try {
  const iterator = runner.run(input);
  while (true) {
    const next = await iterator.next();
    if (next.done) {
      // The AgentLoopRunResult terminal: {result, messages}.
      evidence.terminal = {
        type: next.value.result.type,
        stopReason: next.value.result.stopReason,
        turns: next.value.result.turns,
        errors: next.value.result.errors?.map((error: any) => ({ code: error.code, message: error.message })),
        messageCount: next.value.messages.length,
      };
      break;
    }
    const event = next.value;
    evidence.events.push(event.type);
    evidence.eventDetails.push({
      type: event.type,
      ...(event.type === 'model_event' ? { modelEventType: (event as any).event?.type } : {}),
    });
    // The cancel window is mid-stream: the first wrapped model event proves
    // the provider connection is open and hanging before the abort fires.
    if (scenario === 'cancel' && evidence.abortAfterEvents === null && event.type === 'model_event') {
      evidence.abortAfterEvents = evidence.events.length;
      evidence.abortReason = 'live_e2e_cancel';
      abortController.abort('live_e2e_cancel');
    }
  }
} catch (error) {
  evidence.error = error instanceof Error ? error.message : String(error);
} finally {
  clearTimeout(watchdog);
}
evidence.elapsedMs = Date.now() - evidence.startedAtMs;
writeFileSync(join(caseDir, 'driver-result.json'), JSON.stringify(evidence, null, 2) + '\n');
console.log(`#969 driver: scenario=${scenario} injection=${injection} events=${evidence.events.length} terminal=${evidence.terminal ? 'yes' : 'no'} error=${evidence.error ? 'yes' : 'no'}`);
process.exit(0);
