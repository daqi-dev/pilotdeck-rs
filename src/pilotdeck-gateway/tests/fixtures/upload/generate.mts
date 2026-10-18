import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Real fixed-baseline provider oracle. No copied implementation or mock store.
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, writeFile, rm, realpath, symlink, readdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { Readable } from 'node:stream';
import assert from 'node:assert/strict';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sourcePaths = ['src/gateway/dialog/UploadStore.ts', 'src/gateway/dialog/errors.ts', 'src/gateway/dialog/UploadLifecyclePort.ts', 'src/gateway/dialog/UploadArtifactLeasePort.ts'];
const sources = await baselineSourceHashes(baseline, sourcePaths);
const { UploadStore } = await import(pathToFileURL(join(baseline, sourcePaths[0])).href);
const epoch = Date.parse('2026-10-18T00:00:00.000Z');
const file = { clientFileId: 'file-1', name: '  你好.txt  ', relativePath: 'folder/你好.txt', size: 5, mimeType: 'text/plain', extra: { z: 1, a: 2 } };
const manifest = [file];
const digest = createHash('sha256').update('hello').digest('hex');
const normalize = (text: string, project: string) => text.split(project).join('<PROJECT>');
const clean = (value: any, project: string) => JSON.parse(normalize(JSON.stringify(value), project));
const create = (files = manifest, key?: string) => ({ op: 'create', files, ...(key === undefined ? {} : { key }) });
const part = (bytes = 'hello', id = 'file-1') => ({ op: 'part', id, bytes });
const action = (op: string, rest = {}) => ({ op, ...rest });

if (process.argv[3] === '--verify-rust') {
  const project = await realpath(output);
  // Rust wrote: upload-1 completed (key "once") with one unreleased lease; upload-2 uploading.
  const store = new UploadStore({ resolveProject: async () => project, listProjects: async () => [project], now: () => new Date(epoch), uuid: () => 'ts-new' });
  assert.equal((await store.create('project', manifest, 'once')).uploadId, 'upload-1');
  assert.equal((await store.verifyAttachment('upload-1', 'project'))[0].sha256, digest);
  const record = await store.get('upload-2');
  assert.equal(record.status, 'uploading');
  assert.equal(record.manifest[0].extra.z, 1);
  await store.complete('upload-2');
  const verified = await store.verifyAttachment('upload-2', 'project');
  assert.equal(await readFile(verified[0].path, 'utf8'), 'hello');
  const lease = await store.acquireAttachmentLease('upload-1', 'project');
  await lease.release();
  const leaseRoot = join(project, '.tmp/chat-upload-leases/upload-1');
  const rustLeases = await readdir(leaseRoot);
  assert.equal(rustLeases.length, 1);
  for (const id of rustLeases) {
    const expiry = JSON.parse(await readFile(join(leaseRoot, id, 'lease.json'), 'utf8'));
    assert.equal(expiry.expiresAt, '2026-10-18T06:00:00.000Z');
    assert.equal(await readFile(join(leaseRoot, id, 'file-1'), 'utf8'), 'hello');
  }
  const future = new UploadStore({ resolveProject: async () => project, listProjects: async () => [project], now: () => new Date(epoch + 86_400_000) });
  assert.equal(await future.cleanupExpired(), 2);
  assert.equal(await readdir(join(project, '.tmp/chat-upload-leases')).then(v => v.length), 0);
  console.log('PASS: TS honoured Rust idempotency hash, verified a Rust-completed upload, completed a Rust-started upload, read and expired a Rust lease');
  process.exit(0);
}

await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
const scenarios: any[] = [];
function add(name: string, operations: any[], options = {}) { scenarios.push({ name, options, operations }); }
add('lifecycle', [create(manifest, 'once'), action('get'), part(), action('complete'), action('verify'), action('verify', { ids: [] }), action('verify', { ids: ['file-1', 'file-1'] }), action('cancel'), part(), action('complete')]);
add('idempotency-validation-expiry', [create(manifest, 'once'), create([{ ...file, size: 0 }], 'once'), create([], 'once'), action('advance', { ms: 86400000 }), create(manifest, 'once'), action('get'), create(manifest, 'once')]);
add('concurrency-limit', [create(), create(), create(), create(), action('cancel'), create()]);
add('expiry-active-count', [create(), action('advance', { ms: 86400000 }), create(), action('get'), create()], { maxConcurrentPerProject: 1 });
add('cancel', [create(), action('cancel'), action('cancel'), action('verify'), part(), action('complete')]);
add('missing-parts', [create(), action('complete'), action('get'), action('cancel')]);
add('unknown-duplicate', [create(), part('hello', 'absent'), part(), part(), action('complete')]);
add('short-part', [create(), part('hell'), action('get'), action('cancel'), action('advance', { ms: 86400000 }), action('cleanup')]);
add('long-part', [create(), part('hello!'), action('get')]);
add('bad-digest', [create([{ ...file, sha256: '0'.repeat(64) }]), part(), action('get')]);
add('uppercase-digest', [create([{ ...file, sha256: digest.toUpperCase() }]), part(), action('complete')]);
add('empty-file', [create([{ ...file, size: 0 }]), part(''), action('complete')]);
add('unknown-upload', [action('get'), action('get', { uploadId: '../evil' }), action('cancel'), action('complete'), part()]);
add('attachment-errors', [create(), action('verify'), part(), action('complete'), action('verify', { ids: ['absent'] }), action('verify-other'), action('tamper-file'), action('verify'), action('advance', { ms: 86400000 }), action('verify')]);
add('missing-artifact', [create(), part(), action('complete'), action('remove-file'), action('verify')]);
add('metadata-identity', [create(), action('tamper-identity'), action('get'), create(manifest, 'once'), part(), action('complete'), action('cancel')]);
add('metadata-symlink', [create(), action('escape-metadata'), action('get'), create(manifest, 'once'), part(), action('complete'), action('cancel')]);
add('missing-project', [create()], { missingProject: true });
const badManifests: [string, any][] = [
  ['not-array', null], ['empty', []], ['too-many', [file, { ...file, clientFileId: 'two' }]],
  ['null-file', [null]], ['bad-id', [{ ...file, clientFileId: '../x' }]], ['duplicate-id', [file, file]],
  ['empty-name', [{ ...file, name: '\ufeff \t' }]], ['nonstring-name', [{ ...file, name: 1 }]],
  ['absolute', [{ ...file, relativePath: '/x' }]], ['traversal', [{ ...file, relativePath: '../x' }]],
  ['backslash', [{ ...file, relativePath: 'a\\b' }]], ['dot', [{ ...file, relativePath: 'a/./b' }]],
  ['empty-segment', [{ ...file, relativePath: 'a//b' }]], ['trailing-slash', [{ ...file, relativePath: 'a/' }]],
  ['no-size', [{ ...file, size: undefined }]], ['negative', [{ ...file, size: -1 }]],
  ['fractional', [{ ...file, size: 1.5 }]], ['unsafe-integer', [{ ...file, size: 9007199254740992 }]],
  ['file-limit', [{ ...file, size: 6 }]], ['bad-sha', [{ ...file, sha256: 'xyz' }]], ['null-sha', [{ ...file, sha256: null }]],
  ['task-limit', [file, { ...file, clientFileId: 'two' }]],
];
for (const [name, files] of badManifests) add('manifest-' + name, [create(files)], name === 'too-many' ? { maxFiles: 1 } : name === 'file-limit' ? { maxFileBytes: 5 } : name === 'task-limit' ? { maxTaskBytes: 9 } : {});
// UTF-16 and unknown field order are part of JSON, not just metadata semantics.
add('unicode-and-extension', [create([{ '7': 'numeric-key', ...file, name: '\ufeff\u00a0中\ud800 \ufeff', mimeType: '', extension: { '9': 1, z: '\udfff', a: -0 } }]), part(), action('complete')]);
try {
  for (const scenario of scenarios) {
    const base = join(scratch, scenario.name); await mkdir(base);
    const project = await realpath(base);
    let now = epoch, counter = 0;
    const { missingProject, ...limits } = scenario.options;
    const store = new UploadStore({ resolveProject: async (key: string) => key === 'other' ? scratch : missingProject ? join(project, 'absent') : project, listProjects: async () => [project], now: () => new Date(now), uuid: () => `upload-${++counter}`, ...limits });
    scenario.results = [];
    for (const operation of scenario.operations) {
      const id = operation.uploadId ?? 'upload-1';
      let result: any;
      try {
        switch (operation.op) {
          case 'create': result = await store.create('project', operation.files, operation.key); break;
          case 'get': result = await store.get(id); break;
          case 'part': result = await store.writePart(id, operation.id, Readable.from([Buffer.from(operation.bytes)])); break;
          case 'complete': result = await store.complete(id); break;
          case 'cancel': result = await store.cancel(id); break;
          case 'verify': result = await store.verifyAttachment(id, 'project', operation.ids); break;
          case 'verify-other': result = await store.verifyAttachment(id, 'other'); break;
          case 'advance': now += operation.ms; result = null; break;
          case 'cleanup': result = await store.cleanupExpired(); break;
          case 'tamper-file': await writeFile(join(project, '.tmp/chat-uploads/upload-1/files/file-1'), 'jello'); result = null; break;
          case 'remove-file': await rm(join(project, '.tmp/chat-uploads/upload-1/files/file-1')); result = null; break;
          case 'tamper-identity': {
            const p = join(project, '.tmp/chat-uploads/upload-1/metadata.json');
            const r = JSON.parse(await readFile(p, 'utf8')); r.uploadId = 'other'; await writeFile(p, JSON.stringify(r) + '\n'); result = null; break;
          }
          case 'escape-metadata': {
            const p = join(project, '.tmp/chat-uploads/upload-1/metadata.json');
            const outside = join(project, 'outside.json'); await writeFile(outside, await readFile(p)); await rm(p); await symlink(outside, p); result = null; break;
          }
          default: throw Error('unhandled ' + operation.op);
        }
        scenario.results.push({ ok: clean(result, project) });
      } catch (error: any) { scenario.results.push({ error: { code: error.code ?? null, message: normalize(error.message, project) } }); }
    }
  }
  const project = await realpath(await mkdir(join(scratch, 'golden')).then(() => join(scratch, 'golden')));
  let now = epoch;
  const store = new UploadStore({ resolveProject: async () => project, listProjects: async () => [project], now: () => new Date(now), uuid: () => 'upload-1' });
  const metadata = join(project, '.tmp/chat-uploads/upload-1/metadata.json');
  async function snapshot(name: string) { await writeFile(join(output, name + '.json'), normalize(await readFile(metadata, 'utf8'), project)); }
  await store.create('project', manifest, 'once'); await snapshot('created');
  await store.writePart('upload-1', 'file-1', Readable.from([Buffer.from('hello')])); await snapshot('uploading');
  await store.complete('upload-1'); await snapshot('completed');
  const lease = await store.acquireAttachmentLease('upload-1', 'project');
  const leasePath = join(lease.attachments[0].path, '..', 'lease.json');
  await writeFile(join(output, 'lease.json'), await readFile(leasePath));
  await lease.release();
  now += 86400000; await store.get('upload-1'); await snapshot('expired');
  await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sources, command: 'PDRS_BASELINE=/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/upload/generate.mts <output>', epoch, manifest, content: 'hello', scenarios }, null, 2) + '\n');
  console.log(`PASS: ${scenarios.length} real TS scenarios, ${scenarios.reduce((n, s) => n + s.results.length, 0)} operation results, 5 byte fixtures`);
} finally { await rm(scratch, { recursive: true, force: true }); }
