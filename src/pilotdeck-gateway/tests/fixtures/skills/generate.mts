// Run from repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
import { lstat, mkdir, mkdtemp, readFile, readdir, readlink, rm, symlink, truncate, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory or the Rust scratch root');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', 'baseline must be the fixed PilotDeck commit');
const sources = [
  'src/extension/skills/SkillManager.ts', 'src/extension/skills/types.ts',
  'src/composition/types.ts', 'src/cli/createLocalGateway.ts',
];
const { SkillManager } = await import(pathToFileURL(join(baseline, sources[0])).href);
const { isDisabledModuleBinding, isExternalModuleBinding } = await import(pathToFileURL(join(baseline, sources[2])).href);
// localeCompare / toLocaleLowerCase follow the ICU default locale; Rust pins the same one.
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const sha = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
// Inputs carry "<dir>" for the scenario root and "<cursor:K>" for step K's nextCursor;
// "<cursor:K:how>" additionally feeds that cursor through mangleCursor().
const CURSOR_RE = /^<cursor:(\d+)(?::([a-z-]+))?>$/;
function substitute(value: any, root: string, cursors: (string | undefined)[] = []): any {
  if (typeof value === 'string') {
    const m = CURSOR_RE.exec(value);
    if (m) {
      const raw = cursors[Number(m[1])];
      assert.ok(raw, `step ${m[1]} returned no cursor`);
      return m[2] ? mangleCursor(raw, m[2]) : raw;
    }
    return value.split('<dir>').join(root);
  }
  if (Array.isArray(value)) return value.map(v => substitute(v, root, cursors));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, substitute(v, root, cursors)]));
  return value;
}
// Node's base64 decoding is character-set lenient: "+" decodes like "-", "=" ends
// the payload, and unrelated junk is skipped. Each named probe mangles a cursor
// the manager itself produced, so the real decoder decides the outcome. A cursor
// as emitted never carries '-'/'_': base64url of pure-ASCII JSON yields sextet
// 62/63 only from a 3-byte group's third byte ('>' '?' '~' DEL in ASCII), bytes
// the cursor's JSON never contains. 'ignored-field' lifts exactly that limit: it
// keeps offset and signature but adds a field the decoder never reads, whose
// '>>>???' text supplies those bytes, so the re-encoded cursor does carry both
// specials (asserted) and 'std-alphabet' has real characters to swap for '+'/'/'.
// That pins the two alphabets' equivalence on real TS outcomes: the Rust
// base64_lenient unit tests cover only skipping, '=' termination and truncation.
function mangleCursor(raw: string, how: string): string {
  const half = Math.floor(raw.length / 2);
  if (how === 'plus-prefix') return `+${raw}`;
  if (how === 'equals-middle') return `${raw.slice(0, half)}=${raw.slice(half)}zzzz`;
  if (how === 'equals-suffix') return `${raw}=zzzz`;
  if (how === 'ignored-field') {
    const outer = JSON.parse(Buffer.from(raw, 'base64url').toString('utf8'));
    const url = Buffer.from(JSON.stringify({ ...outer, ignored: '>>>???' })).toString('base64url');
    assert.ok(url.includes('-') && url.includes('_'), 'ignored-field cursor lacks the url-safe specials');
    return url;
  }
  if (how === 'std-alphabet') return mangleCursor(raw, 'ignored-field').replaceAll('-', '+').replaceAll('_', '/');
  throw new Error(`unknown cursor mangle ${how}`);
}
// Outputs: root -> "<dir>", mtime -> "<mtime>" (unless kept), nextCursor decoded.
// Object.fromEntries keeps an own "__proto__" key, as yaml's toJS does.
function normalize(value: any, root: string, keepMtime = false): any {
  if (typeof value === 'string') return value.split(root).join('<dir>');
  if (Array.isArray(value)) return value.map(v => normalize(v, root, keepMtime));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => {
      if (k === 'mtime' && typeof v === 'number' && !keepMtime) return [k, '<mtime>'];
      if (k === 'nextCursor' && typeof v === 'string') return [k, cursorView(v, root)];
      return [k, normalize(v, root, keepMtime)];
    }));
  }
  return value;
}
// The signature embeds projectKey (a machine path): keep the raw cursor only when portable.
function cursorView(raw: string, root: string) {
  const outerText = Buffer.from(raw, 'base64url').toString('utf8');
  const outer = JSON.parse(outerText);
  const innerText = Buffer.from(outer.signature, 'base64url').toString('utf8');
  const portable = !innerText.includes(root);
  return { raw: portable ? raw : null, offset: outer.offset, signature: normalize(JSON.parse(innerText), root) };
}

type Entry =
  | { path: string; kind: 'dir' }
  | { path: string; kind: 'file'; text?: string; base64?: string; size?: number }
  | { path: string; kind: 'symlink'; target: string }
  | { path: string; kind: 'fifo' }
  | { path: string; kind: 'bulk'; count: number };
async function setupTree(root: string, entries: Entry[]) {
  for (const e of entries) {
    const abs = join(root, e.path);
    if (e.kind === 'dir') { await mkdir(abs, { recursive: true }); continue; }
    await mkdir(dirname(abs), { recursive: true });
    if (e.kind === 'symlink') await symlink(e.target, abs);
    else if (e.kind === 'fifo') execFileSync('mkfifo', [abs]);
    else if (e.kind === 'bulk') { await mkdir(abs, { recursive: true }); for (let i = 0; i < e.count; i++) await writeFile(join(abs, `f${String(i).padStart(3, '0')}.txt`), 'x'); }
    else if (e.size !== undefined) { await writeFile(abs, ''); await truncate(abs, e.size); }
    else await writeFile(abs, e.base64 !== undefined ? Buffer.from(e.base64, 'base64') : e.text!);
  }
}
// Every file is recorded by size + SHA-256; small UTF-8 files also by text (BOM kept).
async function snapshotTree(root: string) {
  const out: any[] = [];
  async function walk(rel: string) {
    for (const name of (await readdir(join(root, rel))).sort()) {
      const path = rel ? `${rel}/${name}` : name;
      const abs = join(root, path);
      const st = await lstat(abs);
      if (st.isSymbolicLink()) out.push({ path, kind: 'symlink', target: (await readlink(abs)).split(root).join('<dir>') });
      else if (st.isDirectory()) { out.push({ path, kind: 'dir' }); await walk(path); }
      else if (st.isFIFO()) out.push({ path, kind: 'fifo' }); // reading it would block
      else {
        const bytes = await readFile(abs);
        const entry: any = { path, kind: 'file', size: bytes.length, sha256: sha(bytes) };
        if (bytes.length <= 4096) {
          try { entry.text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes); } catch { entry.base64 = bytes.toString('base64'); }
        }
        out.push(entry);
      }
    }
  }
  await walk('');
  return out;
}
type Options = { builtin?: boolean; env?: Record<string, string>; generalCwdPaths?: string[] };
function makeManager(root: string, options: Options) {
  return new SkillManager({
    pilotHome: join(root, 'home'),
    ...(options.builtin === false ? {} : { builtinSkillsRoot: join(root, 'builtin') }),
    env: substitute(options.env ?? {}, root),
    ...(options.generalCwdPaths ? { generalCwdPaths: substitute(options.generalCwdPaths, root) } : {}),
  });
}
async function runStep(manager: any, step: any, root: string, cursors: (string | undefined)[], keepMtime = false) {
  const input = substitute(step.input, root, cursors);
  try {
    const result = await manager[step.op](input);
    cursors.push(typeof result?.nextCursor === 'string' ? result.nextCursor : undefined);
    if (result && typeof result === 'object' && 'skill' in result && result.skill) assert.equal(typeof result.skill.mtime, 'number');
    return { result: normalize(result, root, keepMtime) };
  } catch (error: any) {
    cursors.push(undefined);
    // V8 TypeError wording is not part of the contract; name and code are.
    const e: any = { name: error.name, code: error.code ?? null };
    if (error.name !== 'TypeError') e.message = normalize(error.message, root);
    if (error.validation) e.validation = normalize(error.validation, root);
    return { error: e };
  }
}
async function runSteps(manager: any, steps: any[], root: string, keepMtime = false) {
  const cursors: (string | undefined)[] = [];
  const out = [];
  const savedHome = process.env.HOME;
  process.env.HOME = root; // os.homedir() for "~"; Rust passes home_dir = root.
  try {
    for (const step of steps) out.push({ ...step, outcome: await runStep(manager, step, root, cursors, keepMtime) });
  } finally { process.env.HOME = savedHome; }
  // Mangled cursors record as literals, like portable nextCursor.raw values; the
  // probes keep projectKey null so no machine path can survive here. The literal
  // scan below cannot see a root hidden inside the encoded signature, so the raw
  // cursor must first survive cursorView's double decode.
  for (const step of steps) {
    const input = step.input;
    if (!input || typeof input.cursor !== 'string') continue;
    const m = /^<cursor:(\d+):([a-z-]+)>$/.exec(input.cursor);
    if (!m) continue;
    const raw = cursors[Number(m[1])];
    assert.ok(raw, `step ${m[1]} returned no cursor`);
    assert.ok(cursorView(raw, root).raw !== null, `${m[2]} source cursor embeds the scenario root`);
    const mangled = mangleCursor(raw, m[2]);
    assert.ok(!mangled.includes(root), `${m[2]} cursor embeds the scenario root`);
    input.cursor = mangled;
  }
  return out;
}

const md = (fm: string, body = '\nBody.\n') => `---\n${fm}\n---\n${body}`;
const file = (path: string, text: string): Entry => ({ path, kind: 'file', text });
const skill = (dir: string, name: string, description: string, extra = ''): Entry => file(`${dir}/SKILL.md`, md(`name: ${name}\ndescription: ${description}${extra}`));
const user = (slug: unknown, extra: object = {}) => ({ scope: 'user', slug, ...extra });
const op = (name: string, input: unknown) => ({ op: name, input });

// ---------------------------------------------------------------------------
// --verify-rust <root> read|mutate: the real TS manager on a tree Rust wrote.
// ---------------------------------------------------------------------------
if (process.argv[3] === '--verify-rust') {
  const mode = process.argv[4];
  const manager = makeManager(output, {});
  const proj = '<dir>/proj';
  if (mode === 'read') {
    const steps = await runSteps(manager, [
      op('list', { projectKey: proj, limit: 50 }),
      op('list', { projectKey: proj, query: 'rust', limit: 2 }),
      op('read', user('rust-made')),
      op('read', user('rust-written')),
      op('read', { scope: 'project', slug: 'rust-proj', projectKey: proj }),
      op('read', { scope: 'builtin', slug: 'rust-bi' }),
      op('validate', { sourcePath: '<dir>/home/skills/rust-made' }),
    ], output, true);
    for (const s of steps) assert.ok(!s.outcome.error, `${s.op} failed: ${JSON.stringify(s.outcome)}`);
    console.log(JSON.stringify({ steps }));
  } else {
    assert.equal(mode, 'mutate');
    const steps = await runSteps(manager, [
      op('create', user('ts-made', { name: 'TS Made', description: 'Created by the TS manager for Rust to read', body: 'TS body' })),
      op('write', user('ts-written', { content: '---\nname: TS \ud800 written\ndescription: Lone surrogate encoded by TS\n---\n' })),
      op('delete', user('rust-doomed')),
      op('create', { scope: 'project', slug: 'ts-proj', projectKey: proj, content: '---\nname: TS Project\n---\n' }),
    ], output, true);
    for (const s of steps) assert.ok(!s.outcome.error, `${s.op} failed: ${JSON.stringify(s.outcome)}`);
    const after = await runSteps(manager, [
      op('list', { projectKey: proj, limit: 50 }),
      op('read', user('ts-made')),
      op('read', user('ts-written')),
      op('read', { scope: 'project', slug: 'ts-proj', projectKey: proj }),
      op('read', user('rust-doomed')),
    ], output, true);
    assert.equal(after.at(-1)!.outcome.error?.code, 'not_found');
    console.log(JSON.stringify({ steps, after }));
  }
  process.exit(0);
}

// ---------------------------------------------------------------------------
// Fixture generation.
// ---------------------------------------------------------------------------
await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
const scenarios: any[] = [];
async function scenario(name: string, spec: { options?: Options; setup?: Entry[]; steps: any[] }) {
  const root = join(scratch, name);
  await mkdir(root, { recursive: true });
  await setupTree(root, spec.setup ?? []);
  const manager = makeManager(root, spec.options ?? {});
  const steps = await runSteps(manager, spec.steps, root);
  scenarios.push({ name, options: spec.options ?? {}, setup: spec.setup ?? [], steps, tree: await snapshotTree(root) });
  console.log(`${name}: steps=${steps.length}`);
}
// Every step must reach the branch its scenario claims.
const kind = (o: any, opName: string) => {
  if (o.error) return o.error.code ?? o.error.name;
  const r = o.result;
  switch (opName) {
    case 'list': return `list:${r.items.length}${r.nextCursor ? '+' : ''}`;
    case 'read': case 'write': case 'create': return `${opName}:${r.skill ? r.skill.name : null}`;
    case 'delete': return 'deleted';
    case 'validate': return `${r.ok ? 'ok' : 'fail'}:${r.hardFails.map((i: any) => i.code).join(',')}|${r.warnings.map((i: any) => i.code).join(',')}`;
    case 'import': return `import:${r.mode}:${r.slug}:${r.skill ? r.skill.name : null}`;
    case 'scan': return `scan:${r.folders.map((f: any) => `${f.folderName}${f.hasSkillMd ? '*' : ''}`).join(',')}`;
    default: throw new Error(`unknown op ${opName}`);
  }
};
function expectKinds(expected: string[]) {
  const last = scenarios.at(-1);
  assert.deepEqual(last.steps.map((s: any) => kind(s.outcome, s.op)), expected, last.name);
  return last.steps.map((s: any) => s.outcome);
}
const times = (n: number, k: string) => Array(n).fill(k);

try {
  // list: scope overrides, skipped entries, locale sort, query, cursor paging, input checks.
  const proj = '<dir>/proj';
  await scenario('list', {
    setup: [
      skill('builtin/alpha', 'Alpha', 'Alpha builtin skill for testing'),
      skill('builtin/beta', 'Beta', 'Beta builtin skill shadowed by the user'),
      skill('builtin/gamma', 'Gamma', 'Gamma builtin skill, shadowed twice', '\nversion: 1.5'),
      skill('builtin/.hidden', 'Hidden', 'Invalid slug, never listed'),
      file('builtin/notes.txt', 'a file, not a skill directory'),
      { path: 'builtin/empty-dir', kind: 'dir' },
      { path: 'builtin/mddir/SKILL.md', kind: 'dir' },
      { path: 'builtin/linked', kind: 'symlink', target: '../src/linked-target' },
      skill('src/linked-target', 'Linked', 'Reached through a directory symlink'),
      { path: 'builtin/broken', kind: 'symlink', target: '../nowhere' },
      skill('home/skills/beta', 'User Beta', 'User override of beta', '\nversion: "2.0.0"'),
      skill('home/skills/gamma', 'User Gamma', 'User gamma, itself shadowed by the project'),
      skill('home/skills/Delta', 'Delta', 'Capitalised slug sorts by locale', '\nversion: true'),
      ...['a_b', 'a-b', 'a.b', 'A2', 'a1', '10', '9'].map(s => skill(`home/skills/${s}`, `Slug ${s}`, `Collation probe ${s}`)),
      skill('proj/.pilotdeck/skills/gamma', 'Project Gamma', 'Project gamma wins over everything'),
      skill('proj/.pilotdeck/skills/alpha', 'Project Alpha', 'Project alpha overrides builtin alpha'),
      skill('proj/.pilotdeck/skills/zeta', 'Zeta', 'alpha alpha in the description'),
    ],
    steps: [
      op('list', { projectKey: proj, limit: 50 }),
      op('list', {}),
      op('list', { projectKey: '<dir>/home', limit: 50 }),
      op('list', { projectKey: '', limit: 50 }),
      op('list', { projectKey: proj, scope: 'builtin' }),
      op('list', { projectKey: proj, scope: 'user' }),
      op('list', { projectKey: proj, scope: 'project' }),
      op('list', { projectKey: proj, scope: 'plugin' }),
      op('list', { projectKey: proj, query: '  ALPHA ', limit: 50 }),
      op('list', { projectKey: proj, query: 'a', limit: 5 }),
      op('list', { projectKey: proj, query: 'a', limit: 5, cursor: '<cursor:9>' }),
      op('list', { projectKey: proj, query: 'a', limit: 5, cursor: '<cursor:10>' }),
      op('list', { projectKey: proj, query: 'a', limit: 5, cursor: '<cursor:11>' }),
      op('list', { projectKey: proj, query: 'b', limit: 5, cursor: '<cursor:9>' }),
      op('list', { projectKey: proj, limit: 5, cursor: '<cursor:9>' }),
      op('list', { scope: 'bogus' }),
      op('list', { scope: 5 }),
      op('list', { query: 'x'.repeat(257) }),
      op('list', { limit: 0 }),
      op('list', { limit: 51 }),
      op('list', { limit: 1.5 }),
      op('list', { limit: '5' }),
      op('list', { cursor: 'garbage!!' }),
      op('list', { cursor: 5 }),
      op('list', { projectKey: proj, limit: 50, cursor: '' }),
      op('list', { projectKey: proj, limit: null }),
      op('list', { projectKey: proj, query: 'zzz-none' }),
      op('list', { query: 'x'.repeat(256) }),
      // Non-string query reaches .trim(); an array cursor goes through Buffer.from(array).
      op('list', { query: 5 }),
      op('list', { cursor: [8, 109] }),
      // Node's lenient base64url decoding, probed on real cursors (projectKey stays
      // null so the recorded literals carry no machine path). Step 33 consumes the
      // cursor untouched; each positive probe must land on that exact page: '='
      // ends the payload before the suffix junk, '+'/'/' decode as '-'/'_'. A
      // decoder made strict (rejecting '+'/'/'/'=') fails steps 34 and 36.
      op('list', { query: 'a', limit: 5 }),
      op('list', { query: 'a', limit: 5, cursor: '<cursor:30:plus-prefix>' }),
      op('list', { query: 'a', limit: 5, cursor: '<cursor:30:equals-middle>' }),
      op('list', { query: 'a', limit: 5, cursor: '<cursor:30>' }),
      op('list', { query: 'a', limit: 5, cursor: '<cursor:30:equals-suffix>' }),
      op('list', { query: 'a', limit: 5, cursor: '<cursor:30:ignored-field>' }),
      op('list', { query: 'a', limit: 5, cursor: '<cursor:30:std-alphabet>' }),
    ],
  });
  {
    const o = expectKinds([
      'list:17', 'list:10+', 'list:14', 'list:14', 'list:4', 'list:10', 'list:3', 'list:0', 'list:3',
      'list:5+', 'list:5+', 'list:5+', 'list:2', ...times(11, 'invalid_input'), 'list:17', 'list:10+', 'list:0', 'list:0',
      'TypeError', 'invalid_input',
      'list:5+', 'invalid_input', 'invalid_input', 'list:5+', 'list:5+', 'list:5+', 'list:5+',
    ]);
    // The projectKey "" quirk: echoed, yet no project listing.
    assert.equal(o[3].result.projectPath, '');
    assert.equal(o[2].result.projectPath, null);
    const byScope = (r: any) => Object.fromEntries(['builtin', 'user', 'project'].map(s => [s, r[s].map((k: any) => [k.slug, k.overriddenBy ?? null, k.overridesBuiltin ?? null])]));
    assert.deepEqual(byScope(o[0].result).builtin, [['alpha', 'project', null], ['beta', 'user', null], ['gamma', 'project', null], ['linked', null, null]]);
    assert.ok(o[8].result.items.find((i: any) => i.slug === 'zeta').matches.length === 2);
    // Every accepted probe lands on the untouched control page (o[33]).
    assert.equal(o[33].result.nextCursor.offset, 10);
    for (const i of [34, 35, 36]) assert.deepEqual(o[i].result, o[33].result);
  }

  await scenario('list-scandir-error', {
    setup: [file('home/skills', 'a file where the user skills root should be')],
    steps: [op('list', {}), op('read', user('x')), op('write', user('x', { content: 'y' })), op('create', user('x')), op('delete', user('x'))],
  });
  expectKinds(times(5, 'ENOTDIR'));

  await scenario('no-builtin', {
    options: { builtin: false },
    setup: [skill('home/skills/solo', 'Solo', 'Only user skills exist here')],
    steps: [op('list', {}), op('read', { scope: 'builtin', slug: 'solo' }), op('write', { scope: 'builtin', slug: 'solo', content: 'x' }), op('read', { scope: 'builtin', slug: '../x' })],
  });
  expectKinds(['list:1', 'not_configured', 'read_only', 'invalid_slug']);

  // read: frontmatter shapes, scopes, every error branch.
  const badUtf8 = Buffer.concat([Buffer.from('---\nname: caf'), Buffer.from([0xff]), Buffer.from('\n---\n')]).toString('base64');
  const readSlugs: [string, string][] = [
    ['plain', 'Plain'], ['nofm', 'nofm'], ['unclosed', 'unclosed'], ['crlf', 'CRLF'], ['tight', 'Tight'],
    ['compat', 'Compat: broken'], ['dupkey', 'B'], ['arrayfm', 'arrayfm'], ['scalarfm', 'scalarfm'], ['types', 'types'],
    ['cyclic', 'Cyc'], ['bom', 'bom'], ['badutf8', 'caf\ufffd'], ['inf', ''], ['surrogate', '\ud800x'],
  ];
  await scenario('read', {
    setup: [
      file('home/skills/plain/SKILL.md', '---\nname: Plain\ndescription: A plain skill used for read tests\nversion: 2.0.0\n---\n\n# body\n'),
      file('home/skills/nofm/SKILL.md', '# just markdown\n'),
      file('home/skills/unclosed/SKILL.md', '---\nname: Never closed\n'),
      file('home/skills/crlf/SKILL.md', '---\r\nname: CRLF\r\ndescription: with crlf\r\n---\r\nbody'),
      file('home/skills/tight/SKILL.md', '---\nname: Tight\n---'),
      file('home/skills/compat/SKILL.md', "---\nname: Compat: broken\ndescription: >\n  folded line one\n  line two\nversion: '1'\n---\n"),
      file('home/skills/dupkey/SKILL.md', '---\nname: A\nname: B\n---\n'),
      file('home/skills/arrayfm/SKILL.md', '---\n- a\n- b\n---\n'),
      file('home/skills/scalarfm/SKILL.md', '---\njust text\n---\n'),
      file('home/skills/types/SKILL.md', '---\nname: 42\ndescription: [x]\nversion: 3\n---\n'),
      file('home/skills/cyclic/SKILL.md', '---\nname: Cyc\na: &x\n  b: *x\n---\n'),
      file('home/skills/bom/SKILL.md', '\ufeff---\nname: Bom\n---\n'),
      { path: 'home/skills/badutf8/SKILL.md', kind: 'file', base64: badUtf8 },
      file('home/skills/inf/SKILL.md', "---\nversion: .inf\nname: ''\n---\n"),
      file('home/skills/surrogate/SKILL.md', '---\nname: "\\ud800x"\ndescription: "\\ud83d\\ude00 emoji"\n---\n'),
      { path: 'home/skills/mddir/SKILL.md', kind: 'dir' },
      file('home/skills/afile', 'a plain file in place of a skill directory'),
      skill('builtin/bi', 'Builtin', 'A builtin skill read in place'),
      skill('proj/.pilotdeck/skills/p1', 'Project One', 'A project skill read by key'),
    ],
    steps: [
      ...readSlugs.map(([slug]) => op('read', user(slug))),
      op('read', { scope: 'builtin', slug: 'bi' }),
      op('read', { scope: 'project', slug: 'p1', projectKey: proj }),
      op('read', { scope: 'project', slug: 'p1', projectKey: `${proj}/` }),
      op('read', { scope: 'plugin', slug: 'plain' }),
      op('read', { slug: 'plain' }),
      op('read', user('missing')),
      op('read', user('mddir')),
      op('read', user('afile')),
      ...['../x', 'a..b', '', '.dot', 'x/y', 'a'.repeat(101), 5, undefined, 'é'].map(slug => op('read', user(slug))),
      op('read', user('a'.repeat(100))),
      op('read', { scope: 'project', slug: 'p1' }),
      op('read', { scope: 'project', slug: 'p1', projectKey: '<dir>/home' }),
      op('read', { scope: 'project', slug: 'p1', projectKey: null }),
      op('read', { scope: 'project', slug: 'p1', projectKey: 5 }),
      op('read', null),
    ],
  });
  {
    const o = expectKinds([
      ...readSlugs.map(([, name]) => `read:${name}`),
      'read:Builtin', 'read:Project One', 'read:Project One', 'read:Plain', 'read:Plain',
      'not_found', 'EISDIR', 'ENOTDIR', ...times(9, 'invalid_slug'), 'not_found',
      'project_required', 'project_required', 'project_required', 'ERR_INVALID_ARG_TYPE', 'TypeError',
    ]);
    assert.equal(o[0].result.skill.version, '2.0.0');
    assert.equal(o[9].result.skill.version, '3');
    assert.equal(o[13].result.skill.version, 'Infinity');
    assert.equal(o[5].result.skill.description, 'folded line one\nline two');
    assert.equal(o[15].result.skill.readonly, true);
  }

  // Virtual project roots: desktop runtime root, its Windows-style suffix, generalCwdPaths and pilotHome.
  await scenario('virtual-roots', {
    options: { env: { PILOTDECK_DESKTOP: '1', PILOTDECK_DESKTOP_RUNTIME_ROOT: proj }, generalCwdPaths: ['<dir>/general'] },
    setup: [
      skill('proj/.pilotdeck/skills/p1', 'Runtime', 'Inside the desktop runtime root'),
      skill('other/.pilotdeck/skills/p2', 'Other', 'A real project'),
      skill('x/Resources/Runtime/.pilotdeck/skills/p3', 'Win', 'Matches the runtime suffix'),
      skill('general/.pilotdeck/skills/g', 'General', 'A configured general cwd'),
    ],
    steps: [
      op('read', { scope: 'project', slug: 'p1', projectKey: proj }),
      op('read', { scope: 'project', slug: 'p3', projectKey: '<dir>/x/Resources/Runtime' }),
      op('read', { scope: 'project', slug: 'g', projectKey: '<dir>/general/' }),
      op('read', { scope: 'project', slug: 'g', projectKey: '<dir>/home' }),
      op('read', { scope: 'project', slug: 'p2', projectKey: '<dir>/other' }),
      op('list', { projectKey: proj, limit: 50 }),
      op('list', { projectKey: '<dir>/other', limit: 50 }),
    ],
  });
  expectKinds(['project_required', 'project_required', 'project_required', 'project_required', 'read:Other', 'list:0', 'list:1']);

  await scenario('write', {
    setup: [
      skill('home/skills/existing', 'Existing', 'Will be overwritten by write'),
      file('home/skills/blocker', 'a file where a skill directory would go'),
      { path: 'home/skills/mdisdir/SKILL.md', kind: 'dir' },
      skill('builtin/bi', 'Builtin', 'Read-only'),
    ],
    steps: [
      op('write', { scope: 'builtin', slug: 'bi', content: 'x' }),
      op('write', { scope: 'builtin', slug: 'bi', content: 5 }),
      op('write', user('fresh', { content: '---\nname: Fresh\ndescription: Written through write()\n---\n' })),
      op('write', user('existing', { content: 'no frontmatter at all' })),
      op('write', user('empty', { content: '' })),
      op('write', user('lone', { content: '---\nname: Lone \ud800\n---\n' })),
      op('write', user('crlf', { content: '---\r\nname: Win\r\n---\r\n' })),
      op('write', user('nocontent')),
      op('write', user('num', { content: 5 })),
      op('write', user('../evil', { content: 5 })),
      op('write', user('../evil', { content: 'x' })),
      op('write', { scope: 'project', slug: 'p', content: 'x' }),
      op('write', { scope: 'project', slug: 'p', content: '---\nname: P\n---\n', projectKey: proj }),
      op('write', user('blocker', { content: 'x' })),
      op('write', user('mdisdir', { content: 'x' })),
    ],
  });
  expectKinds([
    'read_only', 'read_only', 'write:Fresh', 'write:existing', 'write:empty', 'write:Lone \ufffd', 'write:Win',
    'invalid_input', 'invalid_input', 'invalid_input', 'invalid_slug', 'project_required', 'write:P', 'EEXIST', 'EISDIR',
  ]);

  await scenario('create', {
    setup: [
      skill('home/skills/taken', 'Taken', 'Already exists'),
      file('home/skills/plainfile', 'x'),
      file('notadir', 'x'),
      skill('builtin/bi', 'Builtin', 'Read-only'),
    ],
    steps: [
      op('create', user('made', { name: 'Made Skill', description: 'Line one\nLine two', body: '  Body text  ' })),
      op('create', user('defaults')),
      op('create', user('blankcontent', { content: '   \n', name: 'From fields' })),
      op('create', user('custom', { content: '---\nname: Custom\n---\nraw' })),
      op('create', user('spaced', { name: '  Spaced\nName  ', description: '   ' })),
      op('create', user('emptyname', { name: '' })),
      op('create', user('nulldesc', { description: null, body: null })),
      op('create', user('taken')),
      op('create', user('plainfile')),
      op('create', { scope: 'builtin', slug: 'bi' }),
      op('create', user('.bad')),
      op('create', { scope: 'project', slug: 'p', projectKey: '<dir>/notadir' }),
      op('create', user('badname', { name: 5 })),
      op('create', user('badbody', { body: 5 })),
      op('create', { scope: 'project', slug: 'proj-made', projectKey: proj, description: 'Project scoped' }),
      op('create', user('lonename', { name: 'Lone\ud800' })),
      op('create', user('numbody', { body: 0 })),
    ],
  });
  expectKinds([
    'create:Made Skill', 'create:defaults', 'create:From fields', 'create:Custom', 'create:Spaced Name', 'create:emptyname', 'create:nulldesc',
    'conflict', 'conflict', 'read_only', 'invalid_slug', 'ENOTDIR', 'TypeError', 'TypeError', 'create:proj-made', 'create:Lone\ufffd', 'create:numbody',
  ]);

  await scenario('delete', {
    setup: [
      skill('home/skills/doomed', 'Doomed', 'Removed with its tree'),
      file('home/skills/doomed/nested/deep.txt', 'deep'),
      { path: 'home/skills/link', kind: 'symlink', target: '../../src/target' },
      skill('src/target', 'Target', 'Survives deleting the link'),
      file('home/skills/plainfile', 'x'),
      file('notadir', 'x'),
      skill('builtin/bi', 'Builtin', 'Read-only'),
    ],
    steps: [
      op('delete', user('doomed')),
      op('delete', user('doomed')),
      op('delete', user('link')),
      op('delete', user('plainfile')),
      op('delete', { scope: 'builtin', slug: 'bi' }),
      op('delete', user('..')),
      op('delete', { scope: 'project', slug: 'p' }),
      op('delete', { scope: 'project', slug: 'p', projectKey: '<dir>/notadir' }),
      op('list', { limit: 50 }),
    ],
  });
  expectKinds(['deleted', 'deleted', 'deleted', 'deleted', 'read_only', 'invalid_slug', 'project_required', 'ENOTDIR', 'list:1']);

  // validate { sourcePath }: every disk-side hard fail and warning.
  const ok = (dir: string, description = 'A complete skill with every kind of file') => skill(dir, 'Good', description);
  const goodKind = 'ok:|file_large,risky_extension,contains_symlink,risky_extension,risky_extension';
  // The caps compare with `>`: a bundle landing exactly on 50 MiB stays legal, so
  // the SKILL.md is sized to make the walked total hit the boundary exactly.
  const totalEqMd = md('name: Good\ndescription: Total exactly on the fifty MiB cap');
  const totalEqSlack = 50 * 1048576 - 4 * 10 * 1048576 - Buffer.byteLength(totalEqMd);
  assert.ok(totalEqSlack > 1048576 && totalEqSlack <= 10 * 1048576, `slack ${totalEqSlack}`);
  await scenario('validate-disk', {
    setup: [
      ok('src/good'),
      file('src/good/scripts/run.sh', 'echo'),
      file('src/good/bin/tool.EXE', 'MZ'),
      file('src/good/archive.tar.gz', 'gz'),
      file('src/good/.sh', 'a dotfile has no extension'),
      { path: 'src/good/big.dat', kind: 'file', size: 2 * 1048576 },
      file('src/good/nested/deep/x.md', 'deep'),
      { path: 'src/good/link', kind: 'symlink', target: 'nested' },
      file('src/good/数据.Dylib', 'x'),
      ok('src/short', 'Too short'),
      ok('src/long', 'x'.repeat(1100)),
      file('src/noname/SKILL.md', md('title: nothing required')),
      file('src/nofm/SKILL.md', '# no frontmatter\n'),
      file('src/empty/SKILL.md', '  \n'),
      file('src/nomd/readme.md', 'x'),
      file('src/afile', 'x'),
      file('src/compat/SKILL.md', '---\nname: Compat: broken\ndescription: Parsed by the compatibility fallback\n---\n'),
      ok('src/huge'),
      { path: 'src/huge/huge.bin', kind: 'file', size: 11 * 1048576 },
      ok('src/exact10'),
      { path: 'src/exact10/exact.bin', kind: 'file', size: 10 * 1048576 },
      ok('src/total'),
      ...[0, 1, 2, 3, 4, 5].map(i => ({ path: `src/total/t${i}.bin`, kind: 'file' as const, size: 9 * 1048576 })),
      file('src/total-eq/SKILL.md', totalEqMd),
      ...[0, 1, 2, 3].map(i => ({ path: `src/total-eq/t${i}.bin`, kind: 'file' as const, size: 10 * 1048576 })),
      { path: 'src/total-eq/slack.bin', kind: 'file', size: totalEqSlack },
      ok('src/many'),
      { path: 'src/many', kind: 'bulk', count: 501 },
      ok('src/many500'),
      { path: 'src/many500', kind: 'bulk', count: 499 },
      { path: 'src/linkroot', kind: 'symlink', target: 'good' },
    ],
    steps: [
      op('validate', { sourcePath: '<dir>/src/good' }),
      op('validate', { sourcePath: '<dir>/src/short' }),
      op('validate', { sourcePath: '<dir>/src/long' }),
      op('validate', { sourcePath: '<dir>/src/noname' }),
      op('validate', { sourcePath: '<dir>/src/nofm' }),
      op('validate', { sourcePath: '<dir>/src/empty' }),
      op('validate', { sourcePath: '<dir>/src/nomd' }),
      op('validate', { sourcePath: '<dir>/src/afile' }),
      op('validate', { sourcePath: '<dir>/src/missing' }),
      op('validate', { sourcePath: '<dir>/src/compat' }),
      op('validate', { sourcePath: '<dir>/src/huge' }),
      op('validate', { sourcePath: '<dir>/src/total' }),
      op('validate', { sourcePath: '<dir>/src/many' }),
      op('validate', { sourcePath: '  <dir>/src/good  ' }),
      op('validate', { sourcePath: '~/src/short' }),
      op('validate', { sourcePath: '~' }),
      op('validate', { sourcePath: '<dir>/src/good/../short' }),
      op('validate', { sourcePath: '<dir>/src/linkroot' }),
      op('validate', { sourcePath: '   ' }),
      op('validate', {}),
      op('validate', null),
      op('validate', { sourcePath: 5, files: [{ relativePath: 'SKILL.md', size: 1 }], skillMdContent: md('name: M\ndescription: Manifest after a non-string path') }),
      // Boundary equals: exactly 10 MiB per file, 50 MiB total and 500 files stay legal.
      op('validate', { sourcePath: '<dir>/src/exact10' }),
      op('validate', { sourcePath: '<dir>/src/total-eq' }),
      op('validate', { sourcePath: '<dir>/src/many500' }),
    ],
  });
  {
    const o = expectKinds([
      goodKind, 'ok:|description_short', 'ok:|description_long',
      'fail:frontmatter_missing_name,frontmatter_missing_description|', 'fail:frontmatter_missing|', 'fail:no_skill_md|', 'fail:no_skill_md|',
      'fail:source_not_directory|', 'fail:source_missing|', 'ok:|frontmatter_compat_fallback', 'fail:file_too_large|',
      `fail:total_too_large|${times(6, 'file_large').join(',')}`, 'fail:too_many_files|',
      goodKind, 'ok:|description_short', 'fail:no_skill_md|', 'ok:|description_short', goodKind,
      'invalid_input', 'invalid_input', 'TypeError', 'ok:|',
      'ok:|file_large', `ok:|${times(5, 'file_large').join(',')}`, 'ok:|',
    ]);
    assert.deepEqual(o[0].result.stats, { fileCount: 8, totalBytes: o[0].result.stats.totalBytes });
    assert.equal(o[12].result.stats.fileCount, 501);
    assert.equal(o[14].result.sourcePath, '<dir>/src/short');
    assert.equal(o[22].result.stats.fileCount, 2);
    assert.equal(o[23].result.stats.totalBytes, 50 * 1048576);
    assert.equal(o[24].result.stats.fileCount, 500);
  }

  // validate { skillMdContent, files }: manifest checks and frontmatter shapes.
  const good = md('name: M\ndescription: Manifest validation with everything');
  const mdOnly = (skillMdContent: unknown) => op('validate', { skillMdContent, files: [{ relativePath: 'SKILL.md', size: 1 }] });
  const manifest = (files: [string, number][]) => op('validate', { skillMdContent: good, files: files.map(([relativePath, size]) => ({ relativePath, size })) });
  await scenario('validate-manifest', {
    steps: [
      op('validate', {
        skillMdContent: good,
        files: [
          { relativePath: 'SKILL.md', size: 100 }, { relativePath: 'scripts/a.sh', size: 10 }, { relativePath: '../evil', size: 1 },
          { relativePath: '/abs', size: 1 }, { relativePath: 'big.bin', size: 2 * 1048576 }, { relativePath: 'huge.bin', size: 11 * 1048576 },
          { relativePath: '', size: 1 }, { relativePath: 5, size: 1 }, { size: 1 }, 7,
          { relativePath: 'str-size', size: '1024' }, { relativePath: 'hex', size: ' 0x10 ' }, { relativePath: 'bad', size: 'abc' },
          { relativePath: 'arr', size: [5] }, { relativePath: 'exp', size: '1e3' }, { relativePath: 'bool', size: true },
          { relativePath: 'dir.d/file', size: 1 }, { relativePath: 'x.SH', size: 1 },
        ],
      }),
      op('validate', { skillMdContent: good, files: [] }),
      op('validate', { files: [{ relativePath: 'SKILL.md', size: 1 }] }),
      mdOnly(5),
      mdOnly('\ufeff---\nname: x\n---'),
      op('validate', { skillMdContent: good, files: Array.from({ length: 501 }, (_, i) => ({ relativePath: i ? `f${i}` : 'SKILL.md', size: 1 })) }),
      op('validate', { skillMdContent: good, files: [{ relativePath: 'SKILL.md', size: 1 }, ...[0, 1, 2, 3, 4, 5].map(i => ({ relativePath: `t${i}.bin`, size: 9 * 1048576 }))] }),
      op('validate', { skillMdContent: good, files: [{ relativePath: 'SKILL.md', size: 'Infinity' }] }),
      op('validate', { skillMdContent: good, files: [null] }),
      op('validate', { skillMdContent: good, files: {} }),
      mdOnly('---\nname: "   "\ndescription: "  "\n---\n'),
      mdOnly(`---\nname: E\ndescription: ${'😀'.repeat(9)}x\n---\n`),
      mdOnly(`---\nname: E\ndescription: ${'😀'.repeat(10)}\n---\n`),
      mdOnly(`---\nname: L\ndescription: ${'y'.repeat(1025)}\n---\n`),
      mdOnly(`---\nname: L\ndescription: ${'y'.repeat(1024)}\n---\n`),
      mdOnly('---\nname: Keys\ndescription: Integer-like keys enumerate first\n2: two\n__proto__: own\n1: one\n---\n'),
      mdOnly('---\nname: x: y\ndescription: |\n  keep\n  lines\n---\n'),
      mdOnly('---\nname: &n Shared\ndescription: *n\n---\n'),
      mdOnly('---\nname: N\ndescription: Nested values are returned as parsed\nmeta: { tags: [a, 1, true, null], n: -0, f: .nan }\n---\n'),
      op('validate', []),
      // Boundary equals, as on disk: the caps compare with `>`.
      manifest([['SKILL.md', 1], ['eq.bin', 10 * 1048576]]),
      manifest([['SKILL.md', 1], ['over.bin', 10 * 1048576 + 1]]),
      manifest([['SKILL.md', 1], ['mib.bin', 1048576]]),
      manifest([['SKILL.md', 0], ...[0, 1, 2, 3, 4].map(i => [`t${i}.bin`, 10 * 1048576] as [string, number])]),
      manifest([['SKILL.md', 1], ...[0, 1, 2, 3, 4].map(i => [`t${i}.bin`, 10 * 1048576] as [string, number])]),
      manifest(Array.from({ length: 500 }, (_, i) => [i ? `f${i}` : 'SKILL.md', 1] as [string, number])),
    ],
  });
  {
    const o = expectKinds([
      'fail:unsafe_path,unsafe_path,file_too_large|risky_extension,file_large,risky_extension',
      'fail:no_skill_md|', 'fail:no_skill_md|', 'fail:no_skill_md|', 'fail:frontmatter_missing|', 'fail:too_many_files|',
      `fail:total_too_large|${times(6, 'file_large').join(',')}`, 'fail:file_too_large,total_too_large|', 'TypeError', 'invalid_input',
      'fail:frontmatter_missing_name,frontmatter_missing_description|', 'ok:|description_short', 'ok:|', 'ok:|description_long', 'ok:|',
      'ok:|', 'ok:|frontmatter_compat_fallback,description_short', 'ok:|description_short', 'ok:|', 'invalid_input',
      'ok:|file_large', 'fail:file_too_large|', 'ok:|', `ok:|${times(5, 'file_large').join(',')}`,
      `fail:total_too_large|${times(5, 'file_large').join(',')}`, 'ok:|',
    ]);
    // Counted: SKILL.md, a.sh, big, huge, str-size, hex, bad (NaN || 0), arr, exp, bool, dir.d/file, x.SH.
    assert.deepEqual(o[0].result.stats, { fileCount: 12, totalBytes: 100 + 10 + 2 * 1048576 + 11 * 1048576 + 1024 + 16 + 0 + 5 + 1000 + 1 + 1 + 1 });
    assert.equal(o[1].result.frontmatter, null);
    assert.equal(o[7].result.stats.totalBytes, Infinity);
    assert.equal(o[16].result.frontmatter.description, 'keep\nlines');
    assert.equal(o[23].result.stats.totalBytes, 50 * 1048576);
    assert.equal(o[24].result.stats.totalBytes, 50 * 1048576 + 1);
    assert.equal(o[25].result.stats.fileCount, 500);
  }

  // import: copy (fs.cp) and symlink modes, force, slug inference, every error branch.
  const imp = (sourcePath: unknown, extra: object = {}) => op('import', { scope: 'user', sourcePath, ...extra });
  await scenario('import', {
    setup: [
      skill('src/good', 'Good', 'A complete skill imported by copy'),
      file('src/good/scripts/run.sh', 'echo'),
      file('src/good/nested/deep.txt', 'deep'),
      // fs.cp rewrites a relative link target to an absolute one.
      { path: 'src/good/rel-link', kind: 'symlink', target: 'nested' },
      skill('src/other', 'Other', 'Another skill used for scoped imports'),
      skill('src/.hidden', 'Hidden', 'Basename is not a valid slug'),
      skill('src/dangling', 'Dangling', 'Imported over a dangling link'),
      skill('src/piped', 'Piped', 'Bundle that carries a FIFO pipe'),
      { path: 'src/piped/pipe', kind: 'fifo' },
      file('src/nomd/readme.md', 'x'),
      { path: 'src/mddir/SKILL.md', kind: 'dir' },
      file('src/bad/SKILL.md', '# no frontmatter\n'),
      file('src/afile', 'x'),
      { path: 'src/linkroot', kind: 'symlink', target: 'good' },
      skill('home/skills/taken', 'Taken', 'Already installed before the import'),
      { path: 'home/skills/dangling', kind: 'symlink', target: '../../nowhere' },
      // pilotHome itself as a bundle: its own skills root lies inside it.
      file('home/SKILL.md', md('name: Home\ndescription: The pilot home used as a source bundle')),
      skill('builtin/bi', 'Builtin', 'Read-only'),
    ],
    steps: [
      imp('<dir>/src/good'),
      imp('<dir>/src/good'),
      imp('<dir>/src/good', { force: true, mode: 'symlink' }),
      imp('<dir>/src/good', { slug: '  renamed  ', mode: 'bogus' }),
      op('import', { scope: 'project', projectKey: proj, sourcePath: '<dir>/src/other' }),
      imp('~/src/other', { slug: 'tilde' }),
      imp('<dir>/src/linkroot'),
      imp('<dir>/src/dangling'),
      imp('<dir>/src/dangling', { mode: 'symlink' }),
      imp('<dir>/src/piped'),
      // The failed copy leaves a partial tree in readdir order; remove it.
      op('delete', user('piped')),
      imp('<dir>/home', { slug: 'selfsub' }),
      imp('<dir>/home/skills/taken'),
      imp('<dir>/src/missing'),
      imp('<dir>/src/afile'),
      imp('<dir>/src/afile/x'),
      imp('<dir>/src/nomd'),
      imp('<dir>/src/mddir'),
      imp('<dir>/src/bad'),
      imp('<dir>/src/good', { slug: '../x' }),
      imp('<dir>/src/good', { slug: 5 }),
      imp('<dir>/src/good', { slug: '   ' }),
      imp('<dir>/src/.hidden'),
      op('import', { scope: 'builtin', sourcePath: '<dir>/src/good' }),
      op('import', { scope: 'user' }),
      imp('   '),
      imp(5),
      op('import', { scope: 'project', sourcePath: '<dir>/src/other' }),
      op('import', null),
      imp('<dir>/src/other', { slug: 'taken', force: true }),
      op('list', { scope: 'user', limit: 50 }),
    ],
  });
  {
    const o = expectKinds([
      'import:copy:good:Good', 'conflict', 'import:symlink:good:Good', 'import:copy:renamed:Good', 'import:copy:other:Other',
      'import:copy:tilde:Other', 'import:copy:linkroot:Good', 'ERR_FS_CP_DIR_TO_NON_DIR', 'EEXIST', 'ERR_FS_CP_FIFO_PIPE', 'deleted',
      'ERR_FS_CP_EINVAL', 'self_import', 'source_missing', 'source_not_directory', 'ENOTDIR', 'no_skill_md',
      'validation_failed', 'validation_failed', 'invalid_slug', 'TypeError', 'conflict', 'invalid_slug', 'read_only',
      'invalid_input', 'invalid_input', 'invalid_input', 'project_required', 'TypeError', 'import:copy:taken:Other', 'list:5',
    ]);
    assert.equal(o[0].result.validation.ok, true);
    assert.deepEqual(o[17].error.validation.hardFails.map((i: any) => i.code), ['no_skill_md']);
    assert.deepEqual(o[18].error.validation.hardFails.map((i: any) => i.code), ['frontmatter_missing']);
  }

  // scan: child folders, sort order, symlinks, recursive file counts, error branches.
  await scenario('scan', {
    setup: [
      skill('lib/zeta', 'Zeta', 'Sorted after the others'),
      skill('lib/Alpha', 'Alpha', 'Capitalised folder sorts by locale'),
      skill('lib/beta', 'Beta', 'Counts nested files and followed links'),
      file('lib/beta/data/x.txt', '12345'),
      { path: 'lib/beta/flink', kind: 'symlink', target: 'data/x.txt' },
      { path: 'lib/beta/pipe', kind: 'fifo' },
      { path: 'lib/empty', kind: 'dir' },
      file('lib/notes/readme.md', 'x'),
      file('lib/plain.txt', 'x'),
      { path: 'lib/linked', kind: 'symlink', target: '../ext' },
      skill('ext', 'Ext', 'Reached through a directory symlink'),
      { path: 'lib/broken', kind: 'symlink', target: '../nowhere' },
      { path: 'lib/mddir/SKILL.md', kind: 'dir' },
      // The recursive readdir follows directory links: a self loop unrolls until ELOOP.
      skill('lib/loopy', 'Loopy', 'A folder whose link points back at itself'),
      { path: 'lib/loopy/loop', kind: 'symlink', target: '.' },
      skill('top', 'Top', 'A parent folder that is itself a skill'),
      skill('top/child', 'Child', 'Listed after its parent'),
    ],
    steps: [
      op('scan', { parentPath: '<dir>/lib' }),
      op('scan', { parentPath: '<dir>/top' }),
      op('scan', { parentPath: '  <dir>/lib/  ' }),
      op('scan', { parentPath: '~/lib' }),
      op('scan', { parentPath: '<dir>/lib/plain.txt' }),
      op('scan', { parentPath: '<dir>/missing' }),
      op('scan', { parentPath: '<dir>/lib/plain.txt/x' }),
      op('scan', {}),
      op('scan', { parentPath: '   ' }),
      op('scan', { parentPath: 5 }),
      op('scan', null),
      op('scan', { parentPath: '<dir>/lib/linked' }),
      op('scan', { parentPath: '<dir>/lib/empty' }),
    ],
  });
  {
    const libKind = 'scan:Alpha*,beta*,linked*,loopy*,mddir*,zeta*,empty,notes';
    const o = expectKinds([
      libKind, 'scan:top*,child*', libKind, libKind, 'not_directory', 'not_found', 'ENOTDIR',
      'invalid_input', 'invalid_input', 'invalid_input', 'TypeError', 'scan:linked*', 'scan:',
    ]);
    const folder = (name: string) => o[0].result.folders.find((f: any) => f.folderName === name);
    assert.deepEqual([folder('beta').fileCount, folder('beta').totalSize], [3, Buffer.byteLength(md('name: Beta\ndescription: Counts nested files and followed links')) + 10]);
    assert.deepEqual([folder('mddir').name, folder('mddir').fileCount], [null, 0]);
    assert.equal(folder('empty').name, null);
    assert.ok(folder('loopy').fileCount > 1, 'the self link is followed');
  }

  // Binding selection (createLocalGateway.ts:853–860) on the real predicates.
  const bindingCases: { binding?: unknown }[] = [
    {}, { binding: null }, { binding: {} }, { binding: { enabled: true, provider: 'pilotdeck' } }, { binding: { enabled: false } },
    { binding: { enabled: false, implementationId: 'x', transport: 'module-http-v2' } }, { binding: { enabled: 'false' } }, { binding: { enabled: 0 } },
    { binding: { enabled: true, implementationId: 'x', contract: 'skills', transport: 'module-http-v2', endpoint: 'http://127.0.0.1:1', methods: ['list'] } },
    { binding: { implementationId: 'x', transport: 'module-stdio-v2' } }, { binding: { transport: 'module-http-v2' } },
    { binding: { implementationId: null, transport: 'module-http-v2' } }, { binding: [] }, { binding: ['implementationId'] },
    { binding: 'module-http-v2' }, { binding: 5 }, { binding: true }, { binding: false },
  ];
  const bindings = bindingCases.map(c => {
    try {
      const b = c.binding;
      return { ...c, selected: isDisabledModuleBinding(b) ? 'disabled' : isExternalModuleBinding(b) ? 'external' : 'native' };
    } catch (error: any) {
      return { ...c, selected: { error: error.name } };
    }
  });
  assert.deepEqual(bindings.map(b => typeof b.selected === 'string' ? b.selected : b.selected.error), [
    'native', 'TypeError', 'native', 'native', 'disabled', 'disabled', 'native', 'native', 'external', 'native', 'native', 'external',
    'native', 'native', 'TypeError', 'TypeError', 'TypeError', 'TypeError',
  ]);
  // createDisabledSkillManagementPort is module-private: read its one error from the pinned source.
  const gatewaySource = await readFile(join(baseline, sources[3]), 'utf8');
  const disabledMatch = /function createDisabledSkillManagementPort\(\)[\s\S]*?new Error\("([^"]+)"\), \{\s*code: "([A-Z_]+)",/.exec(gatewaySource);
  assert.ok(disabledMatch, 'createDisabledSkillManagementPort changed');
  const disabledError = { name: 'Error', code: disabledMatch[2], message: disabledMatch[1] };
  for (const name of ['list', 'read', 'write', 'create', 'delete', 'import', 'validate', 'scan']) {
    assert.ok(gatewaySource.includes(`    ${name}: unavailable,`), `disabled port lacks ${name}`);
  }

  // Coverage: every error code and validation issue the eight methods can raise.
  const seen = new Set<string>();
  for (const s of scenarios) for (const step of s.steps) {
    const o = step.outcome;
    if (o.error) seen.add(o.error.code ?? o.error.name);
    else if (step.op === 'validate') for (const i of [...o.result.hardFails, ...o.result.warnings]) seen.add(i.code);
  }
  const required = [
    'not_configured', 'project_required', 'read_only', 'invalid_slug', 'invalid_input', 'not_found', 'conflict',
    'ENOTDIR', 'EISDIR', 'EEXIST', 'TypeError', 'ERR_INVALID_ARG_TYPE',
    'no_skill_md', 'frontmatter_missing', 'frontmatter_compat_fallback', 'frontmatter_missing_name', 'frontmatter_missing_description',
    'description_short', 'description_long', 'source_missing', 'source_not_directory', 'too_many_files', 'total_too_large',
    'file_too_large', 'file_large', 'risky_extension', 'contains_symlink', 'unsafe_path',
    'self_import', 'source_missing', 'source_not_directory', 'validation_failed', 'not_directory',
    'ERR_FS_CP_EINVAL', 'ERR_FS_CP_DIR_TO_NON_DIR', 'ERR_FS_CP_FIFO_PIPE',
  ];
  assert.deepEqual(required.filter(c => !seen.has(c)), [], 'uncovered branches');

  const sourceSha256 = Object.fromEntries(await Promise.all(sources.map(async s => [s, sha(await readFile(join(baseline, s)))])));
  const oracle = JSON.stringify({ sourceCommit, sourceSha256, node: process.version, locale, scenarios, bindings, disabledError }, null, 2) + '\n';
  // Fixtures are machine-independent: no generation path survives in the oracle.
  for (const dir of [scratch, output, baseline]) assert.ok(!oracle.includes(dir), `oracle.json contains ${dir}`);
  await writeFile(join(output, 'oracle.json'), oracle);
  console.log(`PASS: generated ${scenarios.length} real TS SkillManager scenarios (${scenarios.reduce((n, s) => n + s.steps.length, 0)} steps)`);
} finally { await rm(scratch, { recursive: true, force: true }); }
