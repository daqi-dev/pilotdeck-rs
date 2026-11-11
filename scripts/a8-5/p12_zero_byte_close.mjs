/**
 * P12：A4 零字节格的 GatewayServer 实例级补测（右删失判据）。
 * 零字节 TCP 连接（建连后不发任何字节）持有下 server.close() 是否 settle。
 * 机制口径（#2259 修正，原「~60s settle、70s 窗区分两者」作废）：headersTimeout
 * 到期不是 60s 即 408——锚点 +60 s 由 30 s connections-checker 网格裁断，零字节
 * 首头 408 实测落在 90,004 ms（specs/gateway-parity/diffs/A8.3-CH-SMS-012.md:37-52）。
 * 因此 70s 窗只证右删失下界：区分「408 才 settle」与「close 清 checker ⇒ 永不
 * settle」（A8.3-CH-SMS-012-R1 的机制推断）须 ≥95 s（保守 120 s）观测
 * （-R1.md:114-120），不得作对拍判 PASS 依据。本装置保留 70s 窗（历史口径），
 * 补 ≥95 s 观测须改 WINDOW_MS 另跑。观察窗有界；结束时收尸（销毁 socket、
 * 若 close 已 settle 则等待完成）。
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
const WINDOW_MS = 70_000;

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

  // 70s 观测窗（只证右删失下界——< 408 裁断点实测 90,004 ms，见 #2259 头注）
  while (Date.now() - t0 < WINDOW_MS && !closeSettled) {
    await sleep(500);
  }
  log("observation", {
    windowMs: WINDOW_MS,
    elapsedMs: Date.now() - t0,
    closeSettled,
    note: closeSettled
      ? "close 在观测窗内 settle（时间见 elapsedMs）"
      : "70s 窗内不 settle（右删失下界 70s；70s < 408 裁断点实测 90,004 ms，区分不了「408 才 settle」与「永不 settle」，见 #2259）",
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
