// Real TS parser/RPC consumer; an in-memory socket only, never a listener.
import { EventEmitter } from 'node:events';
import { writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';

const baseline = resolve(process.argv[2]);
const load = async (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { TextWebSocketConnection } = await load('src/gateway/server/websocket.ts');
const { GatewayWsConnection } = await load('src/gateway/server/GatewayWsConnection.ts');
const { InProcessGateway } = await load('src/gateway/client/InProcessGateway.ts');

class Socket extends EventEmitter {
  writes: Buffer[] = [];
  write(bytes: Buffer) { this.writes.push(Buffer.from(bytes)); return true; }
  end() {}
  take() { const result = Buffer.concat(this.writes); this.writes = []; return result; }
}

function client(payload: string) {
  const bytes = Buffer.from(payload);
  const header = Buffer.alloc(8);
  header[0] = 0x81;
  header[1] = 0x80 | 126;
  header.writeUInt16BE(bytes.length, 2);
  const mask = Buffer.from([0x11, 0x22, 0x33, 0x44]);
  mask.copy(header, 4);
  for (let index = 0; index < bytes.length; index++) bytes[index] ^= mask[index % 4];
  return Buffer.concat([header, bytes]);
}

const tick = () => new Promise<void>(resolve => setImmediate(resolve));
const forwarded = client('{"type":"request","id":"forwarded","method":"describe_server"}');
const local = client('{"type":"request","id":"local","method":"supported_agents"}');
const cases = [];
for (const delayed of [false, true]) {
  const socket = new Socket();
  const ws = new TextWebSocketConnection(socket as any);
  let authenticated = false;
  let release!: (value: object) => void;
  const pending = new Promise<object>(resolve => { release = resolve; });
  new GatewayWsConnection(ws, {
    token: 'offline', serverVersion: 'offline',
    gateway: {
      describeServer: () => authenticated && delayed ? pending : Promise.resolve({}),
      supportedAgents: () => InProcessGateway.prototype.supportedAgents.call({}),
    } as any,
  });
  socket.emit('data', client(JSON.stringify({
    type: 'hello', protocolVersion: '1.2', clientName: 'web',
    clientVersion: 'offline', token: 'offline',
  })));
  await tick();
  socket.take();
  authenticated = true;
  const input = Buffer.concat([forwarded, local]);
  socket.emit('data', input);
  await tick();
  const beforeRelease = socket.take();
  release({});
  await tick();
  const afterRelease = socket.take();
  cases.push({
    name: delayed ? 'delayed-forward' : 'immediate-forward',
    inputHex: input.toString('hex'),
    beforeReleaseHex: beforeRelease.toString('hex'),
    afterReleaseHex: afterRelease.toString('hex'),
    outputHex: Buffer.concat([beforeRelease, afterRelease]).toString('hex'),
  });
}
await writeFile(process.argv[3], JSON.stringify(cases, null, 2) + '\n');
