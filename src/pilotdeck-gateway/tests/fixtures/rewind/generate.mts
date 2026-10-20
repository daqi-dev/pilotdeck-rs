// Real TS recordings for A6.5-pr6 `rewindFilesForSdk`.
//
// Run from the fixed baseline checkout with Node 22:
//   PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
//     node --import /abs/.../tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/rewind/generate.mts /abs/output-parent
//
// Each scenario owns its own mkdtemp tree. Real `write_file` / `edit_file` /
// `read_file` tools produce the snapshots through the real `FileHistoryStore`;
// the real `JsonlTranscriptWriter` records `file_snapshot_recorded`; and the
// unmodified `ProjectRuntimeRegistry.rewindFilesForSdk` runs against a real
// `createAgentProjectSessionStorage`. Only `resolve` (which project root) and
// `createPersistentSessionStorage` (runtime.projectStorage) are stubbed.
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { mkdirSync, realpathSync } from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide an output parent directory; each run writes into its own mkdtemp child');
const baseline = resolve(process.env.PDRS_BASELINE!);
const outputParent = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
bindSourceTree(baseline, commit, git);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const load = (name: string) => import(pathToFileURL(join(baseline, 'src', name)).href);
const { ProjectRuntimeRegistry } = await load('cli/ProjectRuntimeRegistry.ts');
const { createAgentProjectSessionStorage } = await load('session/storage/ProjectSessionStorage.ts');
const { JsonlTranscriptWriter } = await load('session/transcript/JsonlTranscriptWriter.ts');
const { readTranscript } = await load('session/transcript/TranscriptReader.ts');
const { FileHistoryStore } = await load('session/filesystem/FileHistoryStore.ts');
const { getBackupFileName } = await load('session/filesystem/backupNaming.ts');
const { createWriteFileTool } = await load('tool/builtin/writeFile.ts');
const { createEditFileTool } = await load('tool/builtin/editFile.ts');
const { createReadFileTool } = await load('tool/builtin/readFile.ts');

const sourceSha256 = sourceClosureSha256(baseline, commit, git, [
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/session/filesystem/FileHistoryStore.ts',
]);

const scratch = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-rewind-'));
const FIXED_MS = Date.parse('2026-01-01T00:00:00.000Z');
const fixedNow = () => new Date(FIXED_MS);
const SESSION = 'session-1';

const clean = (value: unknown) => value === undefined ? null : JSON.parse(JSON.stringify(value));
const replaceRoot = (value: any, root: string): any => {
  if (typeof value === 'string') return value.split(root).join('<root>');
  if (Array.isArray(value)) return value.map((item) => replaceRoot(item, root));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [replaceRoot(key, root), replaceRoot(item, root)]));
  }
  return value;
};
const substitute = (value: any, root: string): any => {
  if (typeof value === 'string') return value.split('<root>').join(root);
  if (Array.isArray(value)) return value.map((item) => substitute(item, root));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [substitute(key, root), substitute(item, root)]));
  }
  return value;
};

const toolOf = (name: string) => name === 'write_file' ? createWriteFileTool()
  : name === 'edit_file' ? createEditFileTool()
  : name === 'read_file' ? createReadFileTool()
  : (() => { throw new Error(`unknown tool ${name}`); })();

async function applyFile(root: string, path: string, base64: string, mode: number): Promise<void> {
  const target = resolve(root, path);
  await mkdir(dirname(target), { recursive: true });
  await writeFile(target, Buffer.from(base64, 'base64'));
  await chmod(target, mode);
}

async function walk(root: string, prefix = ''): Promise<any[]> {
  let entries;
  try { entries = await readdir(root, { withFileTypes: true }); } catch (error: any) {
    if (error?.code === 'ENOENT') return [];
    throw error;
  }
  const rows: any[] = [];
  for (const entry of entries.sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0)) {
    const absolute = join(root, entry.name);
    const name = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) rows.push(...await walk(absolute, name));
    else rows.push({ name, base64: (await readFile(absolute)).toString('base64'), mode: (await stat(absolute)).mode & 0o777 });
  }
  return rows;
}

const scenarios: any[] = [];
let caseIndex = 0;

function begin(name: string, options: { trackOnly?: boolean } = {}) {
  const caseRoot = join(scratch, `case-${caseIndex++}`);
  mkdirSync(caseRoot);
  const projectRoot = join(caseRoot, 'project');
  const pilotHome = join(caseRoot, 'pilot');
  mkdirSync(projectRoot);
  mkdirSync(pilotHome);
  const storage = createAgentProjectSessionStorage({ projectRoot, pilotHome, sessionId: SESSION, now: fixedNow });
  let uuid = 0;
  const writer = new JsonlTranscriptWriter({
    path: storage.transcriptPath,
    now: fixedNow,
    uuid: () => `entry-${String(++uuid).padStart(4, '0')}`,
  });
  const recorded: any[] = [];
  const store = new FileHistoryStore({
    backupDir: storage.fileHistoryDir,
    now: fixedNow,
    onSnapshotRecorded: (snapshot: any, kind: string) => {
      recorded.push({ kind, snapshot });
      return writer.recordFileHistorySnapshot(SESSION, snapshot.messageId, snapshot, kind);
    },
  });
  // `markEditCommitted` is optional in the tool port. `trackOnly` is a host
  // wiring that records backups but no expected fingerprint, which is the only
  // real-TS path where a file deleted after the snapshot rewinds without a
  // conflict instead of being refused.
  const fileHistory = options.trackOnly
    ? { trackEdit: (path: string, messageId: string) => store.trackEdit(path, messageId) }
    : store;
  const context: any = {
    cwd: projectRoot,
    permissionContext: { mode: 'default', canPrompt: true, additionalWorkingDirectories: [] },
    permissionMode: 'default',
    readFileState: new Map(),
    writeSnapshots: new Map(),
    allowedReadFiles: [],
    env: { PILOT_HOME: pilotHome, HOME: caseRoot },
    now: fixedNow,
    messageId: 'turn-0',
    fileHistory,
  };
  const scenario: any = {
    name,
    initialFiles: [],
    mutations: [],
    transcript: [],
    backups: [],
    missingBackups: [],
    calls: [],
    treeBefore: [],
    __caseRoot: caseRoot,
    __projectRoot: projectRoot,
    __pilotHome: pilotHome,
    __storage: storage,
    __writer: writer,
    __store: store,
    __context: context,
    __recorded: recorded,
  };
  return scenario;
}

async function seed(run: any, path: string, content: string, mode = 0o644) {
  const base64 = Buffer.from(content, 'utf8').toString('base64');
  run.initialFiles.push({ path, base64, mode });
  await applyFile(run.__projectRoot, path, base64, mode);
}

async function mutateFile(run: any, path: string, content: string, mode = 0o644) {
  const base64 = Buffer.from(content, 'utf8').toString('base64');
  run.mutations.push({ op: 'file', path, base64, mode });
  await applyFile(run.__projectRoot, path, base64, mode);
}

async function mutateRemove(run: any, path: string) {
  run.mutations.push({ op: 'remove', path });
  await rm(resolve(run.__projectRoot, path), { force: true });
}

async function callTool(run: any, turnId: string, tool: string, input: any) {
  run.__context.messageId = turnId;
  const definition: any = toolOf(tool);
  const validation = await definition.validateInput(input, run.__context);
  assert.equal(validation.ok, true, `${run.name}: ${tool} input rejected`);
  await definition.execute(input, run.__context);
}

async function snapshotFiles(run: any): Promise<any[]> {
  return walk(run.__projectRoot);
}

function registryFor(run: any) {
  const registry: any = Object.create(ProjectRuntimeRegistry.prototype);
  registry.options = { now: fixedNow };
  registry.resolve = (_projectKey: string) => ({ projectRoot: run.__projectRoot });
  registry.createPersistentSessionStorage = (projectRoot: string, sessionId: string, now: () => Date) =>
    createAgentProjectSessionStorage({
      projectRoot,
      pilotHome: run.__pilotHome,
      sessionId,
      now: now ?? fixedNow,
    });
  return registry;
}

async function finish(run: any, inputs: Array<{ userMessageId: string; dryRun: boolean }>) {
  const root = run.__projectRoot;
  const transcriptRead = await readTranscript(run.__storage.transcriptPath);
  const transcript = transcriptRead.entries;

  // Map the run-specific backup hashes to path-stable tokens before the
  // oracle is written; the Rust replay writes the blob under the same token.
  const nameMap = new Map<string, string>();
  for (const entry of transcript as any[]) {
    if (entry.type !== 'file_snapshot_recorded') continue;
    for (const [path, backup] of Object.entries(entry.trackedFileBackups ?? {}) as [string, any][]) {
      if (typeof backup?.backupFileName !== 'string') continue;
      const relativePath = relative(root, path).split(sep).join('/');
      nameMap.set(backup.backupFileName, `<backup:${relativePath}>@v${backup.version}`);
    }
  }
  const normalizeBackup = (name: string | null, path?: string, version?: number): string | null => {
    if (name === null) return null;
    const mapped = nameMap.get(name);
    if (mapped) return mapped;
    const relativePath = path ? relative(root, path).split(sep).join('/') : 'unknown';
    return `<backup:${relativePath}>@v${version ?? 0}`;
  };

  const backups: any[] = [];
  for (const file of await walk(run.__storage.fileHistoryDir)) {
    const name = normalizeBackup(file.name);
    backups.push({ name, base64: file.base64, mode: file.mode });
  }
  backups.sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0);

  run.transcript = (transcript as any[]).map((entry) => {
    const copy = clean(entry);
    if (copy.type === 'file_snapshot_recorded') {
      for (const [path, backup] of Object.entries(copy.trackedFileBackups ?? {}) as [string, any][]) {
        backup.backupFileName = normalizeBackup(backup.backupFileName, path, backup.version);
      }
    }
    return replaceRoot(copy, root);
  });
  run.backups = backups;
  run.treeBefore = await snapshotFiles(run);

  const registry = registryFor(run);
  for (const input of inputs) {
    const result = await registry.rewindFilesForSdk({ sessionKey: SESSION, projectKey: root, ...input });
    run.calls.push({
      userMessageId: input.userMessageId,
      dryRun: input.dryRun,
      resultJson: JSON.stringify(replaceRoot(result, root)),
      treeAfter: await snapshotFiles(run),
    });
  }
  await run.__storage.dispose();
  delete run.__caseRoot;
  delete run.__projectRoot;
  delete run.__pilotHome;
  delete run.__storage;
  delete run.__writer;
  delete run.__store;
  delete run.__context;
  delete run.__recorded;
  scenarios.push(run);
}

// 1. Two files edited in one turn; dry run leaves the tree alone, then the real
//    rewind restores both v1 backups. Aggregate line delta is 2 / 2.
{
  const run = begin('success-multi-file');
  await seed(run, 'a.txt', 'alpha\nbeta\n');
  await seed(run, 'b.txt', 'bee\n');
  await callTool(run, 'msg-1', 'read_file', { file_path: 'a.txt' });
  await callTool(run, 'msg-1', 'edit_file', { file_path: 'a.txt', old_string: 'beta', new_string: 'BETA' });
  await callTool(run, 'msg-1', 'read_file', { file_path: 'b.txt' });
  await callTool(run, 'msg-1', 'edit_file', { file_path: 'b.txt', old_string: 'bee', new_string: 'BEE' });
  await finish(run, [
    { userMessageId: 'msg-1', dryRun: true },
    { userMessageId: 'msg-1', dryRun: false },
  ]);
}

// 2. External edit after the checkpoint fingerprint: refused before any write,
//    for both dry run and real rewind.
{
  const run = begin('conflict-external-edit');
  await seed(run, 'a.txt', 'one\n');
  await callTool(run, 'msg-1', 'read_file', { file_path: 'a.txt' });
  await callTool(run, 'msg-1', 'edit_file', { file_path: 'a.txt', old_string: 'one', new_string: 'two' });
  await mutateFile(run, 'a.txt', 'external\n');
  await finish(run, [
    { userMessageId: 'msg-1', dryRun: true },
    { userMessageId: 'msg-1', dryRun: false },
  ]);
}

// 3. The file is deleted after a track-only snapshot (no expected fingerprint),
//    so rewind recreates it from the backup; deletions count the backup lines.
{
  const run = begin('deleted-file-restore', { trackOnly: true });
  await seed(run, 'a.txt', 'alpha\nbeta\n');
  await callTool(run, 'msg-1', 'read_file', { file_path: 'a.txt' });
  await callTool(run, 'msg-1', 'edit_file', { file_path: 'a.txt', old_string: 'beta', new_string: 'BETA' });
  await mutateRemove(run, 'a.txt');
  await finish(run, [{ userMessageId: 'msg-1', dryRun: false }]);
}

// 4. Snapshots exist but the requested message id does not: the store throws
//    `No snapshot for messageId ...` and the SDK method returns it as `error`.
{
  const run = begin('unknown-message-id');
  await seed(run, 'a.txt', 'x\n');
  await callTool(run, 'msg-1', 'read_file', { file_path: 'a.txt' });
  await callTool(run, 'msg-1', 'edit_file', { file_path: 'a.txt', old_string: 'x', new_string: 'y' });
  await finish(run, [
    { userMessageId: 'msg-missing', dryRun: false },
    { userMessageId: 'msg-missing', dryRun: true },
  ]);
}

// 5. A live transcript without any file checkpoint.
{
  const run = begin('no-checkpoint');
  await run.__writer.recordAcceptedInput(SESSION, 'turn-1', []);
  await finish(run, [{ userMessageId: 'msg-1', dryRun: false }]);
}

// 6. Rewinding an older message only touches that message's snapshot; a later
//    turn on a different file stays in place.
{
  const run = begin('older-snapshot-isolation');
  await seed(run, 'a.txt', 'a0\n');
  await seed(run, 'b.txt', 'b0\n');
  await callTool(run, 'msg-1', 'read_file', { file_path: 'a.txt' });
  await callTool(run, 'msg-1', 'edit_file', { file_path: 'a.txt', old_string: 'a0', new_string: 'a1' });
  await callTool(run, 'msg-2', 'read_file', { file_path: 'b.txt' });
  await callTool(run, 'msg-2', 'edit_file', { file_path: 'b.txt', old_string: 'b0', new_string: 'b1' });
  await finish(run, [{ userMessageId: 'msg-1', dryRun: false }]);
}

// 7. The backup blob is gone: conflicts are clean, diff treats the before side
//    as absent, and rewind reports the path under `missing` without writing.
{
  const run = begin('missing-backup');
  await seed(run, 'a.txt', 'one\n');
  await callTool(run, 'msg-1', 'read_file', { file_path: 'a.txt' });
  await callTool(run, 'msg-1', 'edit_file', { file_path: 'a.txt', old_string: 'one', new_string: 'two' });
  const backupName = getBackupFileName(resolve(run.__projectRoot, 'a.txt'), 1);
  await rm(join(run.__storage.fileHistoryDir, backupName));
  run.missingBackups.push(`<backup:a.txt>@v1`);
  await finish(run, [{ userMessageId: 'msg-1', dryRun: false }]);
}

assert.equal(scenarios.length, 7, 'scenario count');
assert.equal(scenarios.reduce((total: number, run: any) => total + run.calls.length, 0), 10, 'call count');

const oracle = {
  sourceCommit: commit,
  sourceSha256,
  locales: [locale],
  scenarios,
  sha256: createHash('sha256').update(JSON.stringify({ sourceCommit: commit, sourceSha256, scenarios })).digest('hex'),
};
const output = join(outputParent, `rewind-oracle-${process.pid}-${Date.now()}.json`);
await writeFile(output, `${JSON.stringify(oracle, null, 2)}\n`, { flag: 'wx' });
console.log(output);
// The run-owned tree is removed only after the oracle is written.
await rm(scratch, { recursive: true, force: true });
