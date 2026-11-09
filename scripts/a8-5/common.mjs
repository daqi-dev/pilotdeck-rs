/**
 * #1353 A8.5 切换状态机矩阵 — TS 探针公共库（.mjs，纯 node 直跑；Node ≥ 22）。
 * 驱动只读 TS 基线（PilotDeck）的 dist/ 构建；行号引用一律以 src/*.ts 为准。
 *
 * 用法（每个探针同参）：
 *   node scripts/a8-5/<probe>.mjs [--base <PilotDeck 根>] [--out <证据目录>]
 * 默认 --base  取 env PILOTDECK_BASE，再退回 /Users/da/ws/pdrs-runner/resources/PilotDeck。
 * 默认 --out   取 env A85_OUT，再退回 <repo 根>/_tmp/a8-5-raw/<probe 名>。
 * 只读基线；所有落盘都在 --out 指定的证据目录内（隔离 homes / spill）。
 */
import { mkdirSync, writeFileSync, existsSync, renameSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPTS_DIR = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(SCRIPTS_DIR, "..", "..");

export function parseProbeArgs(defaultOutName) {
  const argv = process.argv.slice(2);
  const get = (flag) => {
    const i = argv.indexOf(flag);
    return i >= 0 ? argv[i + 1] : undefined;
  };
  const base = get("--base") ?? process.env.PILOTDECK_BASE
    ?? "/Users/da/ws/pdrs-runner/resources/PilotDeck";
  const out = resolve(get("--out") ?? process.env.A85_OUT
    ?? resolve(REPO_ROOT, "_tmp", "a8-5-raw", defaultOutName));
  return { base, out };
}

export function makePaths({ base, out }, probeName) {
  mkdirSync(out, { recursive: true });
  return {
    BASE: base,
    DIST: `${base}/dist/src`,
    EVIDENCE_DIR: out,
    RAW_DIR: out,
    PROBE_NAME: probeName,
  };
}

export const TOKEN = "probe-token-1353";

export class Recorder {
  constructor(rawDir) {
    this.rawDir = rawDir;
    this.entries = [];
  }
  log(kind, detail) {
    this.entries.push({ t: new Date().toISOString(), kind, detail });
  }
  dump(name) {
    const path = resolve(this.rawDir, name);
    const tmp = `${path}.tmp`;
    writeFileSync(tmp, JSON.stringify({ node: process.version, entries: this.entries }, null, 2));
    renameSync(tmp, path); // 原子发布
    return path;
  }
}

export function makeHome(rawDir, name) {
  const home = resolve(rawDir, "homes", name);
  mkdirSync(home, { recursive: true });
  return home;
}

export function makeStaticDir(rawDir) {
  const dir = resolve(rawDir, "static");
  mkdirSync(dir, { recursive: true });
  if (!existsSync(resolve(dir, "index.html"))) {
    writeFileSync(resolve(dir, "index.html"), "<html>probe-static</html>\n");
  }
  return dir;
}

/** 记录调用 + 可观察 dispose 的假 Gateway（server 层探针用）。 */
export function makeFakeGateway() {
  const calls = [];
  const gw = {
    calls,
    disposed: false,
    async describeServer() {
      calls.push({ method: "describeServer", at: new Date().toISOString() });
      return { ok: true, probe: "fake-gateway" };
    },
    async listProjects() {
      calls.push({ method: "listProjects", at: new Date().toISOString() });
      return [];
    },
    async describeProject() {
      calls.push({ method: "describeProject", at: new Date().toISOString() });
      return { projectKey: "probe" };
    },
    async listSessions() {
      calls.push({ method: "listSessions", at: new Date().toISOString() });
      return { sessions: [] };
    },
    dispose(reason = "gateway_disposed") {
      calls.push({ method: `dispose:${reason}`, at: new Date().toISOString() });
      gw.disposed = true;
    },
  };
  return gw;
}

export function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

export function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject, entered: false };
}

/** 极简 WS 探针客户端（ws@8 来自基线 node_modules，仅 loopback）。 */
export async function wsConnect(base, port, token, { handshakeTimeout = 3000 } = {}) {
  const mod = await import(`${base}/node_modules/ws/index.js`);
  const WebSocket = mod.default ?? mod.WebSocket;
  const sock = new WebSocket(`ws://127.0.0.1:${port}/ws`, { handshakeTimeout });
  const client = new WsProbeClient(sock, token);
  await client.openPromise;
  return client;
}

export class WsProbeClient {
  constructor(sock, token) {
    this.sock = sock;
    this.token = token;
    this.nextId = 0;
    this.pending = new Map();
    this.helloOk = undefined;
    this.closed = false;
    this.closeEvent = undefined;
    this.openPromise = new Promise((res, rej) => {
      const t = setTimeout(() => rej(new Error("ws open timeout")), 3000);
      sock.once("open", () => {
        clearTimeout(t);
        res();
      });
      sock.once("error", (e) => {
        clearTimeout(t);
        rej(e);
      });
    });
    sock.on("message", (data) => {
      const frame = JSON.parse(data.toString());
      if (frame.type === "hello_ok") {
        this.helloOk = frame;
      } else if ((frame.type === "response" || frame.type === "event") && frame.id && this.pending.has(frame.id)) {
        const settle = this.pending.get(frame.id);
        this.pending.delete(frame.id);
        settle(frame);
      }
    });
    sock.on("close", (...args) => {
      this.closed = true;
      this.closeEvent = args;
    });
    sock.on("error", () => undefined);
  }

  get open() {
    return !this.closed && this.sock.readyState === 1;
  }

  async hello() {
    this.sock.send(JSON.stringify({
      type: "hello",
      protocolVersion: "1.2",
      clientName: "web",
      clientVersion: "probe-a85",
      token: this.token,
    }));
    return this.waitHello();
  }

  waitHello(timeoutMs = 3000) {
    return new Promise((res) => {
      const started = Date.now();
      const tick = () => {
        if (this.helloOk !== undefined) res(this.helloOk);
        else if (this.closed) res({ closed: true, closeEvent: this.closeEvent });
        else if (Date.now() - started > timeoutMs) res({ timeout: true });
        else setTimeout(tick, 20);
      };
      tick();
    });
  }

  request(method, params, timeoutMs = 3000) {
    const id = `probe-${++this.nextId}`;
    const reply = new Promise((res) => {
      const timer = setTimeout(() => res({ timeout: true, method }), timeoutMs);
      this.pending.set(id, (frame) => {
        clearTimeout(timer);
        res(frame);
      });
    });
    this.sock.send(JSON.stringify({ type: "request", id, method, params }));
    return reply;
  }

  close(code, reason) {
    try {
      this.sock.close(code, reason);
    } catch {
      /* already closed */
    }
  }
}

/** 裸 fetch 探测（新连接）。 */
export async function httpProbe(port, pathName, { method, headers, timeoutMs = 3000 } = {}) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetch(`http://127.0.0.1:${port}${pathName}`, { method, headers, signal: controller.signal });
    const body = await res.text();
    return { ok: true, status: res.status, body: body.slice(0, 300) };
  } catch (error) {
    return { ok: false, error: `${error?.name ?? "Error"}: ${error?.message ?? error}` };
  } finally {
    clearTimeout(timeout);
  }
}

/** keep-alive 连接探测：同一条 TCP 上的连续请求（观察 close 后旧连接行为）。 */
export async function keepAliveProbe(port) {
  const http = await import("node:http");
  const agent = new http.Agent({ keepAlive: true, keepAliveMsecs: 1000 });
  const once = (pathName) =>
    new Promise((res) => {
      const req = http.request(
        { host: "127.0.0.1", port, path: pathName, agent, timeout: 3000 },
        (r) => {
          let body = "";
          r.on("data", (c) => (body += c));
          r.on("end", () => res({ ok: true, status: r.statusCode, connection: r.headers.connection, body: body.slice(0, 120) }));
        },
      );
      req.on("timeout", () => {
        req.destroy();
        res({ ok: false, error: "timeout" });
      });
      req.on("error", (e) => res({ ok: false, error: `${e.code ?? e.name}: ${e.message}` }));
      req.end();
    });
  return { once, destroy: () => agent.destroy() };
}

/** 新 TCP 是否被拒（连接层）。 */
export async function tcpRefused(port) {
  const net = await import("node:net");
  return new Promise((res) => {
    const s = new net.Socket();
    s.setTimeout(1500);
    s.once("connect", () => {
      s.destroy();
      res(false);
    });
    s.once("error", () => res(true));
    s.once("timeout", () => {
      s.destroy();
      res(false);
    });
    s.connect(port, "127.0.0.1");
  });
}
