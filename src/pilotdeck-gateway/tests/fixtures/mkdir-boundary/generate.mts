// #244: natural Node errors, plus the real RunRegistry and UploadStore callers.
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/gateway/run/RunRegistry.ts', 'src/gateway/dialog/UploadStore.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { FileRunRegistry } = await import(pathToFileURL(join(baseline, sources[0])).href);
const { UploadStore } = await import(pathToFileURL(join(baseline, sources[1])).href);
const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-mkdir-')));
const cases = [];
const scrub = s => s.split(root).join('<dir>');
const observe = async action => {
  try { await action(); return { ok: true }; }
  catch (e) { return { code: e.code, syscall: e.syscall, path: scrub(e.path), message: scrub(e.message) }; }
};
try {
  for (const [id, suffix, obstacle] of [
    ['ancestor', 'block/child/grand', 'block'], ['leaf', 'block', 'block'],
    ['long', 'x'.repeat(260) + '/child', null], ['new', 'new/child', null], ['existing', 'existing', null],
  ]) {
    const dir = join(root, id); await mkdir(dir);
    if (obstacle) await writeFile(join(dir, obstacle), 'block');
    if (id === 'existing') await mkdir(join(dir, suffix));
    cases.push({ id, kind: 'mkdir', suffix, observed: await observe(() => mkdir(join(dir, suffix), { recursive: true })) });
  }
  for (const id of ['registry', 'upload']) {
    const dir = join(root, id); await mkdir(dir);
    if (id === 'registry') {
      const registry = new FileRunRegistry(join(dir, 'block/child/registry.json'));
      await registry.ready;
      await writeFile(join(dir, 'block'), 'block');
      cases.push({ id, kind: id, observed: await observe(() => registry.accept({ sessionKey: 's', runId: 'r', requestMaterial: 'm' })) });
    } else {
      await writeFile(join(dir, '.tmp'), 'block');
      const store = new UploadStore({ resolveProject: async () => dir, listProjects: async () => [dir],
        now: () => new Date('2026-10-18T00:00:00.000Z'), uuid: () => 'upload-1' });
      const files = [{ clientFileId: 'f', name: 'a.txt', relativePath: 'a.txt', size: 1, mimeType: 'text/plain' }];
      cases.push({ id, kind: id, observed: await observe(() => store.create('p', files)) });
    }
  }
} finally { await rm(root, { recursive: true, force: true }); }
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n');
