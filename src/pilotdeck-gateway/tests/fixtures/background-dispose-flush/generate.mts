// #994: real TS dispose with no snapshot-store fallback and a gated filesystem write.
import assert from 'node:assert/strict';
import {promises as fs} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#994 pinned Node');
assert(process.env.PDRS_BASELINE && process.argv[2], '#994 baseline/output required');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/task/runtime/BackgroundTaskRuntime.ts', 'src/task/storage/TaskOutputStore.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const {BackgroundTaskRuntime} = await import(pathToFileURL(join(baseline, sources[0])).href);
const deadline = setTimeout(() => { throw Error('#994 dispose fixture exceeded 10 seconds'); }, 10000);
const originalAppend = fs.appendFile;
const cases = [];
const content = 'gated-output-中🙂';
try {
  for (const mode of ['pending', 'ready', 'cancelled']) {
    const root = await fs.mkdtemp(join(tmpdir(), 'pdrs-994-dispose-'));
    let enter!: () => void, release!: () => void;
    const entered = new Promise<void>(resolve => { enter = resolve; });
    const gate = new Promise<void>(resolve => { release = resolve; });
    fs.appendFile = async (...args: Parameters<typeof fs.appendFile>) => {
      enter();
      await gate;
      if (mode === 'cancelled') throw Object.assign(Error('cancelled write'), {name:'AbortError'});
      return originalAppend(...args);
    };
    let runtime: any;
    try {
      runtime = new BackgroundTaskRuntime({diskSpillDir: root,
        shell: {async start() { return {pid:42, exit:Promise.resolve({exitCode:0}), terminate() {}}; }},
      });
      const task = await runtime.start({command:'unused', cwd:'/'});
      const entry = runtime.entries.get(task.taskId);
      await entry.done;
      entry.output.append(content);
      await entered;
      if (mode === 'ready') { release(); await entry.output.flush(); }
      let closeEntered!: () => void;
      const closing = new Promise<void>(resolve => { closeEntered = resolve; });
      const originalClose = entry.output.close.bind(entry.output);
      entry.output.close = () => { const result = originalClose(); closeEntered(); return result; };
      let disposed = false;
      const disposal = runtime.dispose().then(() => { disposed = true; });
      await closing;
      // The gate is still held in pending/cancelled. Drain the finite promise
      // reactions after the real close boundary; never use a wall-clock sleep.
      await new Promise<void>(resolve => setImmediate(resolve));
      const before = {disposed, retained:entry.output.readSlice(0).content};
      assert.equal(before.disposed, mode === 'ready', '#994 real close settlement');
      assert.equal(before.retained, mode === 'ready' ? '' : content, '#994 ring lifetime');
      release();
      await disposal;
      let disk: string | null;
      try { disk = await fs.readFile(join(root, `${task.taskId}.log`), 'utf8'); }
      catch (error: any) { assert.equal(error.code, 'ENOENT', '#994 disk read'); disk = null; }
      const row = {mode, before, after:{disposed, disk}};
      assert.equal(disk, mode === 'cancelled' ? null : content, '#994 accepted bytes');
      cases.push(row);
    } finally {
      release();
      if (runtime) await runtime.dispose();
      fs.appendFile = originalAppend;
      await fs.rm(root, {recursive:true, force:true});
    }
  }
  const oracle = {sourceCommit, sourceSha256, content, cases};
  await fs.mkdir(resolve(process.argv[2]), {recursive:true});
  await fs.writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify(oracle, null, 2)+'\n', {flag:'wx'});
  console.log(JSON.stringify({node:process.version, cases}));
} finally {
  fs.appendFile = originalAppend;
  clearTimeout(deadline);
}
