import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { baseline, requireUI } from './env.mjs';
import { assertDeployExecutionEnv, deployPorts } from './deploy-matrix.mjs';

// Generate in memory at the frozen module's exact resolution identity. Relative
// imports resolve to baseline components; no baseline generated file is written.
export async function compositionPlugin(configPath) {
  const { parse } = requireUI('yaml');
  const { renderGeneratedEntrypoint } = await import(pathToFileURL(resolve(baseline, '../scripts/generate-frontend-modules.mjs')));
  const generated = resolve(baseline, 'src/composition/generated/frontend-modules.ts');
  const source = renderGeneratedEntrypoint(parse(await readFile(configPath, 'utf8')), generated);
  return { name: 'fe-parity-isolated-composition', enforce: 'pre',
    load(id) { if (id.split('?')[0] === generated) return source; } };
}
async function main() {
  assertDeployExecutionEnv(process.env);
  process.chdir(baseline);
  const { createServer } = await import(pathToFileURL(requireUI.resolve('vite')));
  const server = await createServer({ root: baseline, configFile: resolve(baseline, 'vite.config.js'), configLoader: 'runner',
    envDir: process.env.PILOTDECK_E2E_RUN_DIR,
    cacheDir: resolve(process.env.PILOTDECK_E2E_RUN_DIR, 'vite-cache'),
    plugins: [await compositionPlugin(process.env.PILOTDECK_CONFIG_PATH)],
    server: { host: '127.0.0.1', port: deployPorts.vite, strictPort: true } });
  await server.listen();
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => { void server.close().then(() => process.exit(0)); });
}
if (process.argv[1] === new URL(import.meta.url).pathname) await main();
