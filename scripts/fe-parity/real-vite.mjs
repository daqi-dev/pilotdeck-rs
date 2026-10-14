import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { baseline, requireUI, ownedPath, realPorts } from './env.mjs';
const runDir = ownedPath(process.env.PILOTDECK_E2E_RUN_DIR);
const mode = process.env.PILOTDECK_E2E_MODE;
if (!['ready', 'onboarding'].includes(mode)) throw new Error('Invalid real mode');
process.chdir(baseline); // baseline aliases use process.cwd(), as in build-old.
const { createServer } = await import(pathToFileURL(requireUI.resolve('vite')));
const server = await createServer({
  root: baseline, configFile: resolve(baseline, 'vite.config.js'), configLoader: 'runner',
  cacheDir: resolve(runDir, 'vite-cache'),
  server: { host: '127.0.0.1', port: realPorts[mode].vite, strictPort: true },
});
await server.listen();
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => { void server.close().then(() => process.exit(0)); });
