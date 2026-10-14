// Build the actual baseline Vite entry, not exp007's isolated React fixture.
import { mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { baseline, oldDist, requireUI } from './env.mjs';
await mkdir(oldDist, { recursive: true });
process.chdir(baseline); // baseline config resolves React aliases using cwd.
process.env.PILOTDECK_DISABLE_LOCAL_AUTH = '0';
process.env.VITE_IS_PLATFORM = 'false'; // OSS auth mock must not inherit a platform .env.
process.env.HOST = '127.0.0.1';
const { build } = await import(pathToFileURL(requireUI.resolve('vite')));
await build({ root: baseline, cacheDir: resolve(oldDist, '../vite-cache'), configFile: resolve(baseline, 'vite.config.js'), configLoader: 'runner', build: { outDir: oldDist, emptyOutDir: true } });
console.log(`BASELINE_VITE_BUILD ${oldDist}`);
