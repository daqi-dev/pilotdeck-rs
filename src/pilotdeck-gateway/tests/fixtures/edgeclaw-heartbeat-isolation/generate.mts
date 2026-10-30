// #1098: real repository discovery and SQLite read-lock/open interaction.
import assert from 'node:assert/strict';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1098 Node version');
assert.ok(process.env.PDRS_BASELINE, '#1098 PDRS_BASELINE');
assert.ok(process.argv[2], '#1098 output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const out = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const source = 'src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const sourceSha256 = await baselineSourceHashes(baseline, [source]);
const home = join(out, 'work', 'home');
mkdirSync(home, {recursive: true});
process.env.HOME = home;
const {MemoryRepository} = await import(pathToFileURL(join(baseline, source)).href);
function open(root: string, general = false) {
  const workspaceDir = general ? join(home, '.pilotdeck') : join(root, 'workspace');
  const repository = new MemoryRepository(join(root, 'control.sqlite'), {
    memoryDir: join(root, 'memory'), globalRootDir: join(root, 'root/global'), workspaceDir,
  });
  repository.setPipelineState('workspaceDir', workspaceDir);
  return repository;
}
const layouts: Record<string, unknown> = {};
for (const isolated of [false, true]) {
  const label = isolated ? 'isolated' : 'shared';
  const lab = join(out, 'work', label);
  const root = (name: string) => isolated ? join(lab, name, 'case') : join(lab, name);
  const general = open(root('general'), true);
  const peer = open(root('peer'));
  try {
    peer.getFileMemoryStore().upsertProjectMeta({projectName: '#1098 peer', description: '', status: 'active'});
    const names = (repo: any) => repo.listReadableProjectCatalog().map((entry: any) => entry.projectName);
    layouts[label] = {peer: names(peer), general: names(general)};
  } finally { peer.close(); general.close(); }
}
assert.deepEqual(layouts.shared, {peer: ['#1098 peer'], general: ['#1098 peer']}, '#1098 old layout discovers sibling');
assert.deepEqual(layouts.isolated, {peer: ['#1098 peer'], general: []}, '#1098 private scan root excludes sibling');

// Hold the schema read lock that a discovery query can transiently acquire.
// No write transaction, timeout tuning or sleep: the reader stays open until
// the real constructor returns, then rollback releases it for the next open.
const locked = join(out, 'work', 'reader');
mkdirSync(locked, {recursive: true});
const reader = new DatabaseSync(join(locked, 'control.sqlite'));
reader.exec('BEGIN');
reader.prepare('SELECT name FROM sqlite_master').all();
let during: unknown;
try {
  try { const repo = open(locked); repo.close(); during = {opened: true}; }
  catch (error: any) { during = {opened: false, message: error.message, code: error.code}; }
} finally { reader.exec('ROLLBACK'); reader.close(); }
assert.deepEqual(during, {opened: false, message: 'database is locked', code: 'ERR_SQLITE_ERROR'}, '#1098 reader blocks WAL initialization');
const after = open(locked);
const busyTimeout = (after as any).db.prepare('PRAGMA busy_timeout').get()?.timeout;
after.close();
const oracle = {node: process.version, sourceCommit, sourceSha256, layouts,
  reader: {during, after: {opened: true, busyTimeout}}};
mkdirSync(out, {recursive: true});
writeFileSync(join(out, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', {flag: 'wx'});
console.log(JSON.stringify(oracle, null, 2));
