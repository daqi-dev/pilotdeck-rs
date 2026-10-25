// Direct imports from the fixed, read-only baseline; no copied implementation.
// node --import <baseline>/node_modules/tsx/dist/loader.mjs probe.mjs <baseline> <scratch>
import { mkdir, readFile, writeFile, rm, open, rename } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
const [baseline, scratchArg] = process.argv.slice(2);
const root = resolve(scratchArg);
const commit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(commit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const load = p => import(pathToFileURL(join(baseline, 'src', p)).href);
const { JsonlSessionEventStore } = await load('session/events/JsonlSessionEventStore.ts');
const { InMemorySessionEventStore } = await load('session/events/InMemorySessionEventStore.ts');
const { resolveWorkspaceSnapshotConfig, ContentAddressedWorkspaceSnapshotRecorder, createWorkspaceSnapshotProvider } = await load('storage/workspaceSnapshot.ts');
await mkdir(root, { recursive: true });
const path = join(root, 'events.jsonl');
let next = 0;
const options = { path, now: () => new Date('2026-10-25T00:00:00.000Z'), uuid: () => `entry-${++next}` };
const store = new JsonlSessionEventStore(options);
await store.append('s', 't1', { type: 'turn_started' });
await store.flush();
const bytes = await readFile(path, 'utf8');
const restarted = new JsonlSessionEventStore(options);
const read = await restarted.read();
await restarted.restore(read.entries);
assert.equal(await readFile(path, 'utf8'), bytes);
await restarted.append('s', 't2', { type: 'turn_started' });
const fullBytes = await readFile(path, 'utf8');
// Inject crash residue after complete events; this is not a syscall short-write or process-kill test.
await writeFile(path, fullBytes + '{"type":');
const torn = new JsonlSessionEventStore(options);
const partial = await torn.read();
await torn.restore(partial.entries);
await torn.append('s', 't3', { type: 'turn_started' });
const repaired = await torn.read();
const repairedBytes = await readFile(path, 'utf8');
assert(repairedBytes.startsWith(fullBytes + '{"type":\n'));
const badParent = join(root, 'blocker');
await writeFile(badParent, 'file');
const failing = new JsonlSessionEventStore({ ...options, path: join(badParent, 'events.jsonl') });
let appendError, appendMessage;
try { await failing.append('s', 'failed', { type: 'turn_started' }); } catch (e) { appendError = e.code; appendMessage = e.message.replaceAll(root, '<ROOT>'); }
assert(appendError);
const stateAfterFailure = failing.snapshotState();
await rm(badParent);
await failing.append('s', 'recovered', { type: 'turn_started' });
const recovered = await failing.read();
await rename(badParent, join(root, 'saved'));
await writeFile(badParent, 'blocker');
let midError;
try { await failing.append('s', 'retry', { type: 'turn_started' }); } catch (e) { midError = e.code; }
const midState = failing.snapshotState();
await rm(badParent); await rename(join(root, 'saved'), badParent);
await failing.append('s', 'retry', { type: 'turn_started' });
const retryState = failing.snapshotState();

const largePath = join(root, 'large.jsonl');
const largeFile = await open(largePath, 'w');
await largeFile.truncate(50 * 1024 * 1024 + 1); await largeFile.close();
const large = new JsonlSessionEventStore({ ...options, path: largePath });
const largeRead = await large.read(); await large.restore(largeRead.entries);
await large.append('s', 'oversized', { type: 'turn_started' });
const oversizedState = large.snapshotState();
const memory = new InMemorySessionEventStore(options);
await memory.append('s', 'm', { type: 'turn_started' });
const memoryRead = await memory.read();
const missing = resolveWorkspaceSnapshotConfig({ PILOTDECK_SNAPSHOT_ROOT: join(root, 'ignored') });
const blank = resolveWorkspaceSnapshotConfig({ PILOTDECK_STORAGE_ROOT: '  ' });
assert.equal(missing, undefined); assert.equal(blank, undefined);
const config = resolveWorkspaceSnapshotConfig({ PILOTDECK_STORAGE_ROOT: join(root, 'storage'), PILOTDECK_SNAPSHOT_ROOT: join(root, 'snapshots') });
const workspaceDir = join(root, 'workspace');
await mkdir(workspaceDir);
const payload = Buffer.from([0, 255, 10, 228, 184, 173]);
await writeFile(join(workspaceDir, 'payload.bin'), payload);
const input = { workspaceId: 'w', sessionId: 's', turnId: 't', runId: 'r', workspaceDir };
const snapshot = new ContentAddressedWorkspaceSnapshotRecorder(config);
const captured = await snapshot.capturePreUser(input);
const reopenedCapture = await new ContentAddressedWorkspaceSnapshotRecorder(config).capturePreUser(input);
assert.deepEqual(reopenedCapture, captured);
const provider = createWorkspaceSnapshotProvider(config);
await provider.restore(captured.snapshotId, join(root, 'restored'));
assert.deepEqual(await readFile(join(root, 'restored/payload.bin')), payload);
const failed = await snapshot.capturePreUser({ ...input, turnId: 'failed', workspaceStable: false });
const failureMarker = JSON.parse(await readFile(failed.failureMarkerPath, 'utf8'));
const missingWorkspace = await snapshot.capturePreUser({ ...input, turnId: 'missing', workspaceDir: join(root, 'absent') });
await writeFile(join(root, 'blocked-root'), 'file');
const hard = new ContentAddressedWorkspaceSnapshotRecorder({ root, snapshotRoot: join(root, 'blocked-root') });
let hardError;
try { await hard.capturePreUser(input); } catch (e) { hardError = e.code; }
assert(hardError);
console.log(JSON.stringify({
  baseline: commit,
  events: { firstBytes: bytes, fullBytes, repairedBytes, partialCodes: partial.diagnostics.map(d => d.code), repairedCount: repaired.entries.length, appendError, appendMessage, stateAfterFailure, midError, midSequence: midState.sequence, retrySequence: retryState.sequence, recoveredSequence: recovered.entries[0].sequence, memoryCount: memoryRead.entries.length, oversizedCodes: largeRead.diagnostics.map(d => d.code), oversizedSequence: oversizedState.sequence },
  snapshots: { disabled: missing === undefined && blank === undefined, state: captured.state, snapshotId: captured.snapshotId, payloadHex: payload.toString('hex'), failedState: failed.state, failureError: failureMarker.error, missingState: missingWorkspace.state, hardError }
}));
