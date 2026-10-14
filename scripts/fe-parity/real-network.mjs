// Preloaded only in the owned runtime tree. No baseline source changes.
import net from 'node:net';
import { syncBuiltinESMExports } from 'node:module';
import { appendFileSync } from 'node:fs';
import { realPorts, REAL_TEST_KEY } from './env.mjs';
export function localFetchURL(input, center) {
  const url = new URL(input instanceof Request ? input.url : input);
  if (url.origin === 'https://api.openai.com' && ['/v1/models', '/models', '/v1/chat/completions', '/chat/completions'].includes(url.pathname)) {
    return new URL(url.pathname + url.search, center).href;
  }
  return url.href;
}
export function installNetworkIsolation(env = process.env) {
  const center = `http://127.0.0.1:${realPorts.mock}`;
  if (env.PILOTDECK_E2E_LLM_CENTER_URL !== center || env.PILOTDECK_E2E_API_KEY !== REAL_TEST_KEY) throw new Error('Invalid local mock environment');
  const record = entry => appendFileSync(env.FE_NETWORK_LOG, JSON.stringify({ timestamp: new Date().toISOString(), pid: process.pid,
    ppid: process.ppid, executable: process.execPath, entrypoint: process.argv[1] ?? null, ...entry }) + '\n');
  record({ action: 'runtime-process' });
  const allowed = new Set([realPorts.mock, ...Object.values(realPorts.ready), ...Object.values(realPorts.onboarding)]);
  const originalListen = net.Server.prototype.listen;
  net.Server.prototype.listen = function (...args) {
    const options = typeof args[0] === 'object' ? args[0] : { port: args[0], host: args[1] };
    if (!(options.host === '127.0.0.1' && allowed.has(Number(options.port)))) {
      record({ action: 'blocked-listen', host: options.host ?? null, port: options.port ?? null });
      throw new Error('F2 runtime listen outside fixed loopback ports blocked');
    }
    return originalListen.apply(this, args);
  };
  const originalConnect = net.Socket.prototype.connect;
  net.Socket.prototype.connect = function (...args) {
    const normalized = Array.isArray(args[0]) ? args[0] : args;
    const options = typeof normalized[0] === 'object' ? normalized[0] : { port: normalized[0], host: typeof normalized[1] === 'string' ? normalized[1] : 'localhost' };
    // IPC descriptors and Unix sockets do not create internet listeners.
    if (!options.path && !(options.host === '127.0.0.1' && allowed.has(Number(options.port)))) {
      record({ action: 'blocked-connect', host: options.host ?? null, port: options.port ?? null });
      throw new Error('F2 runtime connection outside owned loopback ports blocked');
    }
    return originalConnect.apply(this, args);
  };
  const originalFetch = globalThis.fetch;
  globalThis.fetch = function (input, init) {
    const original = input instanceof Request ? input.url : String(input);
    const target = localFetchURL(input, center);
    if (target !== original) {
      record({ action: 'redirect-openai-preset', original, target });
      return originalFetch(input instanceof Request ? new Request(target, input) : target, init);
    }
    return originalFetch(input, init);
  };
  syncBuiltinESMExports();
}
if (process.env.FE_REAL_RUNTIME === '1') installNetworkIsolation();
