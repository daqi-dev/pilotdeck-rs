import http from 'node:http';
import { appendFileSync } from 'node:fs';
const port = Number(process.env.GP_MOCK_PORT);
if (!Number.isInteger(port) || port < 28000 || port > 28999) throw new Error('28xxx port required');
http.createServer(async (req, res) => {
  if (req.url === '/health') { res.end('ok'); return; }
  if (req.url.startsWith('/__ruler/stream/')) {
    const sse = req.url.endsWith('/sse');
    res.writeHead(200, { 'content-type': sse ? 'text/event-stream' : 'application/x-ndjson' });
    res.write(sse ? 'data: {"event":"first"}\n\n' : '{"event":"first"}\n');
    setTimeout(() => res.end(sse ? 'data: {"event":"last"}\n\n' : '{"event":"last"}\n'), 400);
    return;
  }
  try {
    const parts = []; for await (const part of req) parts.push(part);
    const body = JSON.parse(Buffer.concat(parts).toString());
    const text = JSON.stringify(body.messages);
    const toolDone = body.messages.some((item) => item.role === 'tool');
    const question = text.includes('GP_QUESTION') && !toolDone;
    const permission = text.includes('GP_PERMISSION') && !toolDone;
    const tool = question ? { name: 'ask_user_question', arguments: JSON.stringify({ questions: [{
      question: 'Continue gateway parity?', header: 'parity', options: [{ label: 'yes', description: 'Continue' },
        { label: 'no', description: 'Stop' }] }] }) } : { name: 'bash', arguments: JSON.stringify({ command: 'echo GP_PERMISSION_OK' }) };
    const message = question || permission ? { role: 'assistant', content: null,
      tool_calls: [{ id: 'call_gateway_parity', type: 'function', function: tool }] }
      : { role: 'assistant', content: 'GP_MOCK_OK' };
    appendFileSync(process.env.GP_MODEL_LOG, JSON.stringify({ request: body, response: message }) + '\n');
    const finish = question || permission ? 'tool_calls' : 'stop';
    if (!body.stream) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ id: 'chatcmpl-gp', model: 'deterministic', choices: [{ index: 0, message, finish_reason: finish }] }));
    } else {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const delta = message.tool_calls ? { role: 'assistant', tool_calls: message.tool_calls.map((item) => ({ index: 0, ...item })) }
        : { role: 'assistant', content: message.content };
      for (const chunk of [{ choices: [{ index: 0, delta, finish_reason: null }] },
        { choices: [{ index: 0, delta: {}, finish_reason: finish }], usage: { prompt_tokens: 12, completion_tokens: 4, total_tokens: 16 } }]) {
        res.write(`data: ${JSON.stringify(chunk)}\n\n`);
      }
      res.end('data: [DONE]\n\n');
    }
  } catch (error) { res.writeHead(500); res.end(String(error)); }
}).listen(port, '127.0.0.1');
