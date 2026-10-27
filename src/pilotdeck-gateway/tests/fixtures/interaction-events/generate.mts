import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Execute the complete, unmodified baseline module; no source extraction.
const baseline = process.env.PDRS_BASELINE ?? '/Users/da/ws/pilotdeck-rs/resources/PilotDeck';
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, ['src/agent/protocol/events.ts']);
if (!process.argv[2]) throw new Error('#303 provide a provenance output directory');
const output = resolve(process.argv[2]);
await mkdir(output, { recursive: true });
await writeFile(join(output, 'source.json'), JSON.stringify({ sourceCommit, sourceSha256 }, null, 2) + '\n', { flag: 'wx' });
const { createAgentEventBuffer } = await import(`${baseline}/src/agent/protocol/events.ts`);
const buffer = createAgentEventBuffer();
const first = {type: 'text_delta', sessionId: 'session', turnId: 'turn', text: '中文'};
const second = {type: '', sessionId: 'session'};
buffer.emitter(first as never);
buffer.emitter(second as never);
console.log(JSON.stringify([buffer.drain(), buffer.drain()]));
