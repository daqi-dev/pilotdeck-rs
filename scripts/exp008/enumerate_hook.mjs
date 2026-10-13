// exp008 / B1 runtime route enumeration. Loaded as a Node preload *before* the
// real `ui/server/index.js`, it captures the application created at
// `http.createServer(app)` and, after the module has finished registering
// routes, walks `app._router.stack` recursively:
//   * `layer.route` -> `layer.route.path` + `layer.route.methods`
//   * mounted router -> recurse into `layer.handle.stack` with the mount path
//     reconstructed from `layer.regexp` (Express 4 has no mountpath for plain
//     routers).
// The registration order is the stack order. It writes EXP008_ENUM_OUT and
// exits. Prototype patching was tried first and does not intercept Express
// 4.22's internal Router instances, so the stack walk is the source of truth.
import { createRequire } from 'node:module';
import http from 'node:http';
import { writeFileSync } from 'node:fs';

const require = createRequire(
  process.env.EXP008_BASELINE
    ? `${process.env.EXP008_BASELINE}/ui/server/index.js`
    : import.meta.url,
);
const express = require('express');

let app = null;
const origCreate = http.createServer;
http.createServer = function (...args) {
  const server = origCreate.apply(this, args);
  const listener = args.find((value) => typeof value === 'function');
  if (!app && listener && (listener._router || typeof listener.handle === 'function')) app = listener;
  return server;
};
console.error(`[exp008 enumerate] express=${require.resolve('express')} Router.prototype=${express.Router.prototype ? 'present' : 'missing'}`);

function join(...parts) {
  const joined = parts.filter((part) => part && part !== '/').join('/');
  return ('/' + joined).replace(/\/+/g, '/') || '/';
}

// Reconstruct the literal mount prefix from an Express 4 Layer regexp. Mounts
// in ui/server are literal prefixes; path params inside a mount would only be
// reported conservatively as `*`.
function mountPath(layer) {
  const regexp = layer.regexp;
  if (!regexp) return '';
  if (regexp.fast_slash || regexp.source === '^\\/?(?=\\/|$)') return '';
  let source = regexp.source;
  source = source.replace(/^\^/, '').replace(/\$$/, '');
  source = source.replace(/\\\/\?\(\?=\\\/\|\$\)/g, '/');
  source = source.replace(/\(\?=\/|\$\)/g, '');
  source = source.replace(/\(\?=[^)]*\)/g, '');
  source = source.replace(/\\\/\?$/, '/');
  source = source.replace(/\\\//g, '/');
  source = source.replace(/\\\./g, '.');
  source = source.replace(/\(\.\*\)/g, '*');
  source = source.replace(/\([^/]*\)/g, '*');
  return source || '';
}

function walk(router, prefix, out, seen, chain) {
  if (!router || seen.has(router)) return;
  seen.add(router);
  for (const layer of router.stack || []) {
    if (layer.route) {
      const full = join(prefix, layer.route.path);
      for (const method of Object.keys(layer.route.methods)) {
        out.push({ order: out.length + 1, method: method.toUpperCase(), path: full,
          handler: layer.handle && layer.handle.name, mountChain: [...chain, prefix || '/'] });
      }
    } else if (layer.handle && Array.isArray(layer.handle.stack)) {
      const nested = mountPath(layer);
      walk(layer.handle, join(prefix, nested), out, seen, [...chain, join(prefix, nested) || '/']);
    }
  }
}

setTimeout(() => {
  const routes = [];
  if (app && app._router) walk(app._router, '', routes, new Set(), []);
  const unique = new Set(routes.map((r) => `${r.method} ${r.path}`));
  const payload = {
    generatedAt: new Date().toISOString(),
    appCaptured: Boolean(app),
    routeCount: routes.length,
    uniqueRouteCount: unique.size,
    routes,
    middlewareOrUnknownLayers: app && app._router
      ? app._router.stack.filter((layer) => !layer.route && !(layer.handle && layer.handle.stack)).length
      : null,
  };
  writeFileSync(process.env.EXP008_ENUM_OUT, JSON.stringify(payload, null, 2) + '\n');
  console.error(`[exp008 enumerate] routes=${payload.routeCount} unique=${payload.uniqueRouteCount}`);
  process.exit(0);
}, 8000);
