import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
import { baseline } from '../env.mjs';
await import(pathToFileURL(resolve(baseline, 'e2e/real/skills.spec.mjs')));
