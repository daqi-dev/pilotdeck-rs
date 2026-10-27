// #773 fix-r3 P1-2 wiring guard (extended by #774): every Dream observe
// generator must create a fresh scratch root through the shared allocator, not
// reopen/delete a fixed stage path. Pure Node (reads source text), no TS
// baseline needed. The #773 dream-runner messages stay byte-identical because
// edgeclaw-dream-runner/negative.py binds them.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';

// Each fixture's allocator prefix is independent of its directory name: the
// historical fixed stage paths were `/tmp/pdrs-dream-runner-observe` and
// `/tmp/pdrs-dream-run-observe`. Spelling the prefix out keeps this guard
// pointed at the path it actually protects (#774 P2-2).
const fixtures = [
  {name: 'edgeclaw-dream-runner', tag: '#773 dream-runner', scratchPrefix: 'pdrs-dream-runner-observe'},
  {name: 'edgeclaw-dream-run', tag: '#774 edgeclaw-dream-run', scratchPrefix: 'pdrs-dream-run-observe'},
];

for (const fixture of fixtures) {
  const source = readFileSync(
    new URL(`../${fixture.name}/observe.mts`, import.meta.url),
    'utf8',
  );

  test(`${fixture.name} observe owns a fresh scratch root`, () => {
    assert.match(
      source,
      /createRunScratch\(/,
      `${fixture.tag} observe must allocate its scratch through createRunScratch`,
    );
    assert.doesNotMatch(
      source,
      new RegExp(`['"]/tmp/${fixture.scratchPrefix}`),
      `${fixture.tag} observe must not use the shared fixed stage path`,
    );
    assert.doesNotMatch(
      source,
      /rmSync\(\s*scratch\s*,\s*\{\s*recursive:\s*true/,
      `${fixture.tag} observe must not delete a shared scratch root at startup`,
    );
  });
}
