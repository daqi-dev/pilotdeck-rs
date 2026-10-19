// Real TS recordings for the A6.5-pr5 write tools + FileHistoryStore.
// Run with the fixed baseline checkout and Node 22:
//   PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
//     node --import /abs/.../tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/write-tools/generate.mts /abs/output-parent
import { mkdtemp, mkdir, readFile, realpath, rm, symlink, link, writeFile, lstat, chmod, utimes, readdir, stat } from 'node:fs/promises';
import { mkdirSync, readFileSync, realpathSync } from 'node:fs';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
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
const bound = bindSourceTree(baseline, commit, git);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const load = (name: string) => import(pathToFileURL(join(baseline, 'src', name)).href);
const { createWriteFileTool } = await load('tool/builtin/writeFile.ts');
const { createEditFileTool } = await load('tool/builtin/editFile.ts');
const { createEditNotebookTool } = await load('tool/builtin/editNotebook.ts');
const { createReadFileTool } = await load('tool/builtin/readFile.ts');
const { FileHistoryStore } = await load('session/filesystem/FileHistoryStore.ts');
const { getBackupFileName } = await load('session/filesystem/backupNaming.ts');

const sourceSha256 = sourceClosureSha256(baseline, commit, git, [
  'src/tool/builtin/writeFile.ts',
  'src/tool/builtin/editFile.ts',
  'src/tool/builtin/editNotebook.ts',
  'src/tool/builtin/readFile.ts',
  'src/session/filesystem/FileHistoryStore.ts',
]);

const scratch = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-write-tools-'));
const outside = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-write-tools-outside-'));
let currentRoot = scratch;
const fixed = Date.parse('2026-01-01T00:00:00.000Z');

const clean = (value: unknown) => value === undefined ? null : JSON.parse(JSON.stringify(value));
const outsideName = basename(outside);
const replaceRoot = (value: any): any => {
  if (typeof value === 'string') return value.split(currentRoot).join('<root>').split(outside).join('<outside>').split(outsideName).join('<outside>');
  if (Array.isArray(value)) return value.map(replaceRoot);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [replaceRoot(key), replaceRoot(item)]));
  return value;
};
const normalizeBackupName = (name: string | null | undefined): string | null => {
  if (name === null || name === undefined) return null;
  const match = /^[0-9a-f]{16}@v(\d+)$/.exec(name);
  assert.ok(match, `unexpected backup file name ${name}`);
  return `<backup>@v${match[1]}`;
};
const substitute = (value: any): any => {
  if (typeof value === 'string') return value.split('<root>').join(currentRoot).split('<outside>').join(outside);
  if (Array.isArray(value)) return value.map(substitute);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [substitute(key), substitute(item)]));
  return value;
};
const errorOf = (error: any) => ({
  code: error?.name === 'PilotDeckToolRuntimeError' ? error.code : 'tool_execution_failed',
  message: error?.message ?? String(error),
});

type Step = any;
async function apply(step: Step): Promise<void> {
  const target = resolve(currentRoot, step.path);
  if (step.op === 'file') {
    await mkdir(dirname(target), { recursive: true });
    const content = step.base64 !== undefined ? Buffer.from(step.base64, 'base64') : step.content;
    await writeFile(target, content);
    if (step.mtime !== undefined) await utimes(target, new Date(step.mtime), new Date(step.mtime));
    await chmod(target, step.mode ?? 0o644);
  } else if (step.op === 'dir') {
    await mkdir(target, { recursive: true });
  } else if (step.op === 'symlink') {
    await mkdir(dirname(target), { recursive: true });
    try { await symlink(String(step.target).split('<outside>').join(outside), target); } catch (error: any) {
      if (error?.code !== 'EEXIST') throw error;
    }
  } else if (step.op === 'hardlink') {
    await mkdir(dirname(target), { recursive: true });
    await link(resolve(currentRoot, step.target), target);
  } else if (step.op === 'chmod') {
    await chmod(target, step.mode);
  } else if (step.op === 'remove') {
    await rm(target, { force: true, recursive: true });
  } else {
    throw new Error(`unknown setup step ${JSON.stringify(step)}`);
  }
}

const toolOf = (name: string) => name === 'write_file' ? createWriteFileTool()
  : name === 'edit_file' ? createEditFileTool()
  : name === 'edit_notebook' ? createEditNotebookTool()
  : name === 'read_file' ? createReadFileTool()
  : (() => { throw new Error(`unknown tool ${name}`); })();

function createHistory(scenario: any) {
  const backupDir = resolve(currentRoot, scenario.backupDir ?? '.file-history');
  const records: any[] = [];
  const warnings: string[] = [];
  const ops: any[] = [];
  const store = new FileHistoryStore({
    backupDir,
    ...(scenario.maxFileBytes !== undefined ? { maxFileBytes: scenario.maxFileBytes } : {}),
    ...(scenario.maxSnapshots !== undefined ? { maxSnapshots: scenario.maxSnapshots } : {}),
    now: () => new Date(fixed),
    onSnapshotRecorded: (entry: any, kind: string) => { records.push(clean({ kind, entry })); },
    warn: (message: string) => { warnings.push(message); },
  });
  return { store, records, warnings, ops, backupDir };
}

async function walk(dir: string, prefix = ''): Promise<any[]> {
  let entries;
  try { entries = await readdir(dir, { withFileTypes: true }); } catch (error: any) {
    if (error?.code === 'ENOENT') return [];
    throw error;
  }
  const rows: any[] = [];
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name, 'en'))) {
    const path = join(dir, entry.name);
    const name = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) rows.push(...await walk(path, name));
    else rows.push({
      name: normalizeBackupName(name),
      base64: (await readFile(path)).toString('base64'),
      mode: (await stat(path)).mode & 0o777,
    });
  }
  return rows;
}

async function captureHistory(history: any, scenario: any): Promise<any> {
  const state = history.store.getState();
  return {
    records: history.records.map((record: any) => {
      const copy = JSON.parse(JSON.stringify(record));
      for (const [file, backup] of Object.entries(copy.entry.trackedFileBackups ?? {}) as [string, any][]) {
        backup.backupFileName = normalizeBackupName(backup.backupFileName);
        if (backup.mode === undefined) delete backup.mode;
      }
      for (const [file, state] of Object.entries(copy.entry.expectedFileStates ?? {}) as [string, any][]) {
        if (state.exists && scenario.__generatedIds.length > 0 && file.endsWith('.ipynb')) {
          let text = readFileSync(file, 'utf8');
          for (const id of scenario.__generatedIds) text = text.split(id).join('<cellid>');
          state.sha256 = createHash('sha256').update(text, 'utf8').digest('hex');
        }
      }
      return replaceRoot(copy);
    }),
    warnings: history.warnings.map((message: string) => replaceRoot(message)),
    ops: history.ops.map((op: any) => replaceRoot(op)),
    tree: (await walk(history.backupDir)).sort((a, b) => a.name.localeCompare(b.name, 'en') || a.base64.localeCompare(b.base64, 'en')),
    snapshots: state.snapshots.map((snapshot: any) => ({
      messageId: snapshot.messageId,
      timestamp: snapshot.timestamp.toISOString(),
      tracked: Object.entries(snapshot.trackedFileBackups).map(([path, backup]: [string, any]) => ({
        path: replaceRoot(path),
        backupFileName: normalizeBackupName(backup.backupFileName),
        version: backup.version,
        backupTime: backup.backupTime.toISOString(),
        ...(backup.mode !== undefined ? { mode: backup.mode } : {}),
      })),
      expected: Object.entries(snapshot.expectedFileStates ?? {}).map(([path, expected]: [string, any]) => {
        if (expected.exists && scenario.__generatedIds.length > 0 && path.endsWith('.ipynb')) {
          let text = readFileSync(path, 'utf8');
          for (const id of scenario.__generatedIds) text = text.split(id).join('<cellid>');
          const sha256 = createHash('sha256').update(text, 'utf8').digest('hex');
          return {
            path: replaceRoot(path),
            exists: true,
            sha256,
            ...(expected.mode !== undefined ? { mode: expected.mode } : {}),
          };
        }
        return {
          path: replaceRoot(path),
          ...(expected.exists
            ? { exists: true, sha256: expected.sha256, ...(expected.mode !== undefined ? { mode: expected.mode } : {}) }
            : { exists: false }),
        };
      }),
    })),
  };
}

const scenarios: any[] = [];
let caseIndex = 0;
function begin(name: string, extra: any = {}) {
  const caseRoot = join(scratch, `case-${caseIndex++}`);
  mkdirSync(caseRoot);
  currentRoot = caseRoot;
  const scenario: any = { name, cwd: extra.cwd, steps: [], calls: [], readBack: extra.readBack ?? [], ...extra };
  scenario.__root = caseRoot;
  delete scenario.readBack;
  if (scenario.backupDir === undefined) scenario.backupDir = `file-history/${name}`;
  const history = createHistory(scenario);
  scenario.history = history;
  scenario.__generatedIds = [];
  const context: any = {
    cwd: scenario.cwd ? resolve(currentRoot, scenario.cwd) : currentRoot,
    permissionContext: { mode: 'default', canPrompt: true, additionalWorkingDirectories: [] },
    permissionMode: 'default',
    readFileState: new Map(),
    writeSnapshots: new Map(),
    allowedReadFiles: [],
    env: { PILOT_HOME: join(scratch, 'pilot-home'), HOME: scratch },
    now: () => new Date(fixed),
    messageId: 'msg-1',
    fileHistory: history.store,
  };
  return { scenario, context, history };
}

async function call(run: any, tool: string, rawInput: any, extra: any = {}) {
  Object.assign(run.context, extra);
  const input = substitute(rawInput);
  const definition: any = toolOf(tool);
  let validation: any = null;
  let validationFailed = false;
  try {
    const result = await definition.validateInput(input, run.context);
    validation = result.ok ? { ok: true } : { ok: false, issues: result.issues };
    validationFailed = !result.ok;
  } catch (error: any) {
    validation = { error: errorOf(error) };
    validationFailed = true;
  }
  let result: any = null;
  let data: any = null;
  if (!validationFailed) {
    try {
      const output = await definition.execute(input, run.context);
      result = clean({ content: output.content, data: output.data, metadata: output.metadata });
      data = output.data;
    } catch (error: any) {
      result = { error: errorOf(error) };
    }
  }
  if (result?.metadata && result.metadata.mtimeMs !== undefined) result.metadata.mtimeMs = '<mtime>';
  if (data?.edit_mode === 'insert' && data.cell_id && data.original_file) {
    const notebook = JSON.parse(data.original_file);
    const supports = (notebook.nbformat ?? 0) > 4 || (notebook.nbformat ?? 0) === 4 && (notebook.nbformat_minor ?? 0) >= 5;
    if (supports && data.cell_id !== input.cell_id) {
      const id = data.cell_id;
      run.scenario.__generatedIds.push(id);
      result.data.cell_id = '<cellid>';
      result.data.updated_file = result.data.updated_file.split(id).join('<cellid>');
      if (result.metadata) result.metadata.bytesWritten = Buffer.byteLength(result.data.updated_file, 'utf8');
      for (const content of result.content ?? []) {
        if (typeof content.text === 'string') content.text = content.text.split(id).join('<cellid>');
      }
    }
  }
  run.scenario.steps.push({ op: 'call', index: run.scenario.calls.length });
  run.scenario.calls.push({
    tool,
    input: replaceRoot(input),
    validate: replaceRoot(validation),
    execute: replaceRoot(result),
    ...(Object.keys(extra).length > 0 ? { context: replaceRoot(clean(extra)) } : {}),
  });
}

async function setup(run: any, step: Step) {
  await apply(step);
  run.scenario.steps.push(step);
}

async function makeSnapshot(run: any, messageId: string) {
  await run.history.store.makeSnapshot(messageId);
  run.scenario.steps.push({ op: 'make_snapshot', messageId });
}
async function rewind(run: any, messageId: string) {
  const result = await run.history.store.rewind(messageId);
  run.history.ops.push({ op: 'rewind', messageId, result });
  run.scenario.steps.push({ op: 'rewind', messageId });
}
async function diffStats(run: any, messageId: string) {
  const result = await run.history.store.getDiffStats(messageId);
  run.history.ops.push({ op: 'diff_stats', messageId, result });
  run.scenario.steps.push({ op: 'diff_stats', messageId });
}

// --- write_file -----------------------------------------------------------
{
  const run = begin('write-create-new');
  await call(run, 'write_file', { file_path: 'new.txt', content: 'hello\n' });
  run.scenario.readBack = ['new.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-overwrite-after-read');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'one\ntwo\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'write_file', { file_path: 'doc.txt', content: 'one\nthree\n' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-unread-refused');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'old\n', mtime: 1_700_000_000_000 });
  await call(run, 'write_file', { file_path: 'doc.txt', content: 'new\n' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-changed-after-read');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'old\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await setup(run, { op: 'file', path: 'doc.txt', content: 'external\n', mtime: 1_700_000_100_000 });
  await call(run, 'write_file', { file_path: 'doc.txt', content: 'mine\n' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-same-mtime-different-content');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'old\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await setup(run, { op: 'file', path: 'doc.txt', content: 'external\n', mtime: 1_700_000_000_000 });
  await call(run, 'write_file', { file_path: 'doc.txt', content: 'mine\n' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-bom-crlf-create');
  await call(run, 'write_file', { file_path: 'bom.txt', content: '\uFEFFalpha\r\nbeta\r\n' });
  run.scenario.readBack = ['bom.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-empty-create');
  await call(run, 'write_file', { file_path: 'empty.txt', content: '' });
  run.scenario.readBack = ['empty.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-multibyte');
  await call(run, 'write_file', { file_path: '中文 文件.txt', content: '第一行\nemoji 😀\n' });
  run.scenario.readBack = ['中文 文件.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-outside-refused');
  await call(run, 'write_file', { file_path: '<outside>/escape.txt', content: 'x' });
  run.scenario.readBack = [];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-outside-allowed');
  await setup(run, { op: 'file', path: 'inside.txt', content: 'seed' });
  await call(run, 'write_file', { file_path: '<outside>/allowed.txt', content: 'x\n' }, { currentPermissionDecision: { type: 'allow' } });
  run.scenario.readBack = ['<outside>/allowed.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('write-traversal-refused');
  await call(run, 'write_file', { file_path: '../escaped.txt', content: 'x' });
  scenarios.push(run.scenario);
}
{
  const run = begin('write-dotgit-refused');
  await setup(run, { op: 'dir', path: '.git' });
  await call(run, 'write_file', { file_path: '.git/config', content: 'x' });
  scenarios.push(run.scenario);
}
{
  const run = begin('write-history-dedupe');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'v0\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'write_file', { file_path: 'doc.txt', content: 'v1\n' });
  await setup(run, { op: 'file', path: 'doc.txt', content: 'v1\n', mtime: 1_700_000_500_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'write_file', { file_path: 'doc.txt', content: 'v2\n' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}

// --- edit_file ------------------------------------------------------------
{
  const run = begin('edit-simple');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'alpha\nbeta\ngamma\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'beta', new_string: 'BETA' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-replace-all');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'x x x\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'x', new_string: 'y', replace_all: true });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-dollar-substitution');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'A echo PID B\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'echo PID', new_string: "$$|$&|$`|$'|$1|$<n>" });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-replace-all-dollar-literal');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'x x\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'x', new_string: '$& $$', replace_all: true });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-hardlink');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'inside\n', mtime: 1_700_000_000_000 });
  await setup(run, { op: 'hardlink', path: 'alias.txt', target: 'doc.txt' });
  await call(run, 'read_file', { file_path: 'alias.txt' });
  await call(run, 'edit_file', { file_path: 'alias.txt', old_string: 'inside', new_string: 'edited' });
  run.scenario.readBack = ['doc.txt', 'alias.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-multiple-refused');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'x x\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'x', new_string: 'y' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-not-found');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'abc\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'zzz', new_string: 'y' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-identical-strings');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'abc\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'abc', new_string: 'abc' });
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-empty-old-create');
  await call(run, 'edit_file', { file_path: 'brand.txt', old_string: '', new_string: 'created\n' });
  run.scenario.readBack = ['brand.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-empty-old-empty-file');
  await setup(run, { op: 'file', path: 'empty.txt', content: '', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'empty.txt' });
  await call(run, 'edit_file', { file_path: 'empty.txt', old_string: '', new_string: 'filled\n' });
  run.scenario.readBack = ['empty.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-empty-old-nonempty-refused');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'abc', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: '', new_string: 'x' });
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-notebook-path-refused');
  await setup(run, { op: 'file', path: 'book.ipynb', content: '{"cells":[]}', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_file', { file_path: 'book.ipynb', old_string: 'a', new_string: 'b' });
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-trailing-whitespace');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'a  \nb\t\n', mtime: 1_700_000_000_000 });
  await setup(run, { op: 'file', path: 'note.md', content: '# t  \n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'read_file', { file_path: 'note.md' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'a', new_string: 'A  ' });
  await call(run, 'edit_file', { file_path: 'note.md', old_string: '# t', new_string: '# T  ' });
  run.scenario.readBack = ['doc.txt', 'note.md'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-quote-normalization');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'say \u201Chi\u201D now\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'say "hi" now', new_string: 'say "bye" now' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-unread-refused');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'abc', mtime: 1_700_000_000_000 });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'a', new_string: 'b' });
  scenarios.push(run.scenario);
}
{
  const run = begin('edit-bom-crlf');
  await setup(run, { op: 'file', path: 'doc.txt', content: '\uFEFFone\r\ntwo\r\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt', offset: 1, limit: 2 });
  await call(run, 'edit_file', { file_path: 'doc.txt', old_string: 'two', new_string: 'TWO' });
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}

// --- edit_notebook --------------------------------------------------------
const notebook = {
  nbformat: 4,
  nbformat_minor: 4,
  metadata: { language_info: { name: 'python' }, title: 'demo' },
  cells: [
    { cell_type: 'markdown', source: ['# Title\n', 'body'], metadata: {} },
    { cell_type: 'code', execution_count: 2, source: 'print(1)', outputs: [{ output_type: 'stream', name: 'stdout', text: '1\n' }], metadata: {} },
  ],
};
{
  const run = begin('notebook-replace');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', cell_id: 'cell-1', new_source: 'print(2)' });
  run.scenario.readBack = ['book.ipynb'];
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-replace-cell-type');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', cell_id: 'cell-0', new_source: 'plain', cell_type: 'code' });
  run.scenario.readBack = ['book.ipynb'];
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-insert-beginning');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', new_source: 'x = 1', cell_type: 'code', edit_mode: 'insert' });
  run.scenario.readBack = ['book.ipynb'];
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-insert-after-cell');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', cell_id: 'cell-0', new_source: '- item', cell_type: 'markdown', edit_mode: 'insert' });
  run.scenario.readBack = ['book.ipynb'];
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-delete');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', cell_id: 'cell-0', new_source: '', edit_mode: 'delete' });
  run.scenario.readBack = ['book.ipynb'];
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-append-via-replace');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', cell_id: 'cell-2', new_source: 'tail', edit_mode: 'replace' });
  run.scenario.readBack = ['book.ipynb'];
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-missing-cell');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', cell_id: 'cell-9', new_source: 'x' });
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-insert-without-type');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', new_source: 'x', edit_mode: 'insert' });
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-missing-cell-id');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', new_source: 'x' });
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-invalid-json');
  await setup(run, { op: 'file', path: 'book.ipynb', content: 'not json', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', cell_id: 'cell-0', new_source: 'x' });
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-unread-refused');
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(notebook, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', cell_id: 'cell-0', new_source: 'x' });
  scenarios.push(run.scenario);
}
{
  const run = begin('notebook-insert-cell-ids');
  const withIds = { ...notebook, nbformat_minor: 5, cells: notebook.cells.map((cell, index) => ({ ...cell, id: `id-${index}` })) };
  await setup(run, { op: 'file', path: 'book.ipynb', content: JSON.stringify(withIds, null, 1), mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'book.ipynb' });
  await call(run, 'edit_notebook', { notebook_path: 'book.ipynb', cell_id: 'id-0', new_source: 'new', cell_type: 'markdown', edit_mode: 'insert' });
  run.scenario.readBack = ['book.ipynb'];
  scenarios.push(run.scenario);
}

// --- file history snapshots / eviction -----------------------------------
{
  const run = begin('history-version-bump');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'v0\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'write_file', { file_path: 'doc.txt', content: 'v1\n' });
  await makeSnapshot(run, 'msg-1');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'v1-external\n', mtime: 1_700_000_200_000 });
  await makeSnapshot(run, 'msg-2');
  await diffStats(run, 'msg-1');
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('history-evict-oldest', { maxSnapshots: 2 });
  for (const index of [1, 2, 3]) {
    await setup(run, { op: 'file', path: 'doc.txt', content: `base-${index}\n`, mtime: 1_700_000_000_000 + index * 1000 });
    await call(run, 'read_file', { file_path: 'doc.txt' }, { messageId: `msg-${index}` });
    await call(run, 'write_file', { file_path: 'doc.txt', content: `next-${index}\n` }, { messageId: `msg-${index}` });
    await makeSnapshot(run, `msg-${index}`);
  }
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('history-oversize', { maxFileBytes: 4 });
  await setup(run, { op: 'file', path: 'big.txt', content: '0123456789', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'big.txt' });
  await call(run, 'write_file', { file_path: 'big.txt', content: 'small\n' });
  await makeSnapshot(run, 'msg-1');
  run.scenario.readBack = ['big.txt'];
  scenarios.push(run.scenario);
}
{
  const run = begin('history-rewind');
  await setup(run, { op: 'file', path: 'doc.txt', content: 'original\n', mtime: 1_700_000_000_000 });
  await call(run, 'read_file', { file_path: 'doc.txt' });
  await call(run, 'write_file', { file_path: 'doc.txt', content: 'changed\n' });
  await makeSnapshot(run, 'msg-1');
  await rewind(run, 'msg-1');
  run.scenario.readBack = ['doc.txt'];
  scenarios.push(run.scenario);
}

// --- capture --------------------------------------------------------------
const captured: any[] = [];
for (const scenario of scenarios) {
  currentRoot = scenario.__root;
  const files: any[] = [];
  for (const path of scenario.readBack ?? []) {
    const absolute = path.startsWith('<outside>/') ? resolve(outside, path.slice('<outside>/'.length)) : resolve(currentRoot, path);
    try {
      let bytes = await readFile(absolute);
      if (path.endsWith('.ipynb') && scenario.__generatedIds.length > 0) {
        let text = bytes.toString('utf8');
        for (const id of scenario.__generatedIds) text = text.split(id).join('<cellid>');
        bytes = Buffer.from(text, 'utf8');
      }
      files.push({ path, base64: bytes.toString('base64') });
    } catch (error: any) {
      files.push({ path, missing: true });
    }
  }
  const history = await captureHistory(scenario.history, scenario);
  captured.push({
    name: scenario.name,
    cwd: scenario.cwd,
    backupDir: scenario.backupDir,
    ...(scenario.maxFileBytes !== undefined ? { maxFileBytes: scenario.maxFileBytes } : {}),
    ...(scenario.maxSnapshots !== undefined ? { maxSnapshots: scenario.maxSnapshots } : {}),
    steps: scenario.steps,
    calls: scenario.calls,
    files,
    history,
  });
}

const oracle = {
  sourceCommit: commit,
  sourceSha256,
  baselineCount: bound.count,
  backupFileNames: {
    a: getBackupFileName('/fixed/project/a.txt', 1),
    unicode: getBackupFileName('/fixed/project/中文 文件.txt', 2),
  },
  scenarios: captured,
};

const output = join(await mkdtemp(join(outputParent, 'write-tools-')), 'oracle.json');
await writeFile(output, `${JSON.stringify(oracle, null, 2)}\n`, { flag: 'wx' });
console.log(output);
