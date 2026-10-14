/** Coordinator-only network cross-check through the production GatewayServer. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { resolve } from 'node:path';
import { Apparatus, OUTPUT, Journal, upstream, PILOTDECK_GATEWAY_PROTOCOL_VERSION, verifySources, ROOT, sha } from './runtime-b.mts';
import { CASES, REPEATS } from './preregistration-b.mjs';
import { EXTRA_CASES } from './preregistration-b-extra.mjs';
import { checkObservation, requireRule } from './rules-b.mjs';
assert.equal(process.env.A52B_COORDINATOR, '1'); assert.equal(process.env.A52B_WS, '1');
process.env.A52B_LIBRARY_ONLY = '1';
const { dual, multi } = await import('./fixtures-b.mts');
const { startGatewayServer } = await upstream('gateway/server/GatewayServer');
const { TextWebSocketConnection } = await upstream('gateway/server/websocket');
const port = Number(process.env.A52B_PORT);
assert.ok(port >= 40000 && port <= 59999);

class NetworkPeer {
  socket: WebSocket; raw: string[] = []; frames: any[] = []; binding: any; closed = false;
  constructor(readonly name: string, readonly url: string, readonly j: Journal, readonly clientName = 'web') {
    this.socket = new WebSocket(url);
    this.socket.addEventListener('message', (event: any) => {
      const text = String(event.data); this.raw.push(text); this.frames.push(JSON.parse(text));
      this.j.add('wire.client', { connection: name, text });
    });
    this.socket.addEventListener('error', () => this.j.add('wire.client.error', { connection: name }));
    this.socket.addEventListener('close', () => { this.closed = true; this.j.add('wire.client.closed', { connection: name }); });
  }
  async hello() {
    if (this.socket.readyState !== WebSocket.OPEN) await new Promise<void>((resolve, reject) => {
      this.socket.addEventListener('open', () => resolve(), { once: true });
      this.socket.addEventListener('error', () => reject(new Error('owned WS client failed to open')), { once: true });
    });
    this.socket.send(JSON.stringify({ type: 'hello', protocolVersion: PILOTDECK_GATEWAY_PROTOCOL_VERSION,
      clientName: this.clientName, clientVersion: 'fixture', token: 'fixture-token' }));
    const row = await this.j.wait(r => r.kind === 'wire.client' && r.connection === this.name && JSON.parse(r.text).type === 'hello_ok');
    this.binding = JSON.parse(row.text).interactionBinding;
  }
  send(id: string, method: string, params: any) {
    this.j.add('wire.receive', { connection: this.name, id, method, params });
    this.socket.send(JSON.stringify({ type: 'request', id, method, params }));
  }
  async request(id: string, method: string, params: any) {
    this.send(id, method, params);
    const row = await this.j.wait(r => r.kind === 'wire.client' && r.connection === this.name && JSON.parse(r.text).type === 'response' && JSON.parse(r.text).id === id);
    return JSON.parse(row.text);
  }
  async final(id: string) {
    const row = await this.j.wait(r => r.kind === 'wire.client' && r.connection === this.name && JSON.parse(r.text).id === id && JSON.parse(r.text).final);
    return JSON.parse(row.text);
  }
  async close() {
    if (this.closed) return;
    this.socket.close(); await this.j.wait(r => r.kind === 'wire.client.closed' && r.connection === this.name);
  }
  observation() { return { name: this.name, binding: this.binding, clientName: this.clientName, raw: this.raw, frames: this.frames }; }
}
const cases = [...CASES, ...EXTRA_CASES].filter(c => c.id.startsWith('dual/') || c.id.startsWith('multi/'));
const observations: any[] = [];
for (const c of cases) for (let repetition = 1; repetition <= REPEATS; repetition++) {
  const a = new Apparatus(resolve(OUTPUT, c.id, String(repetition)), { kind: c.kind ?? (c.order === 'sdk-control' ? 'permission' : undefined) });
  const transportNames = new Map<any, string>(); let opening = '';
  const realSend = TextWebSocketConnection.prototype.sendText;
  TextWebSocketConnection.prototype.sendText = function(text: string) {
    if (!transportNames.has(this)) transportNames.set(this, opening);
    a.j.add('wire.send', { connection: transportNames.get(this), text, closed: this.closed });
    return realSend.call(this, text);
  };
  let server: any;
  const watchdog = setTimeout(() => {
    fs.writeFileSync(resolve(a.dir, 'watchdog.json'), JSON.stringify(a.j.rows, null, 2));
    process.exit(2);
  }, 30000);
  try {
    server = await startGatewayServer({ gateway: a.gateway, host: '127.0.0.1', port, token: 'fixture-token', serverVersion: 'baseline-fixture' });
    a.j.add('listener', { host: '127.0.0.1', port, pid: process.pid });
    a.peer = async (name: string, clientName = 'web') => {
      opening = name; const peer = new NetworkPeer(name, server.wsUrl, a.j, clientName);
      a.peers.push(peer as any); await peer.hello(); opening = ''; return peer as any;
    };
    const barriers = c.id.startsWith('dual/') ? await dual(a, c) : await multi(a, c);
    const observation = await a.observe(c.id, repetition, ['A', 'B'], barriers);
    checkObservation(observation); observations.push(observation);
    fs.writeFileSync(resolve(a.dir, 'observation.json'), JSON.stringify(observation, null, 2));
  } finally {
    for (const peer of a.peers) await peer.close();
    if (server) await server.close();
    await a.close(); clearTimeout(watchdog); TextWebSocketConnection.prototype.sendText = realSend;
  }
}
requireRule(observations.length === cases.length * REPEATS, 'COVERAGE');
const manifest = { host: '127.0.0.1', port, pid: process.pid, expectedCases: cases.map(c => c.id),
  executedCases: [...new Set(observations.map(o => o.caseId))], repeats: REPEATS, measurements: observations.length,
  preregistrationSha256: sha(fs.readFileSync(resolve(ROOT, 'scripts/a5-19/preregistration-b.mjs'))),
  importedSources: verifySources(), bridgeReconnect: 'NOT_RUN: requires original bridge, separate D5 gate',
  closeRace: 'no-network production dispatcher evidence remains separate; this cross-check covers dual/multi only' };
fs.writeFileSync(resolve(OUTPUT, 'manifest.json'), JSON.stringify(manifest, null, 2));
console.log(`DONE WS ${observations.length}`);
