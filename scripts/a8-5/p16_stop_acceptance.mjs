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
  makeFakeGateway, makeHome, makeStaticDir, keepAliveProbe, keepAlivePremise,
  Recorder, sleep, tcpRefused, makePaths, parseProbeArgs, TOKEN,
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
  // #2662 fix-r1 sibling#4 / fix-r2 三角色同撞：裸层守门——响应 Connection 非 keep-alive
  // （显式 close）时 FIN 来自响应结束而非 server.close()，观察 1/2 的结论适用条件就变了，
  // 记 rawPremise、不成立时降级 note。「头缺失 = 隐式 keep-alive（RFC 7230 §6.3）」只在
  // **完整 HTTP/1.1 响应头**下成立：dataPromise 2s 兜底 resolve 的空/半截 buf、以及
  // HTTP/1.0 的缺省 close，都不得判成立（防空真）。
  // #2756 fix-r1（simplify P2）：隐式规则不再内联第二份——与 node 层共用 common 的
  // keepAlivePremise（单源）；headComplete/http11/connHeader 原值照旧入 record，
  // 口径差异见 common.mjs keepAlivePremise doc（非 1.1 + 显式 keep-alive token）。
  const headComplete = raw.head.includes("\r\n\r\n");
  const http11 = /^HTTP\/1\.1 /.test(statusLine);
  const connValue = connHeader === undefined ? undefined : connHeader.slice(connHeader.indexOf(":") + 1);
  const { premise: rawPremise } = keepAlivePremise({
    ok: headComplete,
    connection: connValue,
    httpVersion: http11 ? "1.1" : "1.0",
  });
  log("A/raw-keepalive-request1", { statusLine, connHeader, headComplete, http11, rawPremise });
  const rawEvents = [];
  watchSock(raw.sock, rawEvents, "raw-ka");

  // 复用直测前提（fix-r1 评审 P1）：close 前经同一 keep-alive agent 完成一次
  // 请求，socket 入池——close 后同 agent 的第二次请求才是「复用已关连接」的
  // 直测，而不是新建 agent 的新连接。
  // #2662：前提须守门——pooled1 失败、或响应 Connection 不是 keep-alive
  // （agent 不入池）时，观察 3 的 agent 复用直测无效，跳过并记
  // A/SKIP-agent-reuse，不再以「close 前池化」的 note 产出错误前提的
  // 证据记录；Connection 头与裸层 rawPremise 的 connHeader 对称捕获（行号易漂，见变量名）。
  // #2756（r5 concurrency/simplify 收敛）：内联 agent 炮台与 common.mjs 的
  // keepAliveProbe 同构，收敛共用；前提判定随 once 返回（premise/premiseReason，
  // 判定形状 = 原 #2662 fix-r1/r2 的 ok + 缺头按完整 HTTP/1.1 隐式 + close 优先，
  // 与 p2b/p1_p2/p6 三探针同形）。
  const ka = await keepAliveProbe(PORT_A);
  const pooled1 = await ka.once("/health");
  const pooledPremise = pooled1.premise === true;
  // fix-r2（DS sibling#1 撞条）：note 不再断言「复用直测前提成立」——该断言在
  // 池被服务端空闲超时收空时与 A/SKIP-pool-drained-before-reuse / agentReuse
  // skipped 互为反证；收窄为「请求 1 已入池」，close 前是否仍占用指向 SKIP 行。
  log("A/agent-pooled-request1", {
    pooled1,
    pooledPremise,
    note: pooledPremise
      ? "请求 1 已入池（close() 前经该 agent 完成）；close 前池内是否仍占用不作断言——见 A/SKIP-pool-drained-before-reuse（该行未出现即仍占用）"
      : "复用直测前提未成立（pooled1 失败或 Connection 非 keep-alive，socket 未入池）——见 A/SKIP-agent-reuse",
  });
  if (!pooledPremise) {
    log("A/SKIP-agent-reuse", {
      reason: pooled1.premiseReason === "request-failed"
        ? "pooled1 failed"
        : pooled1.premiseReason === "no-header-non-http/1.1"
          ? "响应缺 Connection 头且非 HTTP/1.1（隐式 keep-alive 不适用），socket 未入池"
          : "响应 Connection 显式非 keep-alive（close 等），socket 未入池",
      pooled1,
    });
  }
  // #2756 fix-r1（boundary P1 仲裁）：入池前提是「close 前最后一刻池内仍有该 idle
  // socket」，不是 pooled1 的一次性快照——S0→close 跨过服务端空闲收尾窗（实测
  // ~5.5–6s）时池已被收空，close 的 FIN 无池内 socket 可拆、agent 丢弃重建路径
  // 未被走到，观察 3 的复用直测退化为纯新连接（错误串可相同但解释失据）。复检
  // 必须在 close 前：server.close() 本身 ~11ms 内即 FIN 掉池内 socket（close 后
  // 池必空，实测见轮次证据 exp-close-pool.out 场景①）。
  const pooledAtClose = pooledPremise && ka.freeSocketCount() > 0;
  if (pooledPremise && !pooledAtClose) {
    log("A/SKIP-pool-drained-before-reuse", {
      reason: "池在 close 前被服务端空闲超时收尾（pooled1 入池后、close() 前池已空）",
      note: "复用直测降级：close 的 FIN 无池内 idle socket 可拆、agent 丢弃重建路径未被走到，close 后观测是新连接撞已关 listener（新连接对照已由 fetchNew/newTcpRefused 覆盖），「agent 丢弃已关 socket 重建连」解释失据",
    });
  }

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
  // #2662 fix-r3 三角色同撞：干净 FIN 只认 end(FIN/EOF)——allowHalfOpen 下裸 close
  // 事件只可能先经 error（RST 类异常拆链），把 close 算作「观测到 FIN」会让异常
  // 拆链冒充机制 A 的前提。fix-r4 boundary#1：FIN 判定须有 t0 下界（close() 之前
  // 到达的 end——如服务端 keepAliveTimeout 自行收尾——不构成机制 A 的证据），
  // 且每条观察按各自观测窗现算（观察 2 的写之前到达才算「已 FIN 连接上的写」）。
  const sawCleanFin = (from, to = Infinity) =>
    rawEvents.some((e) => e.event === "end(FIN/EOF)" && e.at >= from && e.at <= to);
  const finObserved = sawCleanFin(t0);
  log("A/server-close-idle-keepalive", {
    eventsSoFar: rawEvents.map((e) => ({ ...e, relMs: e.at - t0 })),
    rawPremise,
    finObserved,
    note: rawPremise && finObserved
      ? "机制 A：close() 主动关空闲 keep-alive（A14① 复核，裸层直接观测；FIN 在 close() 之后、窗内到达）"
      : rawPremise
        ? "close() 之后未观测到干净 FIN（finObserved=false，含 error→close 异常拆链情形）：FIN 时刻结论降级，不得据此引用 A14"
        : "前提未成立（见 A/raw-keepalive-request1 的 headComplete/http11/connHeader 原值）：FIN 时刻不代表 server.close() 对空闲 keep-alive 的处置",
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
  // fix-r4 boundary#1：本条的「已 FIN」指写之前（且 close() 之后）到达的干净 FIN，
  // 用写时刻上界现算，不复用观察 1 的布尔（FIN 落在 1.5s 观测窗内时与 note 自洽）。
  const finBeforeWrite = sawCleanFin(t0, writeAt);
  log("A/raw-reuse-after-fin", {
    write2,
    rawPremise,
    finObserved,
    finBeforeWrite,
    eventsAfterWrite: rawEvents.filter((e) => e.at >= writeAt).map((e) => ({ event: e.event, code: e.code, hadError: e.hadError })),
    socketStillOpen: !raw.sock.destroyed,
    note: rawPremise && finBeforeWrite
      ? "裸层复用（allowHalfOpen）：对已收 FIN 的 socket 写第二请求的观测（写本身是否报错、随后 RST/EOF/静默）"
      : rawPremise
        ? `未观测到写前干净 FIN（finBeforeWrite=false${finObserved ? "，FIN 在写后才到达" : "，含 error→close 异常拆链情形"}）：本条按「未 FIN 连接上的写」记录，不作 close 后复用的证据`
        : "前提未成立（见 A/raw-keepalive-request1；成因以该记录 headComplete/http11/connHeader 为准，可能是响应头不完整/非 1.1/显式 close——不预设）：本条按现状记录，不作 close 后复用的证据",
  });

  // 观察 3：同 agent（close 前已池化）复用 + fetch 新连接 + 新 TCP
  // #2662：前提不成立时跳过 agent 复用直测（池为空时测到的是新连接而非
  // 复用，证据无效）；fetch 新连接与新 TCP 探测与入池无关，照常记录。
  // #2756：复用直测改经 common keepAliveProbe 同一 agent（once 错误路径带
  // code/message 分立字段；connect 相位判定保留 e.code，免 message 格式漂移）。
  // fix-r1：前提改为 pooledAtClose（close 前最后一刻池占用，见 A/SKIP-pool-
  // drained-before-reuse）；失败分支保留 once 原字段（error 等随 {...r} 带出，
  // #2756 fix-r1 评审 concurrency P2——「疑似挂起」须与其它非预期形状可辨），
  // 成功分支另带 reusedSocket（fix-r2 措辞限定：close 后正常重建连与池提前
  // 收尾的新连接同为 false、无判别力，判别依据是上面的池占用复检，作参考）。
  // fix-r2（Grok simplify#2）：两处 reason 三元提局部常量，消除双份漂移面。
  const reuseSkipReason = pooledPremise
    ? "池在 close 前被服务端空闲超时收尾（见 A/SKIP-pool-drained-before-reuse）"
    : "复用直测前提未成立（见 A/SKIP-agent-reuse）";
  const agentReuse = pooledAtClose
    ? await ka.once("/health").then((r) => (r.ok === true
        ? r
        : { ...r, phase: r.code === "ECONNREFUSED" ? "connect-phase" : "other" }))
    : { skipped: true, reason: reuseSkipReason };
  ka.destroy();
  // #2662 fix-r3：note 随实测状态推导，不再无条件断言 ECONNREFUSED/connect 相位
  // 与裸层对照——agentReuse 非预期形状或裸层前提未成立时对应句不产出。
  const reuseNote = agentReuse.skipped
    ? `agent 复用直测跳过（${reuseSkipReason}）；fetch 新连接与 newTcpRefused 与入池无关，照常记录`
    : agentReuse.ok !== true && agentReuse.code === "ECONNREFUSED"
      ? "同 agent（close 前池化，见 A/agent-pooled-request1）复用直测：ECONNREFUSED 是 connect 相位错误（agent 丢弃已被服务端关闭的 socket 后重建连），非同 socket 报错"
        + (rawPremise && finBeforeWrite
          ? "；裸层复用对照为静默悬挂（写成功、无回包、无 RST，见 A/raw-reuse-after-fin）"
          : "；裸层对照前提未成立或写前未观测到干净 FIN（见 A/raw-reuse-after-fin），不引用其结论")
      : "agent 复用直测结果非预期形状（见 agentReuse 原值），不套用 ECONNREFUSED connect 相位解释";
  log("A/agent-reuse-after-close", {
    agentReuse,
    fetchNew: await fetchProbe(PORT_A, "/health"),
    newTcpRefused: await tcpRefused(PORT_A),
    note: reuseNote,
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
