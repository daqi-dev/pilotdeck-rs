// Real-TS oracle for #563: FileMemoryStore on a root-internal dangling symlink.
// Expectations are produced only by executing the unmodified fixed TS store.
// The link `global/UserIdentity/user-profile.md -> real.md` has no target, so
// TS follows it: reads see ENOENT (no record), upsertUserProfile creates
// real.md through the link, and deleteEntries skips it but still repairs the
// manifest, creating the root MEMORY.md. editEntry still reports the ordinary
// not-found error and is kept as a message anchor.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  readlinkSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const SOURCE = 'src/context/memory/edgeclaw-memory-core/src/core/file-memory.ts';
const NOW = '2026-10-22T00:00:00.000Z';
const LINK = {
  relativePath: 'global/UserIdentity/user-profile.md',
  target: 'real.md',
} as const;

const baseline = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]!);
const git = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const provenance = bindSourceTree(baseline, COMMIT, git);
const sourceSha256 = sourceClosureSha256(baseline, COMMIT, git, [SOURCE]);

const frozenMs = Date.parse(NOW);
class FrozenDate extends Date {
  constructor(...args: any[]) {
    super(...((args.length ? args : [frozenMs]) as [number]));
  }
  static now() {
    return frozenMs;
  }
}
(globalThis as any).Date = FrozenDate;

const { FileMemoryStore } = await import(pathToFileURL(join(baseline, SOURCE)).href);

function clean(value: any, root: string): any {
  if (typeof value === 'string') return value.split(root).join('$ROOT');
  if (Array.isArray(value)) return value.map((item) => clean(item, root));
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, clean(item, root)]),
    );
  }
  return value === undefined ? null : value;
}

function tree(root: string, dir = ''): any[] {
  return readdirSync(join(root, dir), { withFileTypes: true })
    // Dirent has no default string conversion; compare names explicitly so the
    // oracle order does not depend on the filesystem's readdir order. This is
    // JS UTF-16 code-unit order, matching Rust's `encode_utf16` sort in the test.
    .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
    .flatMap((entry) => {
      const relativePath = dir ? `${dir}/${entry.name}` : entry.name;
      const absolutePath = join(root, relativePath);
      const stat = lstatSync(absolutePath);
      if (stat.isSymbolicLink()) {
        return [{ relativePath, kind: 'symlink', target: readlinkSync(absolutePath) }];
      }
      if (stat.isDirectory()) {
        return [{ relativePath, kind: 'dir' }, ...tree(root, relativePath)];
      }
      return [{ relativePath, kind: 'file', content: readFileSync(absolutePath, 'utf8') }];
    });
}

function invoke(store: any, method: string, input: any) {
  switch (method) {
    case 'getMemoryRecordsByIds':
      return store.getMemoryRecordsByIds(input.ids, input.maxLines);
    case 'deleteEntries':
      return store.deleteEntries(input.relativePaths);
    default:
      return store[method](input);
  }
}

const scenarios: any[] = [];
function run(name: string, options: any, operations: any[]) {
  const root = join(scratch, name);
  mkdirSync(root, { recursive: true });
  const store = new FileMemoryStore(root, options);
  mkdirSync(dirname(join(root, LINK.relativePath)), { recursive: true });
  symlinkSync(LINK.target, join(root, LINK.relativePath));
  const initialFiles = tree(root);
  const observations: any[] = [];
  for (const op of operations) {
    let value: any;
    try {
      value = clean(invoke(store, op.method, op.input), root);
    } catch (error: any) {
      value = error.code ? { error: error.code } : { error: error.name, message: error.message };
    }
    observations.push(value);
  }
  scenarios.push({ name, options, operations, initialFiles, observations, files: tree(root) });
}

const scratch = mkdtempSync(join(tmpdir(), 'pdrs-edgeclaw-dangling-'));
const candidate = {
  type: 'user',
  scope: 'global',
  name: 'User',
  description: '身份',
  profile: 'Rust',
  capturedAt: NOW,
  sourceSessionKey: 's',
};

run('read', {}, [
  {
    method: 'getMemoryRecordsByIds',
    input: { ids: [LINK.relativePath], maxLines: 5000 },
  },
]);
run('write', {}, [{ method: 'upsertUserProfile', input: candidate }]);
run('delete', {}, [
  {
    method: 'deleteEntries',
    input: { relativePaths: [LINK.relativePath] },
  },
]);
run('edit-anchor', {}, [
  {
    method: 'editEntry',
    input: { relativePath: LINK.relativePath, name: 'x', description: 'x' },
  },
]);

// Fail closed if Node ever stops following the link: the three TS outcomes are
// the premise this fixture proves, not inputs to it.
assert.deepEqual(scenarios[0].observations[0], []);
assert.equal(scenarios[1].observations[0].relativePath, LINK.relativePath);
assert.ok(
  scenarios[1].files.some(
    (file: any) => file.relativePath === 'global/UserIdentity/real.md' && file.kind === 'file',
  ),
  'upsertUserProfile must create real.md through the dangling link',
);
assert.ok(
  scenarios[1].files.some(
    (file: any) => file.relativePath === 'MEMORY.md' && file.kind === 'file',
  ),
  'upsertUserProfile must repair the manifest and create MEMORY.md',
);
assert.deepEqual(scenarios[2].observations[0].mutatedIds, []);
assert.ok(
  scenarios[2].files.some((file: any) => file.kind === 'symlink'),
  'deleteEntries must leave the dangling link in place',
);
assert.ok(
  scenarios[2].files.some(
    (file: any) => file.relativePath === 'MEMORY.md' && file.kind === 'file',
  ),
  'deleteEntries must repair the manifest and create MEMORY.md even when it skips the link',
);
assert.equal(
  scenarios[3].observations[0].message,
  `Memory entry not found: ${LINK.relativePath}`,
);

const oracle = {
  sourceCommit: provenance.ref,
  sourceSha256,
  nodeVersion: process.version,
  link: LINK,
  scenarios,
};
mkdirSync(output, { recursive: true });
const text = `${JSON.stringify(oracle, null, 2)}\n`;
writeFileSync(join(output, 'oracle.json'), text);
console.log(
  `oracle sha256 ${createHash('sha256').update(text).digest('hex')}; ${scenarios.length} scenarios`,
);
rmSync(scratch, { recursive: true, force: true });
