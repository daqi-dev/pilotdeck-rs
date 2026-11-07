#!/usr/bin/env node
// Regenerate src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe.json from the
// real Node/ICU runtime, so the CLI skills collator test replays measured
// truth instead of a hand-written table (#132 P2-2).
//
//   node scripts/cli-parity/skills_fr_ca_probe.mjs > src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe.json
//
// The pinned pair is asserted below: a machine with a different Node or ICU
// must not silently produce a different gold. Each locale is measured in a
// fresh child process with an isolated environment, because ICU reads the
// default locale from LANG/LC_ALL once per process.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const PINNED_NODE = 'v22.22.0';
const PINNED_ICU = '78.2';
const LOCALES = [
  'fr_CA.UTF-8',              // bare fr-CA: backward-secondary repair applies
  'fr_Latn_CA.UTF-8',         // script-tagged fr-CA: CLI guard must not fire
  'fr_CA@collation=eor',      // POSIX modifier: Node drops it and stays fr-CA
  'fr_FR.UTF-8',              // fr without CA: repair must not fire
  'en_US.UTF-8',              // non-fr control
];
// Accent position decides fr-CA ordering: fr-CA sorts accents at the primary
// level (cote < côte < coté), generic fr/en at the secondary level.
const PAIRS = [
  ['coté', 'côte'],
  ['côte', 'coté'],
  ['cote', 'côte'],
  ['coté', 'cote'],
];
const SELF = fileURLToPath(import.meta.url);

function measure(lang) {
  const out = execFileSync(process.execPath, [SELF, '--one', lang], {
    encoding: 'utf8',
    env: { PATH: '/usr/bin:/bin', LANG: lang, LC_ALL: lang },
  });
  return JSON.parse(out);
}

if (process.argv[2] === '--one') {
  const collator = new Intl.Collator();
  process.stdout.write(JSON.stringify({
    node: process.version,
    icu: process.versions.icu,
    resolved: collator.resolvedOptions().locale,
    signs: PAIRS.map(([a, b]) => Math.sign(a.localeCompare(b))),
  }));
} else {
  assert.equal(process.version, PINNED_NODE, 'run this on the pinned Node');
  assert.equal(process.versions.icu, PINNED_ICU, 'run this on the pinned ICU');
  const cases = LOCALES.map((lang) => {
    const row = measure(lang);
    assert.equal(row.node, PINNED_NODE, `${lang}: child Node pin`);
    assert.equal(row.icu, PINNED_ICU, `${lang}: child ICU pin`);
    return {
      lang,
      resolved: row.resolved,
      pairs: PAIRS.map(([a, b], index) => [a, b, row.signs[index]]),
    };
  });
  process.stdout.write(JSON.stringify({
    node: process.version,
    icu: process.versions.icu,
    reproduce: 'node scripts/cli-parity/skills_fr_ca_probe.mjs > src/pilotdeck-cli/tests/fixtures/skills-fr-ca-probe.json',
    note: '#132 P2-2: real Node runtime rows for the CLI skills collator; the TS entry (extension/skills/migrateSkills.ts:269) sorts basenames with localeCompare under the process locale. Each row is measured in a child process with LANG=LC_ALL=<lang>.',
    cases,
  }, null, 2) + '\n');
}
