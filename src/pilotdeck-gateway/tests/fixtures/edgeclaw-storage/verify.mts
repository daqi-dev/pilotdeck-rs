// Rust -> TS verifier for the EdgeClaw sqlite storage fixture.
//
// The Rust suite writes a database with `EdgeClawStorage`, then runs:
//   PDRS_BASELINE=/abs/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/edgeclaw-storage/verify.mts \
//     /abs/rust.db /abs/verify.json
//
// It opens the database through the unmodified baseline `MemoryRepository`
// and writes the same observation/schema/row shapes as `generate.mts`, so the
// Rust test can compare a TS read of a Rust-written file against the TS oracle.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';
import {
  dumpRows,
  dumpSchema,
  observe,
  tableInfo,
} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the Rust-written database path');
assert.ok(process.argv[3], 'provide the JSON output path');

const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const SQLITE_TS =
  'src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const baseline = resolve(process.env.PDRS_BASELINE);
const dbPath = resolve(process.argv[2]);
const output = resolve(process.argv[3]);

const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable);
const { MemoryRepository } = await import(
  pathToFileURL(join(baseline, SQLITE_TS)).href
);

const scenarioRoot = dirname(dbPath);
const repo = new MemoryRepository(dbPath, {
  memoryDir: join(scenarioRoot, 'verify-memory'),
  globalRootDir: join(scenarioRoot, 'verify-global'),
  workspaceDir: join(scenarioRoot, 'verify-workspace'),
});
let observations: any;
try {
  observations = observe(repo);
} finally {
  repo.close();
}

const result = {
  observations,
  schema: dumpSchema(dbPath),
  columns: {
    l0_sessions: tableInfo(dbPath, 'l0_sessions'),
    pipeline_state: tableInfo(dbPath, 'pipeline_state'),
  },
  rows: dumpRows(dbPath),
  nodeVersion: process.version,
  sqliteVersion: process.versions.sqlite,
};
const text = `${JSON.stringify(result, null, 2)}\n`;
writeFileSync(output, text);
// Read it back so a write failure cannot look like success.
assert.equal(readFileSync(output, 'utf8'), text);
console.log(`verified ${dbPath} -> ${output}`);
