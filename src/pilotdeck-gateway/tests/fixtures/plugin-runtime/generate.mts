// Records the fixed TypeScript baseline's PluginRuntime generation/lease
// lifecycle and GatewayCommandCatalogBundle.commandsList over real temporary
// plugin directories. Run from repo root with Node 22 and the baseline's tsx
// loader; see README.md. PluginRuntime and the catalog bundle are the real
// classes and read real files; only builtin plugin objects (whose `dispose`
// records an event) and the catalog's listCommands / resolve / warn
// collaborators are recording fakes. Every scenario gets its own directory
// under one mkdtemp root, and only that root is removed.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture output directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const outputDir = resolve(process.argv[2]);

// Shared provenance guard (#280): explicit absolute Git, pinned HEAD, every
// tracked src byte equal to its Git blob, and a clean tracked tree.
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const trackedChanges = execFileSync(gitExecutable,
  ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'], { env: gitEnv }).toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);
// Command / MCP instruction lists are sorted with localeCompare; Rust pins en-US.
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const RUNTIME = 'src/extension/plugins/runtime/PluginRuntime.ts';
const CATALOG = 'src/cli/GatewayCommandCatalogBundle.ts';
const sourceSha256: Record<string, string> = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [RUNTIME, CATALOG]);
const { PluginRuntime } = await import(pathToFileURL(join(baseline, RUNTIME)).href);
const { GatewayCommandCatalogBundle } = await import(pathToFileURL(join(baseline, CATALOG)).href);

type Step = Record<string, any> & { op: string };
type Builtin = { name: string; dir: string; dispose?: string; gate?: string; mcpServers?: Record<string, unknown> };
type Scenario = {
  id: string;
  builtins?: Builtin[];
  enabled?: Record<string, boolean>;
  builtinSkills?: boolean;
  steps: Step[];
};
const scenarios: Scenario[] = [];
const add = (id: string, options: Omit<Scenario, 'id' | 'steps'>, steps: Step[]) => scenarios.push({ id, ...options, steps });
const w = (file: string, text: string) => ({ op: 'write', file, text });
const manifest = (value: unknown) => JSON.stringify(value);
const G = 'home/plugins';
const P = 'project/.pilotdeck/plugins';

add('empty-directories', {}, [
  { op: 'state' },
  { op: 'refresh' },
  { op: 'refresh' },
  { op: 'acquire', kind: 'catalog', id: 'a' },
  { op: 'release', id: 'a' },
  { op: 'state' },
]);
add('command-discovery-and-precedence', {}, [
  w(`${G}/dir-name/plugin.json`, manifest({ name: 'manifest-name', version: '1.0.0' })),
  w(`${G}/dir-name/commands/alpha.md`, '---\ndescription: Alpha command\nargument-hint: <file>\n---\nAlpha body\n'),
  w(`${G}/dir-name/commands/nested/deep/beta10.md`, 'no frontmatter'),
  w(`${G}/dir-name/commands/beta9.md`, '---\ndescription: 42\nargument-hint: true\n---\nnine'),
  w(`${G}/dir-name/commands/skillish/SKILL.md`, '---\ndescription: "quoted"\n---\nskill in commands'),
  w(`${G}/dir-name/commands/SKILL.md`, 'root skill file'),
  w(`${G}/dir-name/commands/notes.txt`, 'ignored'),
  w(`${G}/dir-name/commands/UPPER.MD`, '---\r\ndescription: crlf\r\n---\r\nbody'),
  w(`${G}/dir-name/commands/Zeta.md`, '---\ndescription: \'single\'\nempty:\n: novalue\nflag: false\nnum: 1e3\n---\n'),
  w(`${G}/dir-name/commands/_under.md`, '---\nno closing fence\n'),
  w(`${G}/dir-name/commands/Ålpha.md`, '---\nx: 1\n---\n---\nstill body'),
  w(`${G}/dir-name/commands/.hidden.md`, 'hidden'),
  w(`${G}/custom/plugin.json`, manifest({ name: 'custom', commands: ['cmds', 'more/', 'missing'] })),
  w(`${G}/custom/cmds/one.md`, 'one'),
  w(`${G}/custom/more/two.md`, 'two'),
  w(`${G}/custom/commands/ignored.md`, 'not configured'),
  w(`${G}/single/plugin.json`, manifest({ name: 'single', commands: 'only', skills: 'skills' })),
  w(`${G}/single/only/shared.md`, '---\ndescription: global shared\n---\nglobal'),
  w(`${P}/proj/plugin.json`, manifest({ name: 'proj' })),
  w(`${P}/proj/commands/shared.md`, '---\ndescription: project shared\n---\nproject'),
  // Same model-facing name from two plugins: the higher source priority wins.
  w(`${P}/single/plugin.json`, manifest({ name: 'single-project', commands: 'only' })),
  w(`${P}/single/only/shared.md`, '---\ndescription: project single\n---\nproject single'),
  { op: 'refresh' },
  { op: 'state' },
  { op: 'acquire', kind: 'catalog', id: 'c' },
  { op: 'acquire', kind: 'session', id: 's' },
  { op: 'acquire', kind: 'legacy', id: 'l' },
  { op: 'release', id: 'c' },
  { op: 'release', id: 's' },
  { op: 'release', id: 'l' },
]);
add('skills-hooks-and-mcp', { builtinSkills: true }, [
  w('builtin-skills/review/SKILL.md', '---\ndescription: builtin review\n---\nreview body'),
  w('home/skills/bar/skill.md', 'lowercase skill file'),
  w('home/skills/no-skill/README.md', 'no skill file'),
  w('home/skills/loose.md', 'not a directory'),
  w('project/.pilotdeck/skills/review/Skill.MD', '---\ndescription: project review\n---\nproject review'),
  w(`${G}/mcp-a/plugin.json`, manifest({
    name: 'mcp-a',
    mcpServers: {
      zed: { instructions: '  zed instructions  ' },
      alpha: { instructions: 'x'.repeat(2049) },
      blank: { instructions: '  \n' },
      number: { instructions: 7 },
      plain: 'not an object',
    },
    lspServers: { ts: { command: 'tsls' } },
    hooks: { PreToolUse: [{ matcher: 'Bash', hooks: [{ type: 'command', command: 'echo a' }] }] },
  })),
  w(`${G}/mcp-a/skills/lint/SKILL.md`, '---\ndescription: lint skill\n---\nlint'),
  w(`${G}/mcp-a/skills/lint/reference.md`, 'not a skill'),
  w(`${G}/mcp-b/plugin.json`, manifest({
    name: 'mcp-b',
    mcpServers: { zed: { instructions: 'second zed' }, beta: { instructions: 'beta' } },
    lspServers: { ts: { command: 'other' } },
    hooks: 'custom-hooks.json',
  })),
  w(`${G}/mcp-b/custom-hooks.json`, JSON.stringify({ Stop: [{ hooks: [{ type: 'prompt', prompt: 'p' }] }], Bogus: [] })),
  w(`${G}/mcp-c/plugin.json`, manifest({ name: 'mcp-c', mcpServers: [], hooks: 5 })),
  w(`${G}/mcp-c/hooks/hooks.json`, JSON.stringify({ SessionStart: [{ matcher: 'x', hooks: [{ type: 'command', command: 'c' }] }] })),
  w(`${G}/mcp-d/plugin.json`, manifest({ name: 'mcp-d' })),
  w(`${G}/mcp-d/hooks/hooks.json`, '{not json'),
  { op: 'refresh' },
  { op: 'state' },
  { op: 'acquire', kind: 'session', id: 's' },
  { op: 'acquire', kind: 'legacy', id: 'l' },
  { op: 'release', id: 's' },
  { op: 'release', id: 'l' },
]);
add('staging-failures-keep-or-publish', {}, [
  w(`${G}/a/plugin.json`, manifest({ name: 'a' })),
  w(`${G}/a/commands/a.md`, 'a'),
  w(`${G}/b/plugin.json`, manifest({ name: 'b' })),
  w(`${G}/b/commands/b.md`, 'b'),
  w(`${G}/not-a-dir`, 'file'),
  { op: 'refresh' },
  // New broken candidates are reported; the healthy set is still published.
  w(`${G}/c/plugin.json`, '{broken'),
  w(`${G}/d/plugin.json`, manifest({ version: '1' })),
  w(`${G}/e/plugin.json`, manifest(['array'])),
  w(`${G}/f/README.md`, 'no manifest'),
  w(`${G}/g/plugin.json`, manifest({ name: '   ' })),
  { op: 'refresh' },
  { op: 'state' },
  // An existing source path that no longer loads keeps the old generation.
  w(`${G}/b/plugin.json`, '{broken'),
  { op: 'refresh' },
  { op: 'state' },
  // Deleting it is an ordinary retirement.
  { op: 'rm', file: `${G}/b` },
  { op: 'refresh' },
  { op: 'state' },
]);
add('builtins-enabled-and-reloaded', {
  builtins: [
    { name: 'b1', dir: 'builtin/b1', dispose: 'ok', mcpServers: { b1srv: { instructions: 'from builtin' } } },
    { name: 'b2', dir: 'builtin/b2', dispose: 'ok' },
    { name: 'b3', dir: 'builtin/b3', dispose: 'ok' },
    { name: 'b4', dir: 'builtin/b4', dispose: 'ok' },
  ],
  enabled: { b3: false, b4: true },
}, [
  w('builtin/b2/plugin.json', manifest({ name: 'b2-loaded' })),
  w('builtin/b2/commands/from-disk.md', 'disk'),
  { op: 'refresh' },
  { op: 'state' },
  // b1 becomes loadable: its original (unleased) instance is retired and the
  // refresh awaits its disposal.
  w('builtin/b1/plugin.json', manifest({ name: 'b1' })),
  { op: 'refresh' },
  { op: 'state' },
  { op: 'refresh' },
]);
add('leases-defer-retired-disposal', { builtins: [{ name: 'bx', dir: 'builtin/bx', dispose: 'ok' }] }, [
  w(`${G}/g/plugin.json`, manifest({ name: 'g' })),
  w(`${G}/g/commands/v1.md`, 'v1'),
  { op: 'refresh' },
  { op: 'acquire', kind: 'catalog', id: 'c1' },
  { op: 'acquire', kind: 'session', id: 's1' },
  w('builtin/bx/plugin.json', manifest({ name: 'bx' })),
  { op: 'rm', file: `${G}/g/commands/v1.md` },
  w(`${G}/g/commands/v2.md`, 'v2'),
  { op: 'refresh' },
  { op: 'state' },
  { op: 'acquire', kind: 'catalog', id: 'c2' },
  // A releaser waits for the disposal of every retired plugin it held, which
  // in turn waits for the other lease on that generation.
  { op: 'start', what: 'release', id: 'c1', slot: 'rc1' },
  { op: 'flush' },
  { op: 'pending', slot: 'rc1' },
  { op: 'release', id: 's1' },
  { op: 'await', slot: 'rc1' },
  { op: 'release', id: 'c1' },
  { op: 'release', id: 'c2' },
]);
add('retired-dispose-failure-reaches-releaser', { builtins: [{ name: 'by', dir: 'builtin/by', dispose: 'by dispose failed' }] }, [
  { op: 'refresh' },
  { op: 'acquire', kind: 'catalog', id: 'c' },
  w('builtin/by/plugin.json', manifest({ name: 'by' })),
  { op: 'refresh' },
  { op: 'release', id: 'c' },
  { op: 'release', id: 'c' },
  { op: 'state' },
]);
add('unleased-dispose-failure-rejects-refresh', { builtins: [{ name: 'bz', dir: 'builtin/bz', dispose: 'bz dispose failed' }] }, [
  { op: 'refresh' },
  w('builtin/bz/plugin.json', manifest({ name: 'bz' })),
  { op: 'refresh' },
  { op: 'state' },
]);
add('concurrent-refresh-is-single-flight', {}, [
  w(`${G}/a/plugin.json`, manifest({ name: 'a' })),
  { op: 'start', what: 'refresh', slot: 'r1' },
  { op: 'start', what: 'refresh', slot: 'r2' },
  { op: 'acquire', kind: 'catalog', id: 'early' },
  { op: 'await', slot: 'r1' },
  { op: 'await', slot: 'r2' },
  { op: 'state' },
  { op: 'refresh' },
  { op: 'release', id: 'early' },
]);
add('dispose-drains-leases', { builtins: [{ name: 'bd', dir: 'builtin/bd', dispose: 'ok' }] }, [
  { op: 'refresh' },
  { op: 'acquire', kind: 'catalog', id: 'c' },
  { op: 'start', what: 'dispose', slot: 'd1' },
  { op: 'flush' },
  { op: 'pending', slot: 'd1' },
  { op: 'state' },
  { op: 'acquire', kind: 'catalog', id: 'late' },
  { op: 'refresh' },
  { op: 'release', id: 'c' },
  { op: 'await', slot: 'd1' },
  { op: 'state' },
  { op: 'acquire', kind: 'session', id: 'after' },
  { op: 'start', what: 'dispose', slot: 'd2' },
  { op: 'await', slot: 'd2' },
]);
add('dispose-waits-for-pending-refresh', { builtins: [{ name: 'bw', dir: 'builtin/bw', dispose: 'ok' }] }, [
  { op: 'start', what: 'refresh', slot: 'r' },
  { op: 'start', what: 'dispose', slot: 'd' },
  // Admission is still open until the pending refresh settles.
  { op: 'acquire', kind: 'catalog', id: 'c' },
  { op: 'refresh' },
  { op: 'await', slot: 'r' },
  { op: 'flush' },
  { op: 'pending', slot: 'd' },
  { op: 'state' },
  { op: 'release', id: 'c' },
  { op: 'await', slot: 'd' },
  { op: 'state' },
]);
add('dispose-retired-and-active-together', {
  builtins: [{ name: 'old', dir: 'builtin/old', dispose: 'ok' }, { name: 'keep', dir: 'builtin/keep', dispose: 'ok' }],
}, [
  { op: 'refresh' },
  { op: 'acquire', kind: 'catalog', id: 'c' },
  w('builtin/old/plugin.json', manifest({ name: 'old' })),
  { op: 'refresh' },
  { op: 'start', what: 'dispose', slot: 'd' },
  { op: 'flush' },
  { op: 'pending', slot: 'd' },
  { op: 'release', id: 'c' },
  { op: 'await', slot: 'd' },
  { op: 'state' },
]);
add('dispose-errors-aggregate', {
  builtins: [
    { name: 'e1', dir: 'builtin/e1', dispose: 'e1 failed' },
    { name: 'ok', dir: 'builtin/ok', dispose: 'ok' },
    { name: 'e2', dir: 'builtin/e2', dispose: 'e2 failed' },
  ],
}, [
  { op: 'refresh' },
  { op: 'start', what: 'dispose', slot: 'd' },
  { op: 'await', slot: 'd' },
  { op: 'state' },
]);
add('dispose-single-error', { builtins: [{ name: 'e1', dir: 'builtin/e1', dispose: 'e1 failed' }] }, [
  { op: 'refresh' },
  { op: 'start', what: 'dispose', slot: 'd' },
  { op: 'await', slot: 'd' },
]);
add('dispose-before-any-refresh', {}, [
  { op: 'start', what: 'dispose', slot: 'd' },
  { op: 'acquire', kind: 'catalog', id: 'sync' },
  { op: 'release', id: 'sync' },
  { op: 'await', slot: 'd' },
  { op: 'state' },
  { op: 'refresh' },
]);
// #543: gates expose invocation order, parallelism, and completion without clocks.
add('dispose-active-order-and-parallel', { builtins: [
  { name: 'z-first', dir: 'builtin/z', dispose: 'ok', gate: 'first' },
  { name: 'a-second', dir: 'builtin/a', dispose: 'ok' },
  { name: 'm-third', dir: 'builtin/m', dispose: 'third failed' },
] }, [
  { op: 'refresh' },
  { op: 'start', what: 'dispose', slot: 'd' },
  { op: 'flush' }, { op: 'calls' }, { op: 'pending', slot: 'd' },
  { op: 'open', gate: 'first' },
  { op: 'await', slot: 'd' }, { op: 'state' },
]);
add('dispose-retired-generations', { builtins: [
  { name: 'z-old', dir: 'builtin/z-old', dispose: 'ok' },
  { name: 'a-old', dir: 'builtin/a-old', dispose: 'ok' },
  { name: 'z-next', dir: 'builtin/z-next', dispose: 'ok' },
  { name: 'a-next', dir: 'builtin/a-next', dispose: 'ok' },
] }, [
  { op: 'refresh' }, { op: 'acquire', kind: 'catalog', id: 'g1' },
  w('builtin/z-old/plugin.json', manifest({ name: 'z-old' })),
  w('builtin/a-old/plugin.json', manifest({ name: 'a-old' })),
  { op: 'refresh' }, { op: 'acquire', kind: 'catalog', id: 'g2' },
  w('builtin/z-next/plugin.json', manifest({ name: 'z-next' })),
  w('builtin/a-next/plugin.json', manifest({ name: 'a-next' })),
  { op: 'refresh' }, { op: 'start', what: 'dispose', slot: 'd' },
  { op: 'flush' }, { op: 'calls' }, { op: 'pending', slot: 'd' },
  { op: 'start', what: 'release', id: 'g1', slot: 'r' },
  { op: 'flush' }, { op: 'calls' }, { op: 'pending', slot: 'd' },
  { op: 'release', id: 'g2' }, { op: 'await', slot: 'r' }, { op: 'await', slot: 'd' }, { op: 'state' },
]);
add('dispose-mixed-errors-and-pending-callback', { builtins: [
  { name: 'z-retired', dir: 'builtin/z-retired', dispose: 'retired failed', gate: 'retired' },
  { name: 'z-active', dir: 'builtin/z-active', dispose: 'active failed' },
  { name: 'a-active', dir: 'builtin/a-active', dispose: 'ok' },
] }, [
  { op: 'refresh' }, { op: 'acquire', kind: 'catalog', id: 'g1' },
  w('builtin/z-retired/plugin.json', manifest({ name: 'z-retired' })),
  { op: 'refresh' },
  { op: 'start', what: 'release', id: 'g1', slot: 'r' },
  { op: 'flush' }, { op: 'calls' }, { op: 'pending', slot: 'r' },
  { op: 'start', what: 'dispose', slot: 'd' },
  { op: 'flush' }, { op: 'calls' }, { op: 'pending', slot: 'd' },
  { op: 'open', gate: 'retired' },
  { op: 'await', slot: 'r' }, { op: 'await', slot: 'd' }, { op: 'state' },
]);
add('dispose-reactivated-retiree', { builtins: [
  { name: 'z-retired', dir: 'builtin/z-retired', dispose: 'retired failed' },
  { name: 'a-active', dir: 'builtin/a-active', dispose: 'active failed' },
] }, [
  { op: 'refresh' }, { op: 'acquire', kind: 'catalog', id: 'g1' },
  w('builtin/z-retired/plugin.json', manifest({ name: 'z-retired' })),
  { op: 'refresh' },
  { op: 'rm', file: 'builtin/z-retired/plugin.json' }, { op: 'refresh' },
  // Re-retire the same builtin instance before its original disposal completes.
  w('builtin/z-retired/plugin.json', manifest({ name: 'z-retired' })),
  { op: 'refresh' },
  { op: 'rm', file: 'builtin/z-retired/plugin.json' }, { op: 'refresh' },
  { op: 'start', what: 'release', id: 'g1', slot: 'r' },
  { op: 'flush' }, { op: 'pending', slot: 'r' },
  { op: 'start', what: 'dispose', slot: 'd' },
  { op: 'await', slot: 'r' }, { op: 'await', slot: 'd' }, { op: 'state' },
]);
// GatewayCommandCatalogBundle.commandsList over the real runtime.
add('catalog-lists-one-generation', {}, [
  w(`${P}/proj/plugin.json`, manifest({ name: 'proj' })),
  w(`${P}/proj/commands/run.md`, '---\ndescription: Run it\nargument-hint: [target]\n---\nRun'),
  { op: 'commandsList', input: { query: 'ru', projectKey: 'requested', limit: 5 } },
  { op: 'commandsList', input: { projectKey: 'requested' } },
  { op: 'state' },
]);
add('catalog-list-failure-still-releases', { builtins: [{ name: 'lb', dir: 'builtin/lb', dispose: 'ok' }] }, [
  { op: 'commandsList', input: { projectKey: 'k' }, list: 'throw' },
  // The failed list released its lease: retiring lb now disposes it at once.
  w('builtin/lb/plugin.json', manifest({ name: 'lb' })),
  { op: 'refresh' },
]);
add('catalog-release-failure-only-warns', { builtins: [{ name: 'rb', dir: 'builtin/rb', dispose: 'rb dispose failed' }] }, [
  { op: 'commandsList', input: { projectKey: 'k' }, list: 'retire', retire: 'builtin/rb/plugin.json' },
  { op: 'state' },
]);
add('catalog-resolve-failure-skips-refresh', {}, [
  { op: 'commandsList', input: { projectKey: 'missing' } },
  { op: 'state' },
]);
add('catalog-on-draining-runtime', {}, [
  { op: 'start', what: 'dispose', slot: 'd' },
  { op: 'await', slot: 'd' },
  { op: 'commandsList', input: { projectKey: 'k' } },
]);

// JSON fixture: undefined is recorded as an explicit tag; the scratch root is
// replaced so paths are machine-independent.
function normalize(value: unknown, root: string): unknown {
  if (value === undefined) return { $: 'undefined' };
  if (typeof value === 'string') return value.split(root).join('<ROOT>');
  if (Array.isArray(value)) return value.map((v) => normalize(v, root));
  if (value !== null && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(value)) out[normalize(key, root) as string] = normalize((value as any)[key], root);
    return out;
  }
  return value;
}
const errorRecord = (error: any): Record<string, unknown> => ({
  name: error?.name,
  message: error?.message,
  ...(Array.isArray(error?.errors) ? { errors: error.errors.map(errorRecord) } : {}),
});
function assertInside(root: string, path: string): string {
  const rel = relative(root, path);
  assert.ok(rel !== '' && rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel), `refusing to touch ${path} outside ${root}`);
  return path;
}
const flush = async () => {
  for (let i = 0; i < 5; i++) await new Promise((r) => setImmediate(r));
};
const pluginId = (plugin: any) => `${plugin.name}@${plugin.source}:${plugin.path}`;
const command = (c: any) => ({ name: c.name, description: c.description, argumentHint: c.argumentHint, content: c.content, namespace: c.namespace });
const skill = (s: any) => ({ name: s.name, description: s.description, path: s.path, content: s.content, namespace: s.namespace });
const loaded = (c: any) => ({ name: c.name, path: c.path, content: c.content, frontmatter: c.frontmatter, isSkill: c.isSkill });
const report = (r: any) => ({
  generation: r.generation,
  previous: r.previous.map(pluginId),
  next: r.next.map(pluginId),
  added: r.added.map(pluginId),
  removed: r.removed.map(pluginId),
  stagedFailures: r.stagedFailures,
});
function contributions(kind: string, c: any): unknown {
  switch (kind) {
    case 'catalog':
      return { generation: c.generation, commands: c.commands.map(command) };
    case 'session':
      return {
        generation: c.generation,
        tools: c.tools.length,
        routers: c.routers.length,
        commands: c.commands.map(command),
        skills: c.skills.map(skill),
        prompts: c.prompts.length,
        hooks: c.hooks,
        mcpServers: c.mcpServers,
        mcpInstructions: c.mcpInstructions,
      };
    default:
      return {
        generation: c.generation,
        plugins: c.plugins.map(pluginId),
        tools: c.tools.length,
        routers: c.routers.length,
        commands: c.commands.map(command),
        skills: c.skills.map(skill),
        outputStyles: c.outputStyles.map(loaded),
        hooks: c.hooks,
        mcpServers: c.mcpServers,
        lspServers: c.lspServers,
        mcpInstructions: c.mcpInstructions,
      };
  }
}
const frozen = (value: any) => Object.isFrozen(value) && Object.isFrozen(value.commands);

const scratch = await mkdtemp(join(tmpdir(), 'pdrs-plugin-runtime-'));
const recorded: unknown[] = [];
let totalSteps = 0;
try {
  for (const scenario of scenarios) {
    const root = join(scratch, scenario.id);
    const home = join(root, 'home');
    const project = join(root, 'project');
    await mkdir(home, { recursive: true });
    await mkdir(project, { recursive: true });
    const calls: unknown[] = [];
    const trace: unknown[] = [];
    const event = (name: string, args: Record<string, unknown> = {}) => calls.push(normalize({ event: name, ...args }, root));
    const gates = new Map<string, { promise: Promise<void>; open: () => void }>();
    for (const b of scenario.builtins ?? []) {
      if (b.gate && !gates.has(b.gate)) {
        let open!: () => void;
        const promise = new Promise<void>((resolve) => { open = resolve; });
        gates.set(b.gate, { promise, open });
      }
    }
    const builtins = (scenario.builtins ?? []).map((b) => {
      const plugin: any = {
        name: b.name,
        path: join(root, b.dir),
        source: 'builtin',
        manifest: { name: b.name },
      };
      if (b.mcpServers) plugin.mcpServers = b.mcpServers;
      if (b.dispose) {
        plugin.dispose = async () => {
          event('dispose', { plugin: b.name });
          if (b.gate) {
            await gates.get(b.gate)!.promise;
            event('disposeFinished', { plugin: b.name });
          }
          if (b.dispose !== 'ok') throw new Error(b.dispose);
        };
      }
      return plugin;
    });
    for (const b of scenario.builtins ?? []) await mkdir(join(root, b.dir), { recursive: true });
    if (scenario.builtinSkills) await mkdir(join(root, 'builtin-skills'), { recursive: true });
    const runtime = new PluginRuntime({
      projectRoot: project,
      pilotHome: home,
      ...(scenario.builtinSkills ? { builtinSkillsRoot: join(root, 'builtin-skills') } : {}),
      builtinPlugins: builtins,
      builtinPluginsEnabled: scenario.enabled ?? {},
    });
    let listMode: Step | undefined;
    const catalog = new GatewayCommandCatalogBundle({
      pilotHome: home,
      resolveProjectKey: async (key: string) => {
        event('resolveProjectKey', { key });
        if (key === 'missing') throw new Error('Project not found: missing');
        return project;
      },
      resolveRuntime: (key: string) => {
        event('resolveRuntime', { key });
        return { pluginRuntime: runtime };
      },
      providers: {
        listCommands: async (input: unknown, pilotHome: string, commands: readonly unknown[]) => {
          event('listCommands', { input, pilotHome, commands: commands.map(command), frozen: Object.isFrozen(commands) });
          if (listMode?.list === 'throw') throw new Error('list failed');
          if (listMode?.list === 'retire') {
            await writeFile(assertInside(root, join(root, listMode.retire)), JSON.stringify({ name: 'retired' }));
            const r = await runtime.refreshWithReport();
            event('refreshedDuringList', { generation: r.generation });
          }
          return { listed: commands.length };
        },
      },
      warn: (message: string, error: unknown) => event('warn', { message, error: errorRecord(error) }),
    });
    const leases = new Map<string, any>();
    const slots = new Map<string, { promise: Promise<unknown>; settled: boolean }>();
    const results: unknown[] = [];
    for (const step of scenario.steps) {
      const out: Record<string, unknown> = {};
      switch (step.op) {
        case 'write': {
          const file = assertInside(root, join(root, step.file));
          await mkdir(dirname(file), { recursive: true });
          await writeFile(file, step.text);
          break;
        }
        case 'rm':
          await rm(assertInside(root, join(root, step.file)), { recursive: true });
          break;
        case 'refresh':
          try {
            out.result = normalize(report(await runtime.refreshWithReport()), root);
          } catch (error) {
            out.error = normalize(errorRecord(error), root);
          }
          break;
        case 'state':
          out.result = normalize({
            generation: runtime.generation,
            lifecycleState: runtime.lifecycleState,
            snapshot: runtime.snapshot().map(pluginId),
            view: runtime.createView().getAllCommands().map(command),
            commands: runtime.getAllCommands().map(command),
            skills: runtime.getAllSkills().map(skill),
            mcpServers: runtime.mcpServers(),
            lspServers: runtime.lspServers(),
            mcpInstructions: runtime.getAllMcpInstructions(),
          }, root);
          break;
        case 'acquire':
          try {
            const lease = step.kind === 'catalog'
              ? runtime.acquireCommandCatalog()
              : step.kind === 'session'
                ? runtime.acquireSessionContributions()
                : runtime.acquireContributionSnapshot();
            leases.set(step.id, lease);
            out.result = normalize({
              generation: lease.generation,
              ...(step.kind === 'legacy' ? {} : { frozen: frozen(lease.contributions) }),
              contributions: contributions(step.kind, lease.contributions),
            }, root);
          } catch (error) {
            out.error = normalize(errorRecord(error), root);
          }
          break;
        case 'release':
          assert.ok(leases.has(step.id), `${scenario.id}: release of unacquired lease ${step.id}`);
          try {
            await leases.get(step.id).release();
            out.result = 'released';
          } catch (error) {
            out.error = normalize(errorRecord(error), root);
          }
          break;
        case 'start': {
          const promise = step.what === 'refresh'
            ? runtime.refreshWithReport()
            : step.what === 'release'
              ? leases.get(step.id).release()
              : runtime.dispose();
          const slot = { promise, settled: false };
          promise.then(() => { slot.settled = true; }, () => { slot.settled = true; });
          slots.set(step.slot, slot);
          break;
        }
        case 'await':
          try {
            const value = await slots.get(step.slot)!.promise;
            out.result = value === undefined ? 'done' : normalize(report(value), root);
          } catch (error) {
            out.error = normalize(errorRecord(error), root);
          }
          break;
        case 'pending':
          out.result = !slots.get(step.slot)!.settled;
          break;
        case 'calls':
          out.result = [...trace, ...calls];
          break;
        case 'open':
          gates.get(step.gate)!.open();
          break;
        case 'flush':
          await flush();
          break;
        case 'commandsList':
          listMode = step;
          try {
            out.result = normalize(await catalog.commandsList(step.input), root);
          } catch (error) {
            out.error = normalize(errorRecord(error), root);
          }
          listMode = undefined;
          break;
        default:
          throw new Error(`unknown op ${step.op}`);
      }
      out.calls = calls.splice(0);
      trace.push(...out.calls as unknown[]);
      results.push(out);
      totalSteps += 1;
    }
    for (const slot of slots.values()) await slot.promise.catch(() => undefined);
    await runtime.dispose().catch(() => undefined);
    await flush();
    assert.equal(calls.filter((c: any) => c.event !== 'dispose').length, 0, `${scenario.id}: stray calls`);
    recorded.push({
      id: scenario.id,
      builtins: scenario.builtins ?? [],
      enabled: scenario.enabled ?? {},
      builtinSkills: scenario.builtinSkills ?? false,
      steps: scenario.steps,
      results,
      teardown: calls.splice(0),
    });
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
assert.ok(recorded.length > 0 && totalSteps > 0, 'no scenarios recorded');
const oracle = {
  sourceCommit,
  node: process.version,
  sourceSha256,
  count: recorded.length,
  steps: totalSteps,
  scenarios: recorded,
};
await writeFile(join(outputDir, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.log(JSON.stringify({ scenarios: recorded.length, steps: totalSteps, sources: Object.keys(sourceSha256).length, output: join(outputDir, 'oracle.json') }));
