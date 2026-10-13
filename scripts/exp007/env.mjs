import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { existsSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
export const root = resolve(fileURLToPath(new URL('../..', import.meta.url)));
function findBaseline() {
  for (let directory = root; ; directory = dirname(directory)) {
    const candidate = resolve(directory, 'resources/PilotDeck/ui');
    if (existsSync(resolve(candidate, 'package.json'))) return candidate;
    if (dirname(directory) === directory) throw new Error('Set BASELINE_UI to a baseline ui directory with existing node_modules');
  }
}
export const baseline = process.env.BASELINE_UI ? resolve(process.env.BASELINE_UI) : findBaseline();
export const requireUI = createRequire(`${baseline}/package.json`);
export const evidence = resolve(root, '_tmp/exp007');
export const port = Number(process.env.PROBE_PORT || 28370);
if (!Number.isInteger(port) || port < 28370 || port > 28375) throw new Error('Probe requires a port in 28370–28375');
export const origin = `http://127.0.0.1:${port}`;
