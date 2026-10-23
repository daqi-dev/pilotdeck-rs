// #552 real-TS golden for the bash runner's detached child
// (bash/commandRunner.ts:43 `detached: !isWindows`): what the shell it spawns
// reports about its session, process group, controlling terminal and
// /dev/tty — recorded while the runner's parent demonstrably owns a
// controlling terminal. The probe runs under `script -q /dev/null` so this
// generator is reproducible regardless of its own terminal. Run with the
// fixed baseline checkout and Node 22 (same as bash-tool):
//   PDRS_BASELINE=/abs/fixed/PilotDeck \
//     node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/bash-session/generate.mts /abs/output-parent
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { assertBaseline } from '../_shared/baseline.mts';

assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide an output directory');
assert.equal(process.platform, 'darwin', 'the corpus is recorded on macOS (/bin/bash 3.2, ps "??" for no controlling terminal, ENXIO=6)');
const baseline = resolve(process.env.PDRS_BASELINE!);
const commit = assertBaseline(baseline);
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const sourceSha256 = sourceClosureSha256(baseline, commit, git, ['src/tool/builtin/bash/commandRunner.ts']);

// script(1) gives the probe (and therefore the runner's parent) a controlling
// terminal; stdin is /dev/null because script's tcgetattr on a socket fails.
const scratch = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-bash-session-gen-'));
const probeOut = join(scratch, 'probe.json');
try {
  execFileSync('script', [
    '-q', '/dev/null', process.execPath, '--import',
    join(baseline, 'node_modules/tsx/dist/loader.mjs'), '--',
    join(dirname(fileURLToPath(import.meta.url)), 'probe.mjs'), probeOut,
  ], { env: { ...process.env, PDRS_BASELINE: baseline }, stdio: ['ignore', 'pipe', 'pipe'] });
  const probe = JSON.parse(await readFile(probeOut, 'utf8'));
  assert.equal(probe.parentDevTty, 'open', 'the probe must run with a parent controlling terminal (script -q /dev/null)');
  const oracle = { sourceCommit: commit, sourceSha256, platform: 'darwin', parentDevTty: probe.parentDevTty, cases: probe.cases };
  await mkdir(resolve(process.argv[2]), { recursive: true });
  await writeFile(join(resolve(process.argv[2]), 'oracle.json'), `${JSON.stringify(oracle, null, 2)}\n`, { flag: 'wx' });
} finally {
  await rm(scratch, { recursive: true, force: true });
}
assertBaseline(baseline);
