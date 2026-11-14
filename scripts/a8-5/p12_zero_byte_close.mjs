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
 * 预检失败受控快速失败（exit 3，不落观测记录，照 p16 fix-r4 EADDRINUSE
 * 受控 FAIL 形态）。证据名带窗长+时间戳+pid（run 唯一，tmp 名随之唯一，
 * 不覆写既有证据）；结束时收尸（销毁 socket、若 close 已 settle 则等待完成）。
 * 退出码：0 成功；1 运行失败（落失败记录）；2 非法参数/env（不落盘）；
 * 3 端口占用预检失败（不落盘）。
 */
import { writeSync } from "node:fs";
import { connect } from "node:net";
import {
  makeFakeGateway, makeHome, makeStaticDir, Recorder, sleep,
  makePaths, parseProbeArgs, tcpRefused, TOKEN,
} from "./common.mjs";

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
// ≥95 s 窗已覆盖最坏相位 408 落点（相位窗 (60, 90] s + 毫秒级 tick 抖动）
const COVERS_WORST_PHASE = WINDOW_MS >= 95_000;

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

// 证据名带窗长+时间戳+pid：run 唯一，杜绝两轮互覆（fix-r3 concurrency#3；
// Recorder.dump 的 tmp 名 = 本名 + ".tmp"，随之唯一）
const RUN_STAMP = `${new Date().toISOString().replace(/[:.]/g, "")}-${process.pid}`;
const EVIDENCE_NAME = `p12_zero_byte_close-${WINDOW_MS}ms-${RUN_STAMP}.json`;

async function main() {
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

  // 零字节连接：建连成功后不发任何字节
  const connectedAt = Date.now();
  const zeroByte = await new Promise((res, rej) => {
    const sock = connect(port, "127.0.0.1", () => res(sock));
    sock.once("error", rej);
  });
  log("zero-byte-connected", { localPort: zeroByte.localPort, wroteBytes: 0 });
  const socketEvents = [];
  let firstByteMs = null;
  let bytes = 0;
  let hex = null;
  zeroByte.on("close", () => socketEvents.push({ event: "close", at: Date.now() }));
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
  let closeSettled = false;
  const closePromise = server.close().then(
    () => { closeSettled = true; },
    () => { closeSettled = true; },
  );

  // 观测窗（默认 70 s 只证右删失下界——未覆盖最坏相位 408 落点，见 #2259 头注）
  while (Date.now() - t0 < WINDOW_MS && !closeSettled) {
    await sleep(500);
  }
  log("observation", {
    windowMs: WINDOW_MS,
    windowEnvRaw: rawWindowEnv ?? null, // 原始 env 字符串（复算用；null = 未设置）
    port,
    connectToCloseMs: t0 - connectedAt,
    elapsedMs: Date.now() - t0,
    closeSettled,
    firstByteMs,
    bytes,
    hex,
    note: closeSettled
      ? "close 在观测窗内 settle（时间见 elapsedMs）"
      : firstByteMs !== null
        ? `${WINDOW_S}s 窗内不 settle，但服务端字节已到达（firstByteMs/bytes/hex 见本行）——排除「close 清 checker ⇒ 408 不发」，settle 等待另有原因，见 #2259`
        : COVERS_WORST_PHASE
          ? `${WINDOW_S}s 窗内不 settle 且零字节 socket 未收到任何服务端字节——窗已覆盖最坏相位 408 落点（相位窗 (60, 90] s + 毫秒级 tick 抖动），支持「close 清 checker ⇒ 永不 settle」（SmsChannel 实例机制链 A8.3-CH-SMS-014-R2；GatewayServer 实例路径以本观测为准），见 #2259`
          : `${WINDOW_S}s 窗内不 settle（右删失下界 ${WINDOW_S}s；窗未覆盖最坏相位 408 落点（落点随网格相位在 (60, 90] s 窗内、含毫秒级 tick 抖动），区分不了「408 才 settle」与「永不 settle」，见 #2259）`,
    socketEventsFromServerSide: socketEvents,
    socketLocalStillOpen: !zeroByte.destroyed,
  });

  // 收尾：销毁零字节 socket；若 close 已 settle 则 closePromise 已完成，否则进程显式退出
  zeroByte.destroy();
  log("cleanup", { destroyed: true, closeSettledAtExit: closeSettled });
  rec.dump(EVIDENCE_NAME);
  say("P12 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P12 FAILED: ${e?.stack ?? e}`);
  rec.dump(EVIDENCE_NAME);
  process.exit(1);
});
