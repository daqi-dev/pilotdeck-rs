// Preload BEFORE the production stdio sidecar entry; any listen regression fails.
import { Server } from 'node:net';
import fs from 'node:fs';
import { registerHooks } from 'node:module';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
Server.prototype.listen = function () { throw new Error('A5.2b stdio sidecar forbids TCP listen'); };
registerHooks({ load(url, context, next) {
  const result = next(url, context);
  if (url.startsWith('file:') && process.env.A52B_CHILD_LOADLOG) {
    const path = fileURLToPath(url);
    const bytes = fs.readFileSync(path);
    fs.appendFileSync(process.env.A52B_CHILD_LOADLOG, JSON.stringify({ path, sha256: createHash('sha256').update(bytes).digest('hex') }) + '\n');
  }
  return result;
} });
