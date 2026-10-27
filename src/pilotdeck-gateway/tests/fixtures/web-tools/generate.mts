// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
// Real TS web_fetch / web_search (and their web/* helpers) against an owned
// loopback HTTP/HTTPS mock. Inputs are in cases.mjs.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { createServer as createHttpServer, STATUS_CODES, type IncomingMessage, type ServerResponse } from 'node:http';
import { createServer as createHttpsServer } from 'node:https';
import { register } from 'node:module';
import { createServer as createNetServer } from 'node:net';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
import * as cases from './cases.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the output directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8');
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const sourceCommit = bindBaseline({ baseline, commit: BASELINE_COMMIT, gitExecutable }).provenance.ref;
const ENTRIES = ['src/tool/builtin/webFetch.ts', 'src/tool/builtin/webSearch.ts'];
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, ENTRIES);
// networkFetch jitters retries with Math.random; pin it so delays replay.
Math.random = () => 0.5;
// `?no-turndown` module graphs resolve `turndown` as if it were not installed.
register('./no-turndown.mjs', import.meta.url);
const src = (path: string) => pathToFileURL(join(baseline, 'src', path)).href;
const { createWebFetchTool } = await import(src('tool/builtin/webFetch.ts'));
const masked = await import(`${src('tool/builtin/webFetch.ts')}?no-turndown`);
const { createWebSearchTool } = await import(src('tool/builtin/webSearch.ts'));
const { validateURL, upgradeHttpToHttps, isPermittedRedirect } = await import(src('tool/builtin/web/urlValidation.ts'));
const { isPreapprovedUrl, PREAPPROVED_ENTRIES } = await import(src('tool/builtin/web/preapprovedHosts.ts'));
const { makeSecondaryModelPrompt } = await import(src('tool/builtin/web/secondaryPrompt.ts'));
const { URL_CACHE, WebFetchUrlCache, clearWebFetchCache } = await import(src('tool/builtin/web/urlContentCache.ts'));
const { normalizeToolError } = await import(src('tool/protocol/errors.ts'));
// The masked graph has its own PilotDeckToolRuntimeError class; normalize its
// throws with its own normalizeToolError.
const maskedErrors = await import(`${src('tool/protocol/errors.ts')}?no-turndown`);
assert.notEqual(maskedErrors.normalizeToolError, normalizeToolError);
const undici = await import(pathToFileURL(join(baseline, 'node_modules/undici/index.js')).href);
assert.notEqual(masked.createWebFetchTool, createWebFetchTool, 'the no-turndown graph is a separate instance');

// fetch/undici transport headers, excluded on both sides. A user-agent is kept
// unless it is the fetch default (web_fetch sets its own).
const TRANSPORT_HEADERS = ['host', 'connection', 'content-length', 'accept-language', 'sec-fetch-mode',
  'accept-encoding', 'transfer-encoding', 'keep-alive'];
const DEFAULT_USER_AGENTS = ['node', 'undici'];

type Reply = { status: number; reason?: string; headers: Record<string, string>; body?: string; hex?: string; repeat?: [string, number] };
type Exchange = { replies: Reply[]; requests: any[] };
let current: Exchange = { replies: [], requests: [] };
const replyBytes = (reply: Reply) => reply.repeat ? Buffer.alloc(reply.repeat[1], reply.repeat[0])
  : reply.hex !== undefined ? Buffer.from(reply.hex.replace(/ /g, ''), 'hex') : Buffer.from(reply.body ?? '', 'utf8');
function handler(req: IncomingMessage, res: ServerResponse) {
  const body: Buffer[] = [];
  req.on('data', (part) => body.push(part));
  req.on('end', () => {
    const headers = Object.entries(req.headers)
      .filter(([name, value]) => !TRANSPORT_HEADERS.includes(name)
        && !(name === 'user-agent' && DEFAULT_USER_AGENTS.includes(String(value))))
      .map(([name, value]) => [name, String(value)])
      .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
    current.requests.push({ method: req.method, path: req.url, headers, body: Buffer.concat(body).toString('utf8') });
    const reply = current.replies[current.requests.length - 1];
    assert.ok(reply, 'unexpected extra request');
    const bytes = replyBytes(reply);
    res.on('error', () => {});
    res.writeHead(reply.status, reply.reason ?? STATUS_CODES[reply.status] ?? 'unknown',
      { ...reply.headers, 'content-length': String(bytes.length), connection: 'close' });
    res.end(bytes);
  });
}

const listen = async (server: any) => { await new Promise<void>((r) => server.listen(0, '127.0.0.1', r)); return server.address().port; };
const material = JSON.parse(readFileSync(new URL('../model-stream/tls.json', import.meta.url), 'utf8'));
const http = createHttpServer(handler);
const tls = createHttpsServer({ key: material.key, cert: material.cert }, handler);
const httpPort = await listen(http);
const tlsPort = await listen(tls);
const probe = createNetServer();
const closedPort = await listen(probe);
await new Promise<void>((r) => probe.close(() => r()));
undici.setGlobalDispatcher(new undici.Agent({ connect: { ca: Buffer.from(material.ca) } }));
// Longest first, so one origin that prefixes another never splits it.
const origins: [string, string][] = ([
  [`https://127.0.0.1:${tlsPort}`, '$TLS'], [`http://127.0.0.1:${httpPort}`, '$PLAIN'],
  [`http://127.0.0.1:${tlsPort}`, '$UPGRADE'], [`http://127.0.0.1:${closedPort}`, '$CLOSED'],
] as [string, string][]).sort((a, b) => b[0].length - a[0].length);
const real = (text: string) => origins.reduce((acc, [origin, name]) => acc.split(name).join(origin), text);
const scrub = (text: string) => origins.reduce((acc, [origin, name]) => acc.split(origin).join(name), text);
const realValue = (value: unknown) => value === undefined ? undefined : JSON.parse(real(JSON.stringify(value)));
const scrubValue = (value: unknown) => value === undefined ? undefined : JSON.parse(scrub(JSON.stringify(value)));
const hashUnits = (text: string) => {
  const units = Buffer.alloc(text.length * 2);
  for (let i = 0; i < text.length; i++) units.writeUInt16LE(text.charCodeAt(i), i * 2);
  return { sha256: createHash('sha256').update(units).digest('hex'), units: text.length };
};
const tool = (t: any) => ({ name: t.name, aliases: t.aliases, description: t.description, kind: t.kind,
  inputSchema: t.inputSchema, maxResultBytes: t.maxResultBytes, isReadOnly: t.isReadOnly(), isConcurrencySafe: t.isConcurrencySafe(),
  isOpenWorld: t.isOpenWorld() });

async function run(execute: () => Promise<any>, normalize = normalizeToolError) {
  try {
    const out = await execute();
    return { ok: { content: out.content, data: out.data, metadata: out.metadata } };
  } catch (error) {
    const normalized = normalize(error);
    return { error: { code: normalized.code, message: normalized.message, details: normalized.details } };
  }
}
function hashed(result: any) {
  for (const item of result.ok?.content ?? []) if (typeof item.text === 'string') item.text = hashUnits(item.text);
  return result;
}
function recordedReplies(replies: Reply[]) {
  return replies.map((reply) => ({ ...reply, reason: reply.reason ?? STATUS_CODES[reply.status] ?? 'unknown' }));
}
function makeModel(spec: any, controller: AbortController, requests: unknown[]) {
  if (spec.model === undefined) return undefined;
  const events = Array.isArray(spec.model) ? spec.model : spec.model.events;
  const thrown = Array.isArray(spec.model) ? undefined : spec.model.throw;
  return {
    async *stream(request: unknown) {
      requests.push(request);
      for (const event of events) {
        if (spec.abortDuringModel) controller.abort();
        yield event;
      }
      if (thrown) {
        if (!thrown.error) throw thrown.value;
        const { error: _flag, message, error_, ...props } = thrown;
        throw Object.assign(new Error(message), props, error_ === undefined ? {} : { error: error_ });
      }
    },
  };
}

let requestCount = 0;
function begin(replies: Reply[]) {
  current = { replies: realValue(replies) as Reply[], requests: [] };
}
function finish(name: string, replies: Reply[]) {
  assert.equal(current.requests.length, replies.length, `${name}: every scripted reply is consumed exactly once`);
  requestCount += current.requests.length;
  return scrubValue(current.requests);
}

const fetches = [];
for (const spec of cases.fetches as any[]) {
  clearWebFetchCache();
  if (spec.cacheSeed) {
    const content = spec.cacheSeed.content;
    URL_CACHE.set(spec.cacheSeed.url, { bytes: Buffer.byteLength(content), code: 200, codeText: 'OK', content,
      contentType: 'text/markdown' }, Buffer.byteLength(content));
  }
  begin(spec.replies);
  const controller = new AbortController();
  const modelRequests: unknown[] = [];
  const model = makeModel(spec, controller, modelRequests);
  const factory = spec.turndownMissing ? masked.createWebFetchTool : createWebFetchTool;
  const webFetch = factory(spec.modelVia === 'context' ? {} : { model });
  const context = { cwd: '/w', sessionId: 's', env: {}, abortSignal: controller.signal,
    ...(spec.modelVia === 'context' ? { model } : {}) };
  const normalize = spec.turndownMissing ? maskedErrors.normalizeToolError : normalizeToolError;
  const results = [await run(() => webFetch.execute(realValue(spec.input), context), normalize)];
  if (spec.repeatCall) results.push(await run(() => webFetch.execute(realValue(spec.input), context), normalize));
  if (spec.secondInput) results.push(await run(() => webFetch.execute(realValue(spec.secondInput), context), normalize));
  const requests = finish(spec.name, spec.replies);
  fetches.push({ ...spec, replies: recordedReplies(spec.replies), requests, modelRequests: scrubValue(modelRequests),
    results: scrubValue(spec.hashOnly ? results.map(hashed) : results) });
}

const searches = [];
for (const spec of cases.searches as any[]) {
  begin(spec.replies);
  const options = realValue(spec.options);
  const webSearch = createWebSearchTool(options);
  const context = { cwd: '/w', sessionId: 's', env: realValue(spec.env ?? {}) };
  const result = await run(() => webSearch.execute(spec.input, context));
  const requests = finish(spec.name, spec.replies);
  searches.push({ ...spec, replies: recordedReplies(spec.replies), requests, result: scrubValue(result) });
}
http.close();
tls.close();
http.closeAllConnections?.();
tls.closeAllConnections?.();

const fetchTool = createWebFetchTool({});
const searchTool = createWebSearchTool({});
const validations = [];
for (const input of cases.validations) validations.push({ input, result: await fetchTool.validateInput(input, { cwd: '/w' }) });
const availability = (cases.availability as any[]).map((spec) => ({ ...spec,
  result: createWebSearchTool(spec.options).checkAvailability({ cwd: '/w', env: spec.env }) }));

// WebFetchUrlCache with Date.now pinned per operation; the private LRU map
// and byte total are read after each step.
const cache = new WebFetchUrlCache();
const realNow = Date.now;
const cacheOps = [];
for (const op of cases.cacheOps as any[]) {
  Date.now = () => op.at;
  let result: unknown = null;
  if (op.op === 'set') cache.set(op.url, { bytes: 1, code: 200, codeText: 'OK', content: op.content, contentType: 'text/plain' }, op.bytes);
  else if (op.op === 'get') result = cache.get(op.url)?.content ?? null;
  else cache.clear();
  cacheOps.push({ ...op, result, keys: [...(cache as any).map.keys()], totalBytes: (cache as any).totalBytes });
}
Date.now = realNow;

const oracle = {
  sourceCommit, sourceSha256, node: process.version, v8: process.versions.v8,
  undici: JSON.parse(readFileSync(join(baseline, 'node_modules/undici/package.json'), 'utf8')).version,
  locale, random: 0.5, transportHeaders: TRANSPORT_HEADERS, defaultUserAgents: DEFAULT_USER_AGENTS,
  tools: { webFetch: tool(fetchTool), webSearch: tool(searchTool) },
  permissions: { webFetch: await fetchTool.checkPermissions(), webSearch: await searchTool.checkPermissions() },
  urls: cases.urls.map((url: string) => ({ url, valid: validateURL(url), preapproved: isPreapprovedUrl(url) })),
  upgrades: cases.upgrades.map((url: string) => {
    try { return { url, upgraded: upgradeHttpToHttps(url).upgraded }; } catch (error: any) { return { url, thrown: error.message }; }
  }),
  redirects: cases.redirects.map(([from, to]: [string, string]) => ({ from, to, permitted: isPermittedRedirect(from, to) })),
  preapproved: cases.preapproved.map((url: string) => ({ url, preapproved: isPreapprovedUrl(url) })),
  preapprovedEntries: PREAPPROVED_ENTRIES.map((entry) => ({
    host: entry.host,
    pathPrefix: entry.pathPrefix,
    allowSubdomains: entry.allowSubdomains,
  })),
  prompts: cases.prompts.map(([md, prompt, pre]: [string, string, boolean]) => ({ md, prompt, pre, text: makeSecondaryModelPrompt(md, prompt, pre) })),
  validations, availability, cacheOps, fetches, searches,
};
assert.equal(fetches.length, 48);
assert.equal(searches.length, 43);
assert.equal(cacheOps.length, 17);
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 1) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ output: join(output, 'oracle.json'), fetches: fetches.length, searches: searches.length, requests: requestCount }));
