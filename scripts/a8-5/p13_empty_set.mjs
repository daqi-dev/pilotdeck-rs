/**
 * P13（#2255 空集合格）：零元素停机面的实测——每个停机组件在「零任务/零 channel/零连接」下
 * stop/dispose 是否照常落定、耗时多少；组合级：全空五步 bundle.stop() 分步计时。
 * 组件格：A 空 CronManager（零项目零任务，native storage 隔离 home）、B 空 BackgroundTaskRuntime
 * （零任务 dispose）、C 零 channel 零连接的真实 server.close()。
 * 组合格 D：真实 ProjectAutomationBundle（空 config → 零 provider）+ 真实 server.close +
 * 真实 ChannelAdapterBundle.flush（空）+ 假 gateway dispose + stub telemetry——五步计时 + 端口释放。
 * 观察窗有界；全部组件真实（除 D 的 disposeGateway/telemetry，与 P1–P3 同款注入）。
 */
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import {
  makeFakeGateway, makeHome, makeStaticDir, Recorder, sleep, tcpRefused, httpProbe,
  makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p13-empty-set"), "p13_empty_set");
const say = (s) => process.stdout.write(`${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 300)}`);
  rec.log(k, d);
};

const PORT = 39892;

async function main() {
  // ---- 格 A：空 CronManager（零项目、零任务）----
  const { createCronManager } = await import(`${BASE}/dist/src/cron/runtime/CronManager.js`);
  const homeA = makeHome(RAW_DIR, "p13-cron");
  const gwCron = {
    submitTurnCalls: 0,
    async *submitTurn() { /* 不会触发（任务远期） */ },
    async abortTurn() { gwCron.submitTurnCalls += 0; },
    async closeSession() {},
  };
  const emptyCron = createCronManager({
    config: { enabled: true, timezone: "UTC", maxConcurrentRuns: 1, runTimeoutMinutes: 60 },
    pilotHome: homeA,
    logger: { info: () => {}, warn: () => {} },
  });
  emptyCron.bindAgentGateway(gwCron);
  let t0 = Date.now();
  await emptyCron.start();
  const startMs = Date.now() - t0;
  t0 = Date.now();
  await emptyCron.stop();
  const stopMs = Date.now() - t0;
  t0 = Date.now();
  await emptyCron.stop(); // 二次 stop：started 已 false，但 CronManager 无 started 闸门，照常空转
  const stop2Ms = Date.now() - t0;
  const neverStarted = createCronManager({
    config: { enabled: true, timezone: "UTC", maxConcurrentRuns: 1, runTimeoutMinutes: 60 },
    pilotHome: makeHome(RAW_DIR, "p13-cron-never"),
    logger: { info: () => {}, warn: () => {} },
  });
  t0 = Date.now();
  await neverStarted.stop(); // 从未 start 的实例直接 stop
  const stopNeverStartedMs = Date.now() - t0;
  log("A/empty-cron", {
    startMs, stopMs, stop2Ms, stopNeverStartedMs,
    note: "零项目零任务：start/stop 均照常落定，无挂起无异常；stop 无 started 闸门（二次 stop / 未 start 先 stop 均空转成功）",
  });

  // ---- 格 B：空 BackgroundTaskRuntime（零任务 dispose）----
  const { BackgroundTaskRuntime } = await import(`${BASE}/dist/src/task/runtime/BackgroundTaskRuntime.js`);
  const spill = resolve(RAW_DIR, "p13-spill");
  mkdirSync(spill, { recursive: true });
  const rt = new BackgroundTaskRuntime({ diskSpillDir: spill, maxTasks: 8 });
  t0 = Date.now();
  await rt.dispose();
  const disposeMs = Date.now() - t0;
  t0 = Date.now();
  await rt.dispose(); // 二次 dispose：disposed 闸门下应立即落定（不抛不挂）
  const dispose2Ms = Date.now() - t0;
  log("B/empty-rt-dispose", {
    disposeMs, dispose2Ms, listAfterDispose: rt.list().length,
    note: "零任务：dispose 即时落定（whenStartsDrained 立即满足）；二次 dispose 复用终态不挂",
  });

  // ---- 格 C：零 channel 零连接的真实 server.close() ----
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const gw = makeFakeGateway();
  const server = await startPilotDeckServer({
    gateway: gw, port: PORT, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p13-server-c"),
  });
  const health = await httpProbe(PORT, "/health");
  t0 = Date.now();
  await server.close();
  const closeMs = Date.now() - t0;
  log("C/empty-close", {
    healthBefore: { ok: health.ok, status: health.status },
    closeMs, refusedAfter: await tcpRefused(PORT),
    note: "零连接：close() 即时落定（P2b 状态 A 同结论，server 级复核）",
  });

  // ---- 组合格 D：全空五步 bundle.stop() ----
  const { ProjectAutomationBundle } = await import(`${BASE}/dist/src/cli/ProjectAutomationBundle.js`);
  const { ChannelAdapterBundle } = await import(`${BASE}/dist/src/cli/ChannelAdapterBundle.js`);
  const { PilotDeckServerShutdownBundle } = await import(`${BASE}/dist/src/cli/PilotDeckServerShutdownBundle.js`);

  const automation = new ProjectAutomationBundle({
    config: {}, // 空 config：alwaysOn 无 enabled 项目、cron 无配置 → 双 undefined
    pilotHome: makeHome(RAW_DIR, "p13-automation"),
  });
  const subsystemUpdates = [];
  automation.attach({
    agentGateway: { /* 空 provider 代下不会被调用 */ },
    updateSubsystems: (u) => subsystemUpdates.push(u),
  });
  const startStatus = await automation.start();
  const server2 = await startPilotDeckServer({
    gateway: gw, port: PORT, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p13-server-d"),
  });
  const channelAdapters = new ChannelAdapterBundle({ pilotHome: makeHome(RAW_DIR, "p13-adapters") });
  const stepTs = {};
  const mark = (k) => { stepTs[k] = Date.now(); };
  const origStop = automation.stop.bind(automation);
  automation.stop = async () => { mark("automation.stop"); await origStop(); };
  const shutdown = new PilotDeckServerShutdownBundle({
    automation,
    closeServer: async () => { mark("closeServer"); await server2.close(); },
    flushChannelState: async () => { mark("flushChannelState"); await channelAdapters.flush(); },
    disposeGateway: async () => { mark("disposeGateway"); gw.dispose("local_gateway_disposed"); },
    telemetry: {
      snapshot: () => ({ probe: "p13-empty" }),
      shutdown: async () => { mark("telemetry.shutdown"); },
    },
    log: (m) => rec.log("D/bundle-log", { m }),
  });
  t0 = Date.now();
  await shutdown.stop();
  const totalMs = Date.now() - t0;
  const tEnd = Date.now();
  log("D/all-empty-bundle", {
    startStatus, totalMs,
    stepOffsetsMs: Object.fromEntries(
      Object.entries(stepTs).map(([k, ts]) => [k, ts - t0]),
    ),
    tailMs: tEnd - t0 - totalMs,
    refusedAfter: await tcpRefused(PORT),
    lastSubsystemUpdate: subsystemUpdates[subsystemUpdates.length - 1],
    note: "全空五步：stop() 照常落定、退出路径可达（对照 D2/D3：有连接/有任务才挂）",
  });

  rec.dump("p13_empty_set.json");
  say("P13 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P13 FAILED: ${e?.stack ?? e}`);
  rec.dump("p13_empty_set.json");
  process.exit(1);
});
