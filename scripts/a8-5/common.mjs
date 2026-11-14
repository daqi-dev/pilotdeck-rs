/**
 * #1353 A8.5 切换状态机矩阵 — TS 探针公共库（.mjs，纯 node 直跑；Node ≥ 22——
 * 本件只用 fs/path/url 的稳定 API；a8-5 探针的主模块守卫在 import.meta.main 未提供
 * 的 Node 段（22.0–22.16）经 argv realpath 兜底支持直接执行，见 p16_stop_acceptance
 * .mjs fix-r4）。
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

/** Connection 头值（裸层取「connection:」后的值串、node 层取头原值）→ 是否含
 * keep-alive token：小写、去 OWS、按逗号切分（容忍 `Connection:keep-alive`、
 * 行尾空白、多值列表）。close 优先（RFC 7230 §6.1：close 出现即收尾不持久，
 * 即使同列表还有 keep-alive）。头缺失的语义不在本函数——由调用方结合
 * 「完整 HTTP/1.1 响应头」按 RFC 7230 §6.3 判隐式 keep-alive。
 * #2756 自 p16 内联收敛入 common（原 #2662 fix-r2/r3 形状逐字保留）；
 * fix-r1 起 p16 裸层改经 keepAlivePremise 共用，本函数外部用法归零、收回 private。 */
function keepAliveToken(v) {
  const tokens = v.toLowerCase().split(",").map((t) => t.trim());
  return !tokens.includes("close") && tokens.includes("keep-alive");
}

/** keep-alive 入池前提判定（#2756，#2662 p16 fix-r2 三态形状的推广）：
 * 输入 keepAliveProbe.once 的返回值（或等价形状），输出 { premise, premiseReason }。
 * - 请求失败（ok !== true）→ premise:false, reason:"request-failed"（与连接头失败分立可辨）；
 * - 成功且 Connection 含 keep-alive token（close 优先）→ premise:true, reason:"connection-keep-alive"；
 * - 成功、缺 Connection 头、HTTP/1.1 响应 → premise:true, reason:"implicit-keep-alive-http/1.1"
 *   （RFC 7230 §6.3：完整 HTTP/1.1 响应缺头按隐式 keep-alive，agent 会入池）；
 * - 成功、缺头但非 HTTP/1.1（如 1.0 缺省 close）→ premise:false, reason:"no-header-non-http/1.1"；
 * - 成功且 Connection 显式非 keep-alive（close 等）→ premise:false, reason:"connection-not-keep-alive"。
 * 前提不成立 = agent 池内不会有该 socket 的 idle 复用，后续同 agent 请求是
 * 新连接——「复用已关连接 / 既有 keep-alive 被拆」类结论失据，调用方须降级。
 * #2756 fix-r1（simplify P2）：p16 裸层 rawPremise 同用本件（隐式规则单源），
 * 口径差异对拍登记：非 1.1 响应带显式 `Connection: keep-alive` 时旧内联判 false
 * （http11 一票否决）、本件判 true（显式 token ⇒ 持久，RFC 7230 §6.1）——真实
 * 基线恒 1.1，该分支不可达，语义本件更准；原值照旧由调用方入记录。 */
export function keepAlivePremise(r) {
  if (r?.ok !== true) {
    return { premise: false, premiseReason: "request-failed" };
  }
  const connection = typeof r.connection === "string" ? r.connection : undefined;
  if (connection === undefined) {
    return String(r.httpVersion) === "1.1"
      ? { premise: true, premiseReason: "implicit-keep-alive-http/1.1" }
      : { premise: false, premiseReason: "no-header-non-http/1.1" };
  }
  return keepAliveToken(connection)
    ? { premise: true, premiseReason: "connection-keep-alive" }
    : { premise: false, premiseReason: "connection-not-keep-alive" };
}

/** keep-alive 连接探测：同一条 TCP 上的连续请求（观察 close 后旧连接行为）。
 * #2756：once 返回值带前提判定（premise/premiseReason，见 keepAlivePremise）——
 * 首次请求 ok 且响应可保持连接（Connection: keep-alive，或完整 HTTP/1.1 缺头
 * 按隐式）才会入池；后续「复用」结论以该前提成立为前提。错误路径带 code/message
 * 分立字段（connect 相位判定用）。
 * fix-r1（boundary P1 仲裁）：once 返回加 reusedSocket（本请求是否复用池内
 * socket，req.reusedSocket）——fix-r2 措辞限定：仅对 close/SIGTERM 前发出的
 * 在途请求（p6 keepalive-http）有新旧连接判别力，close 后两路径（正常重建连 /
 * 池提前收尾的新连接）同为 false、只作参考（判别依据是池占用复检）；
 * 另暴露 freeSocketCount()（agent.freeSockets 占用数）供调用方在 close/SIGTERM
 * 前最后一刻复检池占用——服务端空闲超时会收掉池内 idle socket（实测 ~5.5–6s，
 * keepAliveTimeout 5000 + 余量），且 server.close() 本身 ~11ms 内即 FIN 掉池内
 * socket（close 后池必空，复检必须在 close 前）。
 * fix-r2（concurrency#3 残余）：freeSocketCount 改按各 origin 键下数组长度
 * 求和——原 Object.keys().length 数的是 origin 键数，同 origin 多 socket 时
 * 低估；键删除时机虽经实测（v22.22.0 空闲收尾后键已删、freeSockets 同步填充，
 * 见 fix-r2 仲裁矩阵），但求和版不依赖该未文档化行为、语义即函数名。
 * #2759 fix-r2（两线整合）：另暴露 freeSocketKeys()（agent.freeSockets 的 origin
 * 键数组，形如 `host:port:localPort`——Agent.getName 产物，同 origin 多 socket
 * 共用一键，消费方只可判「该 origin 是否有 idle socket」；freeSockets 不可观测时
 * undefined——fix-r3 实测（v22.22.0）：Agent 构造器即把 freeSockets 初始化为对象、
 * keepAlive 与否皆是，该 undefined 支路运行时不可达，系防御性分支（人工格
 * derivePooledPremise(…, undefined) 覆盖），不是运行时降级路径）——
 * p16 的入池直测（derivePooledPremise 的 direct 分量、freeKeysBeforeReuse）在
 * 共用 agent 上取直接观测量，与 freeSocketCount 的 close 前复检并存：前者证
 * 「请求 1 已入池」（direct 压过头推断），后者证「close 前池内仍占用」。 */
export async function keepAliveProbe(port) {
  const http = await import("node:http");
  const agent = new http.Agent({ keepAlive: true, keepAliveMsecs: 1000 });
  const freeSocketCount = () => Object.values(agent.freeSockets ?? {}).reduce(
    (n, a) => n + (Array.isArray(a) ? a.length : 0), 0);
  const freeSocketKeys = () => agent.freeSockets === undefined
    ? undefined
    : Object.keys(agent.freeSockets);
  const once = (pathName) =>
    new Promise((res) => {
      const req = http.request(
        { host: "127.0.0.1", port, path: pathName, agent, timeout: 3000 },
        (r) => {
          let body = "";
          r.on("data", (c) => (body += c));
          r.on("end", () => res({ ok: true, status: r.statusCode, httpVersion: r.httpVersion, connection: r.headers.connection, body: body.slice(0, 120), reusedSocket: req.reusedSocket }));
        },
      );
      req.on("timeout", () => {
        req.destroy();
        res({ ok: false, error: "timeout", reusedSocket: req.reusedSocket });
      });
      req.on("error", (e) => res({ ok: false, code: e.code, message: e.message, error: `${e.code ?? e.name}: ${e.message}`, reusedSocket: req.reusedSocket }));
      req.end();
    }).then((r) => ({ ...r, ...keepAlivePremise(r) }));
  return { once, freeSocketCount, freeSocketKeys, destroy: () => agent.destroy() };
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
