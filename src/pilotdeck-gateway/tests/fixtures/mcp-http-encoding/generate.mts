// #886: real SDK transports and baseline networkFetch, loopback only.
import assert from 'node:assert/strict';
import http from 'node:http';
import z from 'node:zlib';
import { once } from 'node:events';
import { mkdirSync, writeFileSync, readFileSync, realpathSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
assert.equal(process.version, 'v22.22.0', '#886 Node version');
assert.ok(process.env.PDRS_BASELINE, '#886 PDRS_BASELINE required');
assert.ok(process.argv[2], '#886 output directory required');
const baseline = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]);
const git = realpathSync(execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const sourceCommit = bindBaseline({ baseline, commit: 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', gitExecutable: git }).provenance.ref;
const sourceSha256 = sourceClosureSha256(baseline, sourceCommit, git,
  ['src/mcp/client/McpClient.ts', 'src/network/fetch.ts']);
const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const sdkRoot = 'node_modules/@modelcontextprotocol/sdk';
const sdk = JSON.parse(readFileSync(join(baseline, sdkRoot, 'package.json'), 'utf8')).version;
assert.equal(sdk, '1.29.0', '#886 SDK version');
const { StreamableHTTPClientTransport } = await load(`${sdkRoot}/dist/esm/client/streamableHttp.js`);
const { SSEClientTransport } = await load(`${sdkRoot}/dist/esm/client/sse.js`);
const { Protocol } = await load(`${sdkRoot}/dist/esm/shared/protocol.js`);
const { Client } = await load(`${sdkRoot}/dist/esm/client/index.js`);
const { ListToolsResultSchema } = await load(`${sdkRoot}/dist/esm/types.js`);
const { networkFetch } = await load('src/network/fetch.ts');
const reply = { jsonrpc: '2.0', id: 0, result: { tools: [{ name: 'encoded', description: '中文 é', inputSchema: { type: 'object' } }] } };
const requestTimeout = 500;
const specs = [
  ['plain', '', []], ['gzip', 'gzip', ['gzip']], ['br', 'br', ['br']],
  ['deflate', 'deflate', ['deflate']], ['mixed', 'gzip, br', ['gzip', 'br']],
  ['five', 'gzip,br,gzip,br,gzip', ['gzip', 'br', 'gzip', 'br', 'gzip']],
  ['six', 'gzip,br,gzip,br,gzip,br', ['gzip', 'br', 'gzip', 'br', 'gzip', 'br']],
  ['unknown', 'custom', []], ['mixed-unknown', 'gzip, custom', []],
  ['unknown-six', 'custom,custom,custom,custom,custom,custom', []],
  ['corrupt', 'gzip', []],
] as const;
const cases: any[] = [];
function packed(bytes: Buffer, codings: readonly string[]) {
  for (const coding of codings) bytes = coding === 'gzip' ? z.gzipSync(bytes)
    : coding === 'br' ? z.brotliCompressSync(bytes) : z.deflateSync(bytes);
  return bytes;
}
function errorShape(e: any) {
  return { name: e.name, code: e.code ?? null, message: e.message };
}
for (const mode of ['json', 'post-sse', 'get-sse', 'legacy-start', 'streamable-error', 'legacy-error', 'redirect-json']) {
  for (const [name, encoding, codings] of specs) {
    // Redirect cases exercise the terminal coding as well as the skipped
    // six-layer intermediate response. The target remains a JSON response.
    const payload = mode === 'legacy-start' ? 'event: endpoint\ndata: /post\n\n'
      : mode.endsWith('-error') ? '拒绝 é'
      : mode.endsWith('sse') ? `event: message\ndata: ${JSON.stringify(reply)}\n\n`
      : JSON.stringify(reply);
    const wire = name === 'corrupt' ? Buffer.from('invalid compressed body') : packed(Buffer.from(payload), codings);
    const server = http.createServer(async (req, res) => {
      for await (const _ of req) { /* drain each request before the response */ }
      if (mode === 'get-sse' && req.method === 'POST') { res.writeHead(202); res.end(); return; }
      if (mode === 'legacy-error' && req.method === 'GET') {
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        res.write('event: endpoint\ndata: /post\n\n');
        return; // open until transport.close(), no reconnect racing the POST
      }
      if (mode === 'redirect-json' && req.url === '/mcp') {
        res.writeHead(307, { location: '/target', 'content-encoding': 'gzip,gzip,gzip,gzip,gzip,gzip' });
        res.end('ignored intermediate'); return;
      }
      if (req.method === 'POST' && mode === 'legacy-start') { res.writeHead(202); res.end(); return; }
      const headers: Record<string, string> = {
        'content-type': mode.includes('sse') || mode === 'legacy-start' ? 'text/event-stream' : 'application/json',
        'content-length': String(wire.length),
      };
      if (encoding) headers['content-encoding'] = encoding;
      res.writeHead(mode.endsWith('-error') ? 400 : 200, headers);
      res.end(wire);
    });
    server.listen(0, '127.0.0.1'); await once(server, 'listening');
    const url = new URL(`http://127.0.0.1:${(server.address() as any).port}/mcp`);
    const legacy = mode.startsWith('legacy');
    const transport = legacy ? new SSEClientTransport(url) : new StreamableHTTPClientTransport(url, {
      fetch: (input: any, init: any) => networkFetch(input, init, {
        timeoutMs: 2000, retry: { maxRetries: 1, baseDelayMs: 500, maxDelayMs: 5000 },
      }),
    });
    const protocol = new Client({ name: 'encoding-probe', version: '1' });
    protocol.onerror = () => {}; // Protocol errors are observed through request/start.
    let result: any;
    const watchdog = setTimeout(() => { throw Error(`#886 probe watchdog ${mode}/${name}`); }, 10000);
    try {
      // Exercise the SDK protocol/transport without an unrelated initialize exchange.
      await Protocol.prototype.connect.call(protocol, transport);
      if (mode === 'legacy-start') result = { started: true };
      else if (mode === 'legacy-error') {
        await transport.send({ jsonrpc: '2.0', id: 0, method: 'tools/list' });
        result = { sent: true };
      } else {
        const pending = protocol.request({ method: 'tools/list' }, ListToolsResultSchema, { timeout: requestTimeout });
        if (mode === 'get-sse') await transport.send({ jsonrpc: '2.0', method: 'notifications/initialized' });
        result = { value: await pending };
      }
    } catch (error) { result = { error: errorShape(error) }; }
    finally {
      clearTimeout(watchdog);
      await protocol.close();
      server.closeAllConnections();
      await new Promise<void>(done => server.close(() => done()));
    }
    const row = { mode, name, encoding, wire: wire.toString('hex'), result };
    cases.push(row);
    console.log(JSON.stringify({ mode, name, result }));
  }
}
mkdirSync(output, { recursive: true });
writeFileSync(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256,
  node: process.version, sdk, requestTimeout, cases }, null, 2) + '\n', { flag: 'wx' });
