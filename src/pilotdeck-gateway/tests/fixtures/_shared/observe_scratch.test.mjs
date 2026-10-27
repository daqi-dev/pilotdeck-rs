// #773 fix-r3 P1-2 wiring guard: the dream-runner generator must create a
// fresh scratch root through the shared allocator, not reopen/delete a fixed
// stage path. Pure Node (reads source text), no TS baseline needed.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';

const source = readFileSync(
  new URL('../edgeclaw-dream-runner/observe.mts', import.meta.url),
  'utf8',
);

test('dream-runner observe owns a fresh scratch root', () => {
  assert.match(
    source,
    /createRunScratch\(/,
    '#773 dream-runner observe must allocate its scratch through createRunScratch',
  );
  assert.doesNotMatch(
    source,
    /['"]\/tmp\/pdrs-dream-runner-observe/,
    '#773 dream-runner observe must not use the shared fixed stage path',
  );
  assert.doesNotMatch(
    source,
    /rmSync\(\s*scratch\s*,\s*\{\s*recursive:\s*true/,
    '#773 dream-runner observe must not delete a shared scratch root at startup',
  );
});
