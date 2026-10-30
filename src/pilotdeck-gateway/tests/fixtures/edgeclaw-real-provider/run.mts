// #1120: import the fixed TS service, with no stubs or source transformations.
import assert from 'node:assert/strict';
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindBaseline} from '../_shared/generator_guard.mjs';

assert.equal(process.version, 'v22.22.0', '#1120 exact baseline Node version');
const cell = resolve(process.env.EDGECLAW_SMOKE_CELL!);
const baseline = resolve(process.env.PDRS_BASELINE!);
const {provenance} = bindBaseline({baseline,
  commit: 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'});
const input = JSON.parse(readFileSync(join(cell, 'input.json'), 'utf8'));
const save = (name: string, value: unknown) => {
  mkdirSync(cell, {recursive: true});
  writeFileSync(join(cell, name), JSON.stringify(value, null, 2) + '\n', {flag: 'wx'});
};
save('node.json', {node: process.version, entry: 'EdgeClawMemoryService', provenance});
const {EdgeClawMemoryService} = await import(pathToFileURL(join(baseline,
  'src/context/memory/edgeclaw-memory-core/src/service.ts')).href);
const service = new EdgeClawMemoryService({
  workspaceDir: join(cell, 'project'), rootDir: join(cell, 'memory'),
  captureStrategy: 'full_session', includeAssistant: true,
  source: 'issue-1120-synthetic',
  llm: {provider: 'smoke', model: process.env.EDGECLAW_SMOKE_MODEL,
    apiType: 'anthropic', baseUrl: process.env.EDGECLAW_SMOKE_URL,
    apiKey: process.env.EDGECLAW_SMOKE_TOKEN},
});
try {
  save('capture.json', service.captureTurn(input.messages, {sessionKey: 'capture-session'}));
  save('before-flush.json', service.snapshot());
  save('flush.json', await service.flush({sessionKeys: ['capture-session'], reason: 'manual'}));
  const entries = service.list({});
  save('entries.json', entries);
  save('records.json', service.get(entries.map((entry: any) => {
    assert.equal(typeof entry.relativePath, 'string', '#1120 memory entry requires relativePath');
    return entry.relativePath;
  }), 10000));
  save('user-summary.json', service.getUserSummary());
  save('after-flush.json', service.snapshot());
  save('index-traces.json', service.listIndexTraces(100));
  save('recall.json', await service.retrieveContext(input.query, {
    recentMessages: [], workspaceHint: '', retrievalMode: 'auto',
  }));
  console.log('#1120 native capture/flush/recall complete');
} finally {
  service.close();
}
