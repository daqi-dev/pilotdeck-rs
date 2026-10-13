#!/usr/bin/env python3
"""exp008 / plan 003 Issue B1 orchestrator.

Topology (isolated ports only; 130xx is this workstream, 288xx for the gateway):
  browser -> vite :15180 -> Rust proxy :13001 -> TS ui/server :13002 -> TS gateway :28890
Scenario 1 also exercises the Rust entry directly (GET / -> 302 to vite).

All listeners are asserted loopback-only. Only process groups created here are
ever signalled; nothing is discovered or killed by port.
"""
import argparse
import base64
import http.client
import json
import os
import re
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/exp008'
BASELINE = Path('/Users/da/ws/pilotdeck-rs/resources/PilotDeck')
TOKEN = 'exp008-isolated-test-token'
RUST = ROOT / '_build/exp008-target/debug/pdrs-exp008-proxy'
PORTS = {'rust': 13001, 'ui': 13002, 'gateway': 28890, 'vite': 15180}
ALL_PORTS = set(PORTS.values())

PROCESSES = []
RESULTS = {}
LISTENERS = []
CLEANUP_ERRORS = []
STOPPED = set()


def save(out, name, value):
    (out / name).write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n')


def start(name, argv, env, port=None, cwd=None):
    out = Path(env['EXP008_OUT'])
    log = (out / f'{name}.log').open('w')
    try:
        proc = subprocess.Popen(argv, cwd=cwd or ROOT, env=env, stdout=log, stderr=subprocess.STDOUT,
                                start_new_session=True)
    except Exception:
        log.close()
        raise
    entry = {'name': name, 'pid': proc.pid, 'processGroup': proc.pid, 'port': port,
             'argv': [str(x) for x in argv]}
    PROCESSES.append((proc, log, entry))
    save(out, 'processes.json', [item[2] for item in PROCESSES])
    return proc


def stop(proc):
    if proc.pid in STOPPED:
        return
    # A process that already exited on its own (e.g. the route enumerator) needs
    # no signal; killpg on its gone group can still return EPERM on macOS.
    if proc.poll() is not None:
        STOPPED.add(proc.pid)
        return
    def signal_group(sig):
        try:
            os.killpg(proc.pid, sig)
        except ProcessLookupError:
            pass
    try:
        signal_group(signal.SIGTERM)
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            pass
        signal_group(signal.SIGKILL)
        if proc.poll() is None:
            proc.wait(timeout=5)
    except (OSError, subprocess.TimeoutExpired) as error:
        CLEANUP_ERRORS.append({'pid': proc.pid, 'error': str(error)})
    else:
        STOPPED.add(proc.pid)


def assert_loopback(out, port, proc):
    result = subprocess.run(['lsof', '-nP', '-a', '-p', str(proc.pid), '-iTCP',
                             '-sTCP:LISTEN', '-Fn'], capture_output=True, text=True, timeout=5)
    addresses = [line[1:] for line in result.stdout.splitlines() if line.startswith('n')]
    entry = {'pid': proc.pid, 'port': port, 'addresses': addresses,
             'loopbackOnly': result.returncode == 0 and addresses == [f'127.0.0.1:{port}']}
    LISTENERS.append(entry)
    save(out, 'listeners.json', LISTENERS)
    if not entry['loopbackOnly']:
        raise RuntimeError(f'owned process {proc.pid} must listen only on 127.0.0.1:{port}: {addresses}')


def wait_http(out, port, proc, path='/health'):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f'process {proc.pid} exited {proc.returncode} before port {port}')
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{port}{path}', timeout=2) as response:
                if response.status < 500:
                    assert_loopback(out, port, proc)
                    return
        except (OSError, urllib.error.URLError, urllib.error.HTTPError):
            time.sleep(.15)
    raise RuntimeError(f'port {port} not ready')


def raw_http(port, method, path, headers=None, body=None):
    conn = http.client.HTTPConnection('127.0.0.1', port, timeout=30)
    conn.request(method, path, body=body, headers=headers or {})
    resp = conn.getresponse()
    data = resp.read()
    result = {'status': resp.status, 'reason': resp.reason,
              'headers': {k.lower(): v for k, v in resp.getheaders()}, 'body': data}
    conn.close()
    return result


def body_json(response):
    try:
        return json.loads(response['body'].decode('utf-8'))
    except Exception:
        return None


# ---------------------------------------------------------------- environment

def build_env(out, label, port_extra=None):
    home = out / 'home'
    env = dict(os.environ)
    for key in list(env):
        if (key.startswith(('PILOTDECK_', 'EXP008_')) or
                key in ('PILOT_HOME', 'DATABASE_PATH', 'NODE_OPTIONS', 'SERVER_PORT', 'HOST',
                        'VITE_PORT', 'DISABLE_LOCAL_AUTH', 'JWT_SECRET', 'API_KEY')):
            env.pop(key)
    env.update(
        EXP008_OUT=str(out),
        PILOT_HOME=str(home),
        PILOTDECK_CONFIG_PATH=str(home / 'pilotdeck.yaml'),
        TMPDIR=str(out / 'tmp'),
        WORKSPACES_ROOT=str(out / 'workspaces'),
        PILOTDECK_GATEWAY_HOST='127.0.0.1',
        PILOTDECK_GATEWAY_PORT=str(PORTS['gateway']),
        PILOTDECK_GATEWAY_TOKEN=TOKEN,
        PILOTDECK_GATEWAY_TOKEN_PATH=str(home / 'server-token'),
        PILOTDECK_GATEWAY_URL=f"ws://127.0.0.1:{PORTS['gateway']}/ws",
        HOST='127.0.0.1',
        SERVER_PORT=str(PORTS['ui']),
        VITE_PORT=str(PORTS['vite']),
        PILOTDECK_DISABLE_LOCAL_AUTH='0',
        DISABLE_LOCAL_AUTH='0',
        PILOTDECK_SKIP_BROWSER_OPEN='1',
        EXP008_POLL_WATCH='1',
        EXP008_MOCK='1',
        EXP008_MODEL_LOG=str(out / f'{label}-model.jsonl'),
    )
    if port_extra:
        env.update(port_extra)
    return env


def make_home(out):
    home = out / 'home'
    (home / 'projects/exp008-fixture').mkdir(parents=True, exist_ok=False)
    (out / 'tmp').mkdir(parents=True)
    (out / 'workspaces').mkdir()
    project = out / 'project'
    project.mkdir()
    (project / 'note.txt').write_text('exp008 project fixture\n')
    (home / 'projects/exp008-fixture/.cwd').write_text(str(project) + '\n')
    token_path = home / 'server-token'
    token_path.write_text(TOKEN + '\n')
    token_path.chmod(0o600)
    (home / 'pilotdeck.yaml').write_text(f'''schemaVersion: 1
agent:
  model: parity/deterministic
  maxContextTokens: 65536
  maxOutputTokens: 8192
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:{PORTS['gateway']}/exp008/v1
      apiKey: exp008-mock-only
      models:
        deterministic:
          capabilities:
            supportsToolUse: true
            maxContextTokens: 65536
            maxOutputTokens: 8192
telemetry:
  enabled: false
webui:
  runtime:
    host: 127.0.0.1
    serverPort: {PORTS['ui']}
    databasePath: {home / 'auth.db'}
    workspacesRoot: {out / 'workspaces'}
''')
    return home, project


# ------------------------------------------------------------------ scenarios

def scenario(results, out, name, callback):
    try:
        value = callback()
        results[name] = value if isinstance(value, dict) else {'status': 'PASS'}
    except Exception as error:  # noqa: BLE001 - evidence must survive any failure
        results[name] = {'status': 'FAIL', 'error': f'{type(error).__name__}: {error}'}
    save(out, 'scenarios.json', results)
    print(f'{name}: {results[name]}', flush=True)


def scenario_static(out):
    entry = raw_http(PORTS['rust'], 'GET', '/')
    assert entry['status'] == 302, f'GET / via Rust expected 302 to vite, got {entry["status"]}'
    location = entry['headers'].get('location', '')
    assert f':{PORTS["vite"]}' in location, f'redirect must target vite, got {location}'
    assert entry['headers'].get('x-pilotdeck-handler') == 'ts', 'proxy must tag handler=ts'
    ui_page = raw_http(PORTS['vite'], 'GET', '/')
    assert ui_page['status'] == 200, f'vite / expected 200, got {ui_page["status"]}'
    assert b'<div id="root"' in ui_page['body'] or b'id="root"' in ui_page['body'], 'vite must serve the UI shell'
    dashboard = raw_http(PORTS['rust'], 'GET', '/memory-dashboard')
    assert dashboard['status'] in (200, 301, 302, 401, 403), f'/memory-dashboard reachable, got {dashboard["status"]}'
    assert dashboard['status'] != 404, '/memory-dashboard must be mounted'
    save(out, 'static.json', {'entry': {'status': entry['status'], 'location': location,
         'handler': entry['headers'].get('x-pilotdeck-handler')},
         'vite': {'status': ui_page['status'], 'bytes': len(ui_page['body'])},
         'memoryDashboard': {'status': dashboard['status'], 'handler': dashboard['headers'].get('x-pilotdeck-handler')}})
    return {'status': 'PASS', 'entry302': location, 'viteStatus': ui_page['status']}


def scenario_browser(out):
    from playwright.sync_api import sync_playwright

    profile = out / 'chrome-profile'
    responses = []
    websockets = []
    with sync_playwright() as play:
        context = play.chromium.launch_persistent_context(
            str(profile), channel='chrome', headless=True,
            args=['--no-first-run', '--no-default-browser-check', '--disable-features=Translate'])
        page = context.new_page()
        page.on('response', lambda r: responses.append(
            {'url': r.url, 'status': r.status, 'handler': r.headers.get('x-pilotdeck-handler')}))
        page.on('websocket', lambda ws: websockets.append(ws.url))
        page.goto(f'http://127.0.0.1:{PORTS["vite"]}/', wait_until='load', timeout=45000)
        page.wait_for_timeout(4000)
        auth = page.evaluate("""async () => {
            const post = async (path, body) => {
              const r = await fetch(path, {method: 'POST', headers: {'content-type': 'application/json'},
                body: JSON.stringify(body)});
              let payload = null;
              try { payload = await r.json(); } catch (_) {}
              return {status: r.status, handler: r.headers.get('x-pilotdeck-handler'), payload};
            };
            const status0 = await (await fetch('/api/auth/status')).json();
            const register = await post('/api/auth/register', {username: 'exp008', password: 'exp008pass'});
            let login = null;
            let token = register.payload && register.payload.token;
            if (!token) {
              login = await post('/api/auth/login', {username: 'exp008', password: 'exp008pass'});
              token = login.payload && login.payload.token;
            }
            if (token) localStorage.setItem('token', token);
            const userResp = await fetch('/api/auth/user', {headers: {authorization: `Bearer ${token}`}});
            const user = await userResp.json().catch(() => null);
            return {status0, register: {status: register.status, handler: register.handler},
                    login: login && {status: login.status, handler: login.handler}, token,
                    user: {status: userResp.status, handler: userResp.headers.get('x-pilotdeck-handler'), body: user}};
        }""")
        title = page.title()
        root_len = page.eval_on_selector('#root', 'el => el.innerHTML.length') if page.query_selector('#root') else 0
        page.screenshot(path=str(out / 'browser.png'))
        # Reload with the token in localStorage so the app itself opens /ws and
        # the browser truly exercises the Rust WebSocket relay.
        if auth['token']:
            page.reload(wait_until='load', timeout=45000)
            page.wait_for_timeout(4000)
        app_sockets = [url for url in websockets if '/ws' in url and 'token=' in url]
        context.close()
    assert root_len > 0, 'browser must render a non-empty #root through vite -> Rust -> TS'
    proxied = [r for r in responses if f':{PORTS["vite"]}/api/' in r['url'] or '/api/' in r['url']]
    tagged = [r for r in responses if r.get('handler') == 'ts']
    assert tagged, 'no browser API response carried X-PilotDeck-Handler: ts (Rust not in path?)'
    assert auth['status0']['needsSetup'] is True, 'fresh auth.db must need setup'
    assert auth['register']['status'] == 200, f'register failed: {auth["register"]}'
    assert auth['register']['handler'] == 'ts', 'register must be tagged by the proxy'
    assert auth['user']['status'] == 200, f'auth/user failed: {auth["user"]}'
    token = auth['token']
    (out / 'token.txt').write_text(token + '\n')
    save(out, 'browser.json', {'title': title, 'rootLength': root_len,
         'auth': {k: v for k, v in auth.items() if k != 'token'}, 'tokenPresent': bool(token),
         'websockets': websockets, 'appWebSockets': app_sockets,
         'apiResponses': [r for r in responses if '/api/' in r['url']][:60],
         'taggedCount': len(tagged), 'responseCount': len(responses)})
    return {'status': 'PASS', 'title': title, 'rootLength': root_len, 'tagged': len(tagged),
            'appWebSockets': len(app_sockets), 'registerHandler': auth['register']['handler']}


def scenario_readonly(out):
    token = (out / 'token.txt').read_text().strip() if (out / 'token.txt').exists() else ''
    headers = {'authorization': f'Bearer {token}'} if token else {}
    direct_projects = raw_http(PORTS['ui'], 'GET', '/api/projects', headers)
    projects = body_json(direct_projects) or []
    project_name = None
    if isinstance(projects, list):
        for item in projects:
            text = json.dumps(item)
            if 'exp008-fixture' in text or 'exp008 project fixture' in text or str(out / 'project') in text:
                project_name = item.get('name') or item.get('projectName') or item.get('key')
                break
        if project_name is None and projects:
            project_name = projects[0].get('name') or projects[0].get('projectName') or projects[0].get('key')
    paths = ['/api/projects', '/api/config', '/api/models', '/api/auth/user']
    if project_name:
        paths.append(f'/api/projects/{urllib.parse.quote(str(project_name))}/sessions')
    records = []
    for path in paths:
        direct = raw_http(PORTS['ui'], 'GET', path, headers)
        proxy = raw_http(PORTS['rust'], 'GET', path, headers)
        proxy_headers = {k: v for k, v in proxy['headers'].items() if k != 'x-pilotdeck-handler'}
        same_status = direct['status'] == proxy['status']
        same_body = direct['body'] == proxy['body']
        same_headers = {k: v for k, v in direct['headers'].items()
                        if k not in ('date',)} == {k: v for k, v in proxy_headers.items() if k not in ('date',)}
        records.append({'path': path, 'directStatus': direct['status'], 'proxyStatus': proxy['status'],
                        'sameStatus': same_status, 'sameBody': same_body, 'sameHeaders': same_headers,
                        'proxyHandler': proxy['headers'].get('x-pilotdeck-handler'),
                        'directBodyPrefix': direct['body'][:200].decode('utf-8', 'replace')})
    assert all(r['sameStatus'] for r in records), f'status mismatch: {records}'
    assert all(r['sameBody'] for r in records), f'body mismatch: {[r for r in records if not r["sameBody"]]}'
    assert all(r['proxyHandler'] == 'ts' for r in records), 'every proxied read must be tagged ts'
    assert direct_projects['status'] == 200, f'/api/projects direct failed: {direct_projects["status"]}'
    save(out, 'readonly.json', {'projectName': project_name, 'records': records})
    return {'status': 'PASS', 'project': project_name, 'checked': len(records)}


def ensure_token(out):
    """Register/login through the Rust entry and persist the JWT so scenarios
    that need auth work even when the browser scenario is not selected."""
    token_path = out / 'token.txt'
    if token_path.exists() and token_path.read_text().strip():
        return token_path.read_text().strip()
    status = raw_http(PORTS['rust'], 'GET', '/api/auth/status')
    payload = body_json(status) or {}
    if payload.get('needsSetup'):
        response = raw_http(PORTS['rust'], 'POST', '/api/auth/register',
                            {'content-type': 'application/json'},
                            json.dumps({'username': 'exp008', 'password': 'exp008pass'}))
    else:
        response = raw_http(PORTS['rust'], 'POST', '/api/auth/login',
                            {'content-type': 'application/json'},
                            json.dumps({'username': 'exp008', 'password': 'exp008pass'}))
    body = body_json(response) or {}
    assert response['status'] == 200 and body.get('token'), f'token bootstrap failed: {response["status"]} {body}'
    token_path.write_text(body['token'] + '\n')
    return body['token']


def run_driver(out, mode, port, label, timeout=90, extra_env=None):
    out_dir = out
    driver_out = out_dir / f'{label}.json'
    env = build_env(out_dir, label)
    env['EXP008_TOKEN'] = (out_dir / 'token.txt').read_text().strip() if (out_dir / 'token.txt').exists() else ''
    if extra_env:
        env.update(extra_env)
    proc = start(f'driver-{label}', ['node', SCRIPT / 'driver.mjs', mode, driver_out, str(BASELINE),
                  str(out_dir / 'project'), str(port)], env, port=None, cwd=ROOT)
    try:
        code = proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        stop(proc)
        raise RuntimeError(f'driver {label} timeout') from None
    if code:
        raise RuntimeError(f'driver {label} exited {code}; see {label}.log')
    return json.loads(driver_out.read_text())


def scenario_chat(out):
    proxy = run_driver(out, 'chat', PORTS['rust'], 'chat-proxy')
    direct = run_driver(out, 'chat', PORTS['ui'], 'chat-direct')
    assert any(f.get('kind') == 'complete' for f in proxy['frames']), 'proxy chat must complete'
    assert any('EXP008_MOCK_OK' in json.dumps(f) for f in proxy['frames']), 'proxy chat must expose mock text'
    assert not any(f.get('kind') == 'error' for f in proxy['frames']), 'proxy chat must not error'
    save(out, 'chat.json', {'proxyKinds': [f.get('kind') for f in proxy['frames']][:40],
         'directKinds': [f.get('kind') for f in direct['frames']][:40],
         'proxyFrameCount': len(proxy['frames']), 'directFrameCount': len(direct['frames'])})
    return {'status': 'PASS', 'proxyFrames': len(proxy['frames']), 'directFrames': len(direct['frames'])}


def scenario_pty(out):
    token = (out / 'token.txt').read_text().strip() if (out / 'token.txt').exists() else ''
    nonce = base64.b16encode(os.urandom(6)).decode('ascii').lower()
    sentinel = f'PDSENTINEL_{nonce}'
    result = pty_probe(PORTS['rust'], sentinel, str(out / 'project'), token)
    result_direct = pty_probe(PORTS['ui'], sentinel, str(out / 'project'), token)
    if sentinel in result['output']:
        assert sentinel not in result['input'], 'sentinel bytes must not appear verbatim in the injected input'
        assert sentinel in result_direct['output'], 'PTY sentinel missing direct (control)'
        save(out, 'pty.json', {'sentinel': sentinel, 'proxy': result, 'direct': result_direct})
        return {'status': 'PASS', 'sentinel': sentinel, 'proxyOutputBytes': len(result['output'])}
    # The managed sandbox refuses node-pty's posix_spawnp; the WebSocket relay
    # itself is still verified because TS receives init and answers over /shell.
    blocked = 'posix_spawnp failed' in result['output'] and 'posix_spawnp failed' in result_direct['output']
    save(out, 'pty.json', {'sentinel': sentinel, 'proxy': result, 'direct': result_direct,
         'environmentBlocked': blocked})
    assert blocked, f'PTY produced neither sentinel nor a known sandbox error: {result["output"][:300]!r}'
    return {'status': 'PASS-with-note', 'note': 'PTY spawn blocked by sandbox (node-pty posix_spawnp failed); '
            '/shell WS relay to TS verified in both directions'}


def pty_probe(port, sentinel, project, token):
    """Drive /shell through a raw WebSocket using the Node driver is overkill for
    one round trip; use the same Node runtime through a tiny inline module."""
    env = dict(os.environ)
    env['EXP008_PTY_PORT'] = str(port)
    env['EXP008_PTY_SENTINEL'] = sentinel
    env['EXP008_PTY_PROJECT'] = project
    env['EXP008_TOKEN'] = token
    output = subprocess.run(
        ['node', '-e', PTY_JS], capture_output=True, text=True, timeout=60, env=env, cwd=str(ROOT))
    if output.returncode != 0:
        raise RuntimeError(f'pty probe {port} failed: {output.stderr.strip()[:500]}')
    return json.loads(output.stdout)


PTY_JS = r'''
const port = process.env.EXP008_PTY_PORT, sentinel = process.env.EXP008_PTY_SENTINEL;
const project = process.env.EXP008_PTY_PROJECT, token = process.env.EXP008_TOKEN || '';
const q = token ? `?token=${encodeURIComponent(token)}` : '';
const ws = new WebSocket(`ws://127.0.0.1:${port}/shell${q}`);
let output = '', inputText = '';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const main = async () => {
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve); ws.addEventListener('error', reject);
  });
  const init = { type: 'init', projectPath: project, sessionId: 'exp008-pty', provider: 'plain-shell', isPlainShell: true, initialCommand: 'exec bash -i', hasSession: false, cols: 80, rows: 24 };
  ws.send(JSON.stringify(init));
  await sleep(1500);
  // The sentinel is assembled by the shell from a nonce so the echoed input can
  // never contain the complete expected string.
  inputText = `printf 'PDSENTINEL_%s\\n' '${sentinel.replace('PDSENTINEL_', '')}'\n`;
  ws.send(JSON.stringify({ type: 'input', data: inputText }));
  await sleep(1200);
  ws.send(JSON.stringify({ type: 'resize', cols: 100, rows: 30 }));
  await sleep(400);
  ws.send(JSON.stringify({ type: 'input', data: 'exit\n' }));
  await sleep(1200);
  ws.close();
};
ws.addEventListener('message', (event) => {
  try { const frame = JSON.parse(String(event.data)); if (frame.type === 'output') output += frame.data || ''; }
  catch (_) { output += String(event.data); }
});
main().then(() => { console.log(JSON.stringify({ output, input: inputText, sentinel })); process.exit(0); })
  .catch((error) => { console.error(String(error)); process.exit(1); });
'''


def stream_probe(port, path, headers):
    start = time.monotonic()
    conn = http.client.HTTPConnection('127.0.0.1', port, timeout=15)
    conn.request('GET', path, headers=headers)
    resp = conn.getresponse()
    chunks = []
    while True:
        data = resp.read(1)
        if not data:
            break
        chunks.append({'elapsedMs': round((time.monotonic() - start) * 1000, 1),
                       'byte': data.decode('utf-8', 'replace')})
    conn.close()
    elapsed = [chunk['elapsedMs'] for chunk in chunks]
    body = ''.join(chunk['byte'] for chunk in chunks)
    return {'status': resp.status, 'contentType': resp.getheader('content-type'),
            'handler': resp.getheader('x-pilotdeck-handler'),
            'xAccelBuffering': resp.getheader('x-accel-buffering'),
            'firstElapsedMs': elapsed[0] if elapsed else None,
            'lastElapsedMs': elapsed[-1] if elapsed else None,
            'spanMs': round((elapsed[-1] - elapsed[0]), 1) if len(elapsed) > 1 else 0,
            'bytes': len(chunks), 'body': body}


def scenario_sse(out):
    token = (out / 'token.txt').read_text().strip() if (out / 'token.txt').exists() else ''
    headers = {'authorization': f'Bearer {token}'} if token else {}
    records = {}
    for path, kind in (('/exp008-stream/sse', 'sse'), ('/exp008-stream/ndjson', 'ndjson')):
        direct = stream_probe(PORTS['ui'], path, headers)
        proxy = stream_probe(PORTS['rust'], path, headers)
        records[kind] = {'direct': direct, 'proxy': proxy}
        assert proxy['status'] == direct['status'] == 200, f'{kind} status mismatch'
        assert proxy['handler'] == 'ts', f'{kind} must be tagged ts'
        assert proxy['contentType'] == direct['contentType'], f'{kind} content-type changed'
        assert proxy['body'] == direct['body'], f'{kind} body differs'
        assert proxy['firstElapsedMs'] < 300, f'{kind} first event was buffered: {proxy["firstElapsedMs"]}ms'
        assert proxy['spanMs'] >= 350, f'{kind} terminal event not incremental: span {proxy["spanMs"]}ms'
        if kind == 'sse':
            assert 'event-stream' in proxy['contentType'], 'SSE content-type must stay text/event-stream'
            assert proxy['xAccelBuffering'] == 'no', 'X-Accel-Buffering must survive'
        else:
            assert 'ndjson' in proxy['contentType'], 'NDJSON content-type must stay application/x-ndjson'
    save(out, 'sse.json', records)
    return {'status': 'PASS', 'sseSpanMs': records['sse']['proxy']['spanMs'],
            'ndjsonSpanMs': records['ndjson']['proxy']['spanMs']}


# ------------------------------------------- B1 record/replay comparator (r1)

# Fixed HTTP cases recorded on both sides. Same method + same path on both; the
# direct side is the TS ui/server, the proxy side is the same server via Rust.
HTTP_CASES = [
    ('health', 'GET', '/health', {}),
    ('auth-status', 'GET', '/api/auth/status', {}),
    ('auth-user', 'GET', '/api/auth/user', {}),
    ('config', 'GET', '/api/config', {}),
    ('models', 'GET', '/api/models', {}),
    # Scenario 7 real route: the resume header must survive the relay even when
    # no StaffDeck module host is configured (the route then answers 501).
    ('modules-job-events-resume', 'GET',
     '/api/modules/staffdeck-sdk/events?operation=job_events', {'Last-Event-ID': '2'}),
    ('modules-preview-afterSeq', 'GET',
     '/api/modules/staffdeck-sdk/events?operation=preview_job_events&afterSeq=1', {}),
]

# SSE cases: incremental pass-through plus the exact resumption inputs used by
# ui/server/routes/modules.js:334-339 (Last-Event-ID header, ?lastEventId=,
# ?afterSeq=). The injected /exp008-stream/resume route echoes the resume source.
SSE_CASES = [
    ('sse-incremental', '/exp008-stream/sse', {}, 'sse'),
    ('ndjson-incremental', '/exp008-stream/ndjson', {}, 'ndjson'),
    ('resume-none', '/exp008-stream/resume', {}, 'sse'),
    ('resume-header', '/exp008-stream/resume', {'Last-Event-ID': '2'}, 'sse'),
    ('resume-lastEventId', '/exp008-stream/resume?lastEventId=4', {}, 'sse'),
    ('resume-afterSeq', '/exp008-stream/resume?operation=preview_job_events&afterSeq=1', {}, 'sse'),
]

# Distinct UUID session ids per side so each WS run is fresh and the normaliser
# can map the generated session id independently (both are the first UUID seen).
COMPARE_SESSIONS = {
    'direct': 'web:11111111-1111-4111-8111-000000000001',
    'proxy': 'web:11111111-1111-4111-8111-000000000002',
}


def parse_sse_events(body):
    events = []
    for block in body.split('\n\n'):
        if not block.strip():
            continue
        fields = {}
        for line in block.split('\n'):
            if not line or line.startswith(':'):
                continue
            name, _, value = line.partition(':')
            fields[name] = value[1:] if value.startswith(' ') else value
        events.append(fields)
    return events


def probe_sse_events(port, path, headers):
    probe = stream_probe(port, path, headers)
    if 'ndjson' in (probe['contentType'] or ''):
        return [{'data': line} for line in probe['body'].splitlines() if line.strip()]
    return parse_sse_events(probe['body'])


def write_jsonl(path, records):
    path.write_text(''.join(json.dumps(item, ensure_ascii=False) + '\n' for item in records))


def record_http_sse(out, side, port, token):
    """Record deterministic HTTP reads and the SSE event streams for one side.

    Stateful list endpoints (e.g. /api/projects) are deliberately not part of the
    strict recording: both sides share one TS ui/server process, so their
    wall-clock fields and session counts drift between the direct and proxied
    read even with a perfect relay. Relay transparency is exercised on
    deterministic reads plus the SSE streams."""
    directory = out / 'compare' / side
    directory.mkdir(parents=True, exist_ok=True)
    auth = {'authorization': f'Bearer {token}'} if token else {}
    http_records = []
    for case, method, path, extra in HTTP_CASES:
        response = raw_http(port, method, path, {**auth, **extra})
        http_records.append({'case': case, 'method': method, 'path': path,
                             'status': response['status'], 'headers': response['headers'],
                             'body': response['body'].decode('utf-8', 'replace')})
    write_jsonl(directory / 'http.jsonl', http_records)
    sse_records = []
    for case, path, extra, _kind in SSE_CASES:
        events = probe_sse_events(port, path, {**auth, **extra})
        for index, event in enumerate(events):
            sse_records.append({'case': case, 'index': index, 'sseId': event.get('id'),
                                'event': event.get('event', 'message'), 'data': event.get('data', '')})
    write_jsonl(directory / 'sse.jsonl', sse_records)
    return {'http': len(http_records), 'sse': len(sse_records)}


def record_ws(out, side, port):
    directory = out / 'compare' / side
    directory.mkdir(parents=True, exist_ok=True)
    driver = run_driver(out, 'chat-raw', port, f'compare-{side}',
                        extra_env={'EXP008_SESSION': COMPARE_SESSIONS[side]})
    ws_records = ([{'case': 'chat-raw', 'dir': 'c2u', 'opcode': 'text', 'raw': raw}
                   for raw in driver.get('sent', [])]
                  + [{'case': 'chat-raw', 'dir': 'u2c', 'opcode': 'text', 'raw': raw}
                     for raw in driver['raw']])
    write_jsonl(directory / 'ws.jsonl', ws_records)
    return {'ws': len(ws_records)}


def ensure_recordings(out):
    marker = out / 'compare' / 'direct' / 'ws.jsonl'
    if marker.exists():
        return
    token = ensure_token(out)
    # HTTP + SSE for both sides first, then the two stateful WS runs, so neither
    # side's HTTP reads can observe the other side's session.
    for side, port in (('direct', PORTS['ui']), ('proxy', PORTS['rust'])):
        record_http_sse(out, side, port, token)
    for side, port in (('direct', PORTS['ui']), ('proxy', PORTS['rust'])):
        record_ws(out, side, port)


def run_compare(out, name, controls=False):
    report_path = out / f'{name}.json'
    command = [sys.executable, str(SCRIPT / 'compare.py'), str(out / 'compare/direct'),
               str(out / 'compare/proxy'), '--out', str(report_path), '--path', str(out / 'project')]
    if controls:
        command.append('--controls')
    result = subprocess.run(command, capture_output=True, text=True, timeout=180)
    if not report_path.exists():
        raise RuntimeError(f'compare.py produced no report:\n{result.stdout[-800:]}\n{result.stderr[-800:]}')
    return json.loads(report_path.read_text())


def scenario_compare(out):
    ensure_recordings(out)
    report = run_compare(out, 'comparison')
    assert report['verdict'] == 'PASS', f'comparator found differences: {report["differences"][:3]}'
    return {'status': 'PASS', 'counts': report['counts'], 'differenceCount': 0}


def scenario_controls(out):
    ensure_recordings(out)
    report = run_compare(out, 'controls', controls=True)
    broken = [item['name'] for item in report['controls'] if not item['passed']]
    assert report['verdict'] == 'PASS' and not broken, f'positive controls misbehaved: {broken}'
    injected = [item['name'] for item in report['controls'] if item['expect'] == 'FAIL']
    return {'status': 'PASS', 'controls': len(report['controls']), 'injected': injected}


def scenario_sse_resume(out):
    """Scenario 7: prove the relay preserves every resumption input, then probe
    the real route. The real GET branch reads Last-Event-ID / ?lastEventId= /
    ?afterSeq= (ui/server/routes/modules.js:334-339) before it needs a configured
    StaffDeck module host, so without that host the route answers 501 and the
    only end-to-end resume proof available in B1 is the injected route."""
    token = ensure_token(out)
    auth = {'authorization': f'Bearer {token}'} if token else {}
    records = {}
    for case, path, extra, expected_seq, expected_source in (
            ('resume-none', '/exp008-stream/resume', {}, [1, 2, 3, 4, 5], 'none'),
            ('resume-header', '/exp008-stream/resume', {'Last-Event-ID': '2'}, [3, 4, 5], 'header'),
            ('resume-lastEventId', '/exp008-stream/resume?lastEventId=4', {}, [5], 'lastEventId'),
            ('resume-afterSeq', '/exp008-stream/resume?operation=preview_job_events&afterSeq=1',
             {}, [2, 3, 4, 5], 'afterSeq')):
        direct = probe_sse_events(PORTS['ui'], path, {**auth, **extra})
        proxy = probe_sse_events(PORTS['rust'], path, {**auth, **extra})
        direct_seqs = [json.loads(e['data'])['seq'] for e in direct]
        proxy_seqs = [json.loads(e['data'])['seq'] for e in proxy]
        proxy_sources = {json.loads(e['data'])['resumeSource'] for e in proxy if e.get('data')}
        assert direct_seqs == proxy_seqs == expected_seq, \
            f'{case}: direct={direct_seqs} proxy={proxy_seqs} expected={expected_seq}'
        assert proxy_sources == {expected_source}, f'{case}: resume source {proxy_sources}'
        records[case] = {'expectedSeq': expected_seq, 'direct': direct_seqs, 'proxy': proxy_seqs,
                         'resumeSource': sorted(proxy_sources)}
    real = {}
    for side, port in (('direct', PORTS['ui']), ('proxy', PORTS['rust'])):
        response = raw_http(port, 'GET', '/api/modules/staffdeck-sdk/events?operation=job_events',
                            {**auth, 'Last-Event-ID': '2'})
        real[side] = {'status': response['status'],
                      'body': response['body'].decode('utf-8', 'replace')}
    assert real['direct']['status'] == real['proxy']['status'], f'real route status: {real}'
    assert real['direct']['body'] == real['proxy']['body'], f'real route body: {real}'
    save(out, 'sse-resume.json', {'injectedResume': records, 'realRoute': real,
         'realRouteResumable': real['proxy']['status'] < 300})
    return {'status': 'PASS', 'injectedCases': list(records), 'realRouteStatus': real['proxy']['status'],
            'realRouteResumable': real['proxy']['status'] < 300}


def scenario_equivalence(out):
    # Same command through Rust and directly at the TS server, each in its own
    # fresh session, then compare after scrubbing only volatile values.
    proxy = run_driver(out, 'chat-raw', PORTS['rust'], 'equiv-proxy',
                       extra_env={'EXP008_SESSION': 'web:exp008-equiv-proxy'})
    direct = run_driver(out, 'chat-raw', PORTS['ui'], 'equiv-direct',
                        extra_env={'EXP008_SESSION': 'web:exp008-equiv-direct'})
    volatile_keys = {'id', 'entryId', 'sessionId', 'createdAt', 'updatedAt', 'timestamp', 'runId',
                     'messageId', 'requestId', 'generation', 'seq', 'completedRunId', 'revision',
                     'used', 'displayUsed', 'ratio', 'turnId', 'blockId', 'toolCallId'}

    def scrub(value):
        if isinstance(value, dict):
            return {key: ('VOLATILE' if key in volatile_keys else scrub(item))
                    for key, item in value.items()}
        if isinstance(value, list):
            return [scrub(item) for item in value]
        return value

    def normalise(lines):
        normalised = []
        for line in lines:
            try:
                frame = json.loads(line)
            except Exception:
                normalised.append(line)
                continue
            normalised.append(scrub(frame))
        return normalised

    proxy_frames = normalise(proxy['raw'])
    direct_frames = normalise(direct['raw'])
    differences = []
    for index in range(max(len(proxy_frames), len(direct_frames))):
        left = proxy_frames[index] if index < len(proxy_frames) else None
        right = direct_frames[index] if index < len(direct_frames) else None
        if left != right:
            differences.append({'index': index, 'proxy': left, 'direct': right})
    save(out, 'equivalence.json', {'proxyFrames': len(proxy_frames), 'directFrames': len(direct_frames),
         'differences': differences[:10], 'status': 'PASS' if not differences else 'FAIL'})
    assert not differences, f'normalised frame streams differ: {differences[:2]}'
    return {'status': 'PASS', 'frames': len(proxy_frames)}


def scenario_shadow(out):
    token = (out / 'token.txt').read_text().strip() if (out / 'token.txt').exists() else ''
    headers = {'authorization': f'Bearer {token}'} if token else {}
    paths = [
        ('/api/projects/files?projectKey=exp008-fixture', 'projects/files must beat /api/projects'),
        ('/api/sessions/model', '/api/sessions/model must beat /api/sessions'),
        ('/api/mcp/config/read', 'shadowed by /api/mcp/config/:scope'),
    ]
    records = []
    for path, note in paths:
        direct = raw_http(PORTS['ui'], 'GET', path, headers)
        proxy = raw_http(PORTS['rust'], 'GET', path, headers)
        same = direct['status'] == proxy['status'] and direct['body'] == proxy['body']
        records.append({'path': path, 'note': note, 'directStatus': direct['status'],
                        'proxyStatus': proxy['status'], 'same': same,
                        'proxyHandler': proxy['headers'].get('x-pilotdeck-handler')})
    save(out, 'shadow.json', records)
    assert all(r['same'] for r in records), f'shadow probes differ: {records}'
    assert all(r['proxyHandler'] == 'ts' for r in records), 'shadow probes must be tagged ts'
    return {'status': 'PASS', 'probes': [r['path'] for r in records]}


def scenario_routes(out):
    enum_home = out / 'enum-home'
    (enum_home / 'projects/exp008-fixture').mkdir(parents=True, exist_ok=False)
    (enum_home / 'server-token').write_text(TOKEN + '\n')
    (enum_home / 'server-token').chmod(0o600)
    (enum_home / 'pilotdeck.yaml').write_text(
        f"webui:\n  runtime:\n    host: 127.0.0.1\n    serverPort: 13003\n"
        f"    databasePath: {enum_home / 'auth.db'}\n")
    enum_out = out / 'router-enumeration.json'
    env = dict(build_env(out, 'enum'),
               PILOT_HOME=str(enum_home),
               PILOTDECK_CONFIG_PATH=str(enum_home / 'pilotdeck.yaml'),
               SERVER_PORT='13003',
               EXP008_BASELINE=str(BASELINE),
               EXP008_ENUM_OUT=str(enum_out))
    proc = start('enumerate', ['node', '--import', str(SCRIPT / 'enumerate_hook.mjs'), '--import',
                 str(BASELINE / 'node_modules/tsx/dist/loader.mjs'),
                 str(BASELINE / 'ui/server/index.js')], env, 13003, cwd=out / 'project')
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        if enum_out.exists():
            break
        if proc.poll() is not None and not enum_out.exists():
            raise RuntimeError(f'enumerator exited {proc.returncode} without output')
        time.sleep(.25)
    assert enum_out.exists(), 'route enumeration produced no JSON'
    proc.wait(timeout=15)
    data = json.loads(enum_out.read_text())
    assert data['uniqueRouteCount'] > 100, f'implausible route enumeration: {data["uniqueRouteCount"]}'
    reconcile = out / 'route-reconciliation.json'
    subprocess.run(['python3', str(SCRIPT / 'reconcile_routes.py'), str(enum_out),
                    str(ROOT / 'docs/research/006_phase-b-ui-server/route-inventory.md'),
                    '--out', str(reconcile)], check=True, timeout=60)
    # Coarse reconciliation against the static inventory's 264 registrations.
    static = 264
    data['staticInventoryRegistrations'] = static
    data['uniqueRuntimeMinusStatic'] = data['uniqueRouteCount'] - static
    # only-static cannot be computed from the markdown inventory here; record the
    # method+path set so a later reconcile can diff it.
    save(out, 'router-enumeration-summary.json', {
        'uniqueRouteCount': data['uniqueRouteCount'],
        'routeCount': data['routeCount'],
        'middlewareOrUnknownLayers': data.get('middlewareOrUnknownLayers'),
        'staticInventoryRegistrations': static,
        'uniqueRuntimeMinusStatic': data['uniqueRuntimeMinusStatic'],
        'appCaptured': data['appCaptured'],
    })
    return {'status': 'PASS', 'uniqueRuntime': data['uniqueRouteCount'],
            'routeCount': data['routeCount'], 'static': static,
            'diff': data['uniqueRuntimeMinusStatic']}


SCENARIOS = {
    'static': scenario_static,
    'browser': scenario_browser,
    'readonly': scenario_readonly,
    'chat': scenario_chat,
    'pty': scenario_pty,
    'sse': scenario_sse,
    'sse-resume': scenario_sse_resume,
    'compare': scenario_compare,
    'controls': scenario_controls,
    'equivalence': scenario_equivalence,
    'shadow': scenario_shadow,
    'routes': scenario_routes,
}


def fresh_out(out):
    # Refuse to clobber a previous run: archive it next to itself.
    if out.exists():
        index = 1
        while (out.parent / f'{out.name}-attempt-{index}').exists():
            index += 1
        out.rename(out.parent / f'{out.name}-attempt-{index}')
    out.mkdir(parents=True)
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', type=Path, default=ROOT / '_tmp/20261014-exp008')
    parser.add_argument('--only', default='', help='comma-separated scenario names')
    args = parser.parse_args()
    out = fresh_out(args.out.resolve())
    for port in ALL_PORTS:
        with socket.socket() as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(('127.0.0.1', port))
    home, project = make_home(out)
    env = build_env(out, 'main')
    env_gateway = dict(env, EXP008_MOCK='1')

    def gateway():
        proc = start('gateway', ['node', '--import', str(SCRIPT / 'preload.mjs'),
                     str(BASELINE / 'dist/src/cli/pilotdeck.js'), 'server'],
                     env_gateway, PORTS['gateway'], cwd=project)
        wait_http(out, PORTS['gateway'], proc, '/health')
        return proc

    def ui():
        proc = start('ui', ['node', '--import', str(SCRIPT / 'preload.mjs'), '--import',
                     str(BASELINE / 'node_modules/tsx/dist/loader.mjs'),
                     str(BASELINE / 'ui/server/index.js')],
                     dict(env, EXP008_UI_TEST_ROUTES='1'), PORTS['ui'], cwd=project)
        wait_http(out, PORTS['ui'], proc, '/health')
        return proc

    def rust():
        proc = start('rust', [str(RUST)], dict(env, EXP008_LISTEN=f"127.0.0.1:{PORTS['rust']}",
                     EXP008_UPSTREAM=f"http://127.0.0.1:{PORTS['ui']}",
                     EXP008_HANDLER='ts', EXP008_WS_LOG=str(out / 'ws-frames.jsonl')),
                     PORTS['rust'], cwd=ROOT)
        wait_http(out, PORTS['rust'], proc, '/health')
        return proc

    def vite():
        proc = start('vite', ['node', str(BASELINE / 'ui/node_modules/vite/bin/vite.js'),
                     '--config', str(SCRIPT / 'vite.config.mjs'), '--configLoader', 'runner',
                     '--host', '127.0.0.1', '--port', str(PORTS['vite']), '--strictPort'],
                     dict(env, SERVER_PORT=str(PORTS['rust']), PROXY_HOST='127.0.0.1',
                          EXP008_BASELINE=str(BASELINE),
                          EXP008_VITE_CACHE=str(out / 'vite-cache')),
                     PORTS['vite'], cwd=BASELINE / 'ui')
        wait_http(out, PORTS['vite'], proc, '/')
        return proc

    try:
        gateway()
        ui()
        rust()
        vite()
        selected = [name.strip() for name in args.only.split(',') if name.strip()] or list(SCENARIOS)
        if 'browser' not in selected:
            ensure_token(out)
        for name in selected:
            scenario(RESULTS, out, name, lambda name=name: SCENARIOS[name](out))
    finally:
        for proc, log, entry in reversed(PROCESSES):
            stop(proc)
            entry['exitCode'] = proc.returncode
            entry['stopped'] = proc.poll() is not None
            log.close()
        save(out, 'processes.json', [item[2] for item in PROCESSES])
        ports = {}
        for port in sorted(ALL_PORTS):
            try:
                with socket.socket() as sock:
                    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    sock.bind(('127.0.0.1', port))
                ports[str(port)] = 'released'
            except OSError as error:
                ports[str(port)] = str(error)
        owned_groups = {proc.pid for proc, _, _ in PROCESSES}
        remaining = []
        for group in sorted(owned_groups):
            try:
                os.killpg(group, 0)
                remaining.append(group)
            except ProcessLookupError:
                pass
        audit = {'allRecordedProcessesStopped': all(item[0].poll() is not None for item in PROCESSES),
                 'cleanupErrors': CLEANUP_ERRORS, 'ports': ports,
                 'ownedProcessGroupsRemaining': remaining,
                 'allOwnedProcessGroupsGone': not remaining and not CLEANUP_ERRORS,
                 'allListenersLoopback': bool(LISTENERS) and all(item['loopbackOnly'] for item in LISTENERS)}
        save(out, 'audit.json', audit)
    clean = (audit['allRecordedProcessesStopped'] and audit['allListenersLoopback']
             and audit['allOwnedProcessGroupsGone'] and all(v.startswith('released') for v in ports.values()))
    return 0 if clean and RESULTS and all(item['status'] in ('PASS', 'PASS-with-note') for item in RESULTS.values()) else 1


if __name__ == '__main__':
    sys.exit(main())
