import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { existsSync } from 'node:fs';
import { dirname, resolve, sep } from 'node:path';
export const root = fileURLToPath(new URL('../../', import.meta.url));
function findBaseline() {
  for (let directory = root; ; directory = dirname(directory)) {
    for (const relative of ['ui', 'resources/PilotDeck/ui']) {
      const candidate = resolve(directory, relative);
      if (existsSync(resolve(candidate, 'e2e')) && existsSync(resolve(candidate, 'package.json'))) return candidate;
    }
    if (dirname(directory) === directory) throw new Error('Set BASELINE_UI to the baseline ui directory');
  }
}
export const baseline = process.env.BASELINE_UI ? resolve(process.env.BASELINE_UI) : findBaseline();
export const requireUI = createRequire(resolve(baseline, 'package.json'));
export const ports = Object.freeze({ mock: 28376, old: 28377, new: 28378 });
export const origin = frontend => `http://127.0.0.1:${ports[frontend]}`;
export const oldDist = resolve(root, '_build/fe-parity/old');
export const newDist = resolve(root, '_build/dist-leptos');
export const historicalInventory = resolve(root, 'docs/experiments/exp007-frontend-probe/evidence/e2e-inventory.json');
export function ownedPath(path) {
  const absolute = resolve(path);
  if (!absolute.startsWith(resolve(root, '_tmp/fe-parity') + sep)) throw new Error('Output must be under this worktree _tmp/fe-parity/');
  return absolute;
}
// Public fixed test credential. Never a real provider secret.
export const REAL_TEST_KEY = 'fe-parity-mock-key';
export const realPorts = Object.freeze({
  ready: Object.freeze({ server: 3010, vite: 5180, gateway: 18798 }),
  onboarding: Object.freeze({ server: 3011, vite: 5181, gateway: 18799 }),
  mock: 28379,
});
export const realOrigin = mode => `http://127.0.0.1:${realPorts[mode].vite}`;
export const realRunRoot = resolve(root, '_tmp/fe-parity/real-run');
