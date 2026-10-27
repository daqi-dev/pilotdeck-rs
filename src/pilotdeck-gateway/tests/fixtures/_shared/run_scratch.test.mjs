// #773 fix-r3 P1-2: scratch roots must be exclusive and only the owned child
// may be removed. Pure Node so `cargo test` can run it without the TS baseline.
import test from 'node:test';
import assert from 'node:assert/strict';
import {existsSync, mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createRunScratch, removeRunScratch} from './run_scratch.mjs';

test('run scratch roots are exclusive and only the owned root is removed', async () => {
  const parent = mkdtempSync(join(tmpdir(), 'pdrs-773-run-scratch-test-'));
  try {
    writeFileSync(join(parent, 'sentinel.txt'), 'keep');
    const first = createRunScratch(parent, 'pdrs-773-scratch-');
    const second = createRunScratch(parent, 'pdrs-773-scratch-');
    assert.notEqual(
      first.owned,
      second.owned,
      '#773 scratch roots must be distinct mkdtemp dirs',
    );
    writeFileSync(join(first.owned, 'first.txt'), 'first');
    writeFileSync(join(second.owned, 'second.txt'), 'second');

    await removeRunScratch(first);
    assert.equal(
      existsSync(join(parent, 'sentinel.txt')),
      true,
      '#773 scratch cleanup must keep the parent sentinel',
    );
    assert.equal(
      existsSync(join(second.owned, 'second.txt')),
      true,
      '#773 scratch cleanup must keep a sibling run',
    );
    assert.equal(
      existsSync(first.owned),
      false,
      '#773 scratch cleanup must remove its own root',
    );
    let refused;
    try {
      await removeRunScratch({root: parent, owned: parent});
    } catch (error) {
      refused = error;
    }
    assert.match(
      String(refused?.message),
      /#773 remove refused/,
      '#773 remove refused: the run parent must not be removed',
    );

    await removeRunScratch(second);
    assert.equal(
      existsSync(second.owned),
      false,
      '#773 scratch cleanup must remove its own root',
    );
  } finally {
    rmSync(parent, {recursive: true, force: true});
  }
});
