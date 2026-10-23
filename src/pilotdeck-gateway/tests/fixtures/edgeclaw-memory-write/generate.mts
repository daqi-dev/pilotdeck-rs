// #597: execute the pinned, unmodified TS store. Never synthesize expected bytes.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { freezeClock } from '../edgeclaw-file-memory/observe.mts';
assert.equal(process.versions.node.split('.')[0], '22');
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE!);
const source = 'src/context/memory/edgeclaw-memory-core/src/core/file-memory.ts';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const provenance = bindSourceTree(baseline, commit, git);
const sourceSha256 = sourceClosureSha256(baseline, commit, git, [source]);
freezeClock();
const { FileMemoryStore } = await import(pathToFileURL(join(baseline, source)).href);
const scratch = mkdtempSync(join(tmpdir(), 'pdrs-597-ts-'));
try {
  const root = join(scratch, 'memory');
  const store = new FileMemoryStore(root, { enableManifest: false });
  let missingMessage = '';
  try { store.editEntry({ relativePath: 'missing', name: 'x' }); }
  catch (e: any) { missingMessage = e.message; }
  assert.ok(missingMessage.includes('missing'));
  const frontmatter = { name: 'Nested', description: 'UTF-8 中文', type: 'project', scope: 'project', updatedAt: '2026-10-22T00:00:00.000Z' };
  const writes = [];
  for (const [relativePath, body] of [
    ['Project/nested/note.md', 'first long body 中文 😀\n'.repeat(5)],
    ['Project/nested/note.md', 'short'],
    ['Project/nested/alias.md', 'through existing in-root link'],
  ]) {
    if (relativePath.endsWith('alias.md')) symlinkSync('note.md', join(root, relativePath));
    (store as any).writeRecord({ relativePath, frontmatter, body });
    writes.push({ relativePath, frontmatter, body, bytes: readFileSync(join(root, relativePath), 'utf8') });
  }
  store.deleteEntries(['Project/nested/alias.md']);
  const afterUnlink = readFileSync(join(root, 'Project/nested/note.md'), 'utf8');
  const outside = join(scratch, 'outside'); mkdirSync(outside); writeFileSync(join(outside, 'sentinel'), 'outside sentinel');
  symlinkSync(outside, join(root, 'Project', 'outside'));
  store.clearAllData({ rebuildManifest: false });
  const clearSentinel = readFileSync(join(outside, 'sentinel'), 'utf8');
  bindSourceTree(baseline, commit, git);
  assert.deepEqual(sourceClosureSha256(baseline, commit, git, [source]), sourceSha256);
  const output = resolve(process.argv[2]!); mkdirSync(output, { recursive: true });
  // tests/common/baseline.rs reads `sourceCommit`, like every merged fixture.
  const oracle = { sourceCommit: provenance.ref, sourceSha256, missingMessage, writes, afterUnlink, clearSentinel };
  writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
} finally { rmSync(scratch, { recursive: true, force: true }); }
