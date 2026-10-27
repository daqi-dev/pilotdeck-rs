import { baselineSourceHashes } from '../_shared/baseline.mts';
// Run from repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
//
// The corpus is produced by the real baseline, not by a reimplementation:
// ContentAddressedWorkspaceSnapshotRecorder writes the snapshots, then the
// snapshotList / snapshotGet / snapshotRestore methods of a real
// createLocalGateway project the same on-disk tree. Every projection input and
// every outcome (value or name/code/message) is recorded verbatim.
import { lstat, mkdir, mkdtemp, readdir, readFile, readlink, rm, symlink, writeFile } from 'node:fs/promises';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
import { installFixedClock, bindBaseline, removeOwned } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);

// Shared prologue (#322-4): pinned clock, Git provenance binding and the
// ownership-checked recursive delete, identical to workspace-snapshot.
const clock = installFixedClock();
const { git, provenance } = bindBaseline({ baseline, commit: BASELINE_COMMIT });
assert.ok(process.versions.node.startsWith('22.'), `Node 22 required, got ${process.versions.node}`);

// Direct behavior inputs of this projection. The shared guard already binds the
// whole src tree above; these hashes make the replay fail loudly if one of the
// projection's own source files is swapped without regenerating the corpus.
const sourceFiles = [
  'src/cli/createLocalGateway.ts',
  'src/cli/LocalGatewayBootConfig.ts',
  'src/gateway/dialog/projectRegistry.ts',
  'src/pilot/paths.ts',
  'src/storage/workspaceSnapshot.ts',
  'src/web/server/listProjects.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sourceFiles);

type RemoveImpl = (path: string, options: { recursive: boolean; force: boolean }) => Promise<void>;


const { createLocalGateway } = await import(
  pathToFileURL(join(baseline, 'src/cli/createLocalGateway.ts')).href
);
const { ContentAddressedWorkspaceSnapshotRecorder } = await import(
  pathToFileURL(join(baseline, 'src/storage/workspaceSnapshot.ts')).href
);
const { resolveProjectStorageId } = await import(
  pathToFileURL(join(baseline, 'src/pilot/paths.ts')).href
);

const PIPELINE_YAML = `schemaVersion: 1
agent:
  model: test/test
model:
  providers:
    test:
      protocol: openai
      url: http://127.0.0.1:1
      apiKey: test-only
      models:
        test: {}
`;

/** Mask the run directory in every string, recursively. */
function mask<T>(value: T, dir: string): T {
  return JSON.parse(JSON.stringify(value).split(dir).join('<dir>')) as T;
}

/** Node's mkdtemp error quotes the attempted random name, not the template. */
function maskRestoreName(text: string): string {
  return text.replace(/\.pilotdeck-restore-([A-Za-z0-9]{6})/g, (match, suffix: string) =>
    suffix === 'XXXXXX' ? match : '.pilotdeck-restore-<random>');
}

async function recordTree(root: string, maskDir: string) {
  const rows: any[] = [];
  const walk = async (directory: string) => {
    for (const name of await readdir(directory).catch(() => [] as string[])) {
      const path = join(directory, name);
      const info = await lstat(path).catch(() => null);
      if (!info) continue;
      const rel = relative(root, path).split(sep).join('/');
      if (info.isDirectory()) {
        rows.push({ path: rel, kind: 'dir' });
        await walk(path);
      } else if (info.isSymbolicLink()) {
        rows.push({ path: rel, kind: 'symlink', target: await readlink(path) });
      } else {
        let bytes = await readFile(path);
        if (name === 'manifest.json') {
          // A hand-planted absolute objectKey is the one run-dependent value we
          // need to re-materialize. Mask it here; the replay substitutes the
          // scratch dir before writing the manifest again.
          bytes = Buffer.from(bytes.toString('utf8').split(maskDir).join('<dir>'));
        }
        rows.push({ path: rel, kind: 'file', b64: bytes.toString('base64') });
      }
    }
  };
  await walk(root);
  rows.sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0));
  return rows;
}

async function makeGateway(dir: string, options: { configured: boolean }) {
  const projectRoot = join(dir, 'project');
  const targetRoot = join(dir, 'target-project');
  const pilotHome = projectRoot;
  const storageRoot = join(dir, 'storage');
  const snapshotRoot = join(storageRoot, 'snapshots');
  await mkdir(projectRoot, { recursive: true });
  await mkdir(targetRoot, { recursive: true });
  await mkdir(storageRoot, { recursive: true });
  await writeFile(join(projectRoot, 'pilotdeck.yaml'), PIPELINE_YAML, 'utf8');
  // Fixed directory names make resolveProjectStorageId stable even though the
  // project roots themselves live under the per-run mkdtemp.
  await mkdir(join(pilotHome, 'projects', 'p-main'), { recursive: true });
  await writeFile(join(pilotHome, 'projects', 'p-main', '.cwd'), `${projectRoot}\n`, 'utf8');
  await mkdir(join(pilotHome, 'projects', 'p-target'), { recursive: true });
  await writeFile(join(pilotHome, 'projects', 'p-target', '.cwd'), `${targetRoot}\n`, 'utf8');
  const env: Record<string, string | undefined> = {
    ...process.env,
    PILOTDECK_STORAGE_ROOT: options.configured ? storageRoot : undefined,
    PILOTDECK_SNAPSHOT_ROOT: options.configured ? snapshotRoot : undefined,
  };
  const local = createLocalGateway({
    projectRoot,
    pilotHome,
    fallbackProjectRoot: projectRoot,
    env,
  });
  return {
    local,
    projectRoot,
    targetRoot,
    pilotHome,
    storageRoot,
    snapshotRoot,
    mainWorkspaceId: resolveProjectStorageId(projectRoot, pilotHome),
    targetWorkspaceId: resolveProjectStorageId(targetRoot, pilotHome),
  };
}

type Operation = {
  op: 'snapshotList' | 'snapshotGet' | 'snapshotRestore';
  input?: any;
  /** `undefined` / `null` params object: JSON cannot carry it in `input`. */
  params?: 'undefined' | 'null';
  outcome: any;
  targetTree?: any[];
  restoreDir?: boolean;
};

class Recorder {
  readonly operations: Operation[] = [];
  constructor(readonly dir: string) {}
  async call(op: Operation['op'], input?: any, params?: 'undefined' | 'null'): Promise<Operation> {
    const recorded: Operation = {
      op,
      ...(params ? { params } : { input: mask(input, this.dir) }),
      outcome: undefined,
    };
    let target: string | undefined;
    let before: string[] = [];
    if (op === 'snapshotRestore' && !params) {
      target = input.targetProjectKey ?? input.projectKey;
      before = await readdir(target).catch(() => []);
    }
    try {
      const actual = params === 'undefined' ? undefined : params === 'null' ? null : input;
      const value = await (this as any).gateway[op](actual);
      let projected = value;
      let created: string | undefined;
      if (op === 'snapshotRestore' && !params) {
        const after = await readdir(target!).catch(() => []);
        const names = after.filter((name) => name.startsWith('.pilotdeck-restore-') && !before.includes(name));
        assert.equal(names.length, 1, `restore must create exactly one mkdtemp directory, got ${names.length}`);
        created = join(target!, names[0]);
        assert.equal(value.workspaceKey, created, 'workspaceKey is the mkdtemp directory');
        projected = { ...value, workspaceKey: '<restore>' };
        recorded.targetTree = await recordTree(created, this.dir);
        recorded.restoreDir = true;
      }
      recorded.outcome = { ok: mask(projected, this.dir) };
    } catch (error: any) {
      recorded.outcome = {
        error: {
          name: error.name,
          ...(error.code === undefined ? {} : { code: error.code }),
          message: maskRestoreName(String(error.message).split(this.dir).join('<dir>')),
        },
      };
      if (op === 'snapshotRestore' && !params) {
        const after = await readdir(target!).catch(() => []);
        const names = after.filter((name) => name.startsWith('.pilotdeck-restore-') && !before.includes(name));
        assert.ok(names.length <= 1, `restore left more than one mkdtemp directory: ${names}`);
        if (names.length === 1) {
          recorded.restoreDir = true;
          recorded.targetTree = await recordTree(join(target!, names[0]), this.dir);
        } else {
          recorded.restoreDir = false;
          recorded.targetTree = [];
        }
      }
    }
    this.operations.push(recorded);
    return recorded;
  }
}

const scenarios: any[] = [];
const scratch = await mkdtemp('/tmp/pdrs-a66p4-');

/**
 * General workspace storage id over a raw marker spelling whose lexical
 * `resolve` is pilotHome but whose `statSync(raw)` fails: `missing/..` hits
 * ENOENT and `symlink/..` points through a file (ENOTDIR). TS must fall
 * through to the p-general marker; a resolver that stats the normalized path
 * would let `p-a-raw-marker` own the id.
 */
async function recordRawMarkerScenario(kind: 'missing' | 'symlink') {
  const name = `storage-id-${kind}-dotdot`;
  const dir = join(scratch, name);
  const project = join(dir, 'project');
  const pilotHome = join(dir, 'pilot-home');
  const storageRoot = join(dir, 'storage');
  const snapshotRoot = join(storageRoot, 'snapshots');
  for (const path of [project, pilotHome, storageRoot]) {
    await mkdir(path, { recursive: true });
  }
  await writeFile(join(pilotHome, 'pilotdeck.yaml'), PIPELINE_YAML, 'utf8');
  const symlinks: Array<{ path: string; target: string }> = [];
  if (kind === 'symlink') {
    const notADir = join(pilotHome, 'not-a-dir');
    await writeFile(notADir, 'x', 'utf8');
    await symlink(notADir, join(pilotHome, 'link'));
    symlinks.push({
      path: '<dir>/pilot-home/link',
      target: '<dir>/pilot-home/not-a-dir',
    });
  }
  const rawMarker = `${pilotHome}/${kind === 'missing' ? 'missing' : 'link'}/..`;
  for (const [id, marker] of [
    ['p-a-raw-marker', rawMarker],
    ['p-general', pilotHome],
  ] as const) {
    const registry = join(pilotHome, 'projects', id);
    await mkdir(registry, { recursive: true });
    await writeFile(join(registry, '.cwd'), `${marker}\n`, 'utf8');
  }
  const env: Record<string, string | undefined> = {
    ...process.env,
    PILOTDECK_STORAGE_ROOT: storageRoot,
    PILOTDECK_SNAPSHOT_ROOT: snapshotRoot,
  };
  const local = createLocalGateway({
    projectRoot: project,
    pilotHome,
    fallbackProjectRoot: project,
    env,
  });
  const partition = join(
    snapshotRoot,
    'workspaces',
    'p-general',
    'sessions',
    's',
    'snapshots',
    'snap-raw-marker',
  );
  await mkdir(partition, { recursive: true });
  await writeFile(join(partition, 'manifest.json'), JSON.stringify({
    snapshotId: 'snap-raw-marker',
    workspaceId: 'p-general',
    sessionId: 's',
    turnId: 't',
    runId: 'r',
    phase: 'pre_user',
    createdAt: '2026-01-01T00:00:04.000Z',
    entries: [],
  }, null, 2));
  const recorder = new Recorder(dir);
  (recorder as any).gateway = local.gateway;
  await recorder.call('snapshotList', { projectKey: pilotHome });
  const listed = recorder.operations[0].outcome.ok;
  assert.deepEqual(
    listed.items.map((snapshot: any) => snapshot.snapshotId),
    ['snap-raw-marker'],
    `${name}: the General workspace owns the storage id`,
  );
  scenarios.push({
    name,
    tree: await recordTree(storageRoot, dir),
    snapshotRoot: '<dir>/storage/snapshots',
    absoluteFile: null,
    projects: [{ key: '<dir>/pilot-home', workspaceId: 'p-general' }],
    fallbackProjectRoot: '<dir>/project',
    gateway: true,
    native: {
      pilotHome: '<dir>/pilot-home',
      cwd: '<dir>',
      projects: [
        { id: 'p-a-raw-marker', marker: `<dir>/pilot-home/${kind === 'missing' ? 'missing' : 'link'}/..` },
        { id: 'p-general', marker: '<dir>/pilot-home' },
      ],
      files: kind === 'symlink'
        ? [{ path: '<dir>/pilot-home/not-a-dir', content: 'x' }]
        : [],
      symlinks,
    },
    operations: recorder.operations,
  });
  local.dispose();
  console.log(`${name}: operations=${recorder.operations.length}`);
}

try {
  // Ownership negative control: the historical fixed path and a real sentinel
  // outside the guarded tree must both be refused before any rm call.
  {
    const guardRoot = join(scratch, 'guard-root');
    const outsideRoot = join(scratch, 'guard-outside');
    await mkdir(guardRoot, { recursive: true });
    await mkdir(outsideRoot, { recursive: true });
    const sentinel = join(outsideRoot, 'pdrs-a66-pr4-sentinel');
    await writeFile(sentinel, 'keep\n');
    const reached: string[] = [];
    const recordingRemove: RemoveImpl = async (path) => {
      reached.push(String(path));
    };
    await assert.rejects(
      () => removeOwned(guardRoot, '/tmp/pdrs-a66-pr4-legacy', { recursive: true }, recordingRemove),
      /outside the run directory/,
    );
    assert.deepEqual(reached, [], 'a refused delete must never reach the filesystem');
    await assert.rejects(() => removeOwned(guardRoot, sentinel, { recursive: true }), /outside the run directory/);
    assert.equal(await readFile(sentinel, 'utf8'), 'keep\n', 'out-of-tree sentinel survived');
    console.log('negative-control: refused 2 out-of-tree deletes; 0 filesystem calls; sentinel kept');
  }

  // ---------------------------------------------------------------- main corpus
  {
    const dir = join(scratch, 'projection');
    await mkdir(dir, { recursive: true });
    const gateway = await makeGateway(dir, { configured: true });
    const { local, projectRoot, targetRoot, pilotHome, storageRoot, snapshotRoot } = gateway;
    const recorder = new Recorder(dir);
    (recorder as any).gateway = local.gateway;

    const workspace = join(dir, 'workspace');
    await mkdir(join(workspace, 'nested'), { recursive: true });
    await writeFile(join(workspace, 'alpha.txt'), 'alpha\n');
    await writeFile(join(workspace, 'nested', 'beta.txt'), 'beta\n');
    await symlink('alpha.txt', join(workspace, 'link'));

    const config = { root: snapshotRoot, snapshotRoot };
    const snapshots = new ContentAddressedWorkspaceSnapshotRecorder(config);
    const tick = () => {
      clock.tick();
    };
    const capture = async (turnId: string, sessionId: string, phase: 'pre_user' | 'post_agent', workspaceId = gateway.mainWorkspaceId) => {
      const input = {
        workspaceId,
        sessionId,
        turnId,
        runId: `run-${turnId}`,
        workspaceDir: workspace,
      };
      return phase === 'pre_user' ? snapshots.capturePreUser(input) : snapshots.capturePostAgent(input);
    };
    tick();
    const t1 = await capture('t1', 'sess-a', 'pre_user');
    tick();
    const t2 = await capture('t2', 'sess-a', 'pre_user');
    tick();
    const t3 = await capture('t3', 'sess-a', 'post_agent');
    tick();
    const t4 = await capture('t4', 'sess-a', 'pre_user');
    tick();
    const t5 = await capture('t5', 'sess-b', 'pre_user');
    tick();
    const t6 = await capture('t6', 'sess-b', 'post_agent');
    tick();
    const t7 = await capture('t7', 'sess-b', 'pre_user');
    tick();
    const other = await capture('t-other', 'sess-a', 'pre_user', 'other-workspace');
    tick();
    const failed = await snapshots.capturePreUser({
      workspaceId: gateway.mainWorkspaceId,
      sessionId: 'sess-a',
      turnId: 't-failed',
      runId: 'run-t-failed',
      workspaceDir: workspace,
      workspaceStable: false,
    });
    for (const result of [t1, t2, t3, t4, t5, t6, t7, other, failed]) {
      assert.ok(result.snapshotId, 'real recorder produced a snapshot id');
    }
    assert.equal(failed.state, 'failed', 'workspaceStable:false writes a failure marker');

    const main = gateway.mainWorkspaceId;
    const manualBase = join(snapshotRoot, 'workspaces', 'manual', 'sessions', 'sess-a', 'snapshots');
    const writeManifest = async (name: string, manifest: any) => {
      await mkdir(join(manualBase, name), { recursive: true });
      await writeFile(join(manualBase, name, 'manifest.json'), JSON.stringify(manifest, null, 2));
    };
    await writeFile(join(storageRoot, 'outside.txt'), 'outside\n');
    await writeFile(join(dir, 'absolute.txt'), 'absolute\n');
    await mkdir(join(snapshotRoot, 'objects'), { recursive: true });
    await writeFile(join(snapshotRoot, 'objects', 'manual.txt'), 'manual\n');
    await writeManifest('escape-object', {
      snapshotId: 'escape',
      workspaceId: main,
      sessionId: 'sess-a',
      turnId: 'escape-turn',
      runId: 'escape-run',
      phase: 'pre_user',
      createdAt: '2025-12-31T23:59:59.000Z',
      entries: [
        { path: 'escaped.txt', entryType: 'file', size: 8, objectKey: '../outside.txt' },
        { path: 'absolute.txt', entryType: 'file', size: 9, objectKey: join(dir, 'absolute.txt') },
        { path: 'link', entryType: 'symlink', size: 0, linkTarget: 'alpha.txt' },
        { path: 'skip-missing-object.txt', entryType: 'file', size: 5 },
        { path: 'nested/deep.txt', entryType: 'file', size: 7, objectKey: 'objects/manual.txt' },
      ],
    });
    await writeManifest('missing-created-at', {
      snapshotId: 'missing',
      workspaceId: main,
      sessionId: 'sess-a',
      turnId: 'missing-turn',
      runId: 'missing-run',
      phase: 'pre_user',
      entries: [],
    });
    await writeManifest('unsafe-path', {
      snapshotId: 'unsafe',
      workspaceId: main,
      sessionId: 'sess-a',
      turnId: 'unsafe-turn',
      runId: 'unsafe-run',
      phase: 'pre_user',
      createdAt: '2025-12-30T23:59:59.000Z',
      entries: [{ path: '../evil.txt', entryType: 'file', objectKey: 'objects/manual.txt' }],
    });
    await writeManifest('broken-object', {
      snapshotId: 'broken',
      workspaceId: main,
      sessionId: 'sess-a',
      turnId: 'broken-turn',
      runId: 'broken-run',
      phase: 'pre_user',
      createdAt: '2025-12-29T23:59:59.000Z',
      entries: [{ path: 'broken.txt', entryType: 'file', objectKey: 'objects/missing.txt' }],
    });

    const unknownProject = join(dir, 'unknown-project');
    const all = await recorder.call('snapshotList', { projectKey: projectRoot });
    const allItems = all.outcome.ok.items;
    assert.equal(allItems.length, 11, 'seven recorder snapshots + four hand-planted owned snapshots');
    assert.equal(allItems[0].snapshotId, t7.snapshotId, 'newest createdAt first');
    assert.equal(allItems.at(-1).snapshotId, 'missing', 'missing createdAt sorts as the empty string');
    assert.equal(allItems[7].snapshotId, 'escape', 'hand-planted createdAt order');
    assert.equal(allItems[8].snapshotId, 'unsafe');
    assert.equal(allItems[9].snapshotId, 'broken');
    assert.equal(allItems.some((item: any) => item.snapshotId === other.snapshotId), false, 'other workspace filtered out');

    const first = await recorder.call('snapshotList', { projectKey: projectRoot, limit: 1 });
    assert.equal(first.outcome.ok.items.length, 1);
    assert.equal(first.outcome.ok.nextCursor, allItems[0].snapshotId);
    const second = await recorder.call('snapshotList', { projectKey: projectRoot, limit: 2, cursor: first.outcome.ok.nextCursor });
    assert.deepEqual(second.outcome.ok.items.map((item: any) => item.snapshotId), [allItems[1].snapshotId, allItems[2].snapshotId]);
    assert.equal(second.outcome.ok.nextCursor, allItems[2].snapshotId);
    {
      const ids: string[] = [];
      let cursor: string | undefined;
      let pages = 0;
      for (;;) {
        const page = await recorder.call('snapshotList', { projectKey: projectRoot, limit: 3, ...(cursor === undefined ? {} : { cursor }) });
        assert.ok(page.outcome.ok.items.length <= 3);
        ids.push(...page.outcome.ok.items.map((item: any) => item.snapshotId));
        pages += 1;
        cursor = page.outcome.ok.nextCursor;
        if (cursor === undefined) break;
      }
      assert.deepEqual(ids, allItems.map((item: any) => item.snapshotId), 'cursor walk covers the full filtered list once');
      assert.equal(pages, 4, '11 items / 3 per page');
    }
    const lastPage = await recorder.call('snapshotList', { projectKey: projectRoot, limit: 500 });
    assert.equal(lastPage.outcome.ok.items.length, 11);
    assert.equal(lastPage.outcome.ok.nextCursor, undefined, 'the last page has no cursor');
    const lastItem = await recorder.call('snapshotList', { projectKey: projectRoot, cursor: allItems.at(-1).snapshotId, limit: 2 });
    assert.deepEqual(lastItem.outcome.ok.items, [], 'a cursor on the last item yields an empty page');
    assert.equal(lastItem.outcome.ok.nextCursor, undefined);

    for (const limit of [0, -1, 1.5, 501, 1e20, '3', true]) {
      const bad = await recorder.call('snapshotList', { projectKey: projectRoot, limit });
      assert.equal(bad.outcome.error.code, 'INVALID_SNAPSHOT_LIMIT', `limit ${String(limit)}`);
      assert.equal(bad.outcome.error.message, 'limit must be an integer between 1 and 500.');
    }
    const nullLimit = await recorder.call('snapshotList', { projectKey: projectRoot, limit: null });
    assert.equal(nullLimit.outcome.ok.items.length, 11, 'nullish limit defaults to 100');

    const sessionA = await recorder.call('snapshotList', { projectKey: projectRoot, sessionKey: 'sess-a' });
    assert.equal(sessionA.outcome.ok.items.length, 8);
    const sessionB = await recorder.call('snapshotList', { projectKey: projectRoot, sessionKey: 'sess-b', limit: 2 });
    assert.equal(sessionB.outcome.ok.items.length, 2);
    assert.equal(sessionB.outcome.ok.nextCursor, t6.snapshotId);
    const sessionMissing = await recorder.call('snapshotList', { projectKey: projectRoot, sessionKey: 'sess-missing' });
    assert.deepEqual(sessionMissing.outcome.ok.items, []);
    assert.equal(sessionMissing.outcome.ok.nextCursor, undefined);
    const targetList = await recorder.call('snapshotList', { projectKey: targetRoot, sessionKey: 'sess-a' });
    assert.deepEqual(targetList.outcome.ok.items, [], 'the target workspace owns no snapshots');

    for (const cursor of ['does-not-exist', other.snapshotId, '', null, 123]) {
      const bad = await recorder.call('snapshotList', { projectKey: projectRoot, cursor, ...(cursor === other.snapshotId ? {} : { sessionKey: 'sess-a' }) });
      assert.equal(bad.outcome.error.code, 'INVALID_SNAPSHOT_CURSOR', `cursor ${String(cursor)}`);
      assert.equal(bad.outcome.error.message, 'snapshot cursor does not belong to this project.');
    }
    const filteredCursor = await recorder.call('snapshotList', { projectKey: projectRoot, sessionKey: 'sess-a', cursor: t5.snapshotId });
    assert.equal(filteredCursor.outcome.error.code, 'INVALID_SNAPSHOT_CURSOR', 'a cursor filtered out by sessionKey is invalid');
    const unknown = await recorder.call('snapshotList', { projectKey: unknownProject });
    assert.equal(unknown.outcome.error.code, 'PROJECT_NOT_FOUND');
    assert.equal(unknown.outcome.error.message, 'Unknown projectKey: <dir>/unknown-project');

    const got = await recorder.call('snapshotGet', { projectKey: projectRoot, snapshotId: t1.snapshotId });
    assert.equal(got.outcome.ok.snapshot.snapshotId, t1.snapshotId);
    const gotEscape = await recorder.call('snapshotGet', { projectKey: projectRoot, snapshotId: 'escape' });
    assert.equal(gotEscape.outcome.ok.snapshot.entries[1].objectKey, '<dir>/absolute.txt');
    const gotMissingCreated = await recorder.call('snapshotGet', { projectKey: projectRoot, snapshotId: 'missing' });
    assert.equal(gotMissingCreated.outcome.ok.snapshot.createdAt, undefined);
    for (const snapshotId of ['does-not-exist', other.snapshotId]) {
      const notFound = await recorder.call('snapshotGet', { projectKey: projectRoot, snapshotId });
      assert.equal(notFound.outcome.error.code, 'SNAPSHOT_NOT_FOUND');
      assert.equal(notFound.outcome.error.message, `Snapshot ${snapshotId} was not found.`);
    }
    const wrongProject = await recorder.call('snapshotGet', { projectKey: targetRoot, snapshotId: t1.snapshotId });
    assert.equal(wrongProject.outcome.error.code, 'SNAPSHOT_NOT_FOUND');
    const unknownGet = await recorder.call('snapshotGet', { projectKey: unknownProject, snapshotId: t1.snapshotId });
    assert.equal(unknownGet.outcome.error.code, 'PROJECT_NOT_FOUND');

    const restoredEscape = await recorder.call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'escape', targetProjectKey: targetRoot });
    assert.equal(restoredEscape.outcome.ok.restored, true);
    assert.deepEqual(restoredEscape.targetTree!.map((row) => row.path), [
      'absolute.txt',
      'escaped.txt',
      'link',
      'nested',
      'nested/deep.txt',
    ]);
    assert.equal(
      Buffer.from(restoredEscape.targetTree!.find((row) => row.path === 'escaped.txt')!.b64, 'base64').toString('utf8'),
      'outside\n',
    );
    assert.equal(
      Buffer.from(restoredEscape.targetTree!.find((row) => row.path === 'absolute.txt')!.b64, 'base64').toString('utf8'),
      'absolute\n',
    );
    const restoredRecorder = await recorder.call('snapshotRestore', { projectKey: projectRoot, snapshotId: t1.snapshotId, targetProjectKey: targetRoot });
    assert.equal(restoredRecorder.outcome.ok.restored, true);
    assert.ok(restoredRecorder.targetTree!.some((row) => row.path === 'link' && row.kind === 'symlink' && row.target === 'alpha.txt'));
    assert.ok(restoredRecorder.targetTree!.some((row) => row.path === 'nested/beta.txt' && row.kind === 'file'));
    const restoredFallback = await recorder.call('snapshotRestore', { projectKey: projectRoot, snapshotId: t1.snapshotId });
    assert.equal(restoredFallback.outcome.ok.restored, true);
    assert.ok(restoredFallback.outcome.ok.workspaceKey === '<restore>');
    const restoredEmpty = await recorder.call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'missing', targetProjectKey: targetRoot });
    assert.deepEqual(restoredEmpty.targetTree, []);

    const unsafe = await recorder.call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'unsafe', targetProjectKey: targetRoot });
    assert.equal(unsafe.outcome.error.name, 'Error');
    assert.equal(unsafe.outcome.error.message, 'Snapshot contains an unsafe relative path.');
    assert.equal(unsafe.restoreDir, true, 'mkdtemp happens before the entry check');
    assert.deepEqual(unsafe.targetTree, []);
    const broken = await recorder.call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'broken', targetProjectKey: targetRoot });
    assert.equal(broken.outcome.error.name, 'Error');
    assert.match(broken.outcome.error.message, /^ENOENT: no such file or directory, open '<dir>\/storage\/snapshots\/objects\/missing\.txt'$/);
    assert.deepEqual(broken.targetTree, []);

    for (const [snapshotId, targetProjectKey] of [
      [other.snapshotId, targetRoot],
      ['does-not-exist', targetRoot],
    ] as const) {
      const notFound = await recorder.call('snapshotRestore', { projectKey: projectRoot, snapshotId, targetProjectKey });
      assert.equal(notFound.outcome.error.code, 'SNAPSHOT_NOT_FOUND');
      assert.equal(notFound.outcome.error.message, `Snapshot ${snapshotId} was not found.`);
      assert.equal(notFound.restoreDir, false, 'ownership is checked before mkdtemp');
    }
    const unknownTarget = await recorder.call('snapshotRestore', { projectKey: projectRoot, snapshotId: t1.snapshotId, targetProjectKey: unknownProject });
    assert.equal(unknownTarget.outcome.error.code, 'PROJECT_NOT_FOUND');
    assert.equal(unknownTarget.restoreDir, false, 'the target project is resolved before mkdtemp');

    assert.equal(failed.state, 'failed');
    const scene = await recordTree(storageRoot, dir);
    assert.ok(scene.some((row) => row.path === 'snapshots/objects/manual.txt'));
    assert.ok(scene.some((row) => row.path.includes('_FAILED.json')), 'the failed capture wrote a marker');
    scenarios.push({
      name: 'projection',
      tree: scene,
      snapshotRoot: '<dir>/storage/snapshots',
      absoluteFile: { path: '<dir>/absolute.txt', b64: Buffer.from('absolute\n').toString('base64') },
      projects: [
        { key: '<dir>/project', workspaceId: gateway.mainWorkspaceId },
        { key: '<dir>/target-project', workspaceId: gateway.targetWorkspaceId },
      ],
      fallbackProjectRoot: '<dir>/project',
      operations: recorder.operations,
    });
    local.dispose();
    console.log(`projection: operations=${recorder.operations.length} tree=${scene.length}`);
  }

  // ------------------------------------------------------- provider syntax error
  {
    const dir = join(scratch, 'provider-syntax');
    await mkdir(dir, { recursive: true });
    const gateway = await makeGateway(dir, { configured: true });
    const recorder = new Recorder(dir);
    (recorder as any).gateway = gateway.local.gateway;
    const manifestDir = join(gateway.snapshotRoot, 'workspaces', 'manual', 'sessions', 's', 'snapshots', 'bad');
    await mkdir(manifestDir, { recursive: true });
    await writeFile(join(manifestDir, 'manifest.json'), 'not json');
    for (const [op, input] of [
      ['snapshotList', { projectKey: gateway.projectRoot }],
      ['snapshotGet', { projectKey: gateway.projectRoot, snapshotId: 'bad' }],
      ['snapshotRestore', { projectKey: gateway.projectRoot, snapshotId: 'bad' }],
    ] as const) {
      const result = await recorder.call(op, input);
      assert.equal(result.outcome.error.name, 'SyntaxError');
      assert.match(result.outcome.error.message, /^Unexpected token/);
    }
    scenarios.push({
      name: 'provider-syntax',
      tree: await recordTree(gateway.storageRoot, dir),
      snapshotRoot: '<dir>/storage/snapshots',
      absoluteFile: null,
      projects: [{ key: '<dir>/project', workspaceId: gateway.mainWorkspaceId }],
      fallbackProjectRoot: '<dir>/project',
      operations: recorder.operations,
    });
    gateway.local.dispose();
    console.log(`provider-syntax: operations=${recorder.operations.length}`);
  }

  // -------------------------------------------------------- provider null manifest
  {
    const dir = join(scratch, 'provider-null');
    await mkdir(dir, { recursive: true });
    const gateway = await makeGateway(dir, { configured: true });
    const recorder = new Recorder(dir);
    (recorder as any).gateway = gateway.local.gateway;
    const manifestDir = join(gateway.snapshotRoot, 'workspaces', 'manual', 'sessions', 's', 'snapshots', 'null');
    await mkdir(manifestDir, { recursive: true });
    await writeFile(join(manifestDir, 'manifest.json'), 'null');
    for (const [op, input] of [
      ['snapshotList', { projectKey: gateway.projectRoot }],
      ['snapshotGet', { projectKey: gateway.projectRoot, snapshotId: 'null' }],
      ['snapshotRestore', { projectKey: gateway.projectRoot, snapshotId: 'null' }],
    ] as const) {
      const result = await recorder.call(op, input);
      assert.equal(result.outcome.error.name, 'TypeError');
      assert.equal(result.outcome.error.message, "Cannot read properties of null (reading 'snapshotId')");
    }
    scenarios.push({
      name: 'provider-null',
      tree: await recordTree(gateway.storageRoot, dir),
      snapshotRoot: '<dir>/storage/snapshots',
      absoluteFile: null,
      projects: [{ key: '<dir>/project', workspaceId: gateway.mainWorkspaceId }],
      fallbackProjectRoot: '<dir>/project',
      operations: recorder.operations,
    });
    gateway.local.dispose();
    console.log(`provider-null: operations=${recorder.operations.length}`);
  }

  // ------------------------------------------------------------ unconfigured
  {
    const dir = join(scratch, 'unconfigured');
    await mkdir(dir, { recursive: true });
    const gateway = await makeGateway(dir, { configured: false });
    const recorder = new Recorder(dir);
    (recorder as any).gateway = gateway.local.gateway;
    for (const [op, input] of [
      ['snapshotList', { projectKey: gateway.projectRoot }],
      ['snapshotGet', { projectKey: gateway.projectRoot, snapshotId: 'x' }],
      ['snapshotRestore', { projectKey: gateway.projectRoot, snapshotId: 'x' }],
    ] as const) {
      const result = await recorder.call(op, input);
      assert.equal(result.outcome.error.code, 'CAPABILITY_UNAVAILABLE');
      assert.equal(result.outcome.error.message, 'Gateway snapshot provider is unavailable.');
    }
    scenarios.push({
      name: 'unconfigured',
      tree: [],
      snapshotRoot: null,
      absoluteFile: null,
      projects: [{ key: '<dir>/project', workspaceId: gateway.mainWorkspaceId }],
      fallbackProjectRoot: '<dir>/project',
      operations: recorder.operations,
    });
    gateway.local.dispose();
    console.log(`unconfigured: operations=${recorder.operations.length}`);
  }

  // ------------------------------------------------- restore, missing mkdtemp parent
  {
    const dir = join(scratch, 'restore-missing-target');
    await mkdir(dir, { recursive: true });
    const projectRoot = join(dir, 'project');
    const pilotHome = join(dir, 'pilot-home');
    const storageRoot = join(dir, 'storage');
    const snapshotRoot = join(storageRoot, 'snapshots');
    await mkdir(projectRoot, { recursive: true });
    await mkdir(pilotHome, { recursive: true });
    await mkdir(storageRoot, { recursive: true });
    await writeFile(join(pilotHome, 'pilotdeck.yaml'), PIPELINE_YAML, 'utf8');
    const env: Record<string, string | undefined> = {
      ...process.env,
      PILOTDECK_STORAGE_ROOT: storageRoot,
      PILOTDECK_SNAPSHOT_ROOT: snapshotRoot,
    };
    const local = createLocalGateway({
      projectRoot,
      pilotHome,
      fallbackProjectRoot: pilotHome,
      env,
    });
    // The virtual General workspace is registered by construction and is
    // resolved without a filesystem stat. Deleting pilotHome therefore leaves
    // a registry key whose parent is missing, so the real mkdtemp fails ENOENT.
    const workspaceId = resolveProjectStorageId(pilotHome, pilotHome);
    const manifestDir = join(snapshotRoot, 'workspaces', workspaceId, 'sessions', 's', 'snapshots', 'id');
    await mkdir(manifestDir, { recursive: true });
    await writeFile(join(manifestDir, 'manifest.json'), JSON.stringify({
      snapshotId: 'id',
      workspaceId,
      sessionId: 's',
      turnId: 't',
      runId: 'r',
      phase: 'pre_user',
      entries: [],
    }, null, 2));
    const recorder = new Recorder(dir);
    (recorder as any).gateway = local.gateway;
    await rm(pilotHome, { recursive: true, force: true });
    const missing = await recorder.call('snapshotRestore', {
      projectKey: pilotHome,
      snapshotId: 'id',
      targetProjectKey: pilotHome,
    });
    assert.equal(missing.outcome.error.name, 'Error');
    assert.equal(missing.outcome.error.code, 'ENOENT');
    assert.match(
      missing.outcome.error.message,
      /^ENOENT: no such file or directory, mkdtemp '<dir>\/pilot-home\/\.pilotdeck-restore-<random>'$/,
      'the recorded message quotes the attempted random name, not the XXXXXX template',
    );
    assert.equal(missing.restoreDir, false);
    // `workspaceId` is the slug of the per-run pilotHome, so it is not stable
    // across runs. Mask it in both the manifest bytes and the storage paths;
    // the Rust resolver maps the missing project key to the same placeholder.
    const tree = await recordTree(storageRoot, dir);
    for (const row of tree) {
      row.path = row.path.split(workspaceId).join('<workspaceId>');
      if (row.kind === 'file' && row.path.endsWith('manifest.json')) {
        const bytes = Buffer.from(row.b64, 'base64').toString('utf8').split(workspaceId).join('<workspaceId>');
        row.b64 = Buffer.from(bytes).toString('base64');
      }
    }
    scenarios.push({
      name: 'restore-missing-target',
      tree,
      snapshotRoot: '<dir>/storage/snapshots',
      absoluteFile: null,
      projects: [{ key: '<dir>/pilot-home', workspaceId: '<workspaceId>', missing: true }],
      fallbackProjectRoot: '<dir>/pilot-home',
      operations: recorder.operations,
    });
    local.dispose();
    console.log(`restore-missing-target: operations=${recorder.operations.length}`);
  }

  // ----------------------------------- restore fails after an earlier entry wrote
  {
    const dir = join(scratch, 'restore-partial');
    await mkdir(dir, { recursive: true });
    const gateway = await makeGateway(dir, { configured: true });
    const recorder = new Recorder(dir);
    (recorder as any).gateway = gateway.local.gateway;
    await mkdir(join(gateway.snapshotRoot, 'objects'), { recursive: true });
    await writeFile(join(gateway.snapshotRoot, 'objects', 'partial.txt'), 'partial\n');
    const manifestDir = join(gateway.snapshotRoot, 'workspaces', 'manual', 'sessions', 's', 'snapshots', 'partial');
    await mkdir(manifestDir, { recursive: true });
    await writeFile(join(manifestDir, 'manifest.json'), JSON.stringify({
      snapshotId: 'partial',
      workspaceId: gateway.mainWorkspaceId,
      sessionId: 's',
      turnId: 't',
      runId: 'r',
      phase: 'pre_user',
      createdAt: '2025-12-28T23:59:59.000Z',
      entries: [
        { path: 'written.txt', entryType: 'file', size: 8, objectKey: 'objects/partial.txt' },
        { path: '../escape.txt', entryType: 'file', size: 8, objectKey: 'objects/partial.txt' },
      ],
    }, null, 2));
    const partial = await recorder.call('snapshotRestore', {
      projectKey: gateway.projectRoot,
      snapshotId: 'partial',
      targetProjectKey: gateway.targetRoot,
    });
    assert.equal(partial.outcome.error.name, 'Error');
    assert.equal(partial.outcome.error.message, 'Snapshot contains an unsafe relative path.');
    assert.equal(partial.restoreDir, true, 'mkdtemp happens before the entry check');
    assert.deepEqual(partial.targetTree!.map((row) => row.path), ['written.txt']);
    assert.equal(
      Buffer.from(partial.targetTree!.find((row) => row.path === 'written.txt')!.b64, 'base64').toString('utf8'),
      'partial\n',
    );
    scenarios.push({
      name: 'restore-partial',
      tree: await recordTree(gateway.storageRoot, dir),
      snapshotRoot: '<dir>/storage/snapshots',
      absoluteFile: null,
      projects: [
        { key: '<dir>/project', workspaceId: gateway.mainWorkspaceId },
        { key: '<dir>/target-project', workspaceId: gateway.targetWorkspaceId },
      ],
      fallbackProjectRoot: '<dir>/project',
      operations: recorder.operations,
    });
    gateway.local.dispose();
    console.log(`restore-partial: operations=${recorder.operations.length}`);
  }

  // ------------------------------------ native gateway adapter + project ids
  //
  // Unlike the scenarios above, this one is replayed through the production
  // `SnapshotGateway` + `SnapshotProjects`; the oracle records the real
  // InProcessGateway wrapper validation and the real `resolveProjectKey` /
  // `resolveProjectStorageId` over `.cwd` markers. All workspace ids are fixed
  // (`p-main`, `p-target`, `p-general`) so no per-run slug is embedded.
  {
    const dir = join(scratch, 'gateway-native');
    const projectRoot = join(dir, 'project');
    const targetRoot = join(dir, 'target-project');
    const pilotHome = join(dir, 'pilot-home');
    const storageRoot = join(dir, 'storage');
    const snapshotRoot = join(storageRoot, 'snapshots');
    for (const path of [projectRoot, targetRoot, pilotHome, storageRoot]) {
      await mkdir(path, { recursive: true });
    }
    await writeFile(join(pilotHome, 'pilotdeck.yaml'), PIPELINE_YAML, 'utf8');
    const notADir = join(dir, 'not-a-dir');
    await writeFile(notADir, 'x', 'utf8');
    // `p-general` is the virtual General workspace: the registry still lets a
    // marker own its storage id, while `resolveProjectKey` short-circuits home.
    for (const [id, marker] of [
      ['p-main', projectRoot],
      ['p-target', targetRoot],
      ['p-general', pilotHome],
      ['p-file', notADir],
      ['p-gone', join(dir, 'gone-project')],
    ] as const) {
      const registry = join(pilotHome, 'projects', id);
      await mkdir(registry, { recursive: true });
      await writeFile(join(registry, '.cwd'), `${marker}\n`, 'utf8');
    }
    const env: Record<string, string | undefined> = {
      ...process.env,
      PILOTDECK_STORAGE_ROOT: storageRoot,
      PILOTDECK_SNAPSHOT_ROOT: snapshotRoot,
    };
    const local = createLocalGateway({
      projectRoot,
      pilotHome,
      fallbackProjectRoot: projectRoot,
      env,
    });
    await mkdir(join(snapshotRoot, 'objects'), { recursive: true });
    await writeFile(join(snapshotRoot, 'objects', 'a'), 'hi\n', 'utf8');
    const writeManifest = async (
      workspaceId: string,
      snapshotId: string,
      createdAt: string,
      entries: any[],
    ) => {
      const partition = join(
        snapshotRoot,
        'workspaces',
        workspaceId,
        'sessions',
        's',
        'snapshots',
        snapshotId,
      );
      await mkdir(partition, { recursive: true });
      await writeFile(join(partition, 'manifest.json'), JSON.stringify({
        snapshotId,
        workspaceId,
        sessionId: 's',
        turnId: 't',
        runId: 'r',
        phase: 'pre_user',
        createdAt,
        entries,
      }, null, 2));
    };
    const file = (path: string) => ({
      path,
      entryType: 'file',
      objectKey: 'objects/a',
      size: 3,
      digest: createHash('sha256').update('hi\n').digest('hex'),
    });
    await writeManifest('p-main', 'snap-main', '2026-01-01T00:00:03.000Z', [file('a.txt')]);
    await writeManifest('p-main', 'snap-unsafe', '2026-01-01T00:00:02.000Z', [file('../escape.txt')]);
    await writeManifest('p-target', 'snap-target', '2026-01-01T00:00:01.000Z', [file('t.txt')]);
    await writeManifest('p-general', 'snap-general', '2026-01-01T00:00:00.000Z', [file('g.txt')]);
    await writeManifest('p-other', 'snap-other', '2025-12-31T23:59:59.000Z', [file('o.txt')]);

    assert.equal(resolveProjectStorageId(projectRoot, pilotHome), 'p-main');
    assert.equal(resolveProjectStorageId(targetRoot, pilotHome), 'p-target');
    assert.equal(resolveProjectStorageId(pilotHome, pilotHome), 'p-general');

    const recorder = new Recorder(dir);
    (recorder as any).gateway = local.gateway;
    const call = (op: Operation['op'], input?: any, params?: 'undefined' | 'null') =>
      recorder.call(op, input, params);

    await call('snapshotList', { projectKey: projectRoot, limit: 1 });
    await call('snapshotList', { projectKey: projectRoot, limit: 2 });
    await call('snapshotList', { projectKey: projectRoot, cursor: 'snap-main' });
    await call('snapshotList', { projectKey: projectRoot, cursor: null });
    await call('snapshotList', { projectKey: projectRoot, limit: null });
    await call('snapshotList', { projectKey: projectRoot, limit: 0 });
    await call('snapshotList', { projectKey: projectRoot, limit: 501 });
    await call('snapshotList', { projectKey: projectRoot, sessionKey: '' });
    await call('snapshotList', { projectKey: projectRoot, sessionKey: 42 });
    await call('snapshotList', { projectKey: targetRoot });
    await call('snapshotList', { projectKey: pilotHome });
    await call('snapshotList', { projectKey: join(dir, 'outside') });
    await call('snapshotList', { projectKey: `${projectRoot}/../target-project` });
    await call('snapshotList', { projectKey: `${projectRoot}/` });
    await call('snapshotList', { projectKey: `${projectRoot}/missing` });
    await call('snapshotList', { projectKey: notADir });
    await call('snapshotList', { projectKey: undefined });
    await call('snapshotList', { projectKey: null });
    await call('snapshotList', { projectKey: 42 });
    await call('snapshotList', { projectKey: {} });
    await call('snapshotList', { projectKey: '' });
    await call('snapshotList', { projectKey: '   ' });
    await call('snapshotList', { projectKey: projectRoot, limit: {} });
    await call('snapshotList', { projectKey: projectRoot, sessionKey: null });
    await call('snapshotList', { projectKey: projectRoot, sessionKey: '   ' });
    await call('snapshotList', undefined, 'undefined');
    await call('snapshotList', undefined, 'null');
    await call('snapshotList', { projectKey: join(dir, 'gone-project') });
    await call('snapshotList', { projectKey: 'relative-project' });

    await call('snapshotGet', { projectKey: projectRoot, snapshotId: 'snap-main' });
    await call('snapshotGet', { projectKey: projectRoot, snapshotId: 'snap-target' });
    await call('snapshotGet', { projectKey: projectRoot, snapshotId: 'nope' });
    await call('snapshotGet', { projectKey: projectRoot, snapshotId: undefined });
    await call('snapshotGet', { projectKey: projectRoot, snapshotId: null });
    await call('snapshotGet', { projectKey: projectRoot, snapshotId: 42 });
    await call('snapshotGet', { projectKey: projectRoot, snapshotId: '' });
    await call('snapshotGet', { projectKey: projectRoot, snapshotId: '   ' });
    await call('snapshotGet', { projectKey: join(dir, 'outside'), snapshotId: 'snap-main' });

    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: targetRoot });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main' });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: `${projectRoot}/../target-project` });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: pilotHome });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: join(dir, 'outside') });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: notADir });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: null });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: '' });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: 42 });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: '   ' });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: undefined, targetProjectKey: targetRoot });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: null, targetProjectKey: targetRoot });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: '', targetProjectKey: targetRoot });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: '   ', targetProjectKey: targetRoot });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-unsafe', targetProjectKey: targetRoot });
    await call('snapshotRestore', { projectKey: targetRoot, snapshotId: 'snap-main', targetProjectKey: targetRoot });
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'nope', targetProjectKey: targetRoot });
    await call('snapshotRestore', { projectKey: join(dir, 'outside'), snapshotId: 'snap-main', targetProjectKey: targetRoot });
    await call('snapshotRestore', undefined, 'undefined');
    await call('snapshotRestore', { projectKey: projectRoot, snapshotId: 'snap-main', targetProjectKey: join(dir, 'gone-project') });

    const last = recorder.operations.at(-1)!;
    assert.equal(last.outcome.error.code, 'PROJECT_NOT_FOUND');
    scenarios.push({
      name: 'gateway-native',
      tree: await recordTree(storageRoot, dir),
      snapshotRoot: '<dir>/storage/snapshots',
      absoluteFile: null,
      projects: [
        { key: '<dir>/project', workspaceId: 'p-main' },
        { key: '<dir>/target-project', workspaceId: 'p-target' },
        { key: '<dir>/pilot-home', workspaceId: 'p-general' },
      ],
      fallbackProjectRoot: '<dir>/project',
      gateway: true,
      native: {
        pilotHome: '<dir>/pilot-home',
        cwd: '<dir>',
        projects: [
          { id: 'p-main', marker: '<dir>/project' },
          { id: 'p-target', marker: '<dir>/target-project' },
          { id: 'p-general', marker: '<dir>/pilot-home' },
          { id: 'p-file', marker: '<dir>/not-a-dir' },
          { id: 'p-gone', marker: '<dir>/gone-project' },
        ],
        files: [{ path: '<dir>/not-a-dir', content: 'x' }],
        symlinks: [],
      },
      operations: recorder.operations,
    });
    local.dispose();
    console.log(`gateway-native: operations=${recorder.operations.length}`);
  }

  await recordRawMarkerScenario('missing');
  await recordRawMarkerScenario('symlink');

  const oracle = {
    sourceCommit: provenance.ref,
    sourceTreeSha256: provenance.sha256,
    sourceTreeFiles: provenance.count,
    sourceSha256,
    node: process.version,
    v8: process.versions.v8,
    platform: process.platform,
    arch: process.arch,
    locale: Intl.Collator().resolvedOptions().locale,
    scenarios,
  };
  const text = `${JSON.stringify(oracle, null, 2)}\n`;
  assert.ok(!text.includes(scratch), 'oracle must not contain the per-run scratch directory');
  assert.ok(!text.includes(baseline), 'oracle must not contain the baseline path');
  assert.ok(!text.includes('pdrs-a66p4-'), 'oracle must not contain the fixed scratch prefix');
  for (const name of [
    'projection',
    'provider-syntax',
    'provider-null',
    'unconfigured',
    'restore-missing-target',
    'restore-partial',
    'gateway-native',
    'storage-id-missing-dotdot',
    'storage-id-symlink-dotdot',
  ]) {
    assert.ok(scenarios.some((scenario) => scenario.name === name), `missing scenario ${name}`);
  }
  assert.equal(scenarios.length, 9);
  assert.equal(scenarios[0].operations.length, 44, 'projection operation count is pinned');
  const native = scenarios.find((scenario) => scenario.name === 'gateway-native');
  assert.equal(native.operations.length, 58, 'gateway-native operation count is pinned');
  for (const name of ['storage-id-missing-dotdot', 'storage-id-symlink-dotdot']) {
    const scenario = scenarios.find((candidate) => candidate.name === name);
    assert.equal(scenario.operations.length, 1, `${name} operation count is pinned`);
  }
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), text, { flag: 'wx' });
  console.log(`wrote ${join(output, 'oracle.json')} (${text.length} bytes)`);
} finally {
  await removeOwned(scratch, scratch, { recursive: true });
}
