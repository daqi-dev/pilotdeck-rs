import http from 'node:http';
import { requireUI, ports } from './env.mjs';
import { createStream, SESSION_ID } from './stream.mjs';
export const TOKEN = 'f2-parity-token';
const user = { id: 1, username: 'probe', is_admin: true };
const session = { id: 's', summary: 'F2 session', provider: 'pilotdeck', createdAt: '2026-10-14T00:00:00Z', updatedAt: '2026-10-14T00:00:00Z' };
export function createMock({ streamClock = {} } = {}) {
  const watched = new Set();
  const stats = { requests: [], connections: [], outbound: [], historyResponses: [], errors: [] };
  const stream = createStream({ ...streamClock, send: message => {
    let recipients = 0;
    for (const socket of watched) if (socket.readyState === 1) { socket.send(JSON.stringify(message)); recipients++; }
    return recipients;
  } });
  async function handleHTTP(req, res) {
    const url = new URL(req.url, `http://127.0.0.1:${ports.mock}`);
    const json = (body, status = 200) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(body)); };
    stats.requests.push({ path: url.pathname, method: req.method, authorization: req.headers.authorization || null });
    if (url.pathname === '/__parity/stats') return json(stats);
    if (url.pathname === '/__parity/stream') {
      if (req.method === 'GET') return json({ ...stream.state, watchers: [...watched].filter(socket => socket.readyState === 1).length });
      if (req.method !== 'POST') return json({ error: 'Method not allowed' }, 405);
      const action = url.searchParams.get('action');
      if (action === 'reset') { stream.reset(); return json({ status: 'idle' }); }
      if (action === 'stop') { stream.stop(); return json(stream.state); }
      if (action !== 'start') return json({ error: 'Unknown stream action' }, 400);
      if (![...watched].some(socket => socket.readyState === 1)) return json({ error: 'No watched browser session' }, 409);
      try { return json(stream.start(url.searchParams.get('scenario'), url.searchParams.get('protocol') || 'legacy'), 202); }
      catch (error) { return json({ error: error.message }, 400); }
    }
    if (url.pathname === '/api/auth/status') return json({ needsSetup: false, authDisabled: false });
    if (url.pathname === '/api/auth/login') {
      if (req.method !== 'POST') return json({ error: 'Method not allowed' }, 405);
      let body = '';
      for await (const chunk of req) { body += chunk; if (body.length > 16384) return json({ error: 'Too large' }, 413); }
      let data; try { data = JSON.parse(body); } catch { return json({ error: 'Invalid JSON' }, 400); }
      return data?.username === 'probe' && data?.password === 'probe-secret'
        ? json({ token: TOKEN, user }) : json({ error: 'Invalid credentials' }, 401);
    }
    if (!url.pathname.startsWith('/api/')) return json({ error: 'Mock route not found' }, 404);
    if (req.headers.authorization !== `Bearer ${TOKEN}`) return json({ error: 'Unauthorized' }, 401);
    if (url.pathname === '/api/auth/user') return json({ user });
    // Match the frozen native build's generatedFrontendProfile. The shell keeps
    // assembly/chatSurface null until this live projection is validated:
    // ui/src/composition/runtime.ts:77-96,174-180; generated/frontend-modules.ts:67.
    // This advertises the mock's native chat fixture, not B/D deployment coverage.
    if (url.pathname === '/api/modules/runtime') return json({
      modules: {
        agentLoop: { enabled: true, provider: 'pilotdeck' },
        skills: { enabled: true, provider: 'pilotdeck' },
        tools: { enabled: true, provider: 'pilotdeck' },
        context: { enabled: true, provider: 'pilotdeck' },
        modelProvider: { enabled: true, provider: 'pilotdeck' },
        sop: { enabled: false }, knowledge: { enabled: false },
      },
      gatewayCapabilities: [], runtime: { gatewayState: 'ready', unavailableSlots: [] },
    });
    if (['/api/user/onboarding-status', '/api/user/runtime-status'].includes(url.pathname)) return json({
      hasCompletedOnboarding: true, configuration: { state: 'ready', modelRef: 'probe/echo', revision: '1' }, gateway: { state: 'unmanaged' },
    });
    if (url.pathname === '/api/projects') return json([{ name: 'probe', path: '/probe', fullPath: '/probe', displayName: 'F2 project', sessions: [session] }]);
    if (url.pathname === '/api/projects/probe/sessions' || url.pathname === '/api/sessions') return json({ sessions: [session], total: 1, hasMore: false });
    if (url.pathname === '/api/sessions/s/messages') {
      const body = { messages: stream.messages, total: stream.messages.length, hasMore: false };
      // Snapshot the actual response, not a reference changed by later runs/reset.
      // Correlate completion refreshes without adding any browser-side requests.
      stats.historyResponses.push({ requestIndex: stats.requests.length - 1, url: req.url,
        runId: stream.state.runId ?? null, status: stream.state.status,
        body: structuredClone(body) });
      return json(body);
    }
    // Explicit unsupported APIs stay red/visible; no catch-all success fixture.
    return json({ error: 'Unsupported by F2 mock' }, 404);
  }
  function handleMessage(socket, raw) {
    let message; try { message = JSON.parse(raw.toString()); } catch { stats.errors.push('Invalid WS JSON'); return; }
    if (!message || typeof message !== 'object' || typeof message.type !== 'string') { stats.errors.push('Invalid WS shape'); return; }
    stats.outbound.push({ ...message, path: socket.path });
    if (socket.path === '/ws' && ['watch-session', 'check-session-status'].includes(message.type) && message.sessionId === SESSION_ID) watched.add(socket);
    if (message.type === 'unwatch-session') watched.delete(socket);
    if (socket.path === '/ws' && message.type === 'check-session-status') socket.send(JSON.stringify({
      type: 'session-status', sessionId: message.sessionId, provider: 'pilotdeck',
      isProcessing: stream.state.status === 'running', activeRunId: stream.state.status === 'running' ? stream.state.runId : null,
      expectedActiveRunId: message.expectedActiveRunId ?? null, statusRequestId: message.statusRequestId ?? null, activeTurnMessages: [],
    }));
    if (socket.path === '/shell' && message.type === 'input') socket.send(JSON.stringify({ type: 'output', data: message.input }));
    if (socket.path === '/ws' && message.type === 'get-session-activity') socket.send(JSON.stringify({ type: 'session-activity-snapshot', activities: [] }));
    if (socket.path === '/ws' && ['watch-session', 'get-input-queue'].includes(message.type)) socket.send(JSON.stringify({ type: 'input-queue-state', sessionId: message.sessionId, items: [], paused: false }));
  }
  return { stats, stream, handleHTTP, handleMessage, detach: socket => watched.delete(socket), close: () => stream.stop() };
}
export async function startMock() {
  const mock = createMock(), sockets = new Set(), connections = new Set();
  const server = http.createServer((req, res) => mock.handleHTTP(req, res).catch(error => { mock.stats.errors.push(error.message); res.writeHead(500); res.end(); }));
  server.on('connection', socket => { connections.add(socket); socket.on('close', () => connections.delete(socket)); });
  const { WebSocketServer } = requireUI('ws');
  const wss = new WebSocketServer({ noServer: true });
  server.on('upgrade', (req, socket, head) => {
    const url = new URL(req.url, `http://127.0.0.1:${ports.mock}`);
    if (!['/ws', '/shell'].includes(url.pathname) || url.searchParams.get('token') !== TOKEN) {
      socket.end('HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n'); return;
    }
    wss.handleUpgrade(req, socket, head, ws => {
      ws.path = url.pathname; sockets.add(ws);
      mock.stats.connections.push({ path: url.pathname, token: TOKEN });
      ws.on('message', data => mock.handleMessage(ws, data));
      ws.on('error', error => mock.stats.errors.push(error.message));
      ws.on('close', () => { sockets.delete(ws); mock.detach(ws); });
    });
  });
  try { await new Promise((resolve, reject) => { server.once('error', reject); server.listen(ports.mock, '127.0.0.1', resolve); }); }
  catch (error) { await new Promise(resolve => wss.close(resolve)); throw error; }
  return { ...mock, close: async () => { mock.close(); for (const socket of sockets) socket.terminate(); for (const connection of connections) connection.destroy(); await new Promise(resolve => wss.close(resolve)); await new Promise(resolve => server.close(resolve)); } };
}
