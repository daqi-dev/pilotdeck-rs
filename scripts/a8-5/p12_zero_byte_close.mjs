/**
 * P12：A4 零字节格的 GatewayServer 实例级补测（右删失判据）。
 * 零字节 TCP 连接（建连后不发任何字节）持有下 server.close() 是否 settle。
 * 408 落点为相位窗 (60, 90] s 内的相位值（含毫秒级 tick 执行抖动）、随网格
 * 相位变化——70 s 窗只证右删失下界；区分「408 才 settle」与「close 清
 * checker ⇒ 永不 settle」须 ≥95 s（保守 120 s）观测，不得作对拍判 PASS
 * 依据（#2259；机制与在档实测单源见 matrix §六 A4 格内标注）。
 * 观察窗有界；结束时收尸（销毁 socket、若 close 已 settle 则等待完成）。
 * 复跑纪律：窗长用 P12_WINDOW_MS（毫秒，默认 70_000 保留历史口径）；
 * 另跑须传 --out <新目录>（默认输出名固定、last-wins，勿复用默认目录）。
 */
import { writeSync } from "node:fs";
import { connect } from "node:net";
import {
  makeFakeGateway, makeHome, makeStaticDir, Recorder, sleep,
  makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p12-zero-byte-close"), "p12_zero_byte_close");
const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 300)}`);
  rec.log(k, d);
};

const port = 39891;
const WINDOW_MS = Number(process.env.P12_WINDOW_MS ?? 70_000);
const WINDOW_S = WINDOW_MS / 1000;
// ≥95 s 窗已覆盖最坏相位 408 落点（相位窗 (60, 90] s + 毫秒级 tick 抖动）
const COVERS_WORST_PHASE = WINDOW_MS >= 95_000;

async function main() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const server = await startPilotDeckServer({
    gateway: makeFakeGateway(), port, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p12"),
  });

  // 零字节连接：建连成功后不发任何字节
  const zeroByte = await new Promise((res, rej) => {
    const sock = connect(port, "127.0.0.1", () => res(sock));
    sock.once("error", rej);
  });
  log("zero-byte-connected", { localPort: zeroByte.localPort, wroteBytes: 0 });
  const socketEvents = [];
  zeroByte.on("close", () => socketEvents.push({ event: "close", at: Date.now() }));
  zeroByte.on("error", (e) => socketEvents.push({ event: "error", code: e.code, at: Date.now() }));

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
    elapsedMs: Date.now() - t0,
    closeSettled,
    note: closeSettled
      ? "close 在观测窗内 settle（时间见 elapsedMs）"
      : COVERS_WORST_PHASE
        ? `${WINDOW_S}s 窗内不 settle（右删失下界 ${WINDOW_S}s；窗已覆盖最坏相位 408 落点（相位窗 (60, 90] s + 毫秒级 tick 抖动），不 settle 支持「close 清 checker ⇒ 永不 settle」（A8.3-CH-SMS-012-R1 机制推断），见 #2259）`
        : `${WINDOW_S}s 窗内不 settle（右删失下界 ${WINDOW_S}s；窗未覆盖最坏相位 408 落点（落点随网格相位在 (60, 90] s 窗内、含毫秒级 tick 抖动），区分不了「408 才 settle」与「永不 settle」，见 #2259）`,
    socketEventsFromServerSide: socketEvents,
    socketLocalStillOpen: !zeroByte.destroyed,
  });

  // 收尾：销毁零字节 socket；若 close 已 settle 则 closePromise 已完成，否则进程显式退出
  zeroByte.destroy();
  log("cleanup", { destroyed: true, closeSettledAtExit: closeSettled });
  rec.dump("p12_zero_byte_close.json");
  say("P12 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P12 FAILED: ${e?.stack ?? e}`);
  rec.dump("p12_zero_byte_close.json");
  process.exit(1);
});
