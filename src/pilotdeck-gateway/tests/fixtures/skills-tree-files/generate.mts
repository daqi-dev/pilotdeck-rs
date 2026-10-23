// Real-TS oracle for SkillManager tree_files (#595).
// Run from repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
import { assertBaseline, baselineSourceHashes } from '../../_shared/baseline.mts';
import { mkdtemp, writeFile, symlink, mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/extension/skills/SkillManager.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const { SkillManager } = await import(pathToFileURL(join(baseline, sources[0])).href);
const sha256 = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');

type TreeSetup = { dirs: string[]; files: { path: string; text: string }[]; links: { path: string; target: string }[] };

async function setupTree(root: string, spec: TreeSetup) {
  for (const dir of spec.dirs) await mkdir(join(root, dir), { recursive: true });
  for (const { path, text } of spec.files) await writeFile(join(root, path), text);
  for (const { path, target } of spec.links) await symlink(target, join(root, path));
}

async function runCase(name: string, setup: TreeSetup) {
  const scratch = await mkdtemp(join(tmpdir(), 'pdrs-tree-files-'));
  const skillRoot = join(scratch, 'skill');
  await mkdir(skillRoot);
  await setupTree(skillRoot, setup);

  const manager = new SkillManager({ pilotHome: scratch, env: {} });
  const result = await manager.scan({ parentPath: skillRoot });
  const self = result.folders[0];
  assert.ok(self && self.hasSkillMd, 'folders[0] must have SKILL.md');

  await rm(scratch, { recursive: true, force: true });

  return {
    name,
    fileCount: self.fileCount,
    totalSize: self.totalSize,
    entries: 0, // Not directly observable from scan result
  };
}

// Generate all test cases
const cases: { name: string; setup: TreeSetup; expected?: { fileCount: number; totalSize: number } }[] = [
  {
    name: 'dag-10',
    setup: {
      dirs: ['d0', 'd1', 'd2', 'd3', 'd4', 'd5', 'd6', 'd7', 'd8', 'd9'],
      files: Array.from({ length: 1024 }, (_, i) => ({
        path: `d${i % 10}/f${i}.txt`,
        text: 'abc',
      })).concat([{ path: 'SKILL.md', text: '---\nname: dag-10\n---\n' }]),
      links: [],
    },
  },
  {
    name: 'dag-15',
    setup: {
      dirs: Array.from({ length: 100 }, (_, i) => `d${i}`),
      files: Array.from({ length: 32768 }, (_, i) => ({
        path: `d${i % 100}/f${i}.txt`,
        text: 'abc',
      })).concat([{ path: 'SKILL.md', text: '---\nname: dag-15\n---\n' }]),
      links: [],
    },
  },
  {
    name: 'deep-300',
    setup: {
      dirs: Array.from({ length: 300 }, (_, i) => Array.from({ length: i + 1 }, (_, j) => 'd').join('')).map((p, i) =>
        Array.from({ length: i }, (_, j) => 'd').join('/')),
      files: [
        { path: 'SKILL.md', text: '---\nname: deep-300\n---\n' },
        { path: Array.from({ length: 299 }, () => 'd').join('/') + '/file.txt', text: 'deep' },
      ],
      links: [],
    },
  },
  {
    name: 'linear-20',
    setup: {
      dirs: Array.from({ length: 20 }, (_, i) => `d${i}`),
      files: Array.from({ length: 21 }, (_, i) => ({
        path: i === 0 ? 'f0.txt' : `d${i - 1}/f${i}.txt`,
        text: 'abc',
      })).concat([{ path: 'SKILL.md', text: '---\nname: linear-20\n---\n' }]),
      links: Array.from({ length: 19 }, (_, i) => ({
        path: `l${i}`,
        target: `d${i}`,
      })),
    },
  },
  {
    name: 'hops-20',
    setup: {
      dirs: [],
      files: [
        { path: 'SKILL.md', text: '---\nname: hops-20\n---\n' },
        { path: 'target.txt', text: 'end' },
      ],
      links: [{ path: 'l0', target: 'target.txt' }].concat(
        Array.from({ length: 19 }, (_, i) => ({
          path: `l${i + 1}`,
          target: `l${i}`,
        }))
      ),
    },
  },
  {
    name: 'alias-10x5',
    setup: {
      dirs: ['target'].concat(Array.from({ length: 5 }, (_, i) => `target/sub${i}`)),
      files: Array.from({ length: 50 }, (_, i) => ({
        path: `target/sub${i % 5}/f${i}.txt`,
        text: 'abc',
      })).concat([{ path: 'SKILL.md', text: '---\nname: alias-10x5\n---\n' }]),
      links: Array.from({ length: 10 }, (_, i) => ({
        path: `link${i}`,
        target: 'target',
      })),
    },
  },
  {
    name: 'mutual',
    setup: {
      dirs: ['a', 'b'],
      files: Array.from({ length: 30 }, (_, i) => ({
        path: `${i % 2 === 0 ? 'a' : 'b'}/f${i}.txt`,
        text: 'abc',
      })).concat([{ path: 'SKILL.md', text: '---\nname: mutual\n---\n' }]),
      links: [
        { path: 'a/linkb', target: '../b' },
        { path: 'b/linka', target: '../a' },
      ],
    },
  },
];

const results = [];
for (const { name, setup } of cases) {
  console.error(`Running case: ${name}`);
  const result1 = await runCase(name, setup);
  const result2 = await runCase(name, setup);
  assert.equal(result1.fileCount, result2.fileCount, `${name}: fileCount mismatch between runs`);
  assert.equal(result1.totalSize, result2.totalSize, `${name}: totalSize mismatch between runs`);
  results.push(result1);
}

const oracle = {
  sourceCommit: sourceCommit,
  sourceSha256: sourceSha256,
  cases: results,
};

await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.error(`Generated oracle.json with ${results.length} cases`);
