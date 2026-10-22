// Derive D0 scenarios mechanically from the guarded real-TS CLI oracle.
// node scripts/cli-parity/generate_chat_search.mjs > scripts/cli-parity/chat-search.json
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
const oracle = JSON.parse(readFileSync(new URL('../../src/pilotdeck-cli/tests/fixtures/chat-search/oracle.json', import.meta.url), 'utf8'));
assert.equal(oracle.provenance.sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
assert.ok(Object.keys(oracle.provenance.sourceSha256).length > 0);
const ids = new Set(['usage','unknown','empty','flags-only','text','json','case-sensitive',
  'role-user','role-assistant','role-invalid','all','all-json','limit','fraction-limit',
  'zero-limit','negative-limit','nan-limit','hex-limit','infinite-limit','session',
  'internal-session','no-result','snippet','multiword','unknown-flag','missing-role',
  'repeat-limit','regex','regex-invalid-class','unicode']);
const scenarios = oracle.cases.filter(test => ids.has(test.id)).map(test => ({
  id: `chat-${test.id}`, argv: ['chat',...test.argv], stdinHex: '', token: false,
  fixtureFiles: oracle.files, exit: test.exit, stdout: test.stdout, stderr: test.stderr,
}));
assert.equal(scenarios.length, ids.size);
process.stdout.write(JSON.stringify(scenarios, null, 2) + '\n');
