// Deterministic gold sample for the reachable domain of
// `chooseBestRecallProjectFallback` (#765 fix-r3).
//
// Every `picked` value comes from the fixed real TS
// `LlmMemoryExtractor.selectRecallProject` fallback path. The shortlists use
// only fields a real `buildProjectShortlist` can produce: `exact` is 0 or 2 and
// `score = exact * 10 + matchedTokenCount`. NaN/Infinity are deliberately not
// generated here: those values are unreachable from the producer, and a
// NaN comparator is not a total order, so TS's TimSort and Rust's stable sort
// are not required to agree there (see RESULT/negative docs).
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdirSync, realpathSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';

const ENTRY = 'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts';
const COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
assert.equal(process.version, 'v22.22.0');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed PilotDeck checkout');
assert.ok(process.argv[2], 'usage: generate.mts OUT_DIR');
const baseline = resolve(process.env.PDRS_BASELINE!);
const out = resolve(process.argv[2]!);
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
bindSourceTree(baseline, COMMIT, git);
const sourceSha256 = sourceClosureSha256(baseline, COMMIT, git, [ENTRY]);

// Reachable producer domains: `buildProjectShortlist` sets sourceType to
// general_local/workspace_external, exact to 0/2, score to exact*10 + tokens.
// The last date repeats so date ties are exercised too.
const SOURCE_TYPES = ['general_local', 'workspace_external'];
const UPDATED_AT = [
  '2026-01-01T00:00:00.000Z',
  '2026-02-01T00:00:00.000Z',
  '2026-03-01T00:00:00.000Z',
  '2026-03-01T00:00:00.000Z',
];
const MAX_TOKENS = 12;
const CASE_COUNT = 212;

function mulberry32(seed: number): () => number {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

type Row = [score: number, exact: number, source: number, updated: number];
type Sample = {name: string; rows: Row[]};

function row(score: number, exact: number, source: number, updated: number): Row {
  return [score, exact, source, updated];
}

const random = mulberry32(0x765_0833);
const samples: Sample[] = [];
// n=70 is past V8's TimSort run boundary; 2..70 covers every shorter length.
samples.push({name: 'js-sort-all-equal-70', rows: Array.from({length: 70}, () => row(0, 0, 0, 0))});
samples.push({
  name: 'js-sort-source-tie-70',
  rows: Array.from({length: 70}, (_, index) => row(6, 0, index % 2, 0)),
});
samples.push({
  name: 'js-sort-updated-tie-70',
  rows: Array.from({length: 70}, (_, index) => row(6, 0, 0, index % 4)),
});
while (samples.length < CASE_COUNT) {
  const length = 2 + Math.floor(random() * 69);
  const rows: Row[] = [];
  for (let index = 0; index < length; index += 1) {
    const exact = random() < 0.5 ? 0 : 2;
    const score = exact * 10 + Math.floor(random() * (MAX_TOKENS + 1));
    rows.push(row(score, exact, random() < 0.5 ? 0 : 1, Math.floor(random() * UPDATED_AT.length)));
  }
  samples.push({name: `js-sort-${String(samples.length).padStart(3, '0')}`, rows});
}

const {LlmMemoryExtractor} = await import(pathToFileURL(join(baseline, ENTRY)).href);
const extractor = new LlmMemoryExtractor({});
const cases: unknown[] = [];
for (const sample of samples) {
  const shortlist = sample.rows.map(([score, exact, source, updated], index) => ({
    projectId: `p${index}`,
    projectName: `P${index}`,
    description: '',
    status: 'active',
    updatedAt: UPDATED_AT[updated],
    sourceType: SOURCE_TYPES[source],
    score,
    exact,
    source: 'query',
    matchedText: '',
  }));
  const result = await extractor.selectRecallProject({query: '', shortlist});
  const picked = result.projectId;
  assert.ok(typeof picked === 'string' && /^p\d+$/.test(picked), `no fallback pick for ${sample.name}`);
  cases.push({name: sample.name, picked, candidates: sample.rows});
}

const oracle = {
  sourceCommit: COMMIT,
  sourceSha256,
  nodeVersion: process.version,
  domain: {
    caseCount: CASE_COUNT,
    exact: [0, 2],
    score: `exact * 10 + matched token count (0..${MAX_TOKENS})`,
    sourceType: SOURCE_TYPES,
    updatedAt: UPDATED_AT,
    candidate: '[score, exact, sourceTypeIndex, updatedAtIndex]',
    projectId: 'row index -> "p<index>"',
  },
  cases,
};
const text = JSON.stringify(oracle, null, 2) + '\n';
for (const leak of [resolve(process.cwd()), process.cwd()]) {
  assert.ok(!text.includes(leak + '/'), `oracle.json leaks cwd ${leak}`);
}
mkdirSync(out, {recursive: true});
writeFileSync(join(out, 'oracle.json'), text, {flag: 'wx'});
console.log(`${cases.length} reachable-domain cases; ${text.length} bytes`);
