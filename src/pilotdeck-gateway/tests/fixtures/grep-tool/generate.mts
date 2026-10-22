// Real TS recordings for the builtin `grep` tool (A6.5-pr4b).
// Run with the fixed baseline checkout and Node 22:
//   PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
//     node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/grep-tool/generate.mts /abs/output-parent
//
// The baseline tool shells out to the ripgrep binary bundled by
// `@vscode/ripgrep`; every recorded result therefore comes from a real
// ripgrep run over the fixture tree, not from hand-written expectations.
import { mkdtemp, mkdir, writeFile, chmod, utimes, realpath, rm } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
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
const { createGrepTool } = await load('tool/builtin/grep.ts');

const sourceSha256 = sourceClosureSha256(baseline, commit, git, [
  'src/tool/builtin/grep.ts',
]);

const scratch = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-grep-tool-'));
const outside = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-grep-tool-outside-'));
const fixed = Date.parse('2026-01-01T00:00:00.000Z');
const BASE_MTIME = 1_700_000_000_000;

type Step = any;
async function apply(step: Step): Promise<void> {
  const target = resolve(scratch, step.path);
  if (step.op === 'file') {
    await mkdir(dirname(target), { recursive: true });
    const content = step.base64 !== undefined
      ? Buffer.from(step.base64, 'base64')
      : step.repeat !== undefined
        ? step.repeat.unit.repeat(step.repeat.count)
        : step.content;
    await writeFile(target, content);
    if (step.mtime !== undefined) await utimes(target, new Date(step.mtime), new Date(step.mtime));
  } else if (step.op === 'dir') {
    await mkdir(target, { recursive: true });
  } else if (step.op === 'chmod') {
    await chmod(target, step.mode);
  } else {
    throw new Error(`unknown setup step ${JSON.stringify(step)}`);
  }
}

const clean = (value: unknown) => value === undefined ? null : JSON.parse(JSON.stringify(value));
const errorOf = (error: any) => ({
  code: error?.name === 'PilotDeckToolRuntimeError' ? error.code : 'tool_execution_failed',
  message: error?.message ?? String(error),
});
const replaceRoot = (value: any): any => {
  if (typeof value === 'string') return value.split(scratch).join('<root>').split(outside).join('<outside>');
  if (Array.isArray(value)) return value.map(replaceRoot);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, replaceRoot(item)]));
  return value;
};

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

const scenarios: any[] = [];
let current: any;
const execute = async (input: any) => {
  const cwd = join(scratch, current.cwd);
  current.context ??= contextFor({}, cwd);
  const definition = createGrepTool();
  let result: any = null;
  try {
    const output = await definition.execute(input, current.context);
    result = clean({ content: output.content, data: output.data, metadata: output.metadata });
  } catch (error: any) {
    result = { error: errorOf(error) };
  }
  return {
    tool: 'grep',
    now: current.now ?? fixed,
    input: replaceRoot(input),
    execute: replaceRoot(result),
  };
};
const call = async (input: any) => {
  current.calls ??= [];
  const record = await execute(input);
  current.steps.push({ op: 'call', index: current.calls.length });
  current.calls.push(record);
};

type FileSpec = { path: string; content?: string; base64?: string; repeat?: { unit: string; count: number }; mtime?: number };

// One isolated project tree per scenario keeps ignore rules, mtimes and
// file-target bases independent without depending on filesystem ordering.
const standardFiles = (): FileSpec[] => [
  { path: 'src/a.ts', content: 'const foo = 1;\n// bar\nfoo();\n' },
  { path: 'src/b.ts', content: 'foo twice\nfoo again\n' },
  { path: 'src/c.js', content: 'foo js\n' },
  { path: 'src/deep/d.tsx', content: 'foo tsx\n' },
  { path: 'docs/readme.md', content: 'foo markdown\n' },
  { path: 'docs/notes.txt', content: 'foo notes\n' },
  { path: 'ignored.txt', content: 'foo ignored\n' },
  { path: 'build/gen.ts', content: 'foo build\n' },
  { path: '.hidden/h.ts', content: 'foo hidden\n' },
  { path: 'node_modules/pkg/x.ts', content: 'foo nm\n' },
  { path: 'dist/y.ts', content: 'foo dist\n' },
  { path: 'data.bin', base64: 'Zm9vAGJhcgo=' },
  { path: 'long.txt', repeat: { unit: 'x'.repeat(600) + ' foo\n', count: 1 } },
  { path: 'case.txt', content: 'Foo\nfoo\nFOO\n' },
  { path: 'multi.txt', content: 'alpha\nfoo one\nbeta\ngamma\nfoo two\ndelta\n' },
  { path: 'context.txt', content: 'l1\nfoo\nl3\nl4\nl5\nl6\nfoo\nl8\n' },
  { path: 'dash.txt', content: '-foo\n' },
  { path: 'unicode.txt', content: 'héllo foo 世界\n' },
  { path: 'sub/nested.txt', content: 'foo nested\n' },
  { path: '.gitignore', content: 'ignored.txt\nbuild/\n' },
];

async function scenario(
  name: string,
  calls: any[],
  options: { files?: FileSpec[]; outside?: { path: string; content: string }[] } = {},
): Promise<void> {
  const dir = `s/${name}`;
  const files = options.files ?? standardFiles();
  const steps: Step[] = [{ op: 'dir', path: `${dir}/.git` }];
  files.forEach((file, index) => {
    steps.push({
      op: 'file',
      path: `${dir}/${file.path}`,
      ...(file.base64 !== undefined
        ? { base64: file.base64 }
        : file.repeat !== undefined
          ? { repeat: file.repeat }
          : { content: file.content ?? '' }),
      mtime: file.mtime ?? BASE_MTIME + index * 1_000,
    });
  });
  current = {
    name,
    cwd: dir,
    steps,
    calls: [],
    ...(options.outside ? { outside: options.outside } : {}),
  };
  for (const step of steps) await apply(step);
  for (const input of calls) await call(input);
  scenarios.push(current);
}

const content = (input: any) => ({ output_mode: 'content', ...input });
const files = (input: any) => input;
const count = (input: any) => ({ output_mode: 'count', ...input });

// ---------- content ----------
await scenario('grep-content-file-target', [
  content({ pattern: 'foo', path: 'multi.txt' }),
]);
await scenario('grep-content-no-line-numbers', [
  content({ pattern: 'foo', path: 'multi.txt', '-n': false }),
]);
await scenario('grep-content-context-c', [
  content({ pattern: 'foo', path: 'context.txt', '-C': 1 }),
]);
await scenario('grep-content-context-alias', [
  content({ pattern: 'foo', path: 'context.txt', context: 2 }),
]);
await scenario('grep-content-context-before', [
  content({ pattern: 'foo', path: 'multi.txt', '-B': 2 }),
]);
await scenario('grep-content-context-after', [
  content({ pattern: 'foo', path: 'multi.txt', '-A': 2 }),
]);
await scenario('grep-content-context-precedence', [
  content({ pattern: 'foo', path: 'context.txt', context: 1, '-C': 2, '-B': 3, '-A': 4 }),
]);
await scenario('grep-content-regex', [
  content({ pattern: 'fo+ (one|two)', path: 'multi.txt' }),
]);
await scenario('grep-content-case-insensitive', [
  content({ pattern: 'foo', path: 'case.txt', '-i': true }),
]);
await scenario('grep-content-case-sensitive', [
  content({ pattern: 'Foo', path: 'case.txt' }),
]);
await scenario('grep-content-multiline', [
  content({ pattern: 'alpha.*foo two', path: 'multi.txt', multiline: true }),
]);
await scenario('grep-content-long-line', [
  content({ pattern: 'foo', path: 'long.txt' }),
]);
await scenario('grep-content-dash-pattern', [
  content({ pattern: '-foo', path: 'dash.txt' }),
]);
await scenario('grep-content-unicode', [
  content({ pattern: '世界', path: 'unicode.txt' }),
]);
await scenario('grep-content-binary-target', [
  content({ pattern: 'foo', path: 'data.bin' }),
]);
await scenario('grep-content-offset', [
  content({ pattern: 'foo', path: 'multi.txt', offset: 1 }),
]);
await scenario('grep-content-head-limit', [
  content({ pattern: 'foo', path: 'multi.txt', head_limit: 1 }),
]);
await scenario('grep-content-head-limit-zero', [
  content({ pattern: 'foo', path: 'multi.txt', head_limit: 0 }),
]);
await scenario('grep-content-head-limit-negative', [
  content({ pattern: 'foo', path: 'multi.txt', head_limit: -1 }),
]);
await scenario('grep-content-offset-beyond', [
  content({ pattern: 'foo', path: 'multi.txt', offset: 99 }),
]);
await scenario('grep-content-no-matches', [
  content({ pattern: 'zzz', path: 'multi.txt' }),
]);
await scenario('grep-content-subdir-file', [
  content({ pattern: 'foo', path: 'src/a.ts' }),
]);

// ---------- files_with_matches (default) ----------
await scenario('grep-files-default', [files({ pattern: 'foo', path: '.' })]);
await scenario('grep-files-glob-ts', [files({ pattern: 'foo', path: '.', glob: '*.ts' })]);
await scenario('grep-files-glob-brace', [files({ pattern: 'foo', path: '.', glob: '*.{ts,js}' })]);
await scenario('grep-files-glob-comma', [files({ pattern: 'foo', path: '.', glob: '*.ts,*.md' })]);
await scenario('grep-files-glob-space', [files({ pattern: 'foo', path: '.', glob: '*.ts *.txt' })]);
await scenario('grep-files-type-ts', [files({ pattern: 'foo', path: '.', type: 'ts' })]);
await scenario('grep-files-type-js', [files({ pattern: 'foo', path: '.', type: 'js' })]);
await scenario('grep-files-type-py', [files({ pattern: 'foo', path: '.', type: 'py' })]);
await scenario('grep-files-glob-no-match', [files({ pattern: 'foo', path: '.', glob: '*.rs' })]);
await scenario('grep-files-hidden', [files({ pattern: 'hidden' })]);
await scenario('grep-files-ignored-directory', [files({ pattern: 'nm' })]);
await scenario('grep-files-no-matches', [files({ pattern: 'zzz' })]);
await scenario('grep-files-path-directory', [files({ pattern: 'foo', path: 'src' })]);
await scenario('grep-files-path-trailing-slash', [files({ pattern: 'foo', path: 'src/' })]);
await scenario('grep-files-binary-target', [files({ pattern: 'foo', path: 'data.bin' })]);
await scenario('grep-files-head-limit', [files({ pattern: 'foo', head_limit: 2 })]);
await scenario('grep-files-head-limit-zero', [files({ pattern: 'foo', head_limit: 0 })]);
await scenario('grep-files-offset', [files({ pattern: 'foo', head_limit: 2, offset: 1 })]);
await scenario('grep-files-offset-beyond', [files({ pattern: 'foo', offset: 99 })]);

// ---------- count ----------
await scenario('grep-count-default', [count({ pattern: 'foo', path: '.' })]);
await scenario('grep-count-glob-txt', [count({ pattern: 'foo', path: '.', glob: '*.txt' })]);
await scenario('grep-count-head-limit', [count({ pattern: 'foo', path: '.', head_limit: 2 })]);
await scenario('grep-count-binary', [count({ pattern: 'foo', path: 'data.bin' })]);
await scenario('grep-count-no-matches', [count({ pattern: 'zzz', path: '.' })]);

// ---------- path resolution / errors ----------
await scenario('grep-path-missing', [files({ pattern: 'foo', path: 'missing' })]);
await writeFile(join(outside, 'secret.txt'), 'foo secret\n');
await scenario(
  'grep-path-outside',
  [files({ pattern: 'foo', path: join(outside, 'secret.txt') })],
  { outside: [{ path: 'secret.txt', content: 'foo secret\n' }] },
);
await scenario('grep-path-absolute-inside', [
  content({ pattern: 'foo', path: join(scratch, 's', 'grep-path-absolute-inside', 'src', 'a.ts') }),
]);

await scenario('grep-invalid-regex', [content({ pattern: '[', path: 'src/a.ts' })]);

// ---------- extra shapes ----------
await scenario('grep-empty-file', [content({ pattern: 'foo', path: 'empty.txt' })], {
  files: [...standardFiles(), { path: 'empty.txt', content: '' }],
});

const oracle = {
  sourceCommit: commit,
  generatedFrom: bound.sha256,
  sourceSha256,
  scenarios: scenarios.map(({ name, cwd, steps, outside: outsideFiles, calls }) => ({
    name,
    cwd,
    ...(outsideFiles ? { outside: outsideFiles } : {}),
    steps,
    calls,
  })),
};
const output = await mkdtemp(join(outputParent, 'grep-tool-'));
await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify({
  output,
  scenarios: scenarios.length,
  calls: scenarios.reduce((sum, scenario) => sum + scenario.calls.length, 0),
}));

// Every delete stays inside this run's own temp trees.
for (const target of [scratch, outside]) {
  const rel = relative(realpathSync(tmpdir()), target);
  assert.ok(!rel.startsWith('..') && !rel.startsWith(sep), `refusing to remove ${target}`);
  await rm(target, { recursive: true, force: true });
}
