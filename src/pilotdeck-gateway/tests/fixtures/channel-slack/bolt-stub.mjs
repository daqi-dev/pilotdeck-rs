// #1344 stub-contract `@slack/bolt`: the exact seven-member surface
// `SlackChannel.ts` touches (the baseline installs no real bolt, so the
// contract is the committed wire shape both fixture sides implement —
// #1786 人拍 1). `new App({token, appToken, socketMode})` stores its config;
// `event("message", h)` registers the handler (any other event name throws);
// `error(h)` registers the error handler; `start()` POSTs
// `/api/apps.connections.open` with `{"token":appToken}`, opens the returned
// `url` as a WebSocket and resolves once open; `stop()` closes it with 1000;
// `client.auth.test(args)` / `client.chat.postMessage(args)` POST the
// argument object as the JSON body (key order preserved) and throw
// `Error: <error>` on `ok:false`. Any other member throws
// `unexpected SDK call <name>`. The mock endpoint is the loopback base in
// `PDRS_SLACK_MOCK` (e.g. `http://127.0.0.1:18183`).
const WEBSOCKET_ERROR = 'websocket error';

function mockBase() {
  const base = process.env.PDRS_SLACK_MOCK;
  if (!base) throw new Error('PDRS_SLACK_MOCK not set');
  return base;
}

async function post(path, body) {
  const response = await fetch(`${mockBase()}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return response.json();
}

function guarded(target, prefix) {
  return new Proxy(target, {
    get(object, name) {
      if (name in object) return object[name];
      throw new Error(`unexpected SDK call ${prefix}${String(name)}`);
    },
  });
}

class AppImpl {
  constructor(config) {
    this.config = config;
    this.messageHandlers = [];
    this.errorHandlers = [];
    this.ws = null;
    this.client = guarded({
      auth: guarded({
        test: async (args) => {
          const json = await post('/api/auth.test', args);
          if (!json.ok) throw new Error(json.error);
          return json;
        },
      }, 'client.auth.'),
      chat: guarded({
        postMessage: async (args) => {
          const json = await post('/api/chat.postMessage', args);
          if (!json.ok) throw new Error(json.error);
          return json;
        },
      }, 'client.chat.'),
    }, 'client.');
  }

  event(name, handler) {
    if (name !== 'message') throw new Error(`unexpected SDK call event:${name}`);
    this.messageHandlers.push(handler);
  }

  error(handler) {
    this.errorHandlers.push(handler);
  }

  start() {
    return new Promise((resolve, reject) => {
      void (async () => {
        try {
          const open = await post('/api/apps.connections.open', { token: this.config.appToken });
          if (!open.ok) throw new Error(open.error);
          const ws = new WebSocket(open.url);
          this.ws = ws;
          ws.addEventListener('open', () => resolve());
          ws.addEventListener('error', () => reject(new Error(WEBSOCKET_ERROR)));
          ws.addEventListener('close', (event) => {
            if (event.code !== 1000 && event.code !== 1005) {
              for (const handler of this.errorHandlers) void handler(new Error(WEBSOCKET_ERROR));
            }
          });
          ws.addEventListener('message', (frame) => {
            let value;
            try { value = JSON.parse(frame.data); } catch { return; }
            if (value?.type !== 'message') return;
            const event = value?.payload?.event;
            for (const handler of this.messageHandlers) void handler({ event });
          });
        } catch (error) {
          reject(error);
        }
      })();
    });
  }

  async stop() {
    this.ws?.close(1000);
  }
}

export function App(config) {
  return new Proxy(new AppImpl(config), {
    get(target, name) {
      if (name in target) return target[name];
      throw new Error(`unexpected SDK call ${String(name)}`);
    },
  });
}
