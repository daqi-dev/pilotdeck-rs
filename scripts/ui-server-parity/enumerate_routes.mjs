// S1 entry: reuse the B1 runtime Express stack walk, without lab-only routes.
// Run as --import before tsx and ui/server/index.js, with isolated env from run.py.
import { resolve } from 'node:path';
const home = process.env.PILOT_HOME;
if (!home || !process.env.PARITY_ENUM_OUT || !process.env.PARITY_BASELINE)
  throw new Error('enumeration requires isolated PILOT_HOME, PARITY_ENUM_OUT, PARITY_BASELINE');
if (process.env.HOST !== '127.0.0.1' || process.env.SERVER_PORT !== '13104')
  throw new Error('enumeration must bind 127.0.0.1:13104');
process.env.EXP008_ENUM_OUT = resolve(process.env.PARITY_ENUM_OUT);
process.env.EXP008_BASELINE = resolve(process.env.PARITY_BASELINE);
await import('../exp008/enumerate_hook.mjs');
