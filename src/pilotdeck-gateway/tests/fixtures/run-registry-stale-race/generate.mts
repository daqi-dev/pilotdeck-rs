import assert from 'node:assert/strict';
import { fork } from 'node:child_process';
import { mkdir, mkdtemp, readFile, realpath, rm, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/gateway/run/RunRegistry.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-stale-race-')));
const actors = [];
const cases = [];
try {
  const path = join(root, 'race.json');
  await writeFile(`${path}.lock`, '{"pid":2147483647}');
  await utimes(`${path}.lock`, new Date(0), new Date(0));
  function actor() {
    const child = fork(fileURLToPath(new URL('./actor.mts', import.meta.url)), [path], {
      execArgv: ['--import', join(baseline, 'node_modules/tsx/dist/loader.mjs')],
      env: { ...process.env, PDRS_BASELINE: baseline }, stdio: ['ignore', 'ignore', 'inherit', 'ipc'],
    });
    actors.push(child);
    const queue = [], waiters = [];
    child.on('message', message => { if (waiters.length) waiters.shift()(message); else queue.push(message); });
    const next = async () => {
      if (queue.length) return queue.shift();
      let timer;
      try {
        return await Promise.race([
          new Promise(resolve => waiters.push(resolve)),
          new Promise((_, fail) => { timer = setTimeout(() => fail(new Error('actor IPC timeout')), 10000); }),
        ]);
      } finally { clearTimeout(timer); }
    };
    return { child, next };
  }
  const a = actor(), b = actor();
  assert.equal((await a.next()).state, 'before-unlink');
  assert.equal((await b.next()).state, 'before-unlink');
  a.child.send('unlink');
  const first = await a.next(); assert.equal(first.state, 'acquired');
  b.child.send('unlink');
  const second = await b.next(); assert.equal(second.state, 'acquired');
  a.child.send('inspect'); b.child.send('inspect');
  const firstState = await a.next(), secondState = await b.next();
  assert.equal(firstState.state, 'inspected'); assert.equal(secondState.state, 'inspected');
  cases.push({ id: 'two-process-stale-race', observed: {
    bothAcquired: true, distinctLockInodes: first.inode !== second.inode,
    firstHeldOpen: firstState.heldOpen, firstMatchesPath: firstState.heldMatchesPath,
    secondHeldOpen: secondState.heldOpen, secondMatchesPath: secondState.heldMatchesPath,
  } });
  const closed = actors.map(child => new Promise<void>((ok, fail) => {
    child.on('exit', code => code === 0 ? ok() : fail(new Error(`actor exit ${code}`)));
    child.on('error', fail);
  }));
  a.child.send('close'); b.child.send('close'); await Promise.all(closed);
  const { FileRunRegistry } = await import(pathToFileURL(join(baseline, sources[0])).href);
  const sequential = join(root, 'sequential.json');
  const registry = new FileRunRegistry(sequential); await registry.ready;
  await writeFile(`${sequential}.lock`, '{"pid":2147483647}');
  await utimes(`${sequential}.lock`, new Date(0), new Date(0));
  const handle = await registry.acquireFileLock();
  const owner = JSON.parse(await readFile(`${sequential}.lock`, 'utf8'));
  cases.push({ id: 'dead-stale-replaced', observed: { acquired: true, ownerIsSelf: owner.pid === process.pid } });
  await handle.close();
} finally {
  for (const child of actors) if (child.exitCode === null) child.kill();
  await rm(root, { recursive: true, force: true });
}
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n');
