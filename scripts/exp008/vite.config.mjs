// exp008 wrapper: run the baseline ui/vite.config.js unchanged, but redirect
// Vite's on-disk cache/temp dirs out of the read-only baseline tree. The
// baseline config itself is not modified.
import { fileURLToPath } from 'node:url';

const baselineConfig = process.env.EXP008_BASELINE
  ? new URL(`file://${process.env.EXP008_BASELINE}/ui/vite.config.js`).href
  : new URL('file:///Users/da/ws/pilotdeck-rs/resources/PilotDeck/ui/vite.config.js').href;

const loaded = (await import(baselineConfig)).default;

export default async function (env) {
  const config = typeof loaded === 'function' ? await loaded(env) : loaded;
  return {
    ...config,
    cacheDir: process.env.EXP008_VITE_CACHE || config.cacheDir,
  };
}
