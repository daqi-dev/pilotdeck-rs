// Real baseline SDK transport; capture bounded large single-frame observations.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync, writeFileSync, readFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { once } from 'node:events';
import { createHash } from 'node:crypto';
assert.equal(process.version, 'v22.22.0', '#983 transport probe Node version');
assert.ok(process.env.PDRS_BASELINE, '#983 set PDRS_BASELINE');
const baseline = resolve(process.env.PDRS_BASELINE);
const require = createRequire(join(baseline, 'package.json'));
const { register } = await import(pathToFileURL(require.resolve('tsx/esm/api')));
const unregister = register();
const { GatewayTransport } = await import(pathToFileURL(join(baseline, 'packages/sdk/src/transport.ts')));
const Ws = require('ws');
const Native = globalThis.WebSocket;
assert.notEqual(Native, Ws, '#983 SDK default is Node native WebSocket, not ws');
const server = new Ws.WebSocketServer({host:'127.0.0.1', port:0, perMessageDeflate:false});
const deadline = setTimeout(() => { console.error('#983 bounded TS transport probe'); process.exit(1); }, 30000);
const cases = [];
let constructorArgCount;
let wsDefaultMaxPayload;
let client;
let active;
let sendBytes;
let hello;
server.on('connection', socket => {
  socket.on('error', () => {}); // ws rejection is observed at the client below.
  socket.on('message', data => {
    const request = JSON.parse(data.toString());
    if (request.type === 'hello') {
      hello = request;
      socket.send(JSON.stringify({type:'hello_ok', protocolVersion:'1.2', serverVersion:'test', serverInfo:{}}));
    } else {
      assert.equal(request.method, 'list_sessions', '#983 actual SDK ordinary RPC');
      // Trailing JSON whitespace makes the frame large without large CLI stdout.
      const raw = JSON.stringify({type:'response', id:request.id, ok:true, result:{sessions:[]}});
      assert.ok(sendBytes > Buffer.byteLength(raw), '#983 bounded payload target');
      socket.send(raw + ' '.repeat(sendBytes - Buffer.byteLength(raw)), {compress:false, fin:true});
    }
  });
});
try {
  await once(server, 'listening');
  const url = `ws://127.0.0.1:${server.address().port}/ws`;
  for (const [implementation, Constructor, sizes] of [
    ['node-native', Native, [16 * 1024 * 1024 + 1, 100 * 1024 * 1024 + 1]],
    ['ws-default', Ws, [16 * 1024 * 1024 + 1, 100 * 1024 * 1024 + 1]],
  ]) {
    globalThis.WebSocket = new Proxy(Constructor, {construct(target, args) {
      assert.equal(args.length, 1, '#983 SDK supplies only its URL');
      assert.equal(args[0], url, '#983 SDK constructor URL');
      constructorArgCount = args.length;
      active = Reflect.construct(target, args);
      return active;
    }});
    for (const frameBytes of sizes) {
      sendBytes = frameBytes;
      client = new GatewayTransport({url, token:'d1-fixture-not-a-secret', clientVersion:'pilotdeck-cli'});
      await client.connect();
      if (implementation === 'ws-default') {
        wsDefaultMaxPayload = active._receiver._maxPayload;
        assert.equal(wsDefaultMaxPayload, 100 * 1024 * 1024, '#983 measured ws maxPayload');
      }
      let accepted = false;
      let observedResult;
      try {
        const result = await client.request('list_sessions', {});
        observedResult = result;
        accepted = true;
      } catch (error) {
        if (implementation !== 'ws-default' || frameBytes <= wsDefaultMaxPayload) throw error;
        assert.equal(error.code, 'transport_error', '#983 ws rejection is a transport error');
        assert.equal(error.message, 'Gateway WebSocket closed.', '#983 ws rejection closes the socket');
      } finally {
        client.close();
      }
      assert.equal(accepted, implementation === 'node-native' || frameBytes <= wsDefaultMaxPayload,
        '#983 actual implementation large-frame outcome');
      if (accepted) assert.deepEqual(observedResult, {sessions:[]}, '#983 large response projection');
      cases.push({implementation, frameBytes, accepted, ...(accepted ? {result:observedResult} : {})});
      console.log(JSON.stringify({implementation, frameBytes, accepted, constructorArgCount,
        ...(implementation === 'ws-default' ? {maxPayload:wsDefaultMaxPayload} : {})}));
    }
  }
  const source = readFileSync(join(baseline, 'packages/sdk/src/transport.ts'));
  const oracle = {nodeVersion:process.version, undiciVersion:process.versions.undici,
    transportSha256:createHash('sha256').update(source).digest('hex'), constructorArgCount,
    defaultImplementation:'node-native', wsVersion:require('ws/package.json').version,
    wsDefaultMaxPayload, protocolVersion:hello.protocolVersion, cases};
  const output = resolve(process.argv[2]);
  mkdirSync(output, {recursive:true});
  writeFileSync(join(output,'oracle.json'), JSON.stringify(oracle,null,2)+'\n', {flag:'wx'});
} finally {
  clearTimeout(deadline);
  globalThis.WebSocket = Native;
  client?.close();
  for (const socket of server.clients) socket.terminate();
  await new Promise(resolve => server.close(resolve));
  unregister();
}
