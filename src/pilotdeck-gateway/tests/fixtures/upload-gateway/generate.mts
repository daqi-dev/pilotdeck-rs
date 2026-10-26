// Real TS gateway-method oracle for the five U1 upload methods.
//
// Imports InProcessGateway (the method implementation) and UploadStore (the
// provider) from the pinned baseline. A recording lifecycle wrapper captures
// the provider call sequence for every operation, so Rust can compare both the
// visible result and the calls that produced it. The TS error envelope follows
// GatewayWsConnection.ts:249-253 (non-structured errors become
// gateway_request_failed), because a raw TypeError has no code.
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { Readable } from 'node:stream';

const baseline = process.env.PDRS_BASELINE;
if (!baseline) throw new Error('set PDRS_BASELINE');
const outputArg = process.argv[2];
if (!outputArg) throw new Error('provide an output directory');
const root = resolve(baseline);
const output = resolve(outputArg);
const sourceCommit = assertBaseline(root);
const sourcePaths = [
  'src/gateway/client/InProcessGateway.ts',
  'src/gateway/dialog/UploadStore.ts',
  'src/gateway/dialog/errors.ts',
  'src/gateway/dialog/UploadLifecyclePort.ts',
  'src/gateway/dialog/UploadArtifactLeasePort.ts',
  'src/gateway/protocol/types.ts',
  'src/gateway/protocol/frames.ts',
];
const sources = await baselineSourceHashes(root, sourcePaths);

await mkdir(output, { recursive: true });
const isolatedHome = await mkdtemp(join(output, '.home-'));
process.env.HOME = isolatedHome;
process.env.PILOTDECK_HOME = join(isolatedHome, '.pilotdeck');
const { InProcessGateway } = await import(pathToFileURL(join(root, 'src/gateway/client/InProcessGateway.ts')).href);
const { UploadStore } = await import(pathToFileURL(join(root, 'src/gateway/dialog/UploadStore.ts')).href);

const epoch = Date.parse('2026-10-18T00:00:00.000Z');
const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const FILE: any = {
  clientFileId: 'file-1',
  name: '  hello.txt  ',
  relativePath: 'folder/hello.txt',
  size: 5,
  mimeType: 'text/plain',
};
const file = (extra: any = {}) => ({ ...FILE, ...extra });
const digest = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
const ZEROS_8M_SHA = digest(Buffer.alloc(8 * 1024 * 1024));

const normalize = (text: string, project: string) => text.split(project).join('<PROJECT>');
const clean = (value: any, project: string) => JSON.parse(normalize(JSON.stringify(value), project));
const expand = (value: any): any => {
  if (Array.isArray(value)) return value.map(expand);
  if (value && typeof value === 'object') {
    if (typeof value.$zerosBytes === 'number') {
      const bytes = value.$zerosBytes;
      const full = Math.floor(bytes / 3);
      const rem = bytes % 3;
      return 'A'.repeat(full * 4 + (rem === 0 ? 0 : rem + 1)) +
        (rem === 0 ? '' : rem === 1 ? '==' : '=');
    }
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, expand(item)]));
  }
  return value;
};
const wsCode = (error: any) => (typeof error?.code === 'string' ? error.code : 'gateway_request_failed');
const scenarios: any[] = [];
const add = (name: string, operations: any[]) => scenarios.push({ name, operations });
const call = (method: string, params: any = null) => ({ method, params });
const params = (value: any) => call('create', value);

add('create-validation', [
  { method: 'create', undefinedParams: true },
  params(null),
  params({}),
  params({ projectKey: null }),
  params({ projectKey: '' }),
  params({ projectKey: ' \t\n' }),
  params({ projectKey: '\u00a0\ufeff' }),
  params({ projectKey: 5 }),
  params({ projectKey: {} }),
  params({ projectKey: true }),
  params({ projectKey: 'p' }),
  params({ projectKey: 'p', files: null }),
  params({ projectKey: 'p', files: 'x' }),
  params({ projectKey: 'p', files: {} }),
  params({ projectKey: 'p', files: [] }),
  params({ projectKey: 'p', files: [file()] }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: 'once' }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: 'once' }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: '' }),
  params({ projectKey: 'missing', files: [file()] }),
]);

add('idempotency-key', [
  params({ projectKey: 'p', files: [file()], idempotencyKey: 'once' }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: 'once' }),
  call('cancel', { uploadId: 'upload-1' }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: '' }),
  call('cancel', { uploadId: 'upload-2' }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: 0 }),
  call('cancel', { uploadId: 'upload-3' }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: false }),
  call('cancel', { uploadId: 'upload-4' }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: null }),
  call('cancel', { uploadId: 'upload-5' }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: 5 }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: true }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: [] }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: {} }),
  // Cross-invalid inputs: project/manifest errors win over the key type, and
  // Node formats the number with Number#toString (not Rust's expanded form).
  params({ projectKey: 'missing', files: [file()], idempotencyKey: 5 }),
  params({ projectKey: 'p', files: [], idempotencyKey: 5 }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: 1e21 }),
  params({ projectKey: 'p', files: [file()], idempotencyKey: 1e-7 }),
]);

add('get-validation', [
  params({ projectKey: 'p', files: [file()] }),
  call('get', { uploadId: 'upload-1' }),
  call('get', { uploadId: 'absent' }),
  call('get', {}),
  call('get', { uploadId: null }),
  call('get', { uploadId: 5 }),
  call('get', { uploadId: {} }),
  call('get', { uploadId: '' }),
  call('get', { uploadId: '../evil' }),
  call('get', { uploadId: 'a/b' }),
  call('get', { uploadId: 'a..b' }),
  call('get', { uploadId: 'x'.repeat(128) }),
  call('get', { uploadId: 'x'.repeat(129) }),
  call('get', { uploadId: 'é' }),
  { method: 'get', undefinedParams: true },
  call('get', null),
  { advance: DAY },
  call('get', { uploadId: 'upload-1' }),
  call('get', { uploadId: 'upload-1' }),
]);

add('part-validation', [
  params({ projectKey: 'p', files: [file()] }),
  params({ projectKey: 'p', files: [file({ size: 1 })] }),
  call('part', { uploadId: '../evil', clientFileId: '', contentBase64: '' }),
  call('part', { uploadId: 'upload-1', contentBase64: '' }),
  call('part', { uploadId: 'upload-1', clientFileId: '', contentBase64: '' }),
  call('part', { uploadId: 'upload-1', clientFileId: ' \t', contentBase64: '' }),
  call('part', { uploadId: 'upload-1', clientFileId: 5, contentBase64: '' }),
  call('part', { uploadId: 'upload-1', clientFileId: null, contentBase64: '' }),
  call('part', { uploadId: 'upload-1', clientFileId: true, contentBase64: '' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: null }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 5 }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: true }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: '' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'A' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'AAA' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'A===' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: '====' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'AA=A' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'AA-_' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'AAAA\n' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'éééé' }),
  call('part', { uploadId: 'upload-2', clientFileId: 'file-1', contentBase64: 'AB==' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: { $zerosBytes: 8 * 1024 * 1024 + 1 } }),
  call('part', { uploadId: 'upload-1', clientFileId: 'absent', contentBase64: 'aGVsbG8=' }),
  call('part', { uploadId: 'nope', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
  call('complete', { uploadId: 'upload-1' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
  { method: 'part', undefinedParams: true },
  call('part', null),
]);

add('complete-cancel-validation', [
  params({ projectKey: 'p', files: [file()] }),
  params({ projectKey: 'p', files: [file()] }),
  params({ projectKey: 'p', files: [file(), file({ clientFileId: 'file-2' })] }),
  call('complete', { uploadId: 'upload-1' }),
  call('complete', { uploadId: 'upload-1' }),
  call('cancel', { uploadId: 'upload-1' }),
  call('cancel', { uploadId: 'upload-2' }),
  call('cancel', { uploadId: 'upload-2' }),
  call('complete', { uploadId: 'upload-2' }),
  call('complete', { uploadId: 'upload-3' }),
  call('get', { uploadId: 'upload-3' }),
  call('complete', { uploadId: 'absent' }),
  call('complete', { uploadId: '../evil' }),
  call('cancel', { uploadId: 'absent' }),
  call('cancel', { uploadId: '../evil' }),
  { method: 'complete', undefinedParams: true },
  call('complete', null),
  { method: 'cancel', undefinedParams: true },
  call('cancel', null),
  params({ projectKey: 'p', files: [file()] }),
  call('part', { uploadId: 'upload-4', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
  call('complete', { uploadId: 'upload-4' }),
]);

add('provider-unavailable', [
  { method: 'create', undefinedParams: true },
  call('create', {}),
  call('get', { uploadId: '../evil' }),
  call('part', { uploadId: '../evil', clientFileId: '', contentBase64: '' }),
  call('part', { uploadId: 'upload-1', clientFileId: '', contentBase64: '' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'A' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
  call('complete', { uploadId: '../evil' }),
  { method: 'cancel', undefinedParams: true },
  { method: 'part', undefinedParams: true },
]);

add('response-adaptation', [
  params({ projectKey: 'p', files: [file()], idempotencyKey: 'key' }),
  call('get', { uploadId: 'upload-1' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
  call('complete', { uploadId: 'upload-1' }),
  call('get', { uploadId: 'upload-1' }),
  { tamper: 'metadata-extra' },
  call('get', { uploadId: 'upload-1' }),
  { tamper: 'metadata-error' },
  call('get', { uploadId: 'upload-1' }),
  params({ projectKey: 'p', files: [file({ mimeType: '' })] }),
  call('part', { uploadId: 'upload-2', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
  params({ projectKey: 'p', files: [file({ mimeType: undefined })] }),
  call('part', { uploadId: 'upload-3', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
  { tamper: 'attachments-null' },
  call('get', { uploadId: 'upload-1' }),
]);

// Raw UTF-16 parameters must reach the provider/resolver unchanged: a lone
// surrogate is not the real U+FFFD, even though both encode to the same UTF-8
// bytes at the filesystem/hash boundary.
add('parameter-fidelity', [
  params({ projectKey: 'p\ud800', files: [file()] }),
  params({ projectKey: 'p\ufffd', files: [file()] }),
  call('get', { uploadId: 'upload-1' }),
  call('get', { uploadId: 'upload-2' }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: 'aGVsbG8=' }),
]);

// decodeUploadPart checks the 8 MiB ceiling before decoding. Exactly 8 MiB is
// accepted and written; one byte more is rejected before decodeUploadPart
// returns, so the provider is never called.
add('part-size-limits', [
  params({ projectKey: 'p', files: [file({ size: 8388608, sha256: ZEROS_8M_SHA })] }),
  call('part', { uploadId: 'upload-1', clientFileId: 'file-1', contentBase64: { $zerosBytes: 8388608 } }),
  params({ projectKey: 'p', files: [file({ size: 8388609, sha256: ZEROS_8M_SHA })] }),
  call('part', { uploadId: 'upload-2', clientFileId: 'file-1', contentBase64: { $zerosBytes: 8388609 } }),
]);

async function tamper(kind: string, project: string) {
  const path = join(project, '.tmp/chat-uploads/upload-1/metadata.json');
  const record = JSON.parse(await readFile(path, 'utf8'));
  if (kind === 'metadata-extra') {
    record.diskPath = '/secret/record-path';
    record.path = '/secret/record-path';
    record.unknown = { nested: true };
    record.errorCode = '';
    record.errorMessage = '';
    record.attachments[0].path = '/secret/attachment-path';
    record.attachments[0].secret = 'keep-out';
    record.attachments[0].mimeType = '';
  } else if (kind === 'metadata-error') {
    record.status = 'failed';
    record.errorCode = 'E_RECORDED';
    record.errorMessage = 'recorded failure';
  } else if (kind === 'attachments-null') {
    record.attachments = [null];
  } else {
    throw new Error('unknown tamper ' + kind);
  }
  await writeFile(path, JSON.stringify(record) + '\n');
}

function recording(store: any) {
  const calls: any[] = [];
  const port = {
    async create(projectKey: string, files: any, idempotencyKey?: string) {
      calls.push({ method: 'create', projectKey, files: JSON.parse(JSON.stringify(files ?? null)), idempotencyKey: idempotencyKey ?? null });
      return store.create(projectKey, files, idempotencyKey);
    },
    async get(uploadId: string) {
      calls.push({ method: 'get', uploadId });
      return store.get(uploadId);
    },
    async writePart(uploadId: string, clientFileId: string, stream: any) {
      const chunks: Buffer[] = [];
      for await (const chunk of stream) chunks.push(Buffer.from(chunk));
      const bytes = Buffer.concat(chunks);
      calls.push({ method: 'writePart', uploadId, clientFileId, bytes: bytes.length, sha256: digest(bytes) });
      return store.writePart(uploadId, clientFileId, Readable.from([bytes]));
    },
    async complete(uploadId: string) {
      calls.push({ method: 'complete', uploadId });
      return store.complete(uploadId);
    },
    async cancel(uploadId: string) {
      calls.push({ method: 'cancel', uploadId });
      return store.cancel(uploadId);
    },
  };
  return { calls, port };
}

const scratch = await mkdtemp(join(output, '.generate-'));
try {
  for (const [index, scenario] of scenarios.entries()) {
    const base = join(scratch, `${index}-${scenario.name}`);
    await mkdir(base);
    const project = await realpath(base);
    const replacement = join(project, 'replacement');
    await mkdir(replacement);
    let now = epoch;
    let created = 0;
    const store = new UploadStore({
      resolveProject: async (key: string) =>
        key === 'missing' ? join(project, 'absent') : key === 'p\ufffd' ? replacement : project,
      listProjects: async () => [project, replacement],
      now: () => new Date(now),
      uuid: () => `upload-${++created}`,
    });
    const recorder = recording(store);
    const options: any = { now: () => new Date(now), uuid: () => `gateway-${index}` };
    if (scenario.name !== 'provider-unavailable') options.uploadLifecycle = recorder.port;
    const gateway = new InProcessGateway({} as never, options);
    scenario.results = [];
    for (const operation of scenario.operations) {
      const since = recorder.calls.length;
      let result: any;
      try {
        if (operation.advance !== undefined) {
          now += operation.advance;
          result = { ok: null };
        } else if (operation.tamper !== undefined) {
          await tamper(operation.tamper, project);
          result = { ok: null };
        } else {
          const input = operation.undefinedParams ? undefined : expand(operation.params);
          let value: any;
          switch (operation.method) {
            case 'create': value = await gateway.uploadCreate(input as never); break;
            case 'get': value = await gateway.uploadGet(input as never); break;
            case 'part': value = await gateway.uploadPart(input as never); break;
            case 'complete': value = await gateway.uploadComplete(input as never); break;
            case 'cancel': value = await gateway.uploadCancel(input as never); break;
            default: throw new Error('unknown method ' + operation.method);
          }
          result = { ok: clean(value, project) };
        }
      } catch (error: any) {
        result = { error: { code: wsCode(error), message: normalize(String(error?.message ?? error), project) } };
      }
      result.calls = clean(recorder.calls.slice(since), project);
      scenario.results.push(result);
    }
  }

  const oracle = {
    sourceCommit,
    sources,
    command: 'PDRS_BASELINE=/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/upload-gateway/generate.mts <output>',
    node: process.version,
    errorMapping: 'src/gateway/server/GatewayWsConnection.ts:249-253',
    epoch,
    scenarios,
  };
  await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
  console.log(`PASS: ${scenarios.length} real TS gateway scenarios, ${scenarios.reduce((n: number, s: any) => n + s.results.length, 0)} operations`);
} finally {
  await rm(scratch, { recursive: true, force: true });
  await rm(isolatedHome, { recursive: true, force: true });
}
