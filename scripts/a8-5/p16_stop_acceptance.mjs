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
 * 守门与 note 推导为导出纯函数（#2759：供负控微测直调本文件的真实代码，不复制逻辑——
 * 入仓微测 scripts/a8-5/p16_gate_microtest.mjs）；主流程仅在直接执行本文件时运行
 * （被 import 时不跑）。fix-r3 起 import 无副作用：路径/Recorder 惰性初始化于 main()，
 * import 不建目录、不读调用方 argv/env（评审 sibling#7 / boundary#6 / simplify#3）；
 * 主模块守卫用 import.meta.main（fix-r3：v22.22.0 实测可用、symlink 入口下仍为
 * true——旧 pathToFileURL(argv[1]) 直比在 symlink 入口形状下守卫不命中、进程静默
 * no-op，见轮次证据 emp-misc；评审 concurrency#2 / boundary#7 / simplify#7）。
 * #2759 fix-r2（两线整合）：前提判定经 common.mjs 单源（keepAliveProbe 炮台 +
 * keepAlivePremise 判定，#2756）；头/体切分与 freeSockets 直测并入该主干（direct
 * 压过头推断），close 前池占用复检（A/pool-at-close）保留 #2756 登记面。
 */
import { connect, createServer } from "node:net";
import http from "node:http";
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, keepAliveProbe, keepAlivePremise,
  Recorder, sleep, tcpRefused, makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

// 探针流程态（fix-r3：main() 内惰性初始化；导出纯函数不依赖这些状态，import 无副作用）
let BASE;
let RAW_DIR;
let rec;
const say = (s) => writeSync(1, `${s}\n`);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 400)}`);
  rec.log(k, d);
};

const PORT_A = 39896; // 机制 A：真实 PilotDeck server
const PORT_B_RST = 39897; // 机制 B-RST：纯 node
const PORT_B_EOF = 39898; // 机制 B-EOF：纯 node

/** 本 server（origin `127.0.0.1:PORT_A`）的池条目键（fix-r3 simplify#4：origin 前缀
 * 过滤只写一份）。注意 freeSockets 是 origin 键 → socket 数组的映射（键形如
 * `host:port:localPort`，同 origin 多 socket 共用一键——Agent.getName 产物），此处
 * 只判「该 origin 是否有 idle socket」，不做 socket 计数/身份判断。 */
const freeKeysForA = (ka) => ka.freeSocketKeys()?.filter((k) => k.startsWith(`127.0.0.1:${PORT_A}:`));

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

/** 裸层前提守门（#2759；fix-r2 整合 #2756 单源）。head = dataPromise resolve 的原始
 * buf——首个含 `\r\n\r\n` 的 chunk 可能带 body 字节，必须先切头/体（`\r\n\r\n` 取头块）再查
 * 行：body 行以 `connection:` 开头不得被误当响应头（假成立/假降级）。前提判定走 common 的
 * keepAlivePremise 单源（#2756 fix-r1：隐式规则不再内联第二份）——#2759 的「1.0+显式
 * keep-alive 对称」与单源件同形（显式 token 分支本就不查版本，两线口径在 fix-r2 对拍
 * 一致、微测覆盖）；Connection 头缺失时仅完整 HTTP/1.1 响应按隐式 keep-alive 放行
 * （2s 兜底 resolve 的半截 buf、HTTP/1.0 的缺省 close 都不得判成立），headComplete/
 * http11/connHeader 原值照旧入 record。fix-r3（boundary#9）：头块内多条 connection:
 * 行取全量、值以逗号合并——对齐 node 侧 r.headers.connection 的多行合并（实测
 * v22.22.0 两行 Connection 合并为 "keep-alive, close"、keepAliveToken close 优先判
 * false；只取第一行会在该形状与 node 侧前提分歧）；connHeader 保留首行原值入 record。 */
export function parseRawGate(head) {
  const headBlock = head.split("\r\n\r\n")[0];
  const headLines = headBlock.toLowerCase().split("\r\n");
  const statusLine = headBlock.split("\r\n")[0];
  const connHeader = headLines.find((l) => l.startsWith("connection:"));
  const connValues = headLines
    .filter((l) => l.startsWith("connection:"))
    .map((l) => l.slice(l.indexOf(":") + 1));
  const headComplete = head.includes("\r\n\r\n");
  const http11 = /^HTTP\/1\.1 /.test(statusLine);
  const connValue = connValues.length === 0 ? undefined : connValues.join(",");
  const { premise: rawPremise } = keepAlivePremise({
    ok: headComplete,
    connection: connValue,
    httpVersion: http11 ? "1.1" : "1.0",
  });
  return { statusLine, connHeader, headComplete, http11, connValue, rawPremise };
}

/** pooled 侧前提的头推断（降级路径，#2759；fix-r2 整合 #2756 单源）：判定经 common 的
 * keepAlivePremise——与 keepAliveProbe.once 随响应带回的 premise 同源同形（pooled1 ok 且
 * 响应 Connection 含 keep-alive token；头缺失时仅 HTTP/1.1 放行，1.0 缺省 close）。 */
export function pooledHeaderPremise(pooled1) {
  return keepAlivePremise(pooled1).premise;
}

/** pooled 侧前提（#2759）：「socket 已入池」以 agent.freeSockets 直接观测量为准
 * （自定界：响应带 Content-Length / Transfer-Encoding 才可能入池；Connection 头
 * 只是间接推断）；freeKeys 不可观测（undefined）时回退头推断。fix-r2 整合后
 * freeKeys 取自共用 keepAliveProbe 的 freeSocketKeys()（p16 侧按 origin 前缀过滤）。 */
export function derivePooledPremise(pooled1, freeKeys) {
  const header = pooledHeaderPremise(pooled1);
  const direct = freeKeys === undefined ? undefined : freeKeys.length > 0;
  return { header, direct, premise: direct !== undefined ? direct : header };
}

/** 干净 FIN 判定（单一来源推导）：事件 at 列表 + [from, toExclusive) 窗。
 * 只认 end(FIN/EOF)——allowHalfOpen 下裸 close 事件只可能先经 error（RST 类
 * 异常拆链）。上界排他：与写后窗 [writeAt, ∞) 分区、同毫秒事件归写后窗
 * （对 finBeforeWrite 判定保守、不双占）；t0 下界保留（close() 之前到达的
 * end 不构成机制 A 的证据），每条观察按各自观测窗现算。 */
export function sawCleanFin(events, from, toExclusive = Infinity) {
  return events.some((e) => e.event === "end(FIN/EOF)" && e.at >= from && e.at < toExclusive);
}

/** 写后窗事件（#2759）：[writeAt, ∞)，保留 at / relToWrite——FIN 与写事件的
 * 时序结论由 JSON 自证，不丢时间戳。 */
export function eventsAfterWrite(events, writeAt) {
  return events.filter((e) => e.at >= writeAt).map((e) => ({
    event: e.event, code: e.code, hadError: e.hadError, at: e.at, relToWrite: e.at - writeAt,
  }));
}

/** 异常拆链事件（fix-r3 单一来源谓词，评审 sibling#5 / simplify#5）：error 事件或带
 * 错误的 close（hadError=true）。「写后窗是否异常」在 silentHang 的排除、rawReuseNote
 * 的 errorAfterWrite、partBNote 的 RST 分支三处同构判定，共用本谓词——消除同源
 * 判定两处不同构（旧 rst 分支只认 error 事件、措辞却写 error/close hadError）。 */
export function isAbnormalEvent(e) {
  return e.event === "error" || (e.event === "close" && e.hadError === true);
}

/** 「静默悬挂」（写成功、无回包、无 RST）的实测前提（#2759；fix-r3 评审 P1 簇修）：
 * 写同步未抛（writeOk）**且写回调异步确认 flushed**（write2.flush.flushed——pending
 * 未回调 / 失败如 ERR_STREAM_DESTROYED 均降级为「未确立」，不得产出「写成功」句）
 * 且写后观测窗内无 error / 无回包（data）/ 无异常 close（hadError）才成立。
 * flush 守门的可达性已实测（fix-r3 场景 B/C，v22.22.0 稳定 3/3）：对已销毁 socket
 * 写——同步不抛（writeOk=true）、写回调 ~3ms 报 ERR_STREAM_DESTROYED、写后窗
 * **为空**（error 事件全落写前窗 at < writeAt，不进 eventsAfterWrite）——不消费
 * flush 时该形状会被误判「静默悬挂」，与同记录 write2.flush 原值自相矛盾。 */
export function silentHang(write2, afterWrite) {
  return write2.writeOk === true
    && write2.flush?.flushed === true
    && afterWrite.every((e) => !isAbnormalEvent(e) && e.event !== "data");
}

/** 观察 2 的 note（#2759）：分叉全部由落盘实测字段推导——finBeforeWrite（单一
 * 来源 sawCleanFin(t0, writeAt)）+ eventsAfterWrite（含 at/relToWrite）。不再引用
 * 与 finBeforeWrite 恒等的「FIN 在写前观测窗布尔」（两计算点间无 await、事件
 * 不可能到达，旧「FIN 在写后才到达」分支是死码；FIN 迟到由 eventsAfterWrite
 * 实测判定）。 */
export function rawReuseNote({ rawPremise, finBeforeWrite, eventsAfterWrite: afterWrite }) {
  const finArrivedAfterWrite = afterWrite.some((e) => e.event === "end(FIN/EOF)");
  const errorAfterWrite = afterWrite.some(isAbnormalEvent);
  if (rawPremise && finBeforeWrite) {
    return "裸层复用（allowHalfOpen）：对已收 FIN 的 socket 写第二请求的观测（写本身是否报错、随后 RST/EOF/静默——见 eventsAfterWrite）";
  }
  if (rawPremise) {
    const cause = finArrivedAfterWrite
      ? "FIN 在写后观测窗内到达（eventsAfterWrite 实测）"
      : errorAfterWrite
        ? "写后观测窗内为 error→close 异常拆链（eventsAfterWrite 实测）"
        : "写后 1.5s 观测窗内无 FIN 亦无异常事件（静默）";
    return `未观测到写前干净 FIN（finBeforeWrite=false；${cause}）：本条按「未 FIN 连接上的写」记录，不作 close 后复用的证据`;
  }
  return "前提未成立（见 A/raw-keepalive-request1；成因以该记录 headComplete/http11/connHeader 为准，可能是响应头不完整/非 1.1/显式 close——不预设）：本条按现状记录，不作 close 后复用的证据";
}

/** 观察 3 的裸层对照句（#2759）：「静默悬挂」只在实测成立（silentHang——含 fix-r3 的
 * flush 守门）时产出，否则如实写「非静默悬挂」并指向实测字段——写后 RST/EPIPE、写回调
 * pending/失败等形状不再与「写成功、无回包、无 RST」自相矛盾。 */
export function reuseSilentHangClause(rawPremise, finBeforeWrite, silentHangDerived) {
  if (!(rawPremise && finBeforeWrite)) {
    return "；裸层对照前提未成立或写前未观测到干净 FIN（见 A/raw-reuse-after-fin），不引用其结论";
  }
  return silentHangDerived
    ? "；裸层复用对照为静默悬挂（写成功（writeOk + 写回调 flushed）、无回包、无 RST，见 A/raw-reuse-after-fin 的 write2/eventsAfterWrite）"
    : "；裸层复用对照非静默悬挂（写未成功、写回调未确认（flush pending/失败）或写后观测窗内有 error/回包/异常 close，见 A/raw-reuse-after-fin 的 write2/eventsAfterWrite），不引用静默悬挂结论";
}

/** 机制 B raw note（#2759）：由 rawEvents 推导，握手完成/收到 RST/EOF 不再
 * 无条件断言——握手前 error、只 close 未 end 等形状如实降级为现状记录。fix-r3
 * （sibling#5）：rst 分支的异常判定改用 isAbnormalEvent（error ∥ close(hadError)），
 * 与措辞「error/close hadError」及 rawReuseNote 同构——旧谓词只认 error 事件，
 * 采样错过 error 只余 close(hadError=true) 时会被降级为「RST 未到达」。 */
export function partBNote(mode, rawEvents) {
  const label = mode === "rst" ? "B-RST" : "B-EOF";
  const sawConnect = rawEvents.some((e) => e.event === "connect(handshake-ok)");
  const sawEnd = rawEvents.some((e) => e.event === "end(FIN/EOF)");
  const sawError = rawEvents.some((e) => e.event === "error");
  const sawAbnormal = rawEvents.some(isAbnormalEvent);
  let outcome;
  if (!sawConnect) {
    outcome = "未观测到 connect（握手完成不成立），本条按现状记录，不作该机制的观测面证据";
  } else if (mode === "rst") {
    outcome = sawAbnormal
      ? "握手完成后收到 RST（error/close hadError，events 实测）——对照机制 A 的 connect 相位 ECONNREFUSED"
      : "握手完成但观测窗内未收到 error/异常 close（RST 未到达），本条按现状记录，不作 RST 观测面证据";
  } else {
    outcome = sawEnd && !sawError
      ? "握手完成后收到 EOF（end 事件，无错误，events 实测）——对照机制 A 的 connect 相位 ECONNREFUSED"
      : sawError
        ? "握手完成后收到 error 而非干净 EOF（events 实测），本条按现状记录，不作 EOF 观测面证据"
        : "握手完成但观测窗内未观测到 EOF，本条按现状记录，不作 EOF 观测面证据";
  }
  return { note: `机制 ${label}：${outcome}`, sawConnect, sawEnd, sawError, sawAbnormal };
}

/** A/SKIP-agent-reuse 的 reason（fix-r3 boundary#3 导出纯函数化）：按 pooled1 /
 * freeSockets 观测 / 头推断三态分叉——头推断成立（Connection 呈 keep-alive）时未入池
 * 成因指向「响应无自定界」，不再断言「Connection 非 keep-alive」与自身 header=true
 * 反证；头推断不成立时保留旧文案（成因以 pooled1 原值为准）。 */
export function pooledSkipReason(pooled1, pooledFreeKeys, pooled) {
  if (pooled1.ok !== true) {
    return "pooled1 failed";
  }
  if (pooledFreeKeys === undefined) {
    return "freeSockets 不可观测，头推断亦不成立（socket 未入池）";
  }
  return pooled.header
    ? "freeSockets 无该 server 条目（头推断成立：Connection 呈 keep-alive；未入池成因指向响应无自定界——Content-Length/Transfer-Encoding 缺失，agent 不入池）"
    : "freeSockets 无该 server 条目（头推断亦不成立：Connection 非 keep-alive 或缺头且非 1.1——成因以 pooled1 原值为准，socket 未入池）";
}

/** A/agent-reuse-after-close 的 note（fix-r3 sibling#6/boundary#2 导出纯函数化）：
 * 「freeKeysBeforeReuse 空为直接观测」半句现在真的检查数组——close 后池尚未排空
 * （时序边界）时如实写「非空、池未排空」，不再与同记录原值自相矛盾；静默悬挂对照
 * 句由 silentHang 实测推导（reuseSilentHangClause），非预期形状不套 connect 相位解释。 */
export function agentReuseNote({ agentReuse, freeKeysBeforeReuse, reuseSkipReason, rawPremise, finBeforeWrite, silentHangDerived }) {
  if (agentReuse.skipped) {
    return `agent 复用直测跳过（${reuseSkipReason}）；fetch 新连接与 newTcpRefused 与入池无关，照常记录`;
  }
  if (agentReuse.ok !== true && agentReuse.code === "ECONNREFUSED" && agentReuse.reusedSocket === false) {
    const phase = (freeKeysBeforeReuse?.length ?? 0) === 0
      ? "ECONNREFUSED 是 connect 相位错误（agent 丢弃已被服务端关闭的 socket 后重建连，非同 socket 报错——reusedSocket=false、freeKeysBeforeReuse 空为直接观测）"
      : "ECONNREFUSED 且 reusedSocket=false，但 freeKeysBeforeReuse 非空（池未排空，原值见本记录）——「重建连」以 reusedSocket=false 为据，「池已空」半句不成立，按现状记录";
    return `同 agent（close 前池化，见 A/agent-pooled-request1 的 freeSockets）复用直测：${phase}`
      + reuseSilentHangClause(rawPremise, finBeforeWrite, silentHangDerived);
  }
  return "agent 复用直测结果非预期形状（见 agentReuse 原值），不套用 ECONNREFUSED connect 相位解释";
}

async function partA() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const server = await startPilotDeckServer({
    gateway: makeFakeGateway(), port: PORT_A, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p16-a"),
  });

  // 既有 keep-alive：裸 socket 发请求 1（默认 keep-alive），读头后保持打开。
  // 守门推导见 parseRawGate（头/体切分、缺头仅完整 1.1 放行、显式头一律按
  // token、1.0+显式 keep-alive 与 pooled 侧对称；前提判定经 common 的
  // keepAlivePremise 单源——#2756 fix-r1，两线口径 fix-r2 对拍一致）。
  const raw = await rawKeepAliveRequest(PORT_A);
  const gate = parseRawGate(raw.head);
  log("A/raw-keepalive-request1", {
    statusLine: gate.statusLine, connHeader: gate.connHeader,
    headComplete: gate.headComplete, http11: gate.http11, rawPremise: gate.rawPremise,
  });
  const rawEvents = [];
  watchSock(raw.sock, rawEvents, "raw-ka");

  // 复用直测前提：close 前经同一 keep-alive agent 完成一次请求、socket 入池——
  // close 后同 agent 的第二次请求才是「复用已关连接」的直测，而不是新建 agent
  // 的新连接。前提须守门，pooled1 失败或未入池时跳过 agent 复用直测并记
  // A/SKIP-agent-reuse，不以「close 前池化」的 note 产出错误前提的证据记录。
  // #2756（r5 concurrency/simplify 收敛）：内联 agent 炮台与 common.mjs 的
  // keepAliveProbe 同构，收敛共用；前提判定随 once 返回（premise/premiseReason，
  // 判定形状 = 原 #2662 fix-r1/r2 的 ok + 缺头按完整 HTTP/1.1 隐式 + close 优先，
  // 与 p2b/p1_p2/p6 三探针同形）。
  // #2759 fix-r2 整合：入池前提在单源判定之上以 agent.freeSockets 直接观测量为准
  // （自定界：响应带 Content-Length / Transfer-Encoding 才可能入池；Connection 头
  // 推断保留为降级路径——见 derivePooledPremise，direct 压过 header），直接观测量
  // 经共用 keepAliveProbe 的 freeSocketKeys() 在同一 agent 上取。
  const ka = await keepAliveProbe(PORT_A);
  const pooled1 = await ka.once("/health");
  // 入池直接观测量：实测 'end' 后 freeSockets 即含该 socket（#2759；freeSockets 为
  // origin 键 → socket 数组，此处按 freeKeysForA 判该 origin 是否有 idle socket——
  // 见其定义处的 origin 键语义注）。
  const pooledFreeKeys = freeKeysForA(ka);
  const pooled = derivePooledPremise(pooled1, pooledFreeKeys);
  const pooledPremise = pooled.premise;
  // #2756 fix-r2（DS sibling#1 撞条）：note 不断言「复用直测前提成立」——该断言在
  // 池被服务端空闲超时收空时与 A/SKIP-pool-drained-before-reuse / agentReuse
  // skipped 互为反证；收窄为「请求 1 已入池」，close 前是否仍占用指向显式行
  // （#2759 的 freeSockets 直接观测并入「已入池」半句）。
  // fix-r3（Grok sibling P3）：指向 A/pool-at-close 显式行（复检处无论结果都
  // 落一行），不再以「SKIP 行未出现」作负证据推断。
  // fix-r3（boundary#5）：derivePooledPremise 的 header/direct 分量入记录
  // ——「头推断为降级路径」可由落盘原值复算，不再只存在于注释。
  log("A/agent-pooled-request1", {
    pooled1,
    pooledFreeKeys,
    pooledHeader: pooled.header,
    pooledDirect: pooled.direct,
    pooledPremise,
    note: pooledPremise
      ? "请求 1 已入池（close() 前经该 agent 完成、freeSockets 直接观测）；close 前池内是否仍占用见 A/pool-at-close 显式行（不成立时另落 A/SKIP-pool-drained-before-reuse）"
      : "复用直测前提未成立（freeSockets 无该 server 条目，或头推断不成立——socket 未入池），见 A/SKIP-agent-reuse",
  });
  if (!pooledPremise) {
    log("A/SKIP-agent-reuse", {
      reason: pooledSkipReason(pooled1, pooledFreeKeys, pooled),
      pooled1,
      pooledFreeKeys,
    });
  }
  // #2756 fix-r1（boundary P1 仲裁）：入池前提是「close 前最后一刻池内仍有该 idle
  // socket」，不是 pooled1 的一次性快照——S0→close 跨过服务端空闲收尾窗（实测
  // ~5.5–6s）时池已被收空，close 的 FIN 无池内 socket 可拆、agent 丢弃重建路径
  // 未被走到，观察 3 的复用直测退化为纯新连接（错误串可相同但解释失据）。复检
  // 必须在 close 前：server.close() 本身 ~11ms 内即 FIN 掉池内 socket（close 后
  // 池必空，实测见轮次证据 exp-close-pool.out 场景①）。
  const freeSocketsAtClose = ka.freeSocketCount();
  const pooledAtClose = pooledPremise && freeSocketsAtClose > 0;
  // fix-r3（Grok sibling P3）：池占用复检无论结果都落显式行——close 前池状态
  // 不再靠「SKIP 行未出现」负证据推断。
  log("A/pool-at-close", { pooledAtClose, freeSocketsAtClose, pooledPremise });
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
  const finObserved = sawCleanFin(rawEvents, t0);
  log("A/server-close-idle-keepalive", {
    t0,
    eventsSoFar: rawEvents.map((e) => ({ ...e, relMs: e.at - t0 })),
    rawPremise: gate.rawPremise,
    finObserved,
    note: gate.rawPremise && finObserved
      ? "机制 A：close() 主动关空闲 keep-alive（A14① 复核，裸层直接观测；FIN 在 close() 之后、窗内到达）"
      : gate.rawPremise
        ? "close() 之后未观测到干净 FIN（finObserved=false，含 error→close 异常拆链情形）：FIN 时刻结论降级，不得据此引用 A14"
        : "前提未成立（见 A/raw-keepalive-request1 的 headComplete/http11/connHeader 原值）：FIN 时刻不代表 server.close() 对空闲 keep-alive 的处置",
  });

  // 观察 2：在同一条已 FIN 的 socket 上复用发请求 2（裸层复用态——A14 重建连推断的对照面）。
  // 「已 FIN」指写之前（且 close() 之后）到达的干净 FIN，用写时刻上界现算；note 分叉
  // 由 eventsAfterWrite（含 at/relToWrite）实测推导（rawReuseNote）。writeOk 是同步
  // 未抛语义，异步确认走 write 回调（write2.flush：实测对已 FIN socket 写 ~1ms 回调
  // err=null；对已销毁 socket 写 ~3ms 回调 ERR_STREAM_DESTROYED 且写后窗为空——error
  // 事件全落写前窗，见 silentHang 的 flush 守门注；回调不触发记 pending）。
  // fix-r3（simplify#2）：write() 放回 try 内、executor 只落 resolve 句柄——executor
  // 内同步 throw 会被 Promise 构造器转成 rejection 绕过外层 catch（writeOk 恒
  // undefined、partA 硬失败）；write 的同步抛面实测存在（v22.22.0：无效 chunk 类型
  // TypeError/ERR_INVALID_ARG_TYPE），同步抛时 writeCb 置 undefined、flush 不落。
  let write2 = { attempted: true };
  let writeAt = 0;
  let writeCb;
  try {
    writeAt = Date.now();
    let onFlush;
    writeCb = new Promise((res) => { onFlush = res; });
    raw.sock.write(`GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n`, (err) => onFlush({ flushed: err == null, code: err?.code, message: err?.message }));
    write2.writeOk = true;
  } catch (e) {
    write2.writeOk = false;
    write2.error = { code: e.code, message: e.message };
    writeCb = undefined;
  }
  const obsWindow = sleep(1500);
  if (writeCb !== undefined) {
    write2.flush = await Promise.race([writeCb, obsWindow.then(() => ({ pending: true }))]);
  }
  await obsWindow;
  const afterWrite = eventsAfterWrite(rawEvents, writeAt);
  const finBeforeWrite = sawCleanFin(rawEvents, t0, writeAt);
  log("A/raw-reuse-after-fin", {
    t0,
    writeAt,
    write2,
    rawPremise: gate.rawPremise,
    finBeforeWrite,
    eventsAfterWrite: afterWrite,
    socketStillOpen: !raw.sock.destroyed,
    note: rawReuseNote({ rawPremise: gate.rawPremise, finBeforeWrite, eventsAfterWrite: afterWrite }),
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
  // #2759 fix-r2 整合：ECONNREFUSED 的 connect 相位与「重建连」解释以直接观测
  // 量自证（reusedSocket===false、freeKeysBeforeReuse 空为直接观测），静默悬挂
  // 对照句由 silentHang 实测推导（reuseSilentHangClause），不再由前提布尔推出。
  // fix-r3（sibling#6/boundary#2）：note 构造移入导出纯函数 agentReuseNote
  // ——「freeKeysBeforeReuse 空」半句按数组长度分叉，池未排空时如实写非空。
  const freeKeysBeforeReuse = freeKeysForA(ka);
  const reuseSkipReason = pooledPremise
    ? "池在 close 前被服务端空闲超时收尾（见 A/SKIP-pool-drained-before-reuse）"
    : "复用直测前提未成立（见 A/SKIP-agent-reuse）";
  const agentReuse = pooledAtClose
    ? await ka.once("/health").then((r) => (r.ok === true
        ? r
        : { ...r, phase: r.code === "ECONNREFUSED" ? "connect-phase" : "other" }))
    : { skipped: true, reason: reuseSkipReason };
  ka.destroy();
  // note 随实测状态推导，不无条件断言 ECONNREFUSED/connect 相位解释与裸层对照
  // ——agentReuse 非预期形状或裸层实测不成立时对应句不产出。
  const silentHangDerived = silentHang(write2, afterWrite);
  const reuseNote = agentReuseNote({
    agentReuse,
    freeKeysBeforeReuse,
    reuseSkipReason,
    rawPremise: gate.rawPremise,
    finBeforeWrite,
    silentHangDerived,
  });
  log("A/agent-reuse-after-close", {
    freeKeysBeforeReuse,
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
  // fix-r3：net.Server 没有 closeAllConnections（实测 v22.22.0，http.Server 才有）——
  // 旧 `srv.closeAllConnections?.()` 兜底实为静默 no-op。改为跟踪已接受连接，
  // 收尾兜底手工销毁，兑现文件头「结束收尾销毁全部 socket/关闭全部 server」。
  const srvSocks = new Set();
  const srv = createServer((sock) => {
    srvSocks.add(sock);
    sock.on("close", () => srvSocks.delete(sock));
    sock.on("error", () => undefined);
    if (mode === "rst") sock.destroy();
    else sock.end();
  });
  await new Promise((res) => srv.listen(port, "127.0.0.1", res));

  // 裸 socket：握手是否完成 + 随后的事件序列（note 由 rawEvents 推导，见 partBNote）
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
    events: rawEvents.map((e) => ({ event: e.event, code: e.code, hadError: e.hadError, at: e.at })),
    ...partBNote(mode, rawEvents),
  });

  log(`B-${mode}/node-http`, {
    result: await httpProbeRaw(port, "/health"),
  });
  log(`B-${mode}/fetch`, {
    result: await fetchProbe(port, "/health"),
  });

  // 收尾加界（#2759）：B-EOF 的 server 侧 socket 在 end() 后读侧未消费（本
  // 装置不挂 data 监听），收到客户端 FIN 也不 emit close，srv.close() 永不
  // 落定（实测 2s 窗不 settle、main 挂起、进程静默 exit 0、JSON 不落盘）——
  // 1.5s 窗不落定即销毁全部已接受连接清场（旧版调 srv.closeAllConnections?.()，
  // 但 net.Server 无该方法、实为静默 no-op——见上方 fix-r3 注），兑现「收尾
  // 关闭全部 server」。
  // fix-r3（boundary#8）：二次 race 结果入记录（B-<mode>/srv-close-final 显式行），
  // 兜底销毁后仍未 settle 时如实写降级行，不以 process.exit(0) 掩盖。
  const srvClosed = new Promise((res) => srv.close(() => res(true)));
  const settledFirstWindow = await Promise.race([srvClosed, sleep(1500).then(() => false)]);
  if (!settledFirstWindow) {
    const destroyed = srvSocks.size;
    for (const s of srvSocks) s.destroy();
    const settledAfterSocketsDestroyed = await Promise.race([srvClosed, sleep(500).then(() => false)]);
    log(`B-${mode}/srv-close-final`, {
      settledFirstWindow: false,
      closeAllConnectionsAvailable: typeof srv.closeAllConnections === "function",
      socketsDestroyed: destroyed,
      settledAfterSocketsDestroyed,
      note: settledAfterSocketsDestroyed
        ? `srv.close() 1.5s 窗不落定，销毁 ${destroyed} 条残余 server 侧连接后 500ms 内落定（收尾完成）`
        : `srv.close() 1.5s 窗不落定，销毁 ${destroyed} 条残余 server 侧连接后 500ms 仍未落定（收尾未完成，降级记录）`,
    });
  } else {
    log(`B-${mode}/srv-close-final`, { settledFirstWindow: true, note: "srv.close() 1.5s 窗内落定（收尾完成）" });
  }
}

async function main() {
  // fix-r3：路径/Recorder 惰性初始化（import 无副作用，见文件头）
  ({ BASE, RAW_DIR } = makePaths(parseProbeArgs("p16-stop-acceptance"), "p16_stop_acceptance"));
  rec = new Recorder(RAW_DIR);
  await partA();
  await partB(PORT_B_RST, "rst");
  await partB(PORT_B_EOF, "eof");
  rec.dump("p16_stop_acceptance.json");
  say("P16 DONE");
  process.exit(0);
}

// fix-r3（concurrency#2 / boundary#7 / simplify#7）：import.meta.main（v22.22.0 实测
// 可用，symlink 入口下仍为 true——旧 pathToFileURL(argv[1]) 直比在该形状不命中、
// 静默 no-op exit 0，见轮次证据 emp-misc）。
if (import.meta.main) {
  main().catch((e) => {
    say(`P16 FAILED: ${e?.stack ?? e}`);
    rec?.dump?.("p16_stop_acceptance.json");
    process.exit(1);
  });
}
