// Test transport boundary only: hold the real TS unlink syscall after stat/read.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { syncBuiltinESMExports } from 'node:module';
import { pathToFileURL } from 'node:url';
const path = process.argv[2];
const lock = `${path}.lock`;
const original = fs.promises.unlink;
let first = true;
fs.promises.unlink = async filename => {
  if (String(filename) === lock && first) {
    first = false;
    process.send({ state: 'before-unlink' });
    await new Promise<void>(resolve => process.once('message', () => resolve()));
  }
  return original(filename);
};
syncBuiltinESMExports();
const { FileRunRegistry } = await import(pathToFileURL(`${process.env.PDRS_BASELINE}/src/gateway/run/RunRegistry.ts`).href);
const registry = new FileRunRegistry(path);
await registry.ready;
const handle = await registry.acquireFileLock();
const held = await handle.stat();
process.send({ state: 'acquired', inode: String(held.ino) });
process.on('message', async message => {
  if (message === 'inspect') {
    const current = await fs.promises.stat(lock);
    const own = await handle.stat();
    process.send({ state: 'inspected', heldMatchesPath: current.ino === own.ino, heldOpen: own.isFile() });
  } else if (message === 'close') {
    await handle.close(); process.disconnect();
  } else assert.fail(`unexpected command ${message}`);
});
