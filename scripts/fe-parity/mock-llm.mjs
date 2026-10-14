// Minimal OpenAI subset proved necessary by _tmp/triage.md. Node built-ins only.
import http from 'node:http';
import { createHash } from 'node:crypto';
import { inflateSync } from 'node:zlib';
import { appendFile, writeFile } from 'node:fs/promises';
import { REAL_TEST_KEY, realPorts } from './env.mjs';

const usage = { prompt_tokens: 8, completion_tokens: 8, total_tokens: 16 };
const errorReply = (status, message) => ({ status, script: 'rejected', body: { error: { type: 'invalid_request_error', message } } });
function text(content) {
  return typeof content === 'string' ? content : Array.isArray(content)
    ? content.filter(part => part?.type === 'text').map(part => part.text || '').join('\n') : '';
}
// The baseline probe encodes a solid 32x32 RGB PNG with filter=0.
// Decode the actual image instead of returning all four possible colors.
export function probeColor(dataUrl) {
  const match = /^data:image\/png;base64,([A-Za-z0-9+/=]+)$/.exec(dataUrl || '');
  if (!match) throw new Error('Unsupported probe image');
  const png = Buffer.from(match[1], 'base64'), idat = [];
  if (!png.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))) throw new Error('Invalid PNG');
  for (let offset = 8; offset + 12 <= png.length;) {
    const length = png.readUInt32BE(offset), kind = png.toString('ascii', offset + 4, offset + 8);
    if (offset + 12 + length > png.length) throw new Error('Truncated PNG');
    if (kind === 'IHDR' && (png[offset + 16] !== 8 || png[offset + 17] !== 2)) throw new Error('Unsupported PNG format');
    if (kind === 'IDAT') idat.push(png.subarray(offset + 8, offset + 8 + length));
    offset += 12 + length;
  }
  const row = inflateSync(Buffer.concat(idat), { maxOutputLength: 128 * 1024 });
  if (row[0] !== 0) throw new Error('Unsupported PNG filter');
  const colors = { '220,48,48': 'red', '40,96,220': 'blue', '36,168,72': 'green', '232,196,40': 'yellow' };
  const color = colors[[...row.subarray(1, 4)].join(',')];
  if (!color) throw new Error('Unknown probe color');
  return color;
}
export function selectScript(body) {
  const latest = body.messages.findLast(message => message?.role === 'user');
  const prompt = text(latest?.content);
  const image = Array.isArray(latest?.content) && latest.content.find(part => part?.type === 'image_url');
  if (image && prompt.includes('What color is the shape in this image?')) return { script: 'image-color', answer: probeColor(image.image_url?.url) };
  const marker = /Reply with exactly this marker:\s*(E2E-CHAT-(?:openai|anthropic|google)-\d+)/.exec(prompt);
  if (marker && body.metadata?.purpose === 'session_title_generation') return { script: 'session-title', answer: JSON.stringify({ title: 'Verify E2E chat marker' }) };
  if (marker) return { script: 'chat-marker', answer: marker[1] };
  if (prompt.trim() === 'Reply exactly: 1') return { script: 'connection-text', answer: '1' };
  throw new Error('No triage script matches latest user message');
}
export function planResponse({ method, path, headers = {}, body }) {
  // Onboarding's fetchRemoteDefaultModels submits apiKey=''. The config route
  // sends no Authorization header for this read-only catalog request. Do not
  // make that expected discovery request poison the entire runtime's accounting.
  const publicCatalog = method === 'GET' && ['/v1/models', '/models'].includes(path)
    && headers.authorization === undefined && headers['x-api-key'] === undefined && headers['x-goog-api-key'] === undefined;
  if (!publicCatalog && headers.authorization !== `Bearer ${REAL_TEST_KEY}`) return errorReply(401, 'Expected fixed parity test key');
  if (method === 'GET' && ['/v1/models', '/models'].includes(path)) return {
    status: 200, script: 'model-list', body: { object: 'list', data: [{ id: 'gpt-4.1', object: 'model', created: 0, owned_by: 'fe-parity' }] },
  };
  if (!['/v1/chat/completions', '/chat/completions'].includes(path)) return errorReply(404, 'Protocol/endpoint not required by triage');
  if (method !== 'POST') return errorReply(405, 'POST required');
  if (!body || typeof body.model !== 'string' || !Array.isArray(body.messages) || !body.messages.length
    || (body.stream !== undefined && typeof body.stream !== 'boolean')) return errorReply(400, 'Invalid OpenAI request shape');
  let selected;
  try { selected = selectScript(body); } catch (error) { return errorReply(422, error.message); }
  const id = 'chatcmpl-parity-' + createHash('sha256').update(JSON.stringify(body)).digest('hex').slice(0, 16);
  const base = { id, created: 0, model: body.model };
  if (!body.stream) return { status: 200, ...selected, body: {
    ...base, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content: selected.answer }, finish_reason: 'stop' }], usage,
  } };
  const chunk = (delta, finish_reason = null, extras = {}) => ({ ...base, object: 'chat.completion.chunk', choices: [{ index: 0, delta, finish_reason }], ...extras });
  const chunks = [chunk({ role: 'assistant', content: '' })];
  const characters = Array.from(selected.answer);
  for (let index = 0; index < characters.length; index += 7) chunks.push(chunk({ content: characters.slice(index, index + 7).join('') }));
  chunks.push(chunk({}, 'stop', { usage }));
  return { status: 200, ...selected, frames: [...chunks.map(value => `data: ${JSON.stringify(value)}\n\n`), 'data: [DONE]\n\n'] };
}
export function createMockLLM({ record = async () => {}, pause = () => new Promise(done => setTimeout(done, 8)) } = {}) {
  const stats = { requests: 0, rejected: 0, errors: [] };
  async function handleHTTP(req, res) {
    const sequence = ++stats.requests;
    let raw = '', body, plan;
    try {
      const chunks = []; let size = 0;
      for await (const chunk of req) {
        const bytes = Buffer.from(chunk); size += bytes.length;
        if (size > 2 * 1024 * 1024) throw new Error('Request too large');
        chunks.push(bytes);
      }
      raw = Buffer.concat(chunks).toString('utf8');
      if (raw) body = JSON.parse(raw);
      plan = planResponse({ method: req.method, path: new URL(req.url, 'http://127.0.0.1').pathname, headers: req.headers, body });
    } catch (error) { plan = errorReply(400, error.message); }
    if (plan.status !== 200) stats.rejected++;
    // Do not persist credentials. All other request data is local test evidence.
    await record({ sequence, timestamp: new Date().toISOString(), method: req.method, path: req.url,
      authorization: req.headers.authorization === `Bearer ${REAL_TEST_KEY}` ? 'fixed-test-key' : 'invalid-or-absent',
      authHeaders: Object.fromEntries(['authorization', 'x-api-key', 'x-goog-api-key'].map(name => [name,
        req.headers[name] === undefined ? 'absent' : req.headers[name] === (name === 'authorization' ? `Bearer ${REAL_TEST_KEY}` : REAL_TEST_KEY) ? 'fixed-test-key' : 'invalid'])),
      body: body ?? raw, status: plan.status, script: plan.script, answer: plan.answer ?? null,
      response: plan.body ?? null, frames: plan.frames ?? null });
    res.writeHead(plan.status, { 'Content-Type': plan.frames ? 'text/event-stream; charset=utf-8' : 'application/json', 'Cache-Control': 'no-cache' });
    if (!plan.frames) { res.end(JSON.stringify(plan.body)); return; }
    for (const frame of plan.frames) {
      if (res.destroyed) break;
      res.write(frame); await pause();
    }
    res.end();
  }
  return { handleHTTP, stats };
}
export async function startMockLLM(logFile) {
  // appendFile serializes evidence writes; handler failures surface in stats and runner.
  await writeFile(logFile, '', { flag: 'wx' });
  let recording = Promise.resolve();
  const mock = createMockLLM({ record: entry => {
    recording = recording.then(() => appendFile(logFile, JSON.stringify(entry) + '\n'));
    return recording;
  } });
  const sockets = new Set();
  const server = http.createServer((req, res) => mock.handleHTTP(req, res).catch(error => {
    mock.stats.errors.push(error.message); res.destroy(error);
  }));
  server.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
  await new Promise((done, reject) => { server.once('error', reject); server.listen(realPorts.mock, '127.0.0.1', done); });
  return { ...mock, close: async () => {
    for (const socket of sockets) socket.destroy();
    await new Promise(done => server.close(done)); await recording;
  } };
}
