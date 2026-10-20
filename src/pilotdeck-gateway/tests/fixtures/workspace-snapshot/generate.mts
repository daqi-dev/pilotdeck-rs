// Run from repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
import { lstat, mkdir, mkdtemp, readdir, readFile, readlink, rm, symlink, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.equal(process.versions.node.split('.')[0], '22', 'oracle is calibrated on Node 22');
assert.ok(process.argv[2], 'provide the fixture directory or Rust snapshot root');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
// Fixed clock: the only run-varying oracle field is `createdAt`, which the TS
// recorder reads from `new Date().toISOString()`. Pinning the process clock
// keeps oracle.json byte-identical between generator runs without masking any
// field; `tick` advances the fake clock instead of waiting for the wall clock,
// so "newest createdAt first" stays strictly ordered.
const RealDate = Date;
let clockMs = RealDate.UTC(2026, 0, 1);
class FixedDate extends RealDate {
  constructor(...args: any[]) {
    super(...((args.length === 0 ? [clockMs] : args) as []));
  }
  static now(): number {
    return clockMs;
  }
}
globalThis.Date = FixedDate as unknown as DateConstructor;
// #280's shared provenance guard: an explicit absolute Git executable (never a
// workspace-local shim), the pinned HEAD, a fully clean tracked tree, and every
// tracked `src` byte compared against the Git blob at the pinned commit. This
// also catches assume-unchanged/skip-worktree edits that `status` alone misses.
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const git = (...args: string[]) =>
  execFileSync(gitExecutable, ['--no-replace-objects', '-C', baseline, ...args], {
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const trackedChanges = git('status', '--porcelain', '--untracked-files=no').toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);

// Every source reachable from workspaceSnapshot.ts through relative imports is
// part of the calibrating implementation, so `sourceSha256` covers the closure
// rather than the entry file alone (shared helper; bare/node: imports are
// external, each file is verified against and hashed from the commit blob).
const source = 'src/storage/workspaceSnapshot.ts';
const sourceSha256: Record<string, string> = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [source]);

type RemoveImpl = (path: string | Buffer, options: { recursive: boolean; force: boolean }) => Promise<void>;

/**
 * Deletes are confined to the run's own mkdtemp tree. The old fixed-path P0
 * removed /tmp/pdrs-r9-* while every sentinel lived under <scratch>, so those
 * sentinels could not observe that regression (round-4 P1). Every `rm` below
 * goes through removeOwned: an out-of-tree target is refused before the
 * filesystem is touched, independent of where a sentinel lives.
 */
function assertInside(root: string, path: string, action: string): string {
  const rel = relative(root, path);
  if (rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(`${action} refused: ${path} is outside the run directory ${root}`);
  }
  return path;
}
async function removeOwned(
  root: string,
  value: string | Buffer,
  options: { recursive?: boolean } = {},
  remove: RemoveImpl = rm,
): Promise<void> {
  let path: string | Buffer;
  if (typeof value === 'string') {
    path = assertInside(root, resolve(root, value.split('<dir>').join(root)), 'remove');
  } else {
    // The non-UTF-8 probe path is byte-exact, so compare the run-root prefix as
    // bytes instead of decoding a name that is deliberately not UTF-8.
    const prefix = Buffer.from(root.endsWith(sep) ? root : `${root}${sep}`);
    if (!value.subarray(0, prefix.length).equals(prefix)) {
      throw new Error(`remove refused: ${value.toString('hex')} is outside the run directory ${root}`);
    }
    // The byte-prefix check alone accepts `<root>/../outside`. Path segments
    // are ASCII, so the deliberately non-UTF-8 name can be inspected as latin1.
    const rest: string[] = value.subarray(prefix.length).toString('latin1').split(sep);
    if (rest.includes('..')) {
      throw new Error(`remove refused: ${value.toString('hex')} escapes the run directory ${root}`);
    }
    path = value;
  }
  await remove(path, { recursive: !!options.recursive, force: true });
}
const { ContentAddressedWorkspaceSnapshotRecorder, createWorkspaceSnapshotProvider, resolveWorkspaceSnapshotConfig } =
  await import(pathToFileURL(join(baseline, source)).href);

if (process.argv[3] === '--verify-rust') {
  // Reverse cross: the real TS provider reads a snapshot root the Rust test
  // wrote, checks manifest bytes against JSON.stringify(, 2), object digests
  // against content, and restores into a fresh directory.
  const provider = createWorkspaceSnapshotProvider({ root: output, snapshotRoot: output });
  const list = await provider.list();
  assert.ok(list.length >= 2, 'Rust test must write at least two snapshots');
  for (const descriptor of list) {
    assert.ok(['pre_user', 'post_agent'].includes(descriptor.phase), 'phase must be a snapshot phase');
    assert.match(descriptor.createdAt ?? '', /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/, 'createdAt must be ISO millis');
    const manifest = await provider.get(descriptor.snapshotId);
    const text = await readFile(descriptor.manifestPath, 'utf8');
    assert.ok(!text.endsWith('\n'), 'manifest must have no trailing newline');
    assert.equal(text, JSON.stringify(manifest, null, 2), 'manifest bytes must be JSON.stringify(, 2)');
    for (const entry of manifest.entries ?? []) {
      if (entry.entryType === 'file') {
        const bytes = await readFile(join(output, entry.objectKey));
        assert.equal(createHash('sha256').update(bytes).digest('hex'), entry.digest, 'object digest must match content');
        assert.equal(bytes.length, entry.size, 'object size must match manifest entry');
      }
    }
  }
  // Restore inside the Rust-owned snapshot root: the caller drops that whole
  // per-run tree, so no fixed sibling path (the old `../ts-restore`) is needed.
  const restored = await provider.restore(list[0].snapshotId, join(output, 'ts-restore'));
  assert.equal(restored.restored, true);
  console.log(`PASS: real TS listed ${list.length} Rust snapshots, verified manifest bytes, objects and restore`);
  process.exit(0);
}

const tick = async () => {
  clockMs += 1;
};

// Only the `createdAt` field of a recorder-written manifest/marker is
// run-varying. Project it field by field (matched by the snapshot partition
// in `dynamicIds`) instead of replacing every ISO-looking string, so dates in
// failureReason, entries paths, linkTarget and user `.json` files stay part
// of the comparison. A marker's `error` value is the only place that embeds
// the run-specific scenario directory.
function projectArtifact(rel: string, bytes: Buffer, dir: string, dynamicIds: Set<string>): Buffer {
  const name = rel.split('/').pop();
  if (name !== 'manifest.json' && name !== '_FAILED.json') return bytes;
  const partition = rel.split('/').slice(-2, -1)[0];
  if (!dynamicIds.has(partition)) return bytes;
  const lines = bytes.toString('utf8').split('\n');
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    if (line.startsWith('  "createdAt": "')) {
      const prefix = '  "createdAt": "';
      const end = line.lastIndexOf('"');
      if (end >= prefix.length) lines[index] = `${prefix}<ts>${line.slice(end)}`;
    } else if (name === '_FAILED.json' && line.startsWith('  "error": ')) {
      lines[index] = line.split(dir).join('<dir>');
    }
  }
  return Buffer.from(lines.join('\n'));
}

async function recordTree(root: string, dir: string, dynamicIds: Set<string>) {
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
        const bytes = await readFile(path);
        const projected = projectArtifact(rel, bytes, dir, dynamicIds);
        rows.push({
          path: rel,
          kind: 'file',
          size: projected.length,
          sha256: createHash('sha256').update(projected).digest('hex'),
        });
      }
    }
  };
  await walk(root);
  rows.sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0));
  return rows;
}

async function assertNoTemporaries(directory: string) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    assert.ok(!entry.name.includes('.tmp-'), `leftover temporary ${join(directory, entry.name)}`);
    if (entry.isDirectory()) await assertNoTemporaries(join(directory, entry.name));
  }
}

type FileSpec = { path: string; bytes: Buffer };
type WorkspaceSpec = {
  files?: FileSpec[];
  symlinks?: { path: string; target: string }[];
  emptyDirs?: string[];
  rawFiles?: { dir: string; name: Buffer; bytes: Buffer }[];
};

const scenarios: any[] = [];
async function scenario(name: string, options: { workspace?: WorkspaceSpec; snapshotRoot?: string; dirName?: string; steps?: any[] }) {
  // `dirName` lets the capture-root NUL case use the 20-character mkdtemp root
  // itself: its quoted partition must stay under Node's 128-unit inspect limit.
  const dir = join(scratch, options.dirName ?? name);
  const workspaceDir = join(dir, 'workspace');
  await mkdir(workspaceDir, { recursive: true });
  const spec = options.workspace ?? {};
  for (const file of spec.files ?? []) {
    await mkdir(dirname(join(workspaceDir, file.path)), { recursive: true });
    await writeFile(join(workspaceDir, file.path), file.bytes);
  }
  for (const empty of spec.emptyDirs ?? []) await mkdir(join(workspaceDir, empty), { recursive: true });
  for (const link of spec.symlinks ?? []) {
    await mkdir(dirname(join(workspaceDir, link.path)), { recursive: true });
    await symlink(link.target, join(workspaceDir, link.path));
  }
  for (const raw of spec.rawFiles ?? []) {
    // Buffer paths keep non-UTF-8 filenames byte-exact.
    await writeFile(Buffer.concat([Buffer.from(`${join(dir, raw.dir)}/`), raw.name]), raw.bytes);
  }
  const root = resolve(join(dir, options.snapshotRoot ?? 'snapshots'));
  const config = { root, snapshotRoot: root };
  const recorders = [new ContentAddressedWorkspaceSnapshotRecorder(config)];
  const provider = () => createWorkspaceSnapshotProvider(config);
  const maskDir = (value: string) => value.split(dir).join('<dir>');
  // Step paths are scenario-relative ('workspace/...') or '<dir>'-masked
  // absolutes. join concatenates where resolve resets on absolute segments
  // (join(dir, dir + '/x') doubles the prefix — the round-2 failure), so the
  // unmasked value must go through resolve.
  const resolvePath = (value: string) => assertInside(dir, resolve(dir, value.split('<dir>').join(dir)), 'step path');
  const ids = new Map<string, string>();
  const markerDirs = new Map<string, string>();
  // Snapshot ids whose manifest/marker was produced by the real recorder;
  // only their top-level createdAt (and marker error path) is run-varying.
  const dynamicIds = new Set<string>();
  const recorded: any[] = [];
  for (const step of options.steps ?? []) {
    const done = { ...step };
    switch (step.op) {
      case 'tick':
        await tick();
        break;
      case 'recorder':
        recorders.push(new ContentAddressedWorkspaceSnapshotRecorder(config));
        break;
      case 'writeFile':
        await mkdir(dirname(resolvePath(step.path)), { recursive: true });
        await writeFile(resolvePath(step.path), Buffer.from(step.b64, 'base64'));
        break;
      case 'mkdir':
        await mkdir(resolvePath(step.path), { recursive: true });
        break;
      case 'remove':
        await removeOwned(dir, step.path, { recursive: !!step.recursive });
        break;
      case 'writeSnapshot': {
        const base = step.markerOf === undefined ? '' : markerDirs.get(step.markerOf);
        assert.ok(base !== undefined, `unknown marker alias ${step.markerOf}`);
        const path = join(base, step.path ?? step.name);
        await mkdir(dirname(join(root, path)), { recursive: true });
        await writeFile(join(root, path), step.text);
        done.path = path.split(sep).join('/');
        delete done.markerOf;
        delete done.name;
        break;
      }
      case 'mkdirSnapshot':
        await mkdir(join(root, step.path), { recursive: true });
        break;
      case 'symlinkSnapshot': {
        const path = join(root, step.path);
        await mkdir(dirname(path), { recursive: true });
        await removeOwned(root, path, { recursive: true });
        await symlink(step.target, path);
        break;
      }
      case 'removeSnapshot': {
        const base = step.markerOf === undefined ? '' : markerDirs.get(step.markerOf);
        assert.ok(base !== undefined, `unknown marker alias ${step.markerOf}`);
        const path = join(base, step.path ?? step.name);
        await removeOwned(root, path);
        done.path = path.split(sep).join('/');
        delete done.markerOf;
        delete done.name;
        break;
      }
      case 'capture': {
        const input = { ...step.input, workspaceDir: resolvePath(step.input.workspaceDir) };
        // A step-level snapshotRoot pins a config whose root carries a NUL
        // (capture then rejects in the marker-path mkdir, outside the try).
        // The recorded step keeps `<dir>` so the oracle never holds a run path.
        const stepRoot = step.snapshotRoot ? resolvePath(step.snapshotRoot) : undefined;
        const currentConfig = stepRoot ? { root: stepRoot, snapshotRoot: stepRoot } : config;
        try {
          // The latest recorder by default: 'recorder' steps opt the next
          // capture into fresh state (the completed map also caches failures,
          // so a stale recorder could never observe marker changes on disk).
          const current = step.recorder ?? recorders.length - 1;
          const recorder = step.snapshotRoot
            ? new ContentAddressedWorkspaceSnapshotRecorder(currentConfig)
            : recorders[current];
          const result = step.phase === 'pre_user'
            ? await recorder.capturePreUser(input)
            : await recorder.capturePostAgent(input);
          dynamicIds.add(result.snapshotId);
          done.outcome = {
            snapshotId: result.snapshotId,
            phase: result.phase,
            state: result.state,
            ...(result.manifestPath === undefined ? {} : { manifestPath: maskDir(result.manifestPath) }),
            ...(result.failureMarkerPath === undefined ? {} : { failureMarkerPath: maskDir(result.failureMarkerPath) }),
            ...(result.error === undefined ? {} : { error: maskDir(result.error) }),
          };
          if (step.as) {
            ids.set(step.as, result.snapshotId);
            if (result.failureMarkerPath) markerDirs.set(step.as, dirname(relative(root, result.failureMarkerPath)));
          }
        } catch (error: any) {
          done.outcome = { error: { name: error.name, message: maskDir(error.message) } };
        }
        delete done.as;
        break;
      }
      case 'list': {
        try {
          done.outcome = (await provider().list()).map((d: any) => ({ ...d, manifestPath: relative(root, d.manifestPath).split(sep).join('/') }));
        } catch (error: any) {
          done.outcome = { error: { name: error.name, message: maskDir(error.message) } };
        }
        break;
      }
      case 'get': {
        const snapshotId = step.snapshotId.startsWith('$') ? ids.get(step.snapshotId.slice(1)) : step.snapshotId;
        assert.ok(snapshotId, `unknown id alias ${step.snapshotId}`);
        done.snapshotId = snapshotId;
        try {
          const value = await provider().get(snapshotId);
          done.outcome = { value: value ?? null };
        } catch (error: any) {
          done.outcome = { error: { name: error.name, message: maskDir(error.message) } };
        }
        break;
      }
      case 'restore': {
        const snapshotId = step.snapshotId.startsWith('$') ? ids.get(step.snapshotId.slice(1)) : step.snapshotId;
        assert.ok(snapshotId, `unknown id alias ${step.snapshotId}`);
        done.snapshotId = snapshotId;
        // resolvePath keeps absolute targets absolute; join(dir, absolute)
        // would concatenate them (path.join does not reset on an absolute
        // segment). Every absolute target is still under this run's mkdtemp
        // `<dir>`; NUL cases spell it out because the error text quotes it.
        const target = resolvePath(step.target);
        try {
          const value = await provider().restore(snapshotId, target);
          done.outcome = { restored: value.restored, workspaceKey: maskDir(value.workspaceKey) };
          // Restore targets are user content: never project timestamps there.
          done.targetTree = await recordTree(target, dir, new Set());
        } catch (error: any) {
          done.outcome = { error: { name: error.name, message: maskDir(error.message) } };
          // A failed restore may still have changed the target (rm before a
          // rejected symlink); opt-in scenarios record the tree to prove it.
          if (step.targetTreeOnError) done.targetTree = await recordTree(target, dir, new Set());
        }
        break;
      }
      default:
        throw new Error(`unknown step ${step.op}`);
    }
    recorded.push(done);
  }
  const tree = await recordTree(root, dir, dynamicIds);
  await assertNoTemporaries(dir);
  scenarios.push({
    name,
    snapshotRoot: maskDir(root),
    workspace: {
      files: (spec.files ?? []).map((f) => ({ path: f.path, b64: f.bytes.toString('base64') })),
      symlinks: spec.symlinks ?? [],
      emptyDirs: spec.emptyDirs ?? [],
      rawFiles: (spec.rawFiles ?? []).map((f) => ({ dir: f.dir, nameB64: f.name.toString('base64'), b64: f.bytes.toString('base64') })),
    },
    steps: recorded,
    dynamicIds: [...dynamicIds].sort(),
    tree,
  });
  console.log(`${name}: steps=${recorded.length} tree=${tree.length}`);
  return { name, dir, root, ids, steps: recorded };
}

await mkdir(output, { recursive: true });
// A short per-run mkdtemp under /tmp: the masked `<dir>` text must stay short
// enough for Node's 128-UTF-16-unit ERR_INVALID_ARG_VALUE quoting, and the
// directory is exclusively this run's (never a fixed /tmp/pdrs-r9-* path).
const scratch = await mkdtemp('/tmp/pdrs-r10-');
try {
  // Negative control for the fixed-path P0: sentinels carrying the old fixed
  // names live in this run's own temp root. Nothing in the run may remove
  // them; only per-scenario subtrees are touched.
  const sentinels = ['pdrs-r9-linkout', 'pdrs-r9-path', 'pdrs-r9-target', 'ws-snapshot-scenario'];
  for (const name of sentinels) await writeFile(join(scratch, name), 'keep\n');
  // Non-UTF-8 file names need a byte-transparent filesystem (Linux ext4 and
  // friends); APFS/HFS+ reject the bytes with EINVAL, so the scenario probes
  // first and both this generator and the Rust replay drop it where the name
  // cannot exist. The committed oracle was generated on macOS, where the probe
  // is rejected, so it has no non-utf8 scenario and CI never regenerates the
  // fixtures: this path is currently uncovered. Linux generation is pending
  // #283.
  const nonUtf8Probe = Buffer.concat([Buffer.from(`${scratch}/probe-`), Buffer.from([0xff])]);
  const nonUtf8Names = await writeFile(nonUtf8Probe, '')
    .then(() => removeOwned(scratch, nonUtf8Probe))
    .then(
      () => true,
      () => false,
    );

  // Negative control for the ownership guard (round-4 P1). The legacy bug
  // removed the fixed /tmp/pdrs-r9-linkout path; that target is outside this
  // run's <scratch>, so removeOwned must refuse it before any rm call. A
  // recording stub proves the filesystem was never reached; the real sentinel
  // outside the guarded root proves the refusal kept it alive. Setting
  // PDRS_INJECT_LEGACY_DELETE runs the same injection as an explicit
  // regression probe: the run must abort at the guard.
  const guardRoot = join(scratch, 'guard-root');
  const outsideRoot = join(scratch, 'guard-outside');
  await mkdir(guardRoot, { recursive: true });
  await mkdir(outsideRoot, { recursive: true });
  const outsideSentinel = join(outsideRoot, 'pdrs-r9-linkout');
  await writeFile(outsideSentinel, 'keep\n');
  const legacyPaths = ['/tmp/pdrs-r9-linkout', outsideSentinel];
  const reached: string[] = [];
  const recordingRemove: RemoveImpl = async (path) => {
    reached.push(String(path));
  };
  // 1. The exact historical target goes through a recording stub: the guard
  //    must reject it before any rm call.
  await assert.rejects(
    () => removeOwned(guardRoot, legacyPaths[0], { recursive: true }, recordingRemove),
    /outside the run directory/,
    `legacy delete ${legacyPaths[0]} must be refused`,
  );
  assert.deepEqual(reached, [], 'a refused delete must never reach the filesystem');
  // 2. A real out-of-tree sentinel goes through the real remover: rejected,
  //    and the file is still readable afterwards.
  await assert.rejects(
    () => removeOwned(guardRoot, outsideSentinel, { recursive: true }),
    /outside the run directory/,
    `out-of-tree delete ${outsideSentinel} must be refused`,
  );
  assert.equal(await readFile(outsideSentinel, 'utf8'), 'keep\n', 'out-of-tree sentinel survived');
  if (process.env.PDRS_INJECT_LEGACY_DELETE) {
    console.error(`negative-control: injecting legacy delete ${process.env.PDRS_INJECT_LEGACY_DELETE}`);
    await removeOwned(scratch, process.env.PDRS_INJECT_LEGACY_DELETE, { recursive: true }, recordingRemove);
    assert.fail('negative-control FAIL: the legacy delete was not refused');
  }
  console.log(`negative-control: refused ${legacyPaths.length} out-of-tree deletes; 0 filesystem calls; sentinel kept`);

  // resolveWorkspaceSnapshotConfig env table; cwd is the only machine-specific
  // input and is masked on both sides.
  const configCases: any[] = [];
  for (const env of [
    {},
    { PILOTDECK_STORAGE_ROOT: '   ' },
    { PILOTDECK_STORAGE_ROOT: 'rel/root' },
    { PILOTDECK_STORAGE_ROOT: 'rel/root', PILOTDECK_SNAPSHOT_ROOT: '  ' },
    { PILOTDECK_STORAGE_ROOT: '/abs/root/' },
    { PILOTDECK_STORAGE_ROOT: ' /abs/root ', PILOTDECK_SNAPSHOT_ROOT: '/snap root' },
    { PILOTDECK_SNAPSHOT_ROOT: '/only-snap' },
  ] as Record<string, string | undefined>[]) {
    const value = resolveWorkspaceSnapshotConfig(env);
    configCases.push({
      env,
      value: value === undefined ? null : { root: value.root.split(process.cwd()).join('<cwd>'), snapshotRoot: value.snapshotRoot.split(process.cwd()).join('<cwd>') },
    });
  }

  const big = Buffer.alloc(204800);
  for (let i = 0; i < big.length; i++) big[i] = (i * 31 + (i >> 8)) & 0xff;
  const trickyReason = 'line1\nline2 "quoted" back\\slash 中文 \u2028\u2029 end';
  const b64 = (text: string) => Buffer.from(text).toString('base64');

  const normal = await scenario('normal', {
    workspace: {
      files: [
        { path: 'hello.txt', bytes: Buffer.from('hello workspace\n') },
        { path: 'notes/todo.md', bytes: Buffer.from('- [ ] ship\n') },
        { path: 'notes/深さ.md', bytes: Buffer.from('中文 + émoji 🚀 内容\n') },
        { path: 'data/blob.bin', bytes: big },
        { path: 'data/dup.txt', bytes: Buffer.from('duplicate content\n') },
        { path: 'dup.txt', bytes: Buffer.from('duplicate content\n') },
        { path: 'empty.txt', bytes: Buffer.alloc(0) },
        { path: '.hidden', bytes: Buffer.from('dot file\n') },
        { path: 'sort/0.txt', bytes: Buffer.from('zero\n') },
        { path: 'sort/Z.txt', bytes: Buffer.from('upper\n') },
        { path: 'sort/a.txt', bytes: Buffer.from('lower\n') },
        { path: 'sort/é.txt', bytes: Buffer.from('acute\n') },
        { path: 'sort/😀.txt', bytes: Buffer.from('surrogate pair name\n') },
        { path: 'sort/Ａ.txt', bytes: Buffer.from('fullwidth A\n') },
        { path: 'sort/￠x.txt', bytes: Buffer.from('fullwidth cent name\n') },
      ],
      symlinks: [
        { path: 'link-file', target: 'hello.txt' },
        { path: 'link-dir', target: 'notes' },
        { path: 'broken', target: 'nope/missing' },
      ],
      emptyDirs: ['empty-dir', 'notes/inner-empty'],
    },
    steps: [
      { op: 'tick' },
      {
        op: 'capture',
        as: 'pre',
        phase: 'pre_user',
        input: { workspaceId: 'ws-normal', sessionId: 'sess-1', turnId: 'turn-1', runId: 'run-1', workspaceDir: '<dir>/workspace', workspaceStable: true },
      },
      { op: 'writeFile', path: 'workspace/hello.txt', b64: b64('hello workspace v2\n') },
      { op: 'remove', path: 'workspace/notes/todo.md' },
      { op: 'mkdir', path: 'workspace/gen' },
      { op: 'writeFile', path: 'workspace/gen/new.txt', b64: b64('generated\n') },
      { op: 'tick' },
      {
        op: 'capture',
        as: 'post',
        phase: 'post_agent',
        input: { workspaceId: 'ws-normal', sessionId: 'sess-1', turnId: 'turn-1', runId: 'run-2', workspaceDir: '<dir>/workspace', failureKind: 'agent_error', failureReason: trickyReason },
      },
      // Same key on the same recorder: served from the in-memory completed map.
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'ws-normal', sessionId: 'sess-1', turnId: 'turn-1', runId: 'run-1', workspaceDir: '<dir>/workspace', workspaceStable: true } },
      { op: 'recorder' },
      { op: 'remove', path: 'workspace', recursive: true },
      // _COMMITTED early exit on a fresh recorder, with the workspace gone.
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'ws-normal', sessionId: 'sess-1', turnId: 'turn-1', runId: 'run-1', workspaceDir: '<dir>/workspace', workspaceStable: true } },
      { op: 'list' },
      { op: 'get', snapshotId: '$pre' },
      { op: 'get', snapshotId: 'unknown-id' },
      { op: 'restore', snapshotId: '$pre', target: 'restored' },
      { op: 'restore', snapshotId: 'unknown-id', target: 'restored-x' },
    ],
  });
  {
    const [pre, post, cached, early] = normal.steps.filter((s: any) => s.op === 'capture');
    assert.equal(pre.outcome.state, 'committed');
    assert.equal(post.outcome.state, 'committed');
    assert.equal(cached.outcome.snapshotId, pre.outcome.snapshotId);
    assert.equal(early.outcome.state, 'committed');
    const list = normal.steps.find((s: any) => s.op === 'list')!.outcome;
    assert.deepEqual(list.map((d: any) => d.phase), ['post_agent', 'pre_user'], 'newest createdAt first');
    const manifest = normal.steps.filter((s: any) => s.op === 'get')[0].outcome.value;
    assert.deepEqual(
      manifest.entries.map((e: any) => e.path).slice(0, 5),
      ['.hidden', 'broken', 'data/blob.bin', 'data/dup.txt', 'dup.txt'],
      'UTF-16 sorted scan order',
    );
    const restored = normal.steps.filter((s: any) => s.op === 'restore')[0].targetTree.map((t: any) => t.path);
    assert.ok(restored.includes('hello.txt') && restored.includes('link-file') && restored.includes('sort/😀.txt'));
    const text = await readFile(pre.outcome.manifestPath.split('<dir>').join(normal.dir), 'utf8');
    assert.ok(!text.endsWith('\n'), 'manifest has no trailing newline');
    assert.equal(text, JSON.stringify(manifest, null, 2), 'manifest bytes are JSON.stringify(, 2)');
  }

  const quiescent = await scenario('quiescent', {
    workspace: { files: [{ path: 'one.txt', bytes: Buffer.from('one\n') }] },
    steps: [
      { op: 'tick' },
      { op: 'capture', as: 'pre', phase: 'pre_user', input: { workspaceId: 'ws-q', sessionId: 's-q', turnId: 't-q', runId: 'r-q', workspaceDir: '<dir>/workspace', workspaceStable: false, failureKind: 'interrupted' } },
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'ws-q', sessionId: 's-q', turnId: 't-q', runId: 'r-q', workspaceDir: '<dir>/workspace', workspaceStable: false, failureKind: 'interrupted' } },
      { op: 'recorder' },
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'ws-q', sessionId: 's-q', turnId: 't-q', runId: 'r-q', workspaceDir: '<dir>/workspace', workspaceStable: false, failureKind: 'interrupted' } },
      { op: 'writeSnapshot', markerOf: 'pre', name: '_FAILED.json', text: 'not json' },
      { op: 'recorder' },
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'ws-q', sessionId: 's-q', turnId: 't-q', runId: 'r-q', workspaceDir: '<dir>/workspace', workspaceStable: false, failureKind: 'interrupted' } },
      { op: 'removeSnapshot', markerOf: 'pre', name: '_FAILED.json' },
      // Every live recorder has the failed result cached; a fresh one is needed
      // to prove capture works again once the marker is gone.
      { op: 'recorder' },
      { op: 'tick' },
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'ws-q', sessionId: 's-q', turnId: 't-q', runId: 'r-q', workspaceDir: '<dir>/workspace' } },
      { op: 'list' },
    ],
  });
  {
    const [fail, cached, marker, corrupt, recovered] = quiescent.steps.filter((s: any) => s.op === 'capture');
    assert.equal(fail.outcome.error, 'Workspace did not become quiescent before snapshot capture.');
    assert.equal(fail.outcome.state, 'failed');
    assert.equal(cached.outcome.snapshotId, fail.outcome.snapshotId);
    assert.equal(marker.outcome.error, fail.outcome.error, 'marker early exit reads the error back');
    assert.equal(corrupt.outcome.error, undefined, 'unreadable marker yields no error');
    assert.equal(recovered.outcome.state, 'committed', 'capture works again once the marker is gone');
  }

  const failureFields = await scenario('failure-fields', {
    workspace: { files: [{ path: 'a.txt', bytes: Buffer.from('a\n') }] },
    steps: [
      { op: 'tick' },
      { op: 'capture', as: 'post', phase: 'post_agent', input: { workspaceId: 'ws-f', sessionId: 's-f', turnId: 't-f1', runId: 'r-f1', workspaceDir: '<dir>/workspace', failureKind: 'gateway_error', failureReason: trickyReason } },
      { op: 'tick' },
      { op: 'capture', phase: 'post_agent', input: { workspaceId: 'ws-f', sessionId: 's-f', turnId: 't-f2', runId: 'r-f2', workspaceDir: '<dir>/workspace', workspaceStable: false, failureKind: 'unknown' } },
      { op: 'get', snapshotId: '$post' },
    ],
  });
  {
    const manifest = failureFields.steps.find((s: any) => s.op === 'get')!.outcome.value;
    assert.equal(manifest.abnormal, true);
    assert.equal(manifest.failureReason, trickyReason);
    const markerPath = failureFields.steps.filter((s: any) => s.op === 'capture')[1].outcome.failureMarkerPath.split('<dir>').join(failureFields.dir);
    const marker = JSON.parse(await readFile(markerPath, 'utf8'));
    assert.equal(marker.failureKind, 'unknown');
    assert.ok(marker.failureReason === undefined, 'undefined failure fields drop out of the marker');
  }

  // Negative control for the masking scope: ISO-looking dates in
  // failureReason, entries paths, linkTarget and a user `.json` file must
  // survive projection; only the recorder's own createdAt is run-varying.
  const maskScopeUserJson = Buffer.from(
    '{"createdAt":"2021-02-03T04:05:06.007Z","note":"2022-03-04T05:06:07.008Z"}\n',
  );
  const maskScope = await scenario('mask-scope', {
    workspace: {
      files: [
        { path: 'user.json', bytes: maskScopeUserJson },
        { path: 'dated-2023-04-05T06:07:08.009Z.txt', bytes: Buffer.from('dated path\n') },
      ],
      symlinks: [{ path: 'link', target: 'target-2022-03-04T05:06:07.008Z' }],
    },
    steps: [
      { op: 'tick' },
      {
        op: 'capture',
        as: 'pre',
        phase: 'pre_user',
        input: {
          workspaceId: 'ws-mask',
          sessionId: 's-mask',
          turnId: 't-mask',
          runId: 'r-mask',
          workspaceDir: '<dir>/workspace',
          failureKind: 'unknown',
          failureReason: 'failed 2024-05-06T07:08:09.010Z',
        },
      },
      { op: 'get', snapshotId: '$pre' },
      { op: 'restore', snapshotId: '$pre', target: 'restored' },
    ],
  });
  {
    const capture = maskScope.steps.find((s: any) => s.op === 'capture')!;
    const manifestPath = capture.outcome.manifestPath.split('<dir>').join(maskScope.dir);
    const rel = relative(maskScope.root, manifestPath).split(sep).join('/');
    const raw = await readFile(manifestPath);
    const projected = projectArtifact(rel, raw, maskScope.dir, new Set([capture.outcome.snapshotId]));
    const text = projected.toString('utf8');
    assert.ok(text.includes('"createdAt": "<ts>"'), 'recorder createdAt is the masked field');
    for (const date of [
      '2024-05-06T07:08:09.010Z',
      '2023-04-05T06:07:08.009Z',
      '2022-03-04T05:06:07.008Z',
    ]) {
      assert.ok(text.includes(date), `non-createdAt date must stay raw: ${date}`);
    }
    assert.notEqual(projected.toString('hex'), raw.toString('hex'), 'projection changes only createdAt');
    const restored = maskScope.steps.find((s: any) => s.op === 'restore')!;
    const userRow = restored.targetTree.find((row: any) => row.path === 'user.json');
    assert.equal(userRow.size, maskScopeUserJson.length, 'restored user .json keeps raw size');
    assert.equal(
      userRow.sha256,
      createHash('sha256').update(maskScopeUserJson).digest('hex'),
      'restored user .json keeps raw bytes',
    );
  }

  const identitySanitize = await scenario('identity-sanitize', {
    workspace: { files: [{ path: 'x.txt', bytes: Buffer.from('x\n') }] },
    steps: [
      { op: 'tick' },
      { op: 'capture', as: 'pre', phase: 'pre_user', input: { workspaceId: 'My Project!!', sessionId: 'séance 中文 2026', turnId: 'turn--x--', runId: 'r', workspaceDir: '<dir>/workspace' } },
      { op: 'tick' },
      { op: 'capture', phase: 'post_agent', input: { workspaceId: 'My Project!!', sessionId: 'séance 中文 2026', turnId: '...', runId: 'r2', workspaceDir: '<dir>/workspace' } },
      { op: 'list' },
    ],
  });
  {
    const first = identitySanitize.steps.find((s: any) => s.op === 'capture')!.outcome;
    assert.ok(first.manifestPath.includes('workspaces/My-Project/sessions/s-ance-2026/'), `sanitized partition path: ${first.manifestPath}`);
    const list = identitySanitize.steps.find((s: any) => s.op === 'list')!.outcome;
    assert.equal(list[0].workspaceId, 'My Project!!', 'descriptor keeps the raw identity');
  }

  const identityInvalid = await scenario('identity-invalid', {
    workspace: { files: [{ path: 'x.txt', bytes: Buffer.from('x\n') }] },
    steps: [
      { op: 'capture', phase: 'pre_user', input: { workspaceId: '../esc', sessionId: 's', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace' } },
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'w', sessionId: '中文', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace' } },
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'w', sessionId: ' ', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace' } },
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'w', sessionId: 'a\0b', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace' } },
      // Same key as the next call: a hard rejection is not cached, so the
      // caller arriving after the in-flight round settled retries.
      { op: 'capture', phase: 'pre_user', input: { workspaceId: '..', sessionId: 's-retry', turnId: 't', runId: 'r1', workspaceDir: '<dir>/workspace' } },
      { op: 'capture', as: 'retry', phase: 'pre_user', input: { workspaceId: 'w', sessionId: 's-retry', turnId: 't', runId: 'r2', workspaceDir: '<dir>/workspace' } },
    ],
  });
  {
    const captures = identityInvalid.steps.filter((s: any) => s.op === 'capture');
    for (const step of captures.slice(0, 4)) assert.deepEqual(step.outcome.error, { name: 'Error', message: 'Invalid snapshot identity.' });
    assert.equal(captures[4].outcome.error.message, 'Invalid snapshot identity.');
    assert.equal(captures[5].outcome.state, 'committed', 'a new call retries after the rejection settles');
    const recorded = scenarios.find((s: any) => s.name === 'identity-invalid')!;
    assert.equal(recorded.tree.length > 0, true, 'the retry commits one snapshot');
  }

  // Concurrency self-check (the deterministic Rust unit test
  // `hard_failure_is_shared_with_waiters_and_not_cached` covers the same
  // semantics): a hard identity rejection of the first call is the same
  // promise rejection the second, already in-flight call receives; a call
  // made after the round settled retries and commits.
  {
    const concurrentWorkspace = join(scratch, 'concurrent-workspace');
    await mkdir(concurrentWorkspace, { recursive: true });
    await writeFile(join(concurrentWorkspace, 'x.txt'), 'x\n');
    const concurrentRoot = join(scratch, 'concurrent-snapshots');
    const concurrentRecorder = new ContentAddressedWorkspaceSnapshotRecorder({ root: concurrentRoot, snapshotRoot: concurrentRoot });
    const concurrentInput = (workspaceId: string) => ({
      workspaceId,
      sessionId: 's-concurrent',
      turnId: 't',
      runId: 'r',
      workspaceDir: concurrentWorkspace,
    });
    const settled = await Promise.allSettled([
      concurrentRecorder.capturePreUser(concurrentInput('..')),
      concurrentRecorder.capturePreUser(concurrentInput('w')),
    ]);
    const rejected = settled.map((result) => (result.status === 'rejected' ? result.reason.message : `fulfilled:${result.value.state}`));
    assert.deepEqual(rejected, ['Invalid snapshot identity.', 'Invalid snapshot identity.'], 'both waiters share the rejection');
    const retried = await concurrentRecorder.capturePreUser(concurrentInput('w'));
    assert.equal(retried.state, 'committed', 'a later call retries after the in-flight round settles');
  }

  const snapshotInside = await scenario('snapshot-inside', {
    snapshotRoot: 'workspace/.snap',
    workspace: {
      files: [
        { path: 'keep.txt', bytes: Buffer.from('keep\n') },
        { path: '.snap-other/keep.txt', bytes: Buffer.from('sibling of the snapshot root\n') },
        { path: 'plain/dir/file.txt', bytes: Buffer.from('plain\n') },
      ],
    },
    steps: [
      { op: 'tick' },
      { op: 'capture', as: 'pre', phase: 'pre_user', input: { workspaceId: 'w', sessionId: 's', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace' } },
      { op: 'list' },
    ],
  });
  {
    const capture = snapshotInside.steps.find((s: any) => s.op === 'capture')!;
    const manifest = await readFile(capture.outcome.manifestPath.split('<dir>').join(snapshotInside.dir), 'utf8');
    assert.deepEqual(
      JSON.parse(manifest).entries.map((e: any) => e.path),
      ['.snap-other/keep.txt', 'keep.txt', 'plain/dir/file.txt'],
      'the snapshot subtree is excluded, similar prefixes are not',
    );
    assert.equal(snapshotInside.steps.find((s: any) => s.op === 'list')!.outcome.length, 1);
  }

  const workspaceMissing = await scenario('workspace-missing', {
    workspace: {},
    steps: [
      { op: 'tick' },
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'w', sessionId: 's', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace-gone' } },
    ],
  });
  {
    // workspaceSnapshot.ts:292: readdir of the missing workspace throws inside
    // the capture try block, so TS reports it through the failed result of
    // :260-280 (state 'failed' + error string + _FAILED.json), not by throwing.
    const capture = workspaceMissing.steps.find((s: any) => s.op === 'capture')!.outcome;
    assert.equal(capture.state, 'failed');
    assert.ok(capture.failureMarkerPath, 'scandir failure lands a failure marker');
    assert.match(capture.error, /^ENOENT: no such file or directory, scandir '<dir>\/workspace-gone'$/);
    const marker = JSON.parse(await readFile(capture.failureMarkerPath.split('<dir>').join(workspaceMissing.dir), 'utf8'));
    assert.equal(marker.state, 'failed');
    assert.match(marker.error.split(workspaceMissing.dir).join('<dir>'), /^ENOENT: no such file or directory, scandir '<dir>\/workspace-gone'$/);
  }

  const rootEquals = await scenario('root-equals-workspace', {
    snapshotRoot: 'workspace',
    workspace: { files: [{ path: 'x.txt', bytes: Buffer.from('x\n') }] },
    steps: [
      { op: 'tick' },
      { op: 'capture', phase: 'pre_user', input: { workspaceId: 'w', sessionId: 's', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace' } },
      { op: 'list' },
    ],
  });
  {
    assert.equal(rootEquals.steps.find((s: any) => s.op === 'capture')!.outcome.error, 'Snapshot root must not be the workspace root.');
    assert.deepEqual(rootEquals.steps.find((s: any) => s.op === 'list')!.outcome, [], 'only the failure marker exists');
  }

  const nonUtf8 = nonUtf8Names
    ? await scenario('non-utf8', {
        workspace: {
          files: [{ path: 'ok.txt', bytes: Buffer.from('ok\n') }],
          rawFiles: [{ dir: 'workspace', name: Buffer.concat([Buffer.from('bad-'), Buffer.from([0xff]), Buffer.from('.txt')]), bytes: Buffer.from('raw name\n') }],
        },
        steps: [
          { op: 'tick' },
          { op: 'capture', phase: 'pre_user', input: { workspaceId: 'w', sessionId: 's', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace' } },
        ],
      })
    : null;
  if (nonUtf8) {
    // workspaceSnapshot.ts:300: lstat of the readdir-mangled name throws inside
    // the capture try block — the same failed-result shape as workspace-missing.
    const capture = nonUtf8.steps.find((s: any) => s.op === 'capture')!.outcome;
    assert.equal(capture.state, 'failed');
    assert.ok(capture.failureMarkerPath, 'lstat failure lands a failure marker');
    assert.match(capture.error, /^ENOENT: no such file or directory, lstat '<dir>\/workspace\/bad-\uFFFD\.txt'$/);
  } else {
    console.log('non-utf8: skipped (filesystem rejects non-UTF-8 file names)');
  }

  // Hand-planted manifests exercise the provider walk against shapes the
  // recorder never writes. Throwing manifests get their own root so list/get
  // outcomes do not depend on readdir order.
  const quirkBytes = Buffer.from('quirk-object\n');
  const quirkDigest = createHash('sha256').update(quirkBytes).digest('hex');
  const quirkObjectKey = `objects/sha256/${quirkDigest.slice(0, 2)}/${quirkDigest.slice(2, 4)}/${quirkDigest}`;
  const quirkBase = 'workspaces/quirk/sessions/quirk/snapshots';
  const providerQuirks = await scenario('provider-quirks', {
    workspace: {},
    steps: [
      { op: 'writeSnapshot', path: quirkObjectKey, text: 'quirk-object\n' },
      {
        op: 'writeSnapshot',
        path: `${quirkBase}/manual-full/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'full-1',
            workspaceId: 'quirk-ws',
            sessionId: 'quirk-sess',
            turnId: 'quirk-turn',
            runId: 'quirk-run',
            phase: 'pre_user',
            abnormal: false,
            entries: [
              { path: 'nested/dir/quirk.txt', entryType: 'file', size: quirkBytes.length, digest: quirkDigest, objectKey: quirkObjectKey },
              { path: 'no-object.txt', entryType: 'file', size: 5 },
              { path: 'other-type.bin', entryType: 'other', size: 7 },
              { path: 'sym', entryType: 'symlink', size: 0, linkTarget: '../outside.txt' },
            ],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-full/_COMMITTED`, text: '' },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-num/manifest.json`, text: '5' },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-missing/manifest.json`, text: JSON.stringify({ snapshotId: 'missing-1', workspaceId: 'w', sessionId: 's', turnId: 't', runId: 'r', phase: 'bogus', entries: [] }) },
      {
        op: 'writeSnapshot',
        path: `${quirkBase}/manual-nocreated/manifest.json`,
        text: JSON.stringify({ snapshotId: 'nocreated-1', workspaceId: 'w2', sessionId: 's2', turnId: 't2', runId: 'r2', phase: 'post_agent', abnormal: true, entries: [], createdAt: 5 }),
      },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-badnull/manifest.json`, text: JSON.stringify({ snapshotId: 'badnull-1', entries: [null] }) },
      // workspaceSnapshot.ts:115 tests Array.isArray first: a non-array
      // `entries` is a deliberate empty list, not a TypeError. Measured against
      // the real TS provider (probe-r11-entries.log): 5/{}/"x"/null all restore
      // with `restored:true` and an empty target tree.
      { op: 'writeSnapshot', path: `${quirkBase}/manual-entries-number/manifest.json`, text: JSON.stringify({ snapshotId: 'entries-5', entries: 5 }) },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-entries-object/manifest.json`, text: JSON.stringify({ snapshotId: 'entries-object', entries: {} }) },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-entries-string/manifest.json`, text: JSON.stringify({ snapshotId: 'entries-string', entries: 'x' }) },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-entries-null/manifest.json`, text: JSON.stringify({ snapshotId: 'entries-null', entries: null }) },
      // workspaceSnapshot.ts:125/128 guards the type-specific field inside the
      // same condition as entryType: an entry that names the type but omits
      // linkTarget/objectKey is deliberately skipped, and restore still
      // resolves `{restored:true}`. Measured against the real provider
      // (probe-r13-missing-fields.log): both cases succeed with no output.
      { op: 'writeSnapshot', path: `${quirkBase}/manual-link-missing/manifest.json`, text: JSON.stringify({ snapshotId: 'link-missing-1', entries: [{ path: 'link-only', entryType: 'symlink' }] }) },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-file-missing/manifest.json`, text: JSON.stringify({ snapshotId: 'file-missing-1', entries: [{ path: 'file-only', entryType: 'file' }] }) },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-abs/manifest.json`, text: JSON.stringify({ snapshotId: 'abs-1', entries: [{ path: '/etc/passwd', entryType: 'file' }] }) },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-esc/manifest.json`, text: JSON.stringify({ snapshotId: 'esc-1', entries: [{ path: 'a/../../esc.txt', entryType: 'file' }] }) },
      { op: 'writeSnapshot', path: `${quirkBase}/manual-dupkey/manifest.json`, text: '{"snapshotId":"ignored","snapshotId":"dupkey-1","phase":"pre_user"}' },
      { op: 'mkdirSnapshot', path: `${quirkBase}/manual-dirm/manifest.json` },
      { op: 'mkdirSnapshot', path: 'objects/sha256/de/ad/directory-object' },
      {
        op: 'writeSnapshot',
        path: `${quirkBase}/manual-objectdir/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'objdir-1',
            entries: [
              { path: 'from-dir.txt', entryType: 'file', objectKey: 'objects/sha256/de/ad/directory-object' },
            ],
          },
          null,
          2,
        ),
      },
      { op: 'list' },
      { op: 'get', snapshotId: 'full-1' },
      { op: 'get', snapshotId: 'missing-1' },
      { op: 'get', snapshotId: 'dupkey-1' },
      { op: 'get', snapshotId: 'nope' },
      { op: 'restore', snapshotId: 'full-1', target: 'quirk-restored' },
      { op: 'restore', snapshotId: 'badnull-1', target: 'quirk-x1' },
      { op: 'restore', snapshotId: 'abs-1', target: 'quirk-x2' },
      { op: 'restore', snapshotId: 'esc-1', target: 'quirk-x3' },
      { op: 'restore', snapshotId: 'objdir-1', target: 'quirk-x4' },
      { op: 'restore', snapshotId: 'entries-5', target: 'quirk-e5' },
      { op: 'restore', snapshotId: 'entries-object', target: 'quirk-eobj' },
      { op: 'restore', snapshotId: 'entries-string', target: 'quirk-estr' },
      { op: 'restore', snapshotId: 'entries-null', target: 'quirk-enull' },
      { op: 'restore', snapshotId: 'link-missing-1', target: 'quirk-link-missing' },
      { op: 'restore', snapshotId: 'file-missing-1', target: 'quirk-file-missing' },
      { op: 'get', snapshotId: 'entries-5' },
    ],
  });
  {
    const list = providerQuirks.steps.find((s: any) => s.op === 'list')!.outcome;
    assert.deepEqual(list.map((d: any) => d.snapshotId), ['full-1', 'nocreated-1'], 'invalid descriptors skip, createdAt-less sorts last');
    assert.ok(!('createdAt' in list[1]), 'non-string createdAt drops out of the descriptor');
    const restores = providerQuirks.steps.filter((s: any) => s.op === 'restore');
    // Restore mkdirs each entry's parent, so the tree carries the created
    // directories alongside the actionable file and symlink entries.
    assert.deepEqual(restores[0].targetTree.map((t: any) => t.path).sort(), ['nested', 'nested/dir', 'nested/dir/quirk.txt', 'sym'], 'only actionable entries restore');
    assert.equal(restores[0].targetTree.find((t: any) => t.path === 'sym').target, '../outside.txt');
    assert.deepEqual(restores[1].outcome.error, { name: 'TypeError', message: "Cannot read properties of null (reading 'path')" });
    assert.equal(restores[2].outcome.error.message, 'Snapshot contains an unsafe relative path.');
    assert.equal(restores[3].outcome.error.message, 'Snapshot contains an unsafe relative path.');
    // Node readFile opens a directory and fails on read: EISDIR without a path.
    assert.deepEqual(restores[4].outcome.error, { name: 'Error', message: 'EISDIR: illegal operation on a directory, read' });
    // Non-array `entries` restores as an empty snapshot, exactly as the real TS
    // provider does (workspaceSnapshot.ts:115).
    for (const [index, target] of ['quirk-e5', 'quirk-eobj', 'quirk-estr', 'quirk-enull'].entries()) {
      assert.equal(restores[5 + index].outcome.restored, true, `${target} restores`);
      assert.equal(restores[5 + index].outcome.workspaceKey, `<dir>/${target}`);
      assert.deepEqual(restores[5 + index].targetTree, [], `${target} has no entries to restore`);
    }
    // Missing type-specific field: the real TS provider skips the entry and
    // still returns restored:true (probe-r13-missing-fields.log), so the
    // oracle must keep that outcome instead of a fabricated TypeError.
    for (const [id, target] of [
      ['link-missing-1', 'quirk-link-missing'],
      ['file-missing-1', 'quirk-file-missing'],
    ] as const) {
      const step = providerQuirks.steps.find((s: any) => s.op === 'restore' && s.snapshotId === id)!;
      assert.equal(step.outcome.restored, true, `${id} restores`);
      assert.equal(step.outcome.workspaceKey, `<dir>/${target}`);
      assert.deepEqual(step.targetTree, [], `${id} skips the field-less entry`);
    }
    const entriesNumber = providerQuirks.steps.filter((s: any) => s.op === 'get').at(-1)!.outcome.value;
    assert.equal(entriesNumber.entries, 5, 'get returns the non-array entries value verbatim');
    const manualFullRel = `${quirkBase}/manual-full/manifest.json`;
    const manualFull = await readFile(join(providerQuirks.root, manualFullRel));
    assert.ok(manualFull.toString().includes('2020-01-01T00:00:00.000Z'), 'hand-planted fixed date is compared raw');
    assert.deepEqual(
      projectArtifact(manualFullRel, manualFull, providerQuirks.dir, new Set()),
      manualFull,
      'hand-planted manifests are never projected',
    );
  }

  const providerNull = await scenario('provider-null', {
    workspace: {},
    steps: [
      { op: 'writeSnapshot', path: `${quirkBase}/manual-null/manifest.json`, text: 'null' },
      { op: 'list' },
      { op: 'get', snapshotId: 'any' },
    ],
  });
  {
    for (const step of providerNull.steps.slice(1)) {
      assert.deepEqual(step.outcome.error, { name: 'TypeError', message: "Cannot read properties of null (reading 'snapshotId')" });
    }
  }

  // Provider walk order is Node readdir (libuv byte) order: a duplicated
  // snapshotId resolves to the last manifest seen, and list's stable
  // localeCompare sort keeps readdir order for equal createdAt values. The
  // arbitrary strings "a"/"Z"/"same" also pin ICU collation (Z before a).
  const orderBase = 'workspaces/order/sessions/order/snapshots';
  const orderManifest = (snapshotId: string, runId: string, createdAt: string) =>
    JSON.stringify(
      {
        snapshotId,
        workspaceId: 'order-ws',
        sessionId: 'order-sess',
        turnId: 'order-turn',
        runId,
        phase: 'pre_user',
        entries: [],
        createdAt,
      },
      null,
      2,
    );
  const providerOrder = await scenario('provider-order', {
    workspace: {},
    steps: [
      { op: 'writeSnapshot', path: `${orderBase}/a-dup-first/manifest.json`, text: orderManifest('dup', 'first', 'm') },
      { op: 'writeSnapshot', path: `${orderBase}/b-equal/manifest.json`, text: orderManifest('equal-b', 'b', 'same') },
      { op: 'writeSnapshot', path: `${orderBase}/c-equal/manifest.json`, text: orderManifest('equal-c', 'c', 'same') },
      { op: 'writeSnapshot', path: `${orderBase}/d-lower/manifest.json`, text: orderManifest('lower-a', 'd', 'a') },
      { op: 'writeSnapshot', path: `${orderBase}/e-upper/manifest.json`, text: orderManifest('upper-Z', 'e', 'Z') },
      { op: 'writeSnapshot', path: `${orderBase}/z-dup-last/manifest.json`, text: orderManifest('dup', 'last', 'n') },
      { op: 'list' },
      { op: 'get', snapshotId: 'dup' },
    ],
  });
  {
    const list = providerOrder.steps.find((s: any) => s.op === 'list')!.outcome;
    assert.deepEqual(
      list.map((d: any) => d.snapshotId),
      ['upper-Z', 'equal-b', 'equal-c', 'dup', 'dup', 'lower-a'],
      'localeCompare order and stable ties',
    );
    assert.deepEqual(list.map((d: any) => d.createdAt), ['Z', 'same', 'same', 'n', 'm', 'a']);
    const got = providerOrder.steps.find((s: any) => s.op === 'get')!.outcome.value;
    assert.equal(got.runId, 'last', 'duplicate snapshotId resolves to the last readdir entry');
  }

  // Manifest JSON syntax errors use V8's SyntaxError wording (shared with the
  // CLI's recorded json-syntax fixture), not the Rust codec's message.
  const manifestSyntax = await scenario('manifest-syntax', {
    workspace: {},
    steps: [
      { op: 'writeSnapshot', path: `${quirkBase}/manual-corrupt/manifest.json`, text: 'not json' },
      { op: 'list' },
      { op: 'get', snapshotId: 'any' },
    ],
  });
  {
    const expected = { name: 'SyntaxError', message: 'Unexpected token \'o\', "not json" is not valid JSON' };
    for (const step of manifestSyntax.steps.slice(1)) assert.deepEqual(step.outcome.error, expected);
  }

  // A manifest.json that is a symlink to a directory: Node opens it, then
  // readFile fails EISDIR on the descriptor (no path in the message).
  const manifestReadDir = await scenario('manifest-read-dir', {
    workspace: {},
    steps: [
      { op: 'mkdirSnapshot', path: `${quirkBase}/target-dir` },
      { op: 'symlinkSnapshot', path: `${quirkBase}/manual-dirlink/manifest.json`, target: '../target-dir' },
      { op: 'list' },
    ],
  });
  {
    assert.deepEqual(manifestReadDir.steps[2].outcome.error, {
      name: 'Error',
      message: 'EISDIR: illegal operation on a directory, read',
    });
  }

  // A NUL byte in workspaceDir is rejected by Node fs.validatePath before the
  // syscall; the capture catch records the TypeError message in the marker.
  const nulWorkspace = await scenario('nul-workspace-dir', {
    workspace: { files: [{ path: 'x.txt', bytes: Buffer.from('x\n') }] },
    steps: [
      { op: 'tick' },
      {
        op: 'capture',
        phase: 'pre_user',
        input: { workspaceId: 'w', sessionId: 's', turnId: 't', runId: 'r', workspaceDir: '<dir>/nul-workspace\u0000' },
      },
    ],
  });
  {
    const capture = nulWorkspace.steps.find((s: any) => s.op === 'capture')!.outcome;
    assert.equal(capture.state, 'failed');
    assert.match(
      capture.error,
      /^The argument 'path' must be a string, Uint8Array, or URL without null bytes\. Received '<dir>\/nul-workspace\\x00'$/,
    );
  }

  // Lone surrogates written by someone else on disk: list descriptors keep
  // the raw UTF-16 units, and a marker's `error` reads back unchanged.
  const surrogateReadback = await scenario('surrogate-readback', {
    workspace: { files: [{ path: 'x.txt', bytes: Buffer.from('x\n') }] },
    steps: [
      { op: 'tick' },
      {
        op: 'capture',
        as: 'fail',
        phase: 'pre_user',
        input: { workspaceId: 'ws-sur', sessionId: 's-sur', turnId: 't-sur', runId: 'r-sur', workspaceDir: '<dir>/workspace', workspaceStable: false },
      },
      {
        op: 'writeSnapshot',
        markerOf: 'fail',
        name: '_FAILED.json',
        text: JSON.stringify(
          {
            snapshotId: 'overwritten-marker',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            state: 'failed',
            error: 'err\ud800',
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      // The first recorder cached the failed result; a fresh one reads the
      // marker back through readFailureMessage.
      { op: 'recorder' },
      {
        op: 'capture',
        phase: 'pre_user',
        input: { workspaceId: 'ws-sur', sessionId: 's-sur', turnId: 't-sur', runId: 'r-sur', workspaceDir: '<dir>/workspace', workspaceStable: false },
      },
      {
        op: 'writeSnapshot',
        path: `${quirkBase}/surrogate-identity/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'id\ud800',
            workspaceId: 'ws\ud800',
            sessionId: 'sess\ud801',
            turnId: 'turn\ud802',
            runId: 'run\ud803',
            phase: 'pre_user',
            entries: [],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'list' },
    ],
  });
  {
    const captures = surrogateReadback.steps.filter((s: any) => s.op === 'capture');
    assert.equal(captures[0].outcome.state, 'failed');
    assert.equal(captures[1].outcome.error, 'err\ud800', 'marker error keeps its lone surrogate');
    assert.equal(JSON.stringify(captures[1].outcome.error), '"err\\ud800"');
    const list = surrogateReadback.steps.find((s: any) => s.op === 'list')!.outcome;
    const descriptor = list.find((d: any) => d.snapshotId === 'id\ud800');
    assert.ok(descriptor, 'lone-surrogate identity lists');
    for (const [field, expected] of [
      ['workspaceId', 'ws\ud800'],
      ['sessionId', 'sess\ud801'],
      ['turnId', 'turn\ud802'],
      ['runId', 'run\ud803'],
    ] as const) {
      assert.equal(descriptor[field], expected, `${field} keeps its lone surrogate`);
      assert.equal(JSON.stringify(descriptor[field]), JSON.stringify(expected));
    }
  }

  // NUL paths outside workspaceDir: Node fs.validatePath rejects the first
  // syscall with a TypeError naming that call's parameter. These are the
  // restore/capture entries the first review's error-text fix missed.
  const nulPathBase = `${quirkBase}`;
  const nulRestoreTarget = await scenario('nul-restore-target', {
    workspace: { files: [{ path: 'x.txt', bytes: Buffer.from('x\n') }] },
    steps: [
      { op: 'tick' },
      { op: 'capture', as: 'pre', phase: 'pre_user', input: { workspaceId: 'w', sessionId: 's-nul-target', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace' } },
      { op: 'restore', snapshotId: '$pre', target: '<dir>/nul-target\u0000root' },
    ],
  });
  {
    const outcome = nulRestoreTarget.steps.find((s: any) => s.op === 'restore')!.outcome;
    assert.deepEqual(outcome.error, {
      name: 'TypeError',
      message: "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '<dir>/nul-target\\x00root'",
    });
  }

  const nulRestoreObject = await scenario('nul-restore-object', {
    workspace: {},
    steps: [
      {
        op: 'writeSnapshot',
        path: `${nulPathBase}/nul-object/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'nul-object',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            entries: [{ path: 'out.txt', entryType: 'file', objectKey: 'nul-object\u0000key' }],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'restore', snapshotId: 'nul-object', target: 'restore-object' },
    ],
  });
  {
    const outcome = nulRestoreObject.steps.find((s: any) => s.op === 'restore')!.outcome;
    assert.deepEqual(outcome.error, {
      name: 'TypeError',
      message: "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '<dir>/snapshots/nul-object\\x00key'",
    });
  }

  const nulRestoreLink = await scenario('nul-restore-link', {
    workspace: {},
    steps: [
      {
        op: 'writeSnapshot',
        path: `${nulPathBase}/nul-link/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'nul-link',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            entries: [{ path: 'link', entryType: 'symlink', linkTarget: 'tar\u0000get' }],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'restore', snapshotId: 'nul-link', target: 'restore-link' },
    ],
  });
  {
    const outcome = nulRestoreLink.steps.find((s: any) => s.op === 'restore')!.outcome;
    assert.deepEqual(outcome.error, {
      name: 'TypeError',
      message: "The argument 'target' must be a string, Uint8Array, or URL without null bytes. Received 'tar\\x00get'",
    });
  }

  const nulRestoreLinkOutput = await scenario('nul-restore-link-output', {
    workspace: {},
    steps: [
      {
        op: 'writeSnapshot',
        path: `${nulPathBase}/nul-link-out/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'nul-link-out',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            entries: [{ path: 'a\u0000b', entryType: 'symlink', linkTarget: 'ok-target' }],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'restore', snapshotId: 'nul-link-out', target: '<dir>/nul-linkout' },
    ],
  });
  {
    const outcome = nulRestoreLinkOutput.steps.find((s: any) => s.op === 'restore')!.outcome;
    assert.deepEqual(outcome.error, {
      name: 'TypeError',
      message: "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '<dir>/nul-linkout/a\\x00b'",
    });
  }

  const nulRestorePath = await scenario('nul-restore-path', {
    workspace: {},
    steps: [
      { op: 'writeSnapshot', path: 'objects/sha256/00/00/nul-path-object', text: 'ok\n' },
      {
        op: 'writeSnapshot',
        path: `${nulPathBase}/nul-path/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'nul-path',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            entries: [{ path: 'a\u0000b.txt', entryType: 'file', objectKey: 'objects/sha256/00/00/nul-path-object' }],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'restore', snapshotId: 'nul-path', target: '<dir>/nul-path' },
    ],
  });
  {
    const outcome = nulRestorePath.steps.find((s: any) => s.op === 'restore')!.outcome;
    assert.deepEqual(outcome.error, {
      name: 'TypeError',
      message: "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '<dir>/nul-path/a\\x00b.txt'",
    });
  }

  // P1 round 3: TS runs `rm(output, { force: true })` before `symlink`
  // validates `target`, so a pre-existing output file is gone even though the
  // call rejects. The target directory also holds a pre-existing sentinel: the
  // restore may remove only the entry's own output, never the directory.
  const nulRestoreLinkExisting = await scenario('nul-restore-link-existing', {
    workspace: {},
    steps: [
      { op: 'writeFile', path: 'restore-link-existing/link', b64: b64('old file\n') },
      { op: 'writeFile', path: 'restore-link-existing/sentinel', b64: b64('keep\n') },
      {
        op: 'writeSnapshot',
        path: `${nulPathBase}/nul-link-existing/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'nul-link-existing',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            entries: [{ path: 'link', entryType: 'symlink', linkTarget: 'tar\u0000get' }],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'restore', snapshotId: 'nul-link-existing', target: 'restore-link-existing', targetTreeOnError: true },
    ],
  });
  {
    const step = nulRestoreLinkExisting.steps.find((s: any) => s.op === 'restore')!;
    assert.deepEqual(step.outcome.error, {
      name: 'TypeError',
      message: "The argument 'target' must be a string, Uint8Array, or URL without null bytes. Received 'tar\\x00get'",
    });
    assert.deepEqual(
      step.targetTree.map((row: any) => row.path),
      ['sentinel'],
      'rm(output) ran before the rejected symlink; the pre-existing sentinel survives',
    );
    assert.equal(step.targetTree[0].size, 5);
  }

  // P1 round 3: a lone surrogate next to the NUL must stay in the quoted
  // argument. Node reports `tar\ud800\x00get`, not `tar\uFFFD\x00get`.
  const nulLinkTargetSurrogate = await scenario('nul-link-target-surrogate', {
    workspace: {},
    steps: [
      {
        op: 'writeSnapshot',
        path: `${nulPathBase}/nul-link-surrogate/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'nul-link-surrogate',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            entries: [{ path: 'link', entryType: 'symlink', linkTarget: 'tar\ud800\u0000get' }],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'restore', snapshotId: 'nul-link-surrogate', target: 'restore-link-surrogate' },
    ],
  });
  {
    const outcome = nulLinkTargetSurrogate.steps.find((s: any) => s.op === 'restore')!.outcome;
    assert.deepEqual(outcome.error, {
      name: 'TypeError',
      message: "The argument 'target' must be a string, Uint8Array, or URL without null bytes. Received 'tar\\ud800\\x00get'",
    });
  }

  const nulOutputSurrogate = await scenario('nul-output-surrogate', {
    workspace: {},
    steps: [
      {
        op: 'writeSnapshot',
        path: `${nulPathBase}/nul-output-surrogate/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'nul-output-surrogate',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            entries: [{ path: 'a\ud800\u0000b', entryType: 'symlink', linkTarget: 'ok-target' }],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'restore', snapshotId: 'nul-output-surrogate', target: 'restore-output-surrogate' },
    ],
  });
  {
    const outcome = nulOutputSurrogate.steps.find((s: any) => s.op === 'restore')!.outcome;
    assert.deepEqual(outcome.error, {
      name: 'TypeError',
      message: "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '<dir>/restore-output-surrogate/a\\ud800\\x00b'",
    });
  }

  const nulObjectSurrogate = await scenario('nul-object-surrogate', {
    workspace: {},
    steps: [
      {
        op: 'writeSnapshot',
        path: `${nulPathBase}/nul-object-surrogate/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'nul-object-surrogate',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            entries: [{ path: 'out.txt', entryType: 'file', objectKey: 'obj\ud800\u0000key' }],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'restore', snapshotId: 'nul-object-surrogate', target: 'restore-object-surrogate' },
    ],
  });
  {
    const outcome = nulObjectSurrogate.steps.find((s: any) => s.op === 'restore')!.outcome;
    assert.deepEqual(outcome.error, {
      name: 'TypeError',
      message: "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '<dir>/snapshots/obj\\ud800\\x00key'",
    });
  }

  const nulFileOutputSurrogate = await scenario('nul-file-output-surrogate', {
    workspace: {},
    steps: [
      { op: 'writeSnapshot', path: 'objects/sha256/00/00/nul-file-surrogate-object', text: 'ok\n' },
      {
        op: 'writeSnapshot',
        path: `${nulPathBase}/nul-file-surrogate/manifest.json`,
        text: JSON.stringify(
          {
            snapshotId: 'nul-file-surrogate',
            workspaceId: 'w',
            sessionId: 's',
            turnId: 't',
            runId: 'r',
            phase: 'pre_user',
            entries: [{ path: 'a\ud800\u0000b.txt', entryType: 'file', objectKey: 'objects/sha256/00/00/nul-file-surrogate-object' }],
            createdAt: '2020-01-01T00:00:00.000Z',
          },
          null,
          2,
        ),
      },
      { op: 'restore', snapshotId: 'nul-file-surrogate', target: 'restore-file-surrogate' },
    ],
  });
  {
    const outcome = nulFileOutputSurrogate.steps.find((s: any) => s.op === 'restore')!.outcome;
    assert.deepEqual(outcome.error, {
      name: 'TypeError',
      message: "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received '<dir>/restore-file-surrogate/a\\ud800\\x00b.txt'",
    });
  }

  const nulCaptureRoot = await scenario('nul-capture-root', {
    dirName: '.',
    workspace: { files: [{ path: 'x.txt', bytes: Buffer.from('x\n') }] },
    steps: [
      { op: 'tick' },
      {
        op: 'capture',
        snapshotRoot: '<dir>/\u0000r',
        phase: 'pre_user',
        input: { workspaceId: 'w', sessionId: 's', turnId: 't', runId: 'r', workspaceDir: '<dir>/workspace' },
      },
    ],
  });
  {
    const outcome = nulCaptureRoot.steps.find((s: any) => s.op === 'capture')!.outcome;
    assert.equal(outcome.error.name, 'TypeError');
    // The quoted partition stays at 127 inspected units (mkdtemp dir 20 +
    // `/\0r` 3 + partition 99 + quotes 2, NUL escaped +3), just under Node's
    // 128-unit inspect limit, so the full digest survives.
    assert.match(
      outcome.error.message,
      /^The argument 'path' must be a string, Uint8Array, or URL without null bytes\. Received '<dir>\/\\x00r\/workspaces\/w\/sessions\/s\/snapshots\/[0-9a-f]{64}'$/,
    );
  }

  // R19 host boundary: ICU4X folds every unpaired surrogate to U+FFFD, while
  // Node keeps distinct weights. Record the real TS order so the Rust test can
  // prove it rejects the comparison instead of silently ordering it another
  // way (the review's A="\ud800", B="\ufffd" case is the first pair).
  const surrogateSortRoot = join(scratch, 'surrogate-sort-root');
  const surrogateSortBase = join(surrogateSortRoot, 'workspaces/w/sessions/s/snapshots');
  const surrogateSortValues = [
    ['a-lone-high', '\ud800'],
    ['b-replacement', '\ufffd'],
    ['c-pua', '\ue000'],
    ['d-supplementary', '\u{10000}'],
  ] as const;
  const surrogateSortManifests: { path: string; text: string }[] = [];
  for (const [dir, createdAt] of surrogateSortValues) {
    const text = JSON.stringify(
      {
        snapshotId: dir,
        workspaceId: 'w',
        sessionId: 's',
        turnId: 't',
        runId: 'r',
        phase: 'pre_user',
        entries: [],
        createdAt,
      },
      null,
      2,
    );
    await mkdir(join(surrogateSortBase, dir), { recursive: true });
    await writeFile(join(surrogateSortBase, dir, 'manifest.json'), text);
    surrogateSortManifests.push({
      path: `workspaces/w/sessions/s/snapshots/${dir}/manifest.json`,
      text,
    });
  }
  const surrogateSortProvider = createWorkspaceSnapshotProvider({ root: surrogateSortRoot, snapshotRoot: surrogateSortRoot });
  const surrogateSortList = (await surrogateSortProvider.list()).map((d: any) => ({
    snapshotId: d.snapshotId,
    createdAt: d.createdAt,
  }));
  assert.deepEqual(
    surrogateSortList.map((d: any) => d.snapshotId),
    ['b-replacement', 'c-pua', 'a-lone-high', 'd-supplementary'],
    'Node collation keeps distinct weights for lone surrogates',
  );
  const hostBoundaryCases = {
    unpairedSurrogateCollation: {
      manifests: surrogateSortManifests,
      tsList: surrogateSortList,
    },
  };

  // Cross fixture: the exact bytes TS wrote for the normal scenario, for the
  // Rust provider to read back and early-exit on.
  const crossFiles: any[] = [];
  const crossDirs: string[] = [];
  const collectCross = async (directory: string) => {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        crossDirs.push(relative(normal.root, path).split(sep).join('/'));
        await collectCross(path);
      } else {
        assert.ok(!entry.isSymbolicLink(), 'snapshot roots hold no symlinks');
        crossFiles.push({ path: relative(normal.root, path).split(sep).join('/'), b64: (await readFile(path)).toString('base64') });
      }
    }
  };
  await collectCross(normal.root);
  const cross = {
    files: crossFiles,
    dirs: crossDirs,
    preId: normal.ids.get('pre'),
    postId: normal.ids.get('post'),
    listValue: normal.steps.find((s: any) => s.op === 'list')!.outcome,
    getValue: normal.steps.filter((s: any) => s.op === 'get')[0].outcome.value,
    restoreTargetTree: normal.steps.filter((s: any) => s.op === 'restore')[0].targetTree,
    input: normal.steps.find((s: any) => s.op === 'capture')!.input,
  };
  assert.ok(cross.preId && cross.postId);

  for (const name of sentinels) {
    assert.equal(await readFile(join(scratch, name), 'utf8'), 'keep\n', `sentinel ${name} was removed`);
  }

  const oracle = { sourceCommit, sourceSha256, node: process.version, configCases, scenarios, hostBoundaryCases, cross };
  const json = JSON.stringify(oracle, null, 2);
  for (const leak of [scratch, output, baseline, process.cwd()]) assert.ok(!json.includes(leak), 'oracle.json leaks a generation path');
  await writeFile(join(output, 'oracle.json'), `${json}\n`);
  console.log(`oracle.json: ${scenarios.length} scenarios, ${crossFiles.length} cross files`);
} finally {
  // Same cleanup as ../run-registry/generate.mts: without it every run
  // (failed assertions included) leaves a .generate-* directory next to
  // oracle.json in the fixtures tree.
  await removeOwned(scratch, scratch, { recursive: true });
}
