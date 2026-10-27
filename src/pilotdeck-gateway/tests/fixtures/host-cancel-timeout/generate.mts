import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Real fixed-baseline oracle for the host post-cancel exit bound.
// The private DEFAULT_KILL_TIMEOUT_MS is exported in memory only; baseline
// sources are never copied into this repository.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = assertBaseline(baseline);
const source = 'src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts';
const sourceSha256 = await baselineSourceHashes(baseline, [source]);

const requireBaseline = createRequire(join(baseline, 'package.json'));
const requireTsx = createRequire(requireBaseline.resolve('tsx'));
const { transformSync } = requireTsx('esbuild');
// Appending an export changes visibility only. Compile the entire original
// file at its original filename so imports still resolve to real baseline
// modules.
const path = join(baseline, source);
const text = await readFile(path, 'utf8');
assert.ok(
  text.includes('const DEFAULT_KILL_TIMEOUT_MS = 5_000;'),
  'baseline child-close grace declaration moved; update this oracle deliberately',
);
const transformed = transformSync(text + '\nexport { DEFAULT_KILL_TIMEOUT_MS };\n', {
  loader: 'ts',
  format: 'esm',
  target: 'es2022',
  sourcefile: path,
});
const code = transformed.code.replace(
  /from (["'])(\.[^"']+)\1/g,
  (_all: string, quote: string, specifier: string) => {
    let dependency = resolve(dirname(path), specifier);
    if (dependency.endsWith('.js') && existsSync(dependency.slice(0, -3) + '.ts')) {
      dependency = dependency.slice(0, -3) + '.ts';
    }
    return 'from ' + quote + pathToFileURL(dependency).href + quote;
  },
);
const { DEFAULT_KILL_TIMEOUT_MS } = await import(
  'data:text/javascript;base64,' + Buffer.from(code).toString('base64')
);

const oracle = {
  sourceCommit: commit,
  sourceSha256,
  defaultKillTimeoutMs: DEFAULT_KILL_TIMEOUT_MS,
};
await mkdir(output, { recursive: true });
const target = join(output, 'oracle.json');
const temporary = join(output, '.oracle.json.tmp');
await writeFile(temporary, JSON.stringify(oracle, null, 2) + '\n');
await rename(temporary, target);
console.log(JSON.stringify(oracle));
