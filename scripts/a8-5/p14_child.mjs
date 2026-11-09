/**
 * P14 子进程（#2255 半停格 · 场景 B）：真实 server + 真实 PilotDeckServerShutdownBundle +
 * 真实 ProjectAutomationBundle（真 CronManager（包装记录）+ 注入延迟失败的 alwaysOn）。
 * alwaysOn.stop 在 SIGTERM 后延迟 300ms reject——cron 已真停、alwaysOn 未停成的「半停窗口」内
 * 旧 owner 仍可服务（driver 在 +150ms 探测 E1/E2）；随后其余四步照走、退出码 0（P3 结论在
 * 真实 provider 半停下复核）。信号接线照抄 src/cli/pilotdeck.ts:266-280。由 p14_half_stop.mjs 驱动。
 */
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, Recorder, sleep, makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p14-half-stop"), "p14_child");
const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const port = Number(process.env.PROBE_PORT);

const AO_STOP_DELAY_MS = 300;

/** 真实 CronManager 的记录包装（委托全部生命周期面；管理面不经过）。 */
function wrapCron(real) {
  const w = {
    startCalls: 0,
    stopCalls: 0,
    getTools: () => real.getTools(),
    bindAgentGateway: (g) => real.bindAgentGateway(g),
    start: async () => {
      w.startCalls += 1;
      await real.start();
    },
    stop: async () => {
      w.stopCalls += 1;
      const t0 = Date.now();
      await real.stop();
      rec.log("cron-stop", { ms: Date.now() - t0, at: Date.now() });
      say("child-cron-stop-ok");
    },
  };
  return w;
}

async function main() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const { PilotDeckServerShutdownBundle } = await import(`${BASE}/dist/src/cli/PilotDeckServerShutdownBundle.js`);
  const { ProjectAutomationBundle } = await import(`${BASE}/dist/src/cli/ProjectAutomationBundle.js`);
  const { createShutdownAndExit } = await import(`${BASE}/dist/src/cli/shutdownCoordinator.js`);
  const { createCronManager } = await import(`${BASE}/dist/src/cron/runtime/CronManager.js`);

  const gw = makeFakeGateway();
  const server = await startPilotDeckServer({
    gateway: gw, port, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, `${process.env.PROBE_HOME_NAME}-server`),
  });
  say(`child-listening ${server.url}`);

  const cronReal = createCronManager({
    config: { enabled: true, timezone: "UTC", maxConcurrentRuns: 1, runTimeoutMinutes: 60 },
    pilotHome: makeHome(RAW_DIR, `${process.env.PROBE_HOME_NAME}-cron`),
    logger: { info: () => {}, warn: () => {} },
  });
  const cron = wrapCron(cronReal);
  const alwaysOn = {
    getTools: () => [],
    bindAgentGateway() {},
    start: async () => {},
    stop: () => sleep(AO_STOP_DELAY_MS).then(
      () => {
        say("child-ao-stop-reject");
        return Promise.reject(new Error("inject: always-on stop failed"));
      },
    ),
  };
  const automation = new ProjectAutomationBundle({
    config: {
      alwaysOn: { projects: { probe: { enabled: true } } },
      cron: { enabled: true, timezone: "UTC", maxConcurrentRuns: 1, runTimeoutMinutes: 60 },
    },
    pilotHome: makeHome(RAW_DIR, `${process.env.PROBE_HOME_NAME}-automation`),
    createCronManager: () => cron,
    createAlwaysOnManager: () => alwaysOn,
  });
  automation.attach({
    agentGateway: {
      async *submitTurn() { yield { type: "error", code: "probe", message: "not fired" }; },
      async abortTurn() {},
      async closeSession() {},
    },
    updateSubsystems: () => {},
  });
  await automation.start();
  // 真任务（远期 once，不触发）：让 cron.stop 有真实工作面
  await cronReal.createTask({
    projectKey: makeHome(RAW_DIR, `${process.env.PROBE_HOME_NAME}-proj`),
    message: "p14-half-stop",
    schedule: { type: "once", runAt: new Date(Date.now() + 3600_000).toISOString() },
  });
  say("child-started");

  const telemetry = {
    trackError: (e, meta) => rec.log("telemetry-trackError", { message: String(e), meta }),
    snapshot: () => ({ probe: true }),
    shutdown: async () => { say("child-telemetry-shutdown"); },
  };
  const shutdown = new PilotDeckServerShutdownBundle({
    automation,
    closeServer: () => {
      say("child-close-server-enter");
      return server.close();
    },
    flushChannelState: async () => { say("child-flush-enter"); },
    disposeGateway: async () => { say("child-dispose-enter"); },
    telemetry,
    log: (m) => say(`child-bundle-log ${m}`),
  });
  const shutdownAndExit = createShutdownAndExit(() => shutdown.stop(), (code) => {
    rec.log("exit", { code, cronStopCalls: cron.stopCalls, cronStartCalls: cron.startCalls });
    say(`child-exit code=${code}`);
    rec.dump(`p14_child_${port}.json`);
    process.exit(code);
  });
  process.on("uncaughtException", (error) => {
    telemetry.trackError(error, { module: "runtime", metadata: { source: "uncaughtException" } });
    void shutdownAndExit(1);
  });
  process.on("unhandledRejection", (reason) => {
    telemetry.trackError(reason, { module: "runtime", metadata: { source: "unhandledRejection" } });
    void shutdownAndExit(1);
  });
  process.on("SIGINT", () => {
    void shutdownAndExit(0);
  });
  process.on("SIGTERM", () => {
    void shutdownAndExit(0);
  });
  rec.log("ready", { port });
  await new Promise(() => undefined);
}

main().catch((e) => {
  say(`child-fatal ${e?.stack ?? e}`);
  process.exit(2);
});
