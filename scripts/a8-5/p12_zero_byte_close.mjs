/**
 * P12：A4 零字节格的 GatewayServer 实例级补测（右删失判据）。
 * 零字节 TCP 连接（建连后不发任何字节）持有下 server.close() 是否 settle。
 * 408 落点为相位窗 (60, 90] s 内的相位值（含毫秒级 tick 执行抖动）、随网格
 * 相位变化——70 s 窗只证右删失下界；区分「408 才 settle」与「close 清
 * checker ⇒ 永不 settle」须 ≥95 s（保守 120 s）观测，判别面 = 零字节
 * socket 的 data/end 监听（firstByteMs/bytes/hex 落产物：≥95 s 窗内
 * 「不 settle 且无字节」支持后者、「有字节」则排除），不得作对拍判 PASS
 * 依据（#2259；机制与在档实测单源见 matrix §六 A4 格内标注）。
 * 观察窗有界：P12_WINDOW_MS 毫秒，须为 (0, 300000] 内安全整数（未设置或
 * 空串回退默认 70_000；非法值 stderr 一句原因 + exit 2，不落盘任何证据）。
 * 端口默认 39891，可用 --port <N> 或 P12_PORT 改（非法值同 exit 2）；占用
 * 预检失败与 bind 失败（EADDRINUSE——预检→import→bind 间 TOCTOU 竞态败者，
 * fix-r5 concurrency#1）都受控快速失败（exit 3，不落观测记录，照 p16 fix-r4
 * EADDRINUSE 受控 FAIL 形态 + 进程级 uncaughtException/unhandledRejection
 * 兜底）。证据名带窗长+时间戳+pid（run 唯一，tmp 名随之唯一，不覆写既有
 * 证据）；结束时收尸（销毁 socket、若 close 已 settle 则等待完成）。
 * 判据登记（fix-r5 boundary#1、fix-r6 结构性收口，判废条件照
 * A8.3-CH-SMS-012-R1.md:121-134 逐字段对齐、判读以 exp016 readme
 * p12-window-spec 节判废面矩阵 M1–M15 为正本）：产物首条 scenario 记录含
 * headersTimeout/requestTimeout/connectionsCheckingInterval（三字段两层实读：
 * server 对象直读、未命中再读 server.server 包装层——真实基线返回对象可能是
 * 包装层而非原生 http.Server，close() 为 Promise 形态为证；serverShape 记录
 * 命中层级 native/wrapped）；headersTimeout 须 =60000。两层均读不到 ⇒ 第四态
 * probe-shape-mismatch：exit 4、不落观测产物、不进 K/I/S 三态也不判有效/判废
 * ——先修探针再复跑（fix-r6 boundary#2：null 恒判废会把所有合规复跑判成
 * 假 FAIL）。observation 行时间字段——closeCalledMs/elapsedMs 恒有限数值，
 * closeResolvedMs/connectionEndedMs/closeWaitedMs（= closeResolvedMs −
 * closeCalledMs，R1:127「提前 resolve ⇒ 走到的不是真实监听器」的 p12 对应面）
 * 事件发生才为有限数值（「窗内不 settle」正是观察对象，与 R1「必须有限」的
 * 差异点；connectionEndedMs 终值在 cleanup 行落定，200ms 内未达记 null）；
 * 有效性只看产物条件、不看退出码（exit 4 例外：没有产物可判）。
 * note 结论谓词（fix-r5 boundary#2）：判「支持『close 清 checker ⇒ 永不
 * settle』」前须同 observation 行核「bytes=0 且窗内无 end/close/error 服务端
 * 事件（socketEventsFromServerSide 空）且 socketLocalStillOpen===true」；
 * 不满足时只报事实（bytes/firstByteMs/closeSettled + 服务端事件清单）、
 * 不作机制归因。fix-r7 boundary#1（矩阵 M11 自守）：「支持②」另须 scenario 行
 * headersTimeout=60000——读到但 ≠60000（如 stub 抬到 3_600_000 的判废演示形状）
 * 时该产物按矩阵 M11 判废，探针不得输出支持性结论、只报事实。fix-r8
 * boundary#1 + sibling#1（两家独立撞上同一缺口）：该自守从「支持②」分支短路
 * 前置到一切 note 分支之前——≠60000 一律走 M11 判废事实分支，settle 分支与
 * 有字节分支同样不得输出机制结论（分支序收敛进 p12NoteBranch 纯函数，
 * scripts/a8-5/p12_note_branch.mjs；微测 p12_note_microtest.mjs 直调）。
 * settle 分支 note 携带 closeWaitedMs 与「是否 ≥60s 相位窗」
 * （fix-r6 boundary#1），且早 settle 判废面（矩阵 M2：closeWaitedMs<1000
 * 且无可归因证据——firstByteMs=null、无服务端事件、connectionEndedMs=null）
 * 命中时 observation 行落 invalidationHit 标注。
 * 退出码：0 成功；1 运行失败（落失败记录）；2 非法参数/env（不落盘）；
 * 3 端口占用/bind 失败（不落观测记录）；4 probe-shape-mismatch——三超时
 * 字段两层均读不到（不落观测产物，先修探针）。
 */
import { writeSync } from "node:fs";
import { connect } from "node:net";
import {
  makeFakeGateway, makeHome, makeStaticDir, Recorder, sleep,
  makePaths, parseProbeArgs, tcpRefused, TOKEN,
} from "./common.mjs";
import { p12NoteBranch } from "./p12_note_branch.mjs";

const DEFAULT_WINDOW_MS = 70_000;
// 上限量级依据：判废口径只要求窗 > accept + 90 s + 过冲，保守取 120 s
// （A8.3-CH-SMS-012-R1.md:114-120，即 specs/gateway-parity/diffs/ 下同件）；
// 300 s = 2.5 × 120 s 已覆盖任何合理复跑，再大无判别增益、只增挂钟成本。
const MAX_WINDOW_MS = 300_000;

// P12_WINDOW_MS 校验（fix-r3 P1#1，对齐 matrix §六 A4/A3/A11 已登记谓词：
// 超时值为正有限安全整数、不高于上限常量；0/负/NaN/∞/溢出显式拒绝）。
// 空串/未设回退默认 70_000；非法值在 makePaths（建目录）之前拒绝——不落盘任何证据。
const rawWindowEnv = process.env.P12_WINDOW_MS;
let WINDOW_MS;
if (rawWindowEnv === undefined || rawWindowEnv === "") {
  WINDOW_MS = DEFAULT_WINDOW_MS;
} else {
  WINDOW_MS = Number(rawWindowEnv);
  if (!Number.isSafeInteger(WINDOW_MS) || WINDOW_MS <= 0 || WINDOW_MS > MAX_WINDOW_MS) {
    writeSync(2, `P12 INVALID P12_WINDOW_MS=${JSON.stringify(rawWindowEnv)}：须为 (0, ${MAX_WINDOW_MS}] 内的安全整数（毫秒）；未设置或空串回退默认 ${DEFAULT_WINDOW_MS}\n`);
    process.exit(2);
  }
}
const WINDOW_S = WINDOW_MS / 1000;
// 「≥95 s 窗已覆盖最坏相位 408 落点」的窗长判据已收敛进 p12_note_branch.mjs
// （WORST_PHASE_FLOOR_MS，note 分支用；fix-r8 前此处另有 COVERS_WORST_PHASE 局部量）。

const DEFAULT_PORT = 39891;
const rawPortArg = (() => {
  const argv = process.argv.slice(2);
  const i = argv.indexOf("--port");
  return i >= 0 ? argv[i + 1] : process.env.P12_PORT;
})();
const port = (() => {
  if (rawPortArg === undefined) return DEFAULT_PORT;
  const p = Number(rawPortArg);
  if (!Number.isSafeInteger(p) || p < 1 || p > 65535) {
    writeSync(2, `P12 INVALID port=${JSON.stringify(rawPortArg)}：须为 1–65535 内的整数（--port <N> 或 P12_PORT；未设置回退默认 ${DEFAULT_PORT}）\n`);
    process.exit(2);
  }
  return p;
})();

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p12-zero-byte-close"), "p12_zero_byte_close");
const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 300)}`);
  rec.log(k, d);
};

// fix-r5 concurrency#1（TOCTOU 实测后修，实测原始输出见本单证据目录）：预检→
// import→bind 之间存在竞态窗（import 的 ESM 图加载耗时不可忽略），两轮并发同
// 端口时败者的 bind 撞 EADDRINUSE——实测败者走未捕获 'error' 裸崩路径（非文档
// 承诺的受控 exit 3）。基线 startPilotDeckServer 的 listen 在黑盒内、无法像
// p16_stop_acceptance.mjs:534-540 那样在 listen 前挂 error 监听，受控路径的
// 等价面 = 此处进程级兜底 + main().catch 的 EADDRINUSE 分支（同受控 FAIL：
// exit 3、不落观测产物）。非 EADDRINUSE 的迟到异常仍 exit 1（与 main().catch
// 同语义；不在此落盘——任意上下文落盘有覆写竞态风险）。
const eaddrInuseFail = (origin) => {
  writeSync(2, `P12 FAIL: 端口 ${port} bind 失败（EADDRINUSE，${origin}——预检与 bind 之间端口被占，TOCTOU 竞态败者受控快速失败，不落观测记录）\n`);
  process.exit(3);
};
process.on("uncaughtException", (e) => {
  if (e?.code === "EADDRINUSE") eaddrInuseFail("uncaughtException 路径");
  writeSync(2, `P12 FAILED (uncaughtException): ${e?.stack ?? e}\n`);
  process.exit(1);
});
process.on("unhandledRejection", (e) => {
  if (e?.code === "EADDRINUSE") eaddrInuseFail("unhandledRejection 路径");
  writeSync(2, `P12 FAILED (unhandledRejection): ${e?.stack ?? e}\n`);
  process.exit(1);
});

// 证据名带窗长+时间戳+pid：run 唯一，杜绝两轮互覆（fix-r3 concurrency#3；
// Recorder.dump 的 tmp 名 = 本名 + ".tmp"，随之唯一）
const RUN_STAMP = `${new Date().toISOString().replace(/[:.]/g, "")}-${process.pid}`;
const EVIDENCE_NAME = `p12_zero_byte_close-${WINDOW_MS}ms-${RUN_STAMP}.json`;

async function main() {
  const runStartAt = Date.now();
  // 端口占用预检（fix-r3 concurrency#3）：占用时受控快速失败（exit 3、不落观测
  // 记录），不把失败记录伪装成观测、也不让零字节 client 连到别人的 server。
  if (!(await tcpRefused(port))) {
    writeSync(2, `P12 FAIL: 端口 ${port} 已被占用（预检 connect 成功）——先停占用进程或用 --port/P12_PORT 换端口（受控快速失败，不落观测记录）\n`);
    process.exit(3);
  }

  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const server = await startPilotDeckServer({
    gateway: makeFakeGateway(), port, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p12"),
  });

  // fix-r5 boundary#1 + fix-r6 boundary#2：首条 scenario 记录（判据对齐
  // R1:121-134——「产物至少一行 scenario」+「首行 headersTimeout 须 =60000」的
  // p12 对应面）。三个超时字段两层实读（server 直读 → server.server 包装层 →
  // null；真实基线返回对象可能是包装层——close() Promise 形态为证，未实测前
  // 不赌单层形状）；serverShape 记录 headersTimeout 实际命中的层级，两层均
  // 读不到 ⇒ 第四态 probe-shape-mismatch：exit 4、不落观测产物（null 恒判废
  // 会把所有合规复跑判成假 FAIL——fix-r6 修）。
  const finiteNum = (v) => (typeof v === "number" && Number.isFinite(v) ? v : null);
  const readSrvField = (key) => {
    const native = finiteNum(server?.[key]);
    if (native !== null) return { value: native, level: "native" };
    const wrapped = finiteNum(server?.server?.[key]);
    if (wrapped !== null) return { value: wrapped, level: "wrapped" };
    return { value: null, level: "unreachable" };
  };
  const headersTimeoutRead = readSrvField("headersTimeout");
  const serverShape = headersTimeoutRead.level; // 判据载重字段（R1:124）的命中层级
  log("scenario", {
    scenario: "p12-zero-byte-close",
    port,
    windowMs: WINDOW_MS,
    windowEnvRaw: rawWindowEnv ?? null,
    serverShape,
    headersTimeout: headersTimeoutRead.value,
    requestTimeout: readSrvField("requestTimeout").value,
    connectionsCheckingInterval: readSrvField("connectionsCheckingInterval").value,
  });
  if (serverShape === "unreachable") {
    writeSync(2, `P12 FAIL: probe-shape-mismatch——headersTimeout 两层（server 直读/包装层）均读不到（server=${typeof server}，已试 server.headersTimeout/server.server.headersTimeout）。该形状不判有效/判废、不落观测产物（exit 4）：先修探针对真实基线形状的读取再复跑（判据须 =60000 无从谈起，null 恒判废 = 假 FAIL）。\n`);
    process.exit(4);
  }

  // 零字节连接：建连成功后不发任何字节
  const connectedAt = Date.now();
  const zeroByte = await new Promise((res, rej) => {
    const sock = connect(port, "127.0.0.1", () => res(sock));
    sock.once("error", rej);
  });
  log("zero-byte-connected", { localPort: zeroByte.localPort, wroteBytes: 0 });
  const socketEvents = [];
  let socketCloseAt = null;
  let firstByteMs = null;
  let bytes = 0;
  let hex = null;
  zeroByte.on("close", () => {
    socketCloseAt = Date.now();
    socketEvents.push({ event: "close", at: socketCloseAt });
  });
  zeroByte.on("error", (e) => socketEvents.push({ event: "error", code: e.code, at: Date.now() }));
  zeroByte.on("end", () => socketEvents.push({ event: "end", at: Date.now() }));
  // 判别观测面（fix-r3 P1#2a）：服务端若发 408（或任何字节），data 监听落
  // firstByteMs/bytes/hex（首块前 64 字节）——对齐 A8.3-CH-SMS-012-R1.md:114-120
  // 的观察窗下限判据（窗须 > accept + 90 s + 过冲；firstByteMs 相对 close() 起算，
  // connectToCloseMs 供换算回 accept 起算）。
  zeroByte.on("data", (chunk) => {
    if (firstByteMs === null) firstByteMs = Date.now() - t0;
    bytes += chunk.length;
    if (hex === null) hex = chunk.subarray(0, 64).toString("hex");
  });

  const t0 = Date.now();
  const closeCalledMs = t0 - runStartAt;
  let closeSettled = false;
  let closeResolvedAt = null;
  const closePromise = server.close().then(
    () => { closeSettled = true; closeResolvedAt = Date.now(); },
    () => { closeSettled = true; closeResolvedAt = Date.now(); },
  );

  // 观测窗（默认 70 s 只证右删失下界——未覆盖最坏相位 408 落点，见 #2259 头注）
  while (Date.now() - t0 < WINDOW_MS && !closeSettled) {
    await sleep(500);
  }
  // fix-r5 boundary#2 结论谓词 + fix-r7 M11 自守（「支持②」另须 headersTimeout
  // =60000）+ fix-r8 短路前置：分支序整体收敛进 p12NoteBranch 纯函数
  // （scripts/a8-5/p12_note_branch.mjs，M11 → settle → bytes-exclude →
  // support2 → fact → right-censored；微测 p12_note_microtest.mjs 直调两格——
  // 60000+四条件 ⇒ support2、≠60000+四条件 ⇒ m11-invalidated）。
  const noteBranch = p12NoteBranch({
    closeSettled,
    firstByteMs,
    headersTimeout: headersTimeoutRead.value,
    windowMs: WINDOW_MS,
    bytes,
    socketEventCount: socketEvents.length,
    socketLocalStillOpen: !zeroByte.destroyed,
  });
  // fix-r6 boundary#1（矩阵 M2）：早 settle 判废面——close 落定不可归因于该连接的
  // 任何可观测事件（无字节、无窗内服务端事件、连接未在窗内结束）且等待 <1s ⇒
  // 连接未被计入（close 见 0 连接立即 resolve 的形状），非零字节观测、判废。
  // 下界 1000ms：R1:127 对应面；408 驱动 settle ≥60s 相位窗，空闲清理/客户端 FIN
  // 驱动 settle 均伴随可观测事件（error/end/close），不落本面。
  const closeWaitedMs = closeResolvedAt !== null ? closeResolvedAt - t0 : null;
  const EARLY_SETTLE_FLOOR_MS = 1_000;
  const invalidationHit = closeSettled
    && closeWaitedMs !== null
    && closeWaitedMs < EARLY_SETTLE_FLOOR_MS
    && firstByteMs === null
    && socketEvents.length === 0
    && socketCloseAt === null
    ? "early-settle-conn-not-counted（矩阵 M2：closeWaitedMs<1000 且无字节、无服务端事件、连接未在窗内结束——close 落定不可归因于该连接，连接未被计入，非零字节观测，判废）"
    : null;
  // note 模板按 noteBranch 拼装（分支条件见 p12_note_branch.mjs；M11 分支只报
  // 事实——settle/字节面原样、不带「相位窗内」「排除『close 清 checker』」等
  // 机制结论——对按矩阵判废的产物输出机制结论与 p12-window-spec 相反）。
  let note;
  if (noteBranch === "m11-invalidated") {
    note = `${WINDOW_S}s 窗内${closeSettled ? `close 已 settle（closeWaitedMs=${closeWaitedMs}ms）` : "close 不 settle"}、零字节 socket 字节面见本行（bytes=${bytes}、firstByteMs=${firstByteMs}、closeSettled=${closeSettled}、服务端事件清单见本行 socketEventsFromServerSide）——headersTimeout=${headersTimeoutRead.value}≠60000（判废面矩阵 M11，R1:124）：本产物按矩阵判废，只报事实不作机制归因，见 #2259`;
  } else if (noteBranch === "settle") {
    note = `close 在观测窗内 settle（时间见 elapsedMs/closeResolvedMs；closeWaitedMs=${closeWaitedMs}ms，${closeWaitedMs >= 60_000 ? "≥60s 相位窗内（与 408 驱动面一致）" : "<60s 相位窗（非 408 驱动）"}${invalidationHit ? `；${invalidationHit}` : ""}）`;
  } else if (noteBranch === "bytes-exclude") {
    note = `${WINDOW_S}s 窗内不 settle，但服务端字节已到达（firstByteMs/bytes/hex 见本行）——排除「close 清 checker ⇒ 408 不发」，settle 等待另有原因，见 #2259`;
  } else if (noteBranch === "support2") {
    note = `${WINDOW_S}s 窗内不 settle，且零字节 socket 未收到任何服务端字节（bytes=0、窗内无 end/close/error 服务端事件、socket 本地仍开——谓词字段见本行）——窗已覆盖最坏相位 408 落点（相位窗 (60, 90] s + 毫秒级 tick 抖动），支持「close 清 checker ⇒ 永不 settle」（SmsChannel 实例机制链 A8.3-CH-SMS-014-R2；GatewayServer 实例路径以本观测为准），见 #2259`;
  } else if (noteBranch === "fact") {
    note = `${WINDOW_S}s 窗内不 settle 且零字节 socket 未收到服务端字节，但结论谓词不成立（窗内服务端事件=${JSON.stringify(socketEvents.map((e) => e.event))}、socket 本地${zeroByte.destroyed ? "已关" : "仍开"}）——只报事实：bytes=${bytes}、firstByteMs=${firstByteMs}、closeSettled=${closeSettled}、服务端事件清单见本行 socketEventsFromServerSide，不作机制归因，见 #2259`;
  } else {
    note = `${WINDOW_S}s 窗内不 settle（右删失下界 ${WINDOW_S}s；窗未覆盖最坏相位 408 落点（落点随网格相位在 (60, 90] s 窗内、含毫秒级 tick 抖动），区分不了「408 才 settle」与「永不 settle」，见 #2259）`;
  }
  log("observation", {
    windowMs: WINDOW_MS,
    windowEnvRaw: rawWindowEnv ?? null, // 原始 env 字符串（复算用；null = 未设置）
    port,
    closeCalledMs,
    closeResolvedMs: closeResolvedAt !== null ? closeResolvedAt - runStartAt : null,
    connectionEndedMs: socketCloseAt !== null ? socketCloseAt - runStartAt : null,
    closeWaitedMs,
    invalidationHit,
    connectToCloseMs: t0 - connectedAt,
    elapsedMs: Date.now() - t0,
    closeSettled,
    firstByteMs,
    bytes,
    hex,
    note,
    // .slice() 快照：cleanup 的 destroy() 会再 push close 事件，而 Recorder 落的是
    // 对象引用——不快照会把窗后事件混进「窗内」清单（证据完整性，fix-r5 实测发现）
    socketEventsFromServerSide: socketEvents.slice(),
    socketLocalStillOpen: !zeroByte.destroyed,
  });

  // 收尾：销毁零字节 socket；若 close 已 settle 则 closePromise 已完成，否则进程显式退出
  zeroByte.destroy();
  if (socketCloseAt === null) {
    // fix-r5 boundary#1：connectionEndedMs 终值收进 cleanup 行（有界等待 200 ms，
    // 事件不达如实记 null——判废面），observation 行内的值为窗内实际发生值。
    await Promise.race([
      new Promise((res) => zeroByte.once("close", () => res())),
      sleep(200),
    ]);
  }
  log("cleanup", {
    destroyed: true,
    closeSettledAtExit: closeSettled,
    connectionEndedMs: socketCloseAt !== null ? socketCloseAt - runStartAt : null,
  });
  rec.dump(EVIDENCE_NAME);
  say("P12 DONE");
  process.exit(0);
}

main().catch((e) => {
  // fix-r5 concurrency#1：TOCTOU 竞态败者的 bind 失败（startPilotDeckServer 以
  // reject 落定的形态）收进受控路径——exit 3、不落观测记录（照 p16 fix-r4 形态）。
  if (e?.code === "EADDRINUSE") {
    eaddrInuseFail("startPilotDeckServer reject 路径");
  }
  say(`P12 FAILED: ${e?.stack ?? e}`);
  rec.dump(EVIDENCE_NAME);
  process.exit(1);
});
