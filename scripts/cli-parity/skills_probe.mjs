#!/usr/bin/env node
// Coordinator-only runtime probe. No oracle/threshold is updated by this script.
// LANG=C LC_ALL=C node skills_probe.mjs /absolute/read-only/PilotDeck
import { mkdtemp, mkdir, writeFile, symlink, readFile, lstat, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';

if (process.argv.length !== 3 || process.env.LC_ALL !== 'C') {
  throw new Error('Usage: LANG=C LC_ALL=C node skills_probe.mjs <baseline>; LC_ALL must be C.');
}
const baseline = resolve(process.argv[2]);
const { hashDirectoryTree, migrateLegacyBundledSkillCopies } = await import(pathToFileURL(
  join(baseline, 'dist/src/extension/skills/migrateLegacyBundledSkills.js')));
const root = await mkdtemp(join(tmpdir(), 'pdrs-skills-probe-'));
try {
  const names = ['a', 'A', 'a-b', 'a_b', 'a.b', 'a/b', 'a10', 'a2', 'ä', 'á', 'é', 'e\u0301', '中', '😀', 'SKILL.md'];
  const sorted = [...names].sort((a, b) => a.localeCompare(b));
  const tree = join(root, 'hash-tree');
  await mkdir(join(tree, 'nested'), { recursive: true });
  await mkdir(join(tree, 'empty'));
  // macOS normalizes composed/decomposed file names, so test equivalent sort
  // keys in memory above; only one spelling is materialized in the hash tree.
  const paths = ['a-b', 'a_b', 'a.b', 'nested/b', 'a10', 'a2', 'ä', 'á', 'é', '中', '😀', 'SKILL.md'];
  for (const name of paths) await writeFile(join(tree, name), `bytes:${name}\n`);
  await symlink('absent-target', join(tree, 'link'));
  const historical = execFileSync('/usr/bin/git', ['-C', baseline, 'show',
    'db77b86d09441c5fab6340a669598f8fbabe82c7:skills/browser-use/SKILL.md'],
    { env: { PATH: '/usr/bin:/bin', GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null' } });
  const pilot = join(root, 'pilot');
  const builtin = join(root, 'builtin');
  await mkdir(join(pilot, 'skills/browser-use'), { recursive: true });
  await mkdir(join(pilot, 'skills/docx'), { recursive: true });
  await mkdir(join(builtin, 'browser-use'), { recursive: true });
  await writeFile(join(pilot, 'skills/browser-use/SKILL.md'), historical);
  await writeFile(join(pilot, 'skills/docx/SKILL.md'), 'User modified skill.\n');
  await writeFile(join(builtin, 'browser-use/SKILL.md'), 'Current bundle differs.\n');
  const legacyHash = hashDirectoryTree(join(pilot, 'skills/browser-use'));
  const migration = migrateLegacyBundledSkillCopies({ pilotHome: pilot, builtinSkillsRoot: builtin });
  console.log(JSON.stringify({ node: process.version, icu: process.versions.icu,
    locale: new Intl.Collator().resolvedOptions(), names, sorted,
    comparisons: names.map(a => names.map(b => Math.sign(a.localeCompare(b)))),
    hashTree: { paths, link: 'absent-target', sha256: hashDirectoryTree(tree) },
    historicalBlob: { size: historical.length, sha256: createHash('sha256').update(historical).digest('hex'), treeHash: legacyHash },
    migration, marker: await readFile(join(pilot, '.legacy-bundled-skills-migrated-v1'), 'utf8'),
    modifiedRetained: (await lstat(join(pilot, 'skills/docx/SKILL.md'))).isFile(),
  }, null, 2));
} finally {
  await rm(root, { recursive: true, force: true });
}
