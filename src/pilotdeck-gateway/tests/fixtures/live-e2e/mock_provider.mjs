// #748 loopback OpenAI SSE peer shared by the TS and Rust production hosts.
import http from 'node:http';
import {appendFileSync, writeFileSync, renameSync} from 'node:fs';
const port = Number(process.env.LIVE_E2E_MOCK_PORT ?? 0);
const log = process.env.LIVE_E2E_MOCK_LOG;
if (!Number.isInteger(port) || port < 0 || port > 65535 || port === 18789 || !log) throw Error('#748 mock configuration');
const publish = (path, value) => { writeFileSync(path + '.tmp', JSON.stringify(value) + '\n', {flag:'wx'}); renameSync(path + '.tmp', path); };
const record = value => appendFileSync(log, JSON.stringify(value) + '\n');
let serial = 0;
const releases = new Set();
const server = http.createServer(async (req, res) => {
  if (req.url === '/health') { res.end('ok'); return; }
  if (req.url === '/release-window') { for (const release of releases) release(); res.end('released'); return; }
  if (req.method !== 'POST' || !['/v1/chat/completions', '/chat/completions'].includes(req.url)) { res.writeHead(404).end(); return; }
  let body = '';
  for await (const chunk of req) body += chunk;
  const request = JSON.parse(body);
  const user = [...request.messages].reverse().find(m => m.role === 'user');
  const text = typeof user?.content === 'string' ? user.content : (user?.content ?? []).map(b => b.text ?? '').join('');
  const scenario = text.includes('LIVE_E2E_CANCEL_TOOL') ? 'cancel-tool' : text.includes('LIVE_E2E_TOOL') ? 'tool' : text.includes('LIVE_E2E_HANG') ? 'cancel-stream' : 'text';
  const index = ++serial;
  const hasResult = request.messages.some(m => m.role === 'tool');
  record({request:index, scenario, roles:request.messages.map(m => m.role), userText:text, hasToolResult:hasResult});
  let finished = false, sent = 0;
  res.on('close', () => record({request:index, connection:finished ? 'completed' : 'aborted', sent}));
  res.writeHead(200, {'content-type':'text/event-stream'});
  const emit = (delta, finish_reason = null) => { if (res.destroyed) return; sent++; res.write('data: ' + JSON.stringify({id:'chatcmpl-live-e2e', object:'chat.completion.chunk', created:1792972800, model:'mock-model', choices:[{index:0, delta, finish_reason}]}) + '\n\n'); };
  const hold = async () => {
    await new Promise(resolve => {
      const done = () => { clearTimeout(timer); releases.delete(done); resolve(); };
      const timer = setTimeout(done, 10000);
      releases.add(done); res.once('close', done);
    });
  };
  emit({role:'assistant', content:''});
  if (['tool','cancel-tool'].includes(scenario) && !hasResult) {
    const path = /LIVE_E2E_(?:TOOL|CANCEL_TOOL)\s+(\S+)/.exec(text)?.[1];
    if (!path) throw Error('#748 tool target absent');
    emit({tool_calls:[{index:0,id:'call-live-e2e-read',type:'function',function:{name:'read_file',arguments:JSON.stringify({file_path:path})}}]});
    emit({}, 'tool_calls');
  } else {
    emit({content:hasResult ? 'read tool result observed by the mock provider' : scenario === 'cancel-stream' ? 'hanging mid-stream for cancel' : 'hello live-e2e text stream'});
    if (scenario === 'cancel-stream' || ['drop','early-final'].includes(process.env.LIVE_E2E_INJECTION)) await hold();
    if (res.destroyed) return;
    emit({}, 'stop');
  }
  finished = true;
  res.end('data: [DONE]\n\n');
});
server.listen(port, '127.0.0.1', () => {
  const ready = {listening:true, port:server.address().port};
  record(ready);
  if (process.env.LIVE_E2E_MOCK_READY) publish(process.env.LIVE_E2E_MOCK_READY, ready);
  process.stdout.write(JSON.stringify(ready) + '\n');
});
for (const signal of ['SIGTERM','SIGINT']) process.on(signal, () => { server.closeAllConnections(); server.close(() => process.exit(0)); });
