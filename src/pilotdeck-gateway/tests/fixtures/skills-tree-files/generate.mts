// Real-TS oracle for SkillManager tree_files (#595). Topology follows design
// v3 `probes/build.mjs` verbatim: the link chains live outside the skill root
// so only the link paths are enumerated.
//
// Two oracle files:
// - oracle.json: portable cases whose values do not depend on the kernel
//   symlink budget on either macOS (MAXSYMLINKS 32) or Linux (40); replayed by
//   the default test `tree_files_matches_ts_baseline`.
// - oracle-macos.json (written on darwin only): cases whose values depend on
//   macOS's budget and a one-symlink scratch prefix (`/tmp -> /private/tmp`);
//   replayed by the macOS-gated test `tree_files_matches_ts_baseline_macos`.
//   Per tests/fixtures/skills/README.md these never enter the portable oracle.
//
// Run from repo root with Node 22 and the fixed baseline's tsx loader; see
// README.md. Every case runs twice and both runs must serialize identically.
import { assertBaseline, baselineSourceHashes } from '../../_shared/baseline.mts';
import { mkdtemp, writeFile, symlink, mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node v22');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/extension/skills/SkillManager.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const { SkillManager } = await import(pathToFileURL(join(baseline, sources[0])).href);

const SKILL_MD = '---\nname: Probe\ndescription: tree_files probe fixture\n---\n';

// Portable cases: with the default one-symlink temp prefix the deepest lookup
// follows at most 22 links (hops-20), against budgets of 32 (macOS) and 40
// (Linux); deep-300 stays under PATH_MAX (~660 B of the 1024 B macOS limit).
const PORTABLE = ['dag-10', 'linear-20', 'hops-20', 'deep-300', 'alias-10x5'];
// macOS-only cases: dag-15's deepest lookup follows 31 links of the 32 allowed
// (one-symlink prefix + 2 per absolute-link level); dag-16/dag-20 truncate
// (1 + 2*16 > 32); mutual and self-1 expand until ELOOP among real files.
const MACOS = ['dag-15', 'dag-16', 'dag-20', 'mutual', 'self-1'];

async function buildCase(base: string, name: string) {
  const skill = join(base, 'skill');
  await mkdir(skill, {recursive: true});
  await writeFile(join(skill, 'SKILL.md'), SKILL_MD);
  const file = async (p: string, text: string) => {
    await mkdir(dirname(p), {recursive: true});
    await writeFile(p, text);
  };
  const link = (target: string, p: string) => symlink(target, p);
  const m = /^dag-(\d+)$/.exec(name);
  if (m) {
    // k link levels, two absolute links per level, chain kept outside the
    // skill root: only the 2^k link paths root/{a1,a2}/.../{a1,a2}/only exist.
    // Absolute targets re-resolve the scratch prefix, so each level follows
    // prefix + 2 links.
    const k = Number(m[1]);
    const chain = join(base, 'chain');
    const node = (i: number) => (i < k ? join(chain, `d${i}`) : join(chain, 'leaf'));
    for (let i = 1; i < k; i++) await mkdir(node(i), {recursive: true});
    await file(join(chain, 'leaf', 'only'), 'xyz');
    for (const a of ['a1', 'a2']) await link(node(1), join(skill, a));
    for (let i = 1; i < k; i++) for (const a of ['a1', 'a2']) await link(node(i + 1), join(node(i), a));
  } else if (name === 'mutual') {
    await file(join(skill, 'A', 'a.txt'), '12345');
    await file(join(skill, 'B', 'b.txt'), '67');
    await link('../B', join(skill, 'A', 'l'));
    await link('../A', join(skill, 'B', 'm'));
  } else if (name === 'self-1') {
    await file(join(skill, 'a.txt'), '12345');
    await link('.', join(skill, 'l1'));
  } else if (/^linear-(\d+)$/.test(name)) {
    // Link to the next real directory, relative targets: skill/l/l/.../l
    // follows the prefix plus one link per level. Each level has f.
    const n = Number(/^linear-(\d+)$/.exec(name)![1]);
    const chain = join(base, 'chain');
    for (let i = 1; i <= n; i++) await file(join(chain, `d${i}`, 'f'), 'x');
    await link('../chain/d1', join(skill, 'l'));
    for (let i = 1; i < n; i++) await link(`../d${i + 1}`, join(chain, `d${i}`, 'l'));
  } else if (/^hops-(\d+)$/.test(name)) {
    // Link to the next link: skill/x -> h1 -> ... -> hN -> real (one path).
    const n = Number(/^hops-(\d+)$/.exec(name)![1]);
    const hops = join(base, 'hops');
    await mkdir(hops, {recursive: true});
    await file(join(base, 'real', 'f'), 'xyz');
    await link('../hops/h1', join(skill, 'x'));
    for (let i = 1; i < n; i++) await link(`h${i + 1}`, join(hops, `h${i}`));
    await link('../real', join(hops, `h${n}`));
  } else if (/^deep-(\d+)$/.test(name)) {
    const n = Number(/^deep-(\d+)$/.exec(name)![1]);
    const nested = Array.from({length: n}, () => 'a').join('/');
    await file(join(skill, nested, 'end.txt'), '12345');
  } else if (name === 'alias-10x5') {
    // Ten relative links to `real`, which holds f and 5 subdirectories.
    await file(join(skill, 'real', 'f'), '12345');
    for (let i = 0; i < 5; i++) await file(join(skill, 'real', `s${i}`, 'g'), '67');
    for (let i = 0; i < 10; i++) await link('real', join(skill, `l${i}`));
  } else {
    throw Error(`unknown case ${name}`);
  }
}

// macOS cases must run under a literal /tmp root: their values assume exactly
// one symlink in the scratch prefix (/tmp -> /private/tmp on macOS).
async function runCase(name: string, parent: string) {
  const scratch = await mkdtemp(join(parent, 'pdrs-tree-files-'));
  await buildCase(scratch, name);
  const manager = new SkillManager({pilotHome: scratch, env: {}});
  const result = await manager.scan({parentPath: join(scratch, 'skill')});
  const self = result.folders[0];
  assert.ok(self && self.hasSkillMd, 'folders[0] must have SKILL.md');
  await rm(scratch, {recursive: true, force: true});
  return {name, fileCount: self.fileCount, totalSize: self.totalSize};
}

async function runTwice(names: string[], parent: string) {
  const results = [];
  for (const name of names) {
    console.error(`Running case: ${name}`);
    const run1 = await runCase(name, parent);
    const run2 = await runCase(name, parent);
    assert.equal(JSON.stringify(run1), JSON.stringify(run2), `${name}: two runs differ`);
    results.push(run1);
  }
  return results;
}

const cases = await runTwice(PORTABLE, tmpdir());
const oracle = {sourceCommit, sourceSha256, cases};
await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.error(`Generated oracle.json with ${cases.length} cases`);

if (process.platform !== 'darwin') {
  console.error('Skipping oracle-macos.json: its values are macOS-only; regenerate on darwin');
} else {
  const macosCases = await runTwice(MACOS, '/tmp');
  const oracleMacos = {sourceCommit, sourceSha256, platform: 'darwin', cases: macosCases};
  await writeFile(join(output, 'oracle-macos.json'), JSON.stringify(oracleMacos, null, 2) + '\n');
  console.error(`Generated oracle-macos.json with ${macosCases.length} cases`);
}
