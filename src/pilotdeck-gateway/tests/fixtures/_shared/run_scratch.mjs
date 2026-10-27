// Exclusive, ownership-checked scratch roots for real-TS fixture generators.
//
// A generator must never reuse (or delete) a fixed stage directory: two
// concurrent runs would remove each other's HOME / SQLite tree (#773 P1-2,
// the same class as the #888 fixed-path delete). `createRunScratch` makes a
// fresh mkdtemp child of the caller's parent; `removeRunScratch` refuses to
// delete anything that is not that child and routes the delete through the
// shared `assertInside` / `removeOwned` guard.
import {mkdirSync, mkdtempSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {assertInside, removeOwned} from './generator_guard.mjs';

export function createRunScratch(parent, prefix) {
  const root = resolve(parent);
  mkdirSync(root, {recursive: true});
  return {root, owned: mkdtempSync(join(root, prefix))};
}

export async function removeRunScratch(scratch) {
  const {root, owned} = scratch;
  if (resolve(owned) === resolve(root)) {
    throw new Error(`#773 remove refused: ${owned} is the run parent, not an owned child`);
  }
  try {
    assertInside(root, owned, 'remove');
  } catch (error) {
    throw new Error(`#773 ${error.message}`);
  }
  await removeOwned(root, owned, {recursive: true});
}
