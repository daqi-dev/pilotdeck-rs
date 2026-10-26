// Exercise the fixed baseline's original exported entry points. Each child
// changes cwd only after imports, and never warms process.cwd() before removal.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0');
assert.ok(process.env.PDRS_BASELINE);
const baseline = resolve(process.env.PDRS_BASELINE);
const sources = [
  'src/gateway/storage/AsyncTranscriptStorageAdapter.ts',
  'src/session/storage/ProjectSessionStorage.ts',
  'src/session/transcript/JsonlTranscriptWriter.ts',
  'src/pilot/paths.ts',
];
const cases = ['relative-normal', 'relative-deleted', 'absolute-normal',
  'absolute-deleted', 'project-relative-deleted', 'join-relative-normal', 'join-relative-deleted'];

if (process.argv[2] === '--child') {
  const name = process.argv[3];
  assert.ok(cases.includes(name));
  const { createGatewayAsyncTranscriptStorageAdapter } = await import(pathToFileURL(join(baseline, sources[0])).href);
  const { JsonlTranscriptWriter } = await import(pathToFileURL(join(baseline, sources[2])).href);
  const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-transcript-resolve-')));
  const cwd = join(root, 'cwd');
  await mkdir(cwd);
  const absolute = name.startsWith('absolute') || name.startsWith('project');
  const input = {
    projectRoot: name.startsWith('project') ? 'project/../project' : '/pdrs-fixture/project',
    pilotHome: absolute ? `${root}/home/../home` : 'relative/../home',
    sessionId: '/s/../one/',
  };
  const adapter = createGatewayAsyncTranscriptStorageAdapter({ store: {
    append() { throw new Error('unexpected append'); },
    async read() { throw new Error('unexpected read'); },
  } });
  let outcome: any;
  process.chdir(cwd);
  if (name.endsWith('deleted')) await rm(cwd, { recursive: true });
  try {
    if (name.startsWith('join-')) {
      const writer = new JsonlTranscriptWriter({ path: 'logs/../logs/s.jsonl', appendEntry() {} });
      outcome = { transcriptPath: writer.forSubagent('kid').transcriptPath };
    } else {
      const storage = adapter.createSessionStorage(input);
      const child = storage.transcript.forSubagent('kid');
      outcome = Object.fromEntries([
        ...['chatDir', 'transcriptPath', 'toolResultsDir', 'fileHistoryDir', 'subagentsDir', 'projectionCheckpointPath']
          .map(key => [key, storage[key]]),
        ['subagentPath', child.transcriptPath],
        ['nestedSubagentPath', child.writer.forSubagent('grandchild').transcriptPath],
      ]);
      await storage.dispose();
    }
  } catch (error: any) {
    outcome = { error: { name: error.name, code: error.code, message: error.message } };
  } finally {
    process.chdir(root);
    await rm(root, { recursive: true });
  }
  // Only the fresh scratch prefix varies; relative/absolute semantics and
  // every suffix/error byte are retained.
  process.stdout.write(JSON.stringify({ name, outcome }).split(root).join('<root>') + '\n');
} else {
  assert.ok(process.argv[2], 'output directory required');
  const output = resolve(process.argv[2]);
  const sourceCommit = assertBaseline(baseline);
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const rows = cases.map(name => {
    const raw = execFileSync(process.execPath, [
      '--disable-warning=ExperimentalWarning', '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'),
      fileURLToPath(import.meta.url), '--child', name,
    ], { encoding: 'utf8', timeout: 30_000 });
    process.stdout.write(raw);
    return JSON.parse(raw);
  });
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), JSON.stringify({
    node: process.version, sourceCommit, sourceSha256, cases: rows,
  }, null, 2) + '\n', { flag: 'wx' });
}
