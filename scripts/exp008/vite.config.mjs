// exp008 wrapper: run the baseline ui/vite.config.js unchanged, but redirect
// Vite's on-disk cache/temp dirs out of the read-only baseline tree. The
// baseline config itself is not modified.
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

// 显式 EXP008_BASELINE（run.py 传入）> PDRS_BASELINE > 本 checkout 的
// resources/PilotDeck；缺失时 BLOCKED，不回落到主仓绝对路径（#195）。
const localBaseline = fileURLToPath(new URL('../../resources/PilotDeck', import.meta.url));
const baseline = process.env.EXP008_BASELINE || process.env.PDRS_BASELINE || localBaseline;
const baselineConfig = path.join(baseline, 'ui/vite.config.js');
if (!existsSync(baselineConfig)) {
  throw new Error(`BLOCKED: baseline vite config not found at ${baselineConfig}; `
    + 'pass --baseline or set PDRS_BASELINE');
}

const loaded = (await import(pathToFileURL(baselineConfig).href)).default;

export default async function (env) {
  const config = typeof loaded === 'function' ? await loaded(env) : loaded;
  return {
    ...config,
    cacheDir: process.env.EXP008_VITE_CACHE || config.cacheDir,
  };
}
