/**
 * P1+P2：E1/E2（WS/HTTP）× S0–S4 阶段逐格 + t3 边界并发。
 * 真实 startPilotDeckServer + 真实 PilotDeckServerShutdownBundle；automation/flush/disposeGateway
 * 三步用受控 deferred 提供阶段驻留点；closeServer 用真实 server.close（channel 停 + Node server.close）。
 */
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, httpProbe, keepAliveProbe,
  tcpRefused, wsConnect, Recorder, deferred, sleep, makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const args = parseProbeArgs("p1-p2-stages");
const { BASE, RAW_DIR } = makePaths(args, "p1_p2_stages");
const port = 39872;
const rec = new Recorder(RAW_DIR);
const say = (s) => writeSync(1, `${s}\n`);
const _log = rec.log.bind(rec);
rec.log = (kind, detail) => {
  say(`[${kind}] ${JSON.stringify(detail).slice(0, 220)}`);
  _log(kind, detail);
};

async function probeHttpEntries(stage) {
  rec.log(`http@${stage}/health`, await httpProbe(port, "/health"));
  rec.log(`http@${stage}/api-web-401`, await httpProbe(port, "/api/web/projects"));
  rec.log(`http@${stage}/api-web-authed`, await httpProbe(port, "/api/web/projects", {
    headers: { authorization: `Bearer ${TOKEN}` },
  }));
}

async function main() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const { PilotDeckServerShutdownBundle } = await import(`${BASE}/dist/src/cli/PilotDeckServerShutdownBundle.js`);
  const gw = makeFakeGateway();
  const server = await startPilotDeckServer({
    gateway: gw,
    port,
    host: "127.0.0.1",
    token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR),
    pilotHome: makeHome(RAW_DIR, "p12"),
  });
  rec.log("server", { url: server.url });

  // ---- S0 基线 ----
  await probeHttpEntries("S0");
  const wsA = await wsConnect(BASE, port, TOKEN);
  await wsA.hello();
  rec.log("ws@S0/describe_server", await wsA.request("describe_server"));
  const wsB = await wsConnect(BASE, port, TOKEN); // 只建连不 hello，留到 close 后再 hello（闸门格）
  const ka = await keepAliveProbe(port);
  // #2756（同 p16 #2662 守门形状）：建池前提守门——首次请求失败或响应 Connection
  // 非 keep-alive（缺头按完整 HTTP/1.1 隐式）时 agent 池空，close 后的
  // ka@after-close/reuse* 是新连接撞已关 listener（错误串相同但「复用已关连接」
  // 解释无依据，matrix.md §4.2 该格结论失据），故记 SKIP 行、reuse 两行跳过。
  const kaFirst = await ka.once("/health");
  const kaPool = kaFirst.premise === true;
  rec.log("ka@S0/first", kaFirst);
  if (!kaPool) {
    rec.log("ka/SKIP-after-close-reuse", {
      reason: kaFirst.premiseReason,
      kaFirst,
      note: "建池前提未成立：ka@after-close/reuse* 跳过（新连接对照已由 http@after-close/health 覆盖，不重复产出），「关闭后复用报 ECONNREFUSED」结论不作对照",
    });
  }

  // ---- 组装 shutdown，三步受控 ----
  const autoStop = deferred();
  const flush = deferred();
  const disposeGw = deferred();
  const stepOrder = [];
  const bundle = new PilotDeckServerShutdownBundle({
    automation: {
      stop: () => {
        stepOrder.push("automation.stop:enter");
        autoStop.entered = true;
        return autoStop.promise;
      },
    },
    closeServer: () => {
      stepOrder.push("closeServer:enter");
      return server.close();
    },
    flushChannelState: async () => {
      stepOrder.push("flush:enter");
      await flush.promise;
    },
    disposeGateway: async () => {
      stepOrder.push("disposeGateway:enter");
      await disposeGw.promise;
      gw.dispose("local_gateway_disposed");
    },
    telemetry: { snapshot: () => ({ probe: true }), shutdown: async () => { stepOrder.push("telemetry.shutdown"); } },
    log: (m) => rec.log("bundle-log", m),
  });
  const stopPromise = bundle.stop();
  while (!autoStop.entered) await sleep(5);

  // ---- S1：automation drain 进行中（listener 仍开） ----
  rec.log("stage", "S1 (automation.stop in flight)");
  await probeHttpEntries("S1");
  const wsS1 = await wsConnect(BASE, port, TOKEN);
  rec.log("ws@S1/hello", await wsS1.hello());
  rec.log("ws@S1/describe_server", await wsS1.request("describe_server"));
  wsS1.close(1000, "probe-done");

  autoStop.resolve();
  // 等 closeServer 完成（listener 关闭）
  const t0 = Date.now();
  while (!(await tcpRefused(port)) && Date.now() - t0 < 5000) await sleep(10);
  rec.log("listener", { refusedAfterMs: Date.now() - t0 });

  // ---- S2 完成、S3 flush 挂起中 ----
  rec.log("stage", "S2 done / S3 (flush in flight)");
  rec.log("tcp@after-close/refused", await tcpRefused(port));
  rec.log("http@after-close/health", await httpProbe(port, "/health"));
  rec.log("ws@after-close/existing-describe", await wsA.request("describe_server"));
  rec.log("ws@after-close/existing-hello", await wsB.hello());
  // #2756：前提未成立时跳过（池空时测到的是新连接而非复用，证据无效——同 p16 观察 3 形状）。
  if (kaPool) {
    rec.log("ka@after-close/reuse", await ka.once("/health"));
    rec.log("ka@after-close/reuse2", await ka.once("/api/web/projects"));
  } else {
    rec.log("ka@after-close/reuse", { skipped: true, reason: "建池前提未成立（见 ka/SKIP-after-close-reuse）" });
    rec.log("ka@after-close/reuse2", { skipped: true, reason: "建池前提未成立（见 ka/SKIP-after-close-reuse）" });
  }
  try {
    await server.reconcileChannels({ channels: [], managedChannelKeys: [] });
    rec.log("reconcile@after-close", { ok: true });
  } catch (error) {
    rec.log("reconcile@after-close", { error: error.message });
  }

  // closeServer 阻塞格：既有 WS 连接不断开 → flush 不进入；客户端断开后才推进。
  {
    const flushed = stepOrder.includes("flush:enter");
    rec.log("closeServer-blocked-by-ws", { flushEnteredWhileWsOpen: flushed });
    wsA.close(1000, "probe-release");
    wsB.close(1000, "probe-release");
    ka.destroy();
    const t0 = Date.now();
    while (!stepOrder.includes("flush:enter") && Date.now() - t0 < 5000) await sleep(10);
    rec.log("flush-after-client-close", { flushEnteredMs: Date.now() - t0, stepOrder });
  }

  flush.resolve();
  while (!stepOrder.includes("disposeGateway:enter")) await sleep(5);

  // ---- S4：disposeGateway 挂起中（gateway 未 dispose；此时已无客户端连接） ----
  rec.log("stage", "S4 (disposeGateway in flight, before dispose)");
  disposeGw.resolve();
  const stopError = await stopPromise.then(
    () => undefined,
    (e) => ({ name: e?.constructor?.name, message: e?.message }),
  );
  rec.log("stop-settled", { stopError, stepOrder, gatewayDisposed: gw.disposed });

  // ---- S4 后：gateway 已 dispose；客户端面的行为在 p2c 补格 ----
  rec.log("stage", "S4 done");
  rec.log("gateway-calls-tail", gw.calls.slice(-4));
  try {
    await server.reconcileChannels({ channels: [], managedChannelKeys: [] });
    rec.log("reconcile@after-dispose", { ok: true });
  } catch (error) {
    rec.log("reconcile@after-dispose", { error: error.message });
  }

  wsA.close();
  wsB.close();
  rec.dump("p1_p2_stages.json");
  say("P1/P2 DONE; entries: " + rec.entries.length);
  process.exit(0);
}

main().then(
  () => process.exit(0),
  (error) => {
    console.error("P1/P2 FAILED:", error);
    rec.log("fatal", { message: String(error?.stack ?? error) });
    rec.dump("p1_p2_stages.json");
    process.exit(1);
  },
);
