import { baselineSourceHashes } from '../_shared/baseline.mts';
// Real TS recordings for the builtin read-only file tools.
// Run with the fixed baseline checkout and Node 22:
//   PDRS_BASELINE=/abs/fixed/PilotDeck node --import /abs/.../tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/builtin-tools/generate.mts /abs/output-parent
import { mkdtemp, mkdir, readFile, realpath, rm, symlink, writeFile, lstat, chmod, utimes } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { bindBaseline } from '../_shared/generator_guard.mjs';
import { executeFile } from '../_shared/ripgrep-subprocess.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide an output parent directory; each run writes into its own mkdtemp child');
const baseline = resolve(process.env.PDRS_BASELINE!);
const outputParent = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const bound = bindBaseline({ baseline, commit, gitExecutable: git }).provenance;
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const load = (name: string) => import(pathToFileURL(join(baseline, 'src', name)).href);
const { createReadFileTool } = await load('tool/builtin/readFile.ts');
const { createGlobTool } = await load('tool/builtin/glob.ts');
const { createGetCurrentTimeTool } = await load('tool/builtin/getCurrentTime.ts');

async function resolveSource(absolute: string): Promise<string | undefined> {
  const stem = absolute.endsWith('.js') ? absolute.slice(0, -3) : undefined;
  const candidates = [absolute, `${absolute}.ts`, `${absolute}.tsx`, `${absolute}.mts`, join(absolute, 'index.ts'), ...(stem ? [`${stem}.ts`, `${stem}.tsx`] : [])];
  for (const candidate of candidates) {
    try {
      if ((await lstat(candidate)).isFile()) return candidate;
    } catch {}
  }
  return undefined;
}
async function sourceClosure(entry: string): Promise<string[]> {
  const seen = new Set<string>([entry]);
  const queue = [entry];
  while (queue.length > 0) {
    const relativePath = queue.shift()!;
    const source = await readFile(join(baseline, relativePath), 'utf8');
    const specifiers = [...source.matchAll(/(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1/g)].map(match => match[2]);
    for (const specifier of specifiers) {
      if (!specifier.startsWith('./') && !specifier.startsWith('../')) continue;
      const resolved = await resolveSource(join(baseline, dirname(relativePath), specifier));
      assert.ok(resolved, `unresolved baseline import ${specifier} in ${relativePath}`);
      const path = relative(baseline, resolved).split(sep).join('/');
      if (!seen.has(path)) {
        seen.add(path);
        queue.push(path);
      }
    }
  }
  return [...seen].sort();
}
const closure = (await Promise.all([
  'src/tool/builtin/readFile.ts',
  'src/tool/builtin/glob.ts',
  'src/tool/builtin/getCurrentTime.ts',
].map(sourceClosure))).flat();
const sourceSha256 = await baselineSourceHashes(baseline, [...new Set(closure)].sort());

const scratch = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-builtin-tools-'));
const owner = await lstat(scratch);
const outside = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-builtin-outside-'));
const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));
const clean = (value: unknown) => value === undefined ? null : JSON.parse(JSON.stringify(value));
const errorOf = (error: any) => ({
  code: error?.name === 'PilotDeckToolRuntimeError' ? error.code : 'tool_execution_failed',
  message: error?.message ?? String(error),
});
const replaceRoot = (value: any, root: string, outsideRoot: string): any => {
  if (typeof value === 'string') return value.split(root).join('<root>').split(outsideRoot).join('<outside>');
  if (Array.isArray(value)) return value.map(item => replaceRoot(item, root, outsideRoot));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, replaceRoot(item, root, outsideRoot)]));
  return value;
};

type Step = any;
async function apply(step: Step): Promise<void> {
  const target = (path: string) => resolve(scratch, path);
  if (step.op === 'file') {
    await mkdir(dirname(target(step.path)), { recursive: true });
    const content = step.base64 !== undefined
      ? Buffer.from(step.base64, 'base64')
      : step.repeat !== undefined
        ? step.repeat.unit.repeat(step.repeat.count)
        : step.content;
    await writeFile(target(step.path), content);
    if (step.mtime !== undefined) await utimes(target(step.path), new Date(step.mtime), new Date(step.mtime));
  } else if (step.op === 'dir') {
    await mkdir(target(step.path), { recursive: true });
  } else if (step.op === 'symlink') {
    const linkTarget = String(step.target).split('<outside>').join(outside);
    try {
      await symlink(linkTarget, target(step.path));
    } catch (error: any) {
      if (error?.code !== 'EEXIST') throw error;
    }
  } else if (step.op === 'chmod') {
    await chmod(target(step.path), step.mode);
  } else if (step.op === 'sleep') {
    await sleep(step.ms);
  } else {
    throw new Error(`unknown setup step ${JSON.stringify(step)}`);
  }
}

const fixed = Date.parse('2026-01-01T00:00:00.000Z');
const summer = Date.parse('2026-07-01T12:00:00.000Z');
const contextFor = (extra: any = {}, cwd = scratch) => ({
  cwd,
  permissionContext: { mode: 'default', canPrompt: true, additionalWorkingDirectories: [] },
  permissionMode: 'default',
  readFileState: new Map(),
  writeSnapshots: new Map(),
  allowedReadFiles: [],
  env: { PILOT_HOME: join(scratch, 'pilot-home'), HOME: scratch },
  now: () => new Date(fixed),
  ...extra,
});

const execute = async (tool: string, input: any, extra: any = {}, now = fixed) => {
  current.now = now;
  const cwd = current.cwd ? join(scratch, current.cwd) : scratch;
  current.context ??= contextFor({ now: () => new Date(current.now) }, cwd);
  Object.assign(current.context, extra);
  const definition = tool === 'read_file' ? createReadFileTool() : tool === 'glob' ? createGlobTool({ subprocess: { executeFile } }) : createGetCurrentTimeTool();
  let validation: any = null;
  let validationFailed = false;
  try {
    if (definition.validateInput) {
      const result = await definition.validateInput(input, current.context);
      validation = result.ok ? { ok: true } : { ok: false, issues: result.issues };
      validationFailed = !result.ok;
    }
  } catch (error: any) {
    validation = { error: errorOf(error) };
    validationFailed = true;
  }
  let result: any = null;
  if (!validationFailed) {
    try {
      const output = await definition.execute(input, current.context);
      result = clean({ content: output.content, data: output.data, metadata: output.metadata });
    } catch (error: any) {
      result = { error: errorOf(error) };
    }
  }
  return { tool, now, input: replaceRoot(input, scratch, outside), validate: replaceRoot(clean(validation), scratch, outside), execute: replaceRoot(result, scratch, outside) };
};

const scenarios: any[] = [];
let current: any;
const call = async (tool: string, input: any, extra: any = {}, now = fixed) => {
  current.calls ??= [];
  const record = await execute(tool, input, extra, now);
  current.steps.push({ op: 'call', index: current.calls.length });
  current.calls.push({
    ...record,
    ...(Object.keys(extra).length > 0
      ? { context: replaceRoot(clean(extra), scratch, outside) }
      : {}),
  });
};

// ---------- read_file ----------
current = { name: 'read-text-bom-crlf', steps: [] };
current.steps.push({ op: 'file', path: 'a.txt', content: '\uFEFFalpha\r\nbeta\r\n' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'a.txt' });
scenarios.push(current);

current = { name: 'read-text-offset-limit', steps: [] };
current.steps.push({ op: 'file', path: 'lines.txt', content: 'a\nb\nc\nd' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'lines.txt', offset: 2, limit: 2 });
scenarios.push(current);

current = { name: 'read-text-limit-zero', steps: [] };
current.steps.push({ op: 'file', path: 'lines.txt', content: 'a\nb\nc\nd' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'lines.txt', offset: 2, limit: 0 });
scenarios.push(current);

current = { name: 'read-text-offset-beyond', steps: [] };
current.steps.push({ op: 'file', path: 'lines.txt', content: 'a\nb\nc\nd' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'lines.txt', offset: 10 });
scenarios.push(current);

current = { name: 'read-text-empty-full', steps: [] };
current.steps.push({ op: 'file', path: 'empty.txt', content: '' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'empty.txt' });
scenarios.push(current);

current = { name: 'read-text-empty-limit', steps: [] };
current.steps.push({ op: 'file', path: 'empty.txt', content: '' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'empty.txt', offset: 1, limit: 5 });
scenarios.push(current);

current = { name: 'read-text-lone-cr-with-limit', steps: [] };
current.steps.push({ op: 'file', path: 'cr.txt', content: 'a\rb\rc' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'cr.txt', offset: 1, limit: 10 });
scenarios.push(current);

current = { name: 'read-text-lone-cr-no-limit', steps: [] };
current.steps.push({ op: 'file', path: 'cr-no-limit.txt', content: 'a\rb' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'cr-no-limit.txt' });
scenarios.push(current);

current = { name: 'read-text-eof-lone-cr', steps: [] };
current.steps.push({ op: 'file', path: 'cr-eof.txt', content: 'abc\r' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'cr-eof.txt' });
scenarios.push(current);

current = { name: 'read-invalid-utf8', steps: [] };
current.steps.push({ op: 'file', path: 'invalid.txt', base64: 'Yf9iCg==' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'invalid.txt' });
scenarios.push(current);

current = { name: 'read-unicode-path', steps: [] };
current.steps.push({ op: 'file', path: '中文 文件.txt', content: '内容\n' });
await apply(current.steps[0]);
await call('read_file', { file_path: '中文 文件.txt' });
scenarios.push(current);

current = { name: 'read-binary-extension-validate', steps: [] };
current.steps.push({ op: 'file', path: 'archive.zip', base64: 'UEsDBA==' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'archive.zip' });
scenarios.push(current);

current = { name: 'read-binary-nul', steps: [] };
current.steps.push({ op: 'file', path: 'nul-content.txt', base64: 'YQBi' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'nul-content.txt' });
scenarios.push(current);

current = { name: 'read-notebook', steps: [] };
current.steps.push({ op: 'file', path: 'book.ipynb', content: JSON.stringify({ cells: [
  { cell_type: 'markdown', source: ['# Title\n', 'body'] },
  { cell_type: 'code', execution_count: 2, source: 'print(1)', outputs: [
    { text: ['1', '\n'] },
    { traceback: ['Traceback', 'ValueError: bad'] },
    { ename: 'ValueError', evalue: 'bad' },
    { data: { 'text/plain': ['plain ', 'text'] } },
  ] },
  { cell_type: null, execution_count: null, source: 5, outputs: [] },
] }) });
await apply(current.steps[0]);
await call('read_file', { file_path: 'book.ipynb' });
await call('read_file', { file_path: 'book.ipynb', offset: 3, limit: 4 });
scenarios.push(current);

current = { name: 'read-notebook-invalid-json', steps: [] };
current.steps.push({ op: 'file', path: 'bad.ipynb', content: 'not json' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'bad.ipynb' });
scenarios.push(current);

current = { name: 'read-notebook-cell-type-number', steps: [] };
current.steps.push({ op: 'file', path: 'typed.ipynb', content: JSON.stringify({ cells: [
  { cell_type: 5, source: 'x' },
] }) });
await apply(current.steps[0]);
await call('read_file', { file_path: 'typed.ipynb' });
scenarios.push(current);

current = { name: 'read-notebook-non-array-cells', steps: [] };
current.steps.push({ op: 'file', path: 'cells-object.ipynb', content: JSON.stringify({ cells: {} }) });
await apply(current.steps[0]);
await call('read_file', { file_path: 'cells-object.ipynb' });
scenarios.push(current);

current = { name: 'read-dedup-unchanged', steps: [] };
current.steps.push({ op: 'file', path: 'same.txt', content: 'one\ntwo\n' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'same.txt' });
await call('read_file', { file_path: 'same.txt' });
scenarios.push(current);

current = { name: 'read-modify-between-calls', steps: [] };
current.steps.push({ op: 'file', path: 'changing.txt', content: 'old\n', mtime: 1700000000000 });
await apply(current.steps[0]);
await call('read_file', { file_path: 'changing.txt' });
current.steps.push({ op: 'file', path: 'changing.txt', content: 'new\ncontent\n', mtime: 1700000001000 });
await apply(current.steps[current.steps.length - 1]);
await call('read_file', { file_path: 'changing.txt' });
scenarios.push(current);

current = { name: 'read-symlink-inside', steps: [] };
current.steps.push({ op: 'file', path: 'target.txt', content: 'target\n' });
current.steps.push({ op: 'symlink', path: 'link.txt', target: 'target.txt' });
for (const step of current.steps) await apply(step);
await call('read_file', { file_path: 'link.txt' });
scenarios.push(current);

current = { name: 'read-symlink-outside', steps: [], outside: [{ path: 'secret.txt', content: 'outside\n' }] };
current.steps.push({ op: 'symlink', path: 'escape.txt', target: '<outside>/secret.txt' });
await writeFile(join(outside, 'secret.txt'), 'outside\n');
await apply(current.steps[0]);
await call('read_file', { file_path: 'escape.txt' });
scenarios.push(current);

current = { name: 'read-directory', steps: [] };
current.steps.push({ op: 'dir', path: 'folder' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'folder' });
scenarios.push(current);

current = { name: 'read-permission-denied', steps: [] };
current.steps.push({ op: 'file', path: 'secret.txt', content: 'secret\n' });
current.steps.push({ op: 'chmod', path: 'secret.txt', mode: 0o000 });
for (const step of current.steps) await apply(step);
await call('read_file', { file_path: 'secret.txt' });
scenarios.push(current);

current = { name: 'read-large-auto-page', steps: [] };
current.steps.push({ op: 'file', path: 'large.txt', repeat: { unit: `${'x'.repeat(49)}\n`, count: 5000 } });
await apply(current.steps[0]);
await call('read_file', { file_path: 'large.txt' });
scenarios.push(current);

current = { name: 'read-image-no-multimodal', steps: [] };
current.steps.push({ op: 'file', path: 'pic.png', base64: 'iVBORw0KGgo=' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'pic.png' });
scenarios.push(current);

current = { name: 'read-pdf-no-multimodal', steps: [] };
current.steps.push({ op: 'file', path: 'doc.pdf', content: 'not a pdf' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'doc.pdf' });
await call('read_file', { file_path: 'doc.pdf', pages: '1-2' });
await call('read_file', { file_path: 'doc.pdf', pages: '0-1' });
await call('read_file', { file_path: 'doc.pdf', pages: '1-99999999999999999999' });
scenarios.push(current);

current = { name: 'read-image-jpeg-fast-path', steps: [] };
const jpegBytes = Buffer.concat([Buffer.alloc(1024, 0x41), Buffer.from([0xff, 0xd9])]);
current.steps.push({ op: 'file', path: 'pic.jpg', base64: jpegBytes.toString('base64') });
await apply(current.steps[0]);
await call('read_file', { file_path: 'pic.jpg' }, { modelMultimodal: { input: ['image'], imageDetail: 'low' } });
await call('read_file', { file_path: 'pic.jpg' }, { modelMultimodal: { input: ['image'], imageDetail: 'low' } });
scenarios.push(current);

current = { name: 'read-pdf-document-block', steps: [] };
current.steps.push({ op: 'file', path: 'doc.pdf', content: 'not a pdf' });
await apply(current.steps[0]);
await call('read_file', { file_path: 'doc.pdf' }, { modelMultimodal: { input: ['pdf'] } });
await call('read_file', { file_path: 'doc.pdf' }, { modelMultimodal: { input: ['pdf'] } });
scenarios.push(current);

// ---------- glob ----------
let globTick = 1700000000000;
const globFile = (path: string, content: string): Step => ({ op: 'file', path, content, mtime: (globTick += 1000) });
const globSetup: Step[] = [
  globFile('g/root.txt', 'r'),
  globFile('g/src/a.ts', 'a'),
  globFile('g/src/b.ts', 'b'),
  globFile('g/src/deep/c.ts', 'c'),
  globFile('g/src/deep/D.TS', 'd'),
  globFile('g/.hidden.txt', 'h'),
  globFile('g/node_modules/pkg/index.js', 'n'),
  globFile('g/.git/config', 'g'),
  globFile('g/sub/note.txt', 'n'),
  globFile('g/dist', 'd'),
  { op: 'dir', path: 'g/empty' },
  { op: 'symlink', path: 'g/link.txt', target: 'root.txt' },
];

const globs: [string, any][] = [
  ['glob-double-star-txt', { pattern: '**/*.txt' }],
  ['glob-basename-txt', { pattern: '*.txt' }],
  ['glob-src-star-ts', { pattern: 'src/*.ts' }],
  ['glob-src-double-star-ts', { pattern: 'src/**/*.ts' }],
  ['glob-brace-char-question', { pattern: 'src/{a,b}.?s' }],
  ['glob-negation', { pattern: '!*.txt' }],
  ['glob-absolute', { pattern: '<root>/g/src/**/*.ts' }],
  ['glob-path-parameter', { pattern: '**/*.ts', path: 'src/deep' }],
  ['glob-limit-two', { pattern: '**/*.txt', limit: 2 }],
  ['glob-limit-negative', { pattern: '**/*.txt', limit: -2 }],
  ['glob-empty-directory', { pattern: '**/*', path: 'empty' }],
  ['glob-case-sensitive', { pattern: '**/*.TS' }],
  ['glob-missing-path', { pattern: '**/*', path: 'missing' }],
  ['glob-trailing-slash', { pattern: 'src/' }],
  ['glob-ignored-file-segment', { pattern: 'dist' }],
];
for (const [name, input] of globs) {
  const scenarioInput = JSON.parse(JSON.stringify(input).split('<root>').join(scratch));
  current = { name, cwd: 'g', steps: globSetup.map(step => ({ ...step })), calls: [] };
  for (const step of current.steps) await apply(step);
  await call('glob', scenarioInput);
  scenarios.push(current);
}

// ---------- get_current_time ----------
const zones: [string, any, number][] = [
  ['time-utc', 'UTC', fixed],
  ['time-shanghai', 'Asia/Shanghai', fixed],
  ['time-new-york-winter', 'America/New_York', fixed],
  ['time-new-york-summer', 'America/New_York', summer],
  ['time-lord-howe', 'Australia/Lord_Howe', fixed],
  ['time-chatham', 'Pacific/Chatham', fixed],
  ['time-offset-colon', '+05:30', fixed],
  ['time-offset-compact', '-0800', fixed],
  ['time-etc-gmt-plus', 'Etc/GMT+5', fixed],
  ['time-alias', 'US/Pacific', fixed],
  ['time-lowercase', 'asia/shanghai', fixed],
  ['time-invalid', 'Invalid/Zone', fixed],
];
for (const [name, timezone, now] of zones) {
  current = { name, steps: [], calls: [] };
  await call('get_current_time', { timezone }, {}, now);
  scenarios.push(current);
}

const oracle = {
  sourceCommit: commit,
  generatedFrom: bound.sha256,
  sourceSha256,
  scenarios: scenarios.map(({ name, cwd, steps, outside, calls }) => ({ name, ...(cwd ? { cwd } : {}), ...(outside ? { outside } : {}), steps, calls })),
};
const output = await mkdtemp(join(outputParent, 'builtin-tools-'));
await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ output, scenarios: scenarios.length, calls: scenarios.reduce((sum, scenario) => sum + scenario.calls.length, 0) }));

// Every delete stays inside this run's own temp trees.
for (const target of [scratch, outside]) {
  const rel = relative(realpathSync(tmpdir()), target);
  assert.ok(!rel.startsWith('..') && !rel.startsWith(sep), `refusing to remove ${target}`);
  await rm(target, { recursive: true, force: true });
}
