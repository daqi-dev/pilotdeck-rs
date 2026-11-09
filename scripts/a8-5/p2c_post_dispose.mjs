/**
 * P2c：S3/S4 阶段的既有 WS 连接行为（closeServer 用替身：不真关 listener，避免
 * 「close() 等连接」死锁——该死锁本身已由 p2b/p1_p2 记录为基线事实）。
 * 观察：flush 中、dispose 前后，既有连接上的请求/hello 是否仍被分发。
 */
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, wsConnect, Recorder, deferred, sleep,
  makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p2c-post-dispose"), "p2c_post_dispose");
const port = 39877;
const rec = new Recorder(RAW_DIR);
const say = (s) => writeSync(1, `${s}\n`);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 240)}`);
  rec.log(k, d);
};

async function main() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const { PilotDeckServerShutdownBundle } = await import(`${BASE}/dist/src/cli/PilotDeckServerShutdownBundle.js`);
  const gw = makeFakeGateway();
  const server = await startPilotDeckServer({
    gateway: gw, port, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p2c"),
  });
  const wsA = await wsConnect(BASE, port, TOKEN);
  await wsA.hello();
  const wsB = await wsConnect(BASE, port, TOKEN); // 未 hello

  const autoStop = deferred();
  const flush = deferred();
  const disposeGw = deferred();
  const bundle = new PilotDeckServerShutdownBundle({
    automation: { stop: () => { autoStop.entered = true; return autoStop.promise; } },
    // 替身：真实 closeServer 的「停 channel + 等 Node server.close（含等待既有连接）」
    // 已在 p1_p2/p2b 记录；这里只推进阶段顺序，保留连接以观察后续阶段。
    closeServer: async () => { log("closeServer-stub", { note: "listener NOT closed (stub)" }); },
    flushChannelState: async () => { await flush.promise; },
    disposeGateway: async () => { await disposeGw.promise; gw.dispose("local_gateway_disposed"); },
    telemetry: { snapshot: () => ({}), shutdown: async () => {} },
    log: () => {},
  });
  const stopPromise = bundle.stop();
  while (!autoStop.entered) await sleep(5);
  autoStop.resolve();
  while (!flush.promise) await sleep(5);

  // S3：flush 进行中
  log("stage", "S3 flush in flight");
  log("ws@S3/describe", await wsA.request("describe_server"));
  flush.resolve();

  // S4：disposeGateway 进行中（gateway 尚未 dispose）
  await sleep(50);
  log("stage", "S4 disposeGateway in flight (pre-dispose)");
  log("ws@S4/pre-dispose-describe", await wsA.request("describe_server"));
  disposeGw.resolve();
  await stopPromise;
  log("stop-settled", { gatewayDisposed: gw.disposed });

  // S4 后：gateway 已 dispose，既有连接仍分发
  log("stage", "S4 done (gateway disposed)");
  log("ws@after-dispose/describe", await wsA.request("describe_server"));
  log("ws@after-dispose/hello", await wsB.hello());
  const disposeCallIdx = gw.calls.findIndex((c) => c.method.startsWith("dispose:"));
  const afterDispose = gw.calls.length - disposeCallIdx - 1;
  log("calls-after-dispose", { disposeCallIdx, callsAfterDispose: afterDispose, tail: gw.calls.slice(-3) });

  wsA.close();
  wsB.close();
  await server.close();
  rec.dump("p2c_post_dispose.json");
  say("P2c DONE");
  process.exit(0);
}

main().then(
  () => process.exit(0),
  (e) => {
    say(`P2c FAILED: ${e?.stack ?? e}`);
    rec.dump("p2c_post_dispose.json");
    process.exit(1);
  },
);
