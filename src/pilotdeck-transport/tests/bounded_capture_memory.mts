// Usage: node --import <frozen-oracle>/node_modules/tsx/dist/loader.mjs this.mts <oracle> <owned-output>
// Controlled production memory/dispatcher fixture; no model provider or user storage.
import assert from 'node:assert/strict';
import { mkdirSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
const [oracle, output] = process.argv.slice(2).map((value) => resolve(value));
assert(oracle && output && output !== oracle && !output.startsWith(oracle + '/'));
mkdirSync(output, { recursive: true });
const load = (p: string) => import(pathToFileURL(join(oracle, p)).href);
const { DefaultContextRuntime } = await load('src/context/DefaultContextRuntime.ts');
const { EdgeClawMemoryProvider } = await load('src/context/memory/EdgeClawMemoryProvider.ts');
const { EdgeClawMemoryService } = await load('src/context/memory/edgeclaw-memory-core/src/service.ts');
const { createSidecarDefaultModuleDispatcher } = await load('src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts');
const dbPath = join(output, 'control.sqlite');
const service = new EdgeClawMemoryService({ workspaceDir: join(output, 'workspace'), rootDir: join(output, 'root'), dbPath, memoryDir: join(output, 'memory') });
const provider = new EdgeClawMemoryProvider({ service, now: () => new Date('2026-10-12T00:00:00Z') });
const context = new DefaultContextRuntime({ memoryResolver: provider, projectRoot: join(output, 'workspace') });
const abort = new AbortController(); abort.abort('user cancel');
const input = { sessionId: 'controlled-memory', turnId: 'turn', abortSignal: abort.signal, execution: { runId: 'run', operationId: 'op', operationDeadline: '2000-01-01T00:00:00Z' } };
const dispatcher = createSidecarDefaultModuleDispatcher({
  config: { cwd: join(output, 'workspace'), env: {} }, input, checkpoint: {},
  modules: {
    model: {},
    capability: { execution: { list: () => [] }, runtimeContext: { bindTurn: () => ({ permissionContext: () => ({}) }) } },
    context: { execution: context, requestIdentity: { bindTurn: () => ({ contextIdentity: (value: unknown) => value }) } },
  }, capabilityResultObserver: () => {}, planTodoHandler: () => {},
});
const messages = [
  { id: 'user', role: 'user', content: [{ type: 'text', text: 'Controlled preference: remember the indigo lamp.' }] },
  { id: 'assistant', role: 'assistant', content: [{ type: 'text', text: 'Safe partial reply', blockId: 'block-1' }] },
  { id: 'fork', role: 'user', content: [{ type: 'text', text: 'Fork carryover must not be captured.' }], metadata: { forkCarryover: true } },
];
const call = (sessionId = input.sessionId) => dispatcher.handlers.context({ kind: 'request', method: 'module_call', messageId: 'capture', module: 'context', runId: 'run', operationId: 'op', requestId: 'capture-request', payload: { operation: 'capture_turn', input: { sessionId, turnId: 'turn', messages, errored: false } } });
const db = new DatabaseSync(dbPath);
const rows = () => db.prepare('SELECT * FROM l0_sessions').all();
assert.equal(rows().length, 0);
assert.deepEqual(await call(), { result: null });
assert.equal(rows().length, 1);
const first = JSON.stringify(rows());
assert(first.includes('indigo lamp') && first.includes('Safe partial reply'));
assert(!first.includes('Fork carryover'));
await call(); assert.equal(rows().length, 1, 'production duplicate suppression');
await call('always-on/discovery:controlled'); assert.equal(rows().length, 1, 'always-on exclusion');
await new DefaultContextRuntime({ projectRoot: output }).captureTurn({ sessionId: 'absent', turnId: 'turn', messages, errored: false });
assert.equal(rows().length, 1, 'absent provider');
console.log(JSON.stringify({ passed: true, pid: process.pid, dbPath, rows: rows().length, expiredExecutionDeadline: input.execution.operationDeadline, aborted: abort.signal.aborted, chain: ['real dispatcher', 'DefaultContextRuntime', 'EdgeClawMemoryProvider', 'EdgeClawMemoryService', 'HeartbeatIndexer', 'MemoryRepository', 'SQLite'], gap: 'This fixture directly invokes the genuine host dispatcher; it does not prove sidecarClient pending-model scheduling or transport-to-SQLite integration.' }));
await dispatcher.dispose(); db.close(); service.repository.close();
