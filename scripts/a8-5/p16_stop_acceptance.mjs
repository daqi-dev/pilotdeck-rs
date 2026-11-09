/**
 * P16（#2258 停接纳机制）：两种停接纳机制的客户端观测面受控实测。
 * 机制 A（关 listener——TS 基线 `server.close()` 的实际机制）· 对真实 PilotDeck server：
 *   既有 keep-alive（裸 socket）收到服务端 FIN（EOF）；在同一条已 FIN 的 socket 上复用发第二请求——
 *   裸层观测（A14 的「重建连解释」此前只是机制推断，本格补实测：写是否成功、随后收到什么）；
 *   node:http agent 复用与 fetch 新连接的观测面；新 TCP ECONNREFUSED。
 * 机制 B（保留 bind + accept 后拒绝——基线不存在，纯 node 受控构造）：
 *   B-RST：accept 后 `destroy()`（RST）；B-EOF：accept 后 `end()`（FIN）。
 *   各以 裸 socket / node:http / fetch 三层观测：握手是否完成、随后收到 RST 还是 EOF、
 *   各层错误码形态。
 * 观察窗全部有界；结束收尾销毁全部 socket/关闭全部 server。
 */
import { connect, createServer } from "node:net";
import http from "node:http";
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, Recorder, sleep, tcpRefused,
  makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p16-stop-acceptance"), "p16_stop_acceptance");
const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 400)}`);
  rec.log(k, d);
};

const PORT_A = 39896; // 机制 A：真实 PilotDeck server
const PORT_B_RST = 39897; // 机制 B-RST：纯 node
const PORT_B_EOF = 39898; // 机制 B-EOF：纯 node

/** 本地 fetch 探测（记录 cause 链）。 */
async function fetchProbe(port, pathName) {
  try {
    const res = await fetch(`http://127.0.0.1:${port}${pathName}`);
    const body = await res.text();
    return { ok: true, status: res.status, body: body.slice(0, 80) };
  } catch (error) {
    return {
      ok: false,
      name: error?.name,
      message: error?.message,
      causeCode: error?.cause?.code,
      causeMessage: error?.cause?.message,
    };
  }
}

/** node:http 请求观测（新连接、无 agent 复用）。 */
function httpProbeRaw(port, pathName, timeoutMs = 3000) {
  return new Promise((res) => {
    const req = http.get({ host: "127.0.0.1", port, path: pathName, timeout: timeoutMs }, (r) => {
      let body = "";
      r.on("data", (c) => (body += c));
      r.on("end", () => res({ ok: true, status: r.statusCode, body: body.slice(0, 80) }));
    });
    req.on("timeout", () => {
      req.destroy();
      res({ ok: false, error: "timeout" });
    });
    req.on("error", (e) => res({ ok: false, code: e.code, message: e.message }));
  });
}

/** 裸 socket：读到一个完整（近似）响应头后保持打开。allowHalfOpen——收到服务端 FIN 后
 * 客户端可写侧保持打开，才能真实观测「复用已 FIN 连接」发送第二请求的裸层后果。 */
async function rawKeepAliveRequest(port) {
  const sock = connect({ port, host: "127.0.0.1", allowHalfOpen: true });
  const events = [];
  const opened = await new Promise((res, rej) => {
    sock.once("connect", () => res(true));
    sock.once("error", (e) => rej(e));
  });
  const dataPromise = new Promise((res) => {
    let buf = "";
    const onData = (c) => {
      buf += c.toString();
      if (buf.includes("\r\n\r\n") && buf.length > 60) {
        sock.off("data", onData);
        res(buf);
      }
    };
    sock.on("data", onData);
    setTimeout(() => res(buf), 2000);
  });
  sock.write(`GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n`);
  const head = await dataPromise;
  return { sock, events, opened, head };
}

function watchSock(sock, events, label) {
  sock.on("end", () => events.push({ label, event: "end(FIN/EOF)", at: Date.now() }));
  sock.on("close", (hadError) => events.push({ label, event: "close", hadError, at: Date.now() }));
  sock.on("error", (e) => events.push({ label, event: "error", code: e.code, message: e.message, at: Date.now() }));
  sock.on("data", (c) => events.push({ label, event: "data", bytes: c.length, at: Date.now() }));
}

async function partA() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const server = await startPilotDeckServer({
    gateway: makeFakeGateway(), port: PORT_A, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p16-a"),
  });

  // 既有 keep-alive：裸 socket 发请求 1（默认 keep-alive），读头后保持打开
  const raw = await rawKeepAliveRequest(PORT_A);
  const statusLine = raw.head.split("\r\n")[0];
  const connHeader = raw.head.toLowerCase().split("\r\n").find((l) => l.startsWith("connection:"));
  log("A/raw-keepalive-request1", { statusLine, connHeader });
  const rawEvents = [];
  watchSock(raw.sock, rawEvents, "raw-ka");

  const t0 = Date.now();
  const closePromise = server.close().then(
    () => ({ settled: true, ms: Date.now() - t0 }),
    () => ({ settled: true, rejected: true }),
  );

  // 观察 1：服务端对空闲 keep-alive 的处置（FIN 到达时刻）
  const finDeadline = Date.now() + 3000;
  while (Date.now() < finDeadline && !rawEvents.some((e) => e.event === "end(FIN/EOF)" || e.event === "close" || e.event === "error")) {
    await sleep(50);
  }
  await sleep(200);
  log("A/server-close-idle-keepalive", {
    eventsSoFar: rawEvents.map((e) => ({ ...e, relMs: e.at - t0 })),
    note: "机制 A：close() 主动关空闲 keep-alive（A14① 复核，裸层直接观测）",
  });

  // 观察 2：在同一条已 FIN 的 socket 上复用发请求 2（裸层复用态——A14 重建连推断的对照面）
  let write2 = { attempted: true };
  let writeAt = 0;
  try {
    writeAt = Date.now();
    raw.sock.write(`GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n`);
    write2.writeOk = true;
  } catch (e) {
    write2.writeOk = false;
    write2.error = { code: e.code, message: e.message };
  }
  await sleep(1500);
  log("A/raw-reuse-after-fin", {
    write2,
    eventsAfterWrite: rawEvents.filter((e) => e.at >= writeAt).map((e) => ({ event: e.event, code: e.code, hadError: e.hadError })),
    socketStillOpen: !raw.sock.destroyed,
    note: "裸层复用（allowHalfOpen）：对已收 FIN 的 socket 写第二请求的观测（写本身是否报错、随后 RST/EOF/静默）",
  });

  // 观察 3：agent 复用（node:http.Agent keep-alive）与 fetch、新 TCP
  const http2 = await import("node:http");
  const agent = new http2.Agent({ keepAlive: true, keepAliveMsecs: 1000 });
  const agentReuse = await new Promise((res) => {
    const req = http2.request({ host: "127.0.0.1", port: PORT_A, path: "/health", agent, timeout: 3000 }, (r) => {
      let body = "";
      r.on("data", (c) => (body += c));
      r.on("end", () => res({ ok: true, status: r.statusCode }));
    });
    req.on("timeout", () => { req.destroy(); res({ ok: false, error: "timeout" }); });
    req.on("error", (e) => res({ ok: false, code: e.code, message: e.message, phase: e.message.startsWith("connect") ? "connect-phase" : "other" }));
    req.end();
  });
  agent.destroy();
  log("A/agent-reuse-after-close", {
    agentReuse,
    fetchNew: await fetchProbe(PORT_A, "/health"),
    newTcpRefused: await tcpRefused(PORT_A),
    note: "agent 复用观测到的 ECONNREFUSED 是 connect 相位错误（重建连），非同 socket 报错——与裸层复用（写成功后 RST）对照",
  });

  log("A/close-settled", await Promise.race([closePromise, sleep(4000).then(() => ({ settled: false, note: "4s 窗未 settle（有活连接时预期）" }))]));
  raw.sock.destroy();
  log("A/close-settled-final", await Promise.race([closePromise, sleep(3000).then(() => ({ settled: false }))]));
}

/** 机制 B 纯 node 受控 server：accept 后 RST 或 EOF。 */
async function partB(port, mode /* "rst" | "eof" */) {
  const srv = createServer((sock) => {
    if (mode === "rst") sock.destroy();
    else sock.end();
  });
  await new Promise((res) => srv.listen(port, "127.0.0.1", res));

  // 裸 socket：握手是否完成 + 随后的事件序列
  const rawEvents = [];
  const rawResult = await new Promise((res) => {
    const sock = connect(port, "127.0.0.1");
    watchSock(sock, rawEvents, `b-${mode}`);
    sock.once("connect", () => rawEvents.push({ label: `b-${mode}`, event: "connect(handshake-ok)", at: Date.now() }));
    sock.write("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    setTimeout(() => { sock.destroy(); res(null); }, 2500);
  });
  await sleep(50);
  log(`B-${mode}/raw`, {
    events: rawEvents.map((e) => ({ event: e.event, code: e.code, hadError: e.hadError })),
    note: `机制 B-${mode === "rst" ? "RST" : "EOF"}：TCP 握手完成（connect 事件）后 ${
      mode === "rst" ? "收到 RST（error ECONNRESET/close hadError）" : "收到 EOF（end 事件，无错误）"
    }——对照机制 A 的 connect 相位 ECONNREFUSED`,
  });

  log(`B-${mode}/node-http`, {
    result: await httpProbeRaw(port, "/health"),
  });
  log(`B-${mode}/fetch`, {
    result: await fetchProbe(port, "/health"),
  });

  await new Promise((res) => srv.close(res));
}

async function main() {
  await partA();
  await partB(PORT_B_RST, "rst");
  await partB(PORT_B_EOF, "eof");
  rec.dump("p16_stop_acceptance.json");
  say("P16 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P16 FAILED: ${e?.stack ?? e}`);
  rec.dump("p16_stop_acceptance.json");
  process.exit(1);
});
