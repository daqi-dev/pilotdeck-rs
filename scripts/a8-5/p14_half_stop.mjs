/**
 * P14（#2255 半停格）：停掉一部分组件后失败——TS 基线半停语义实测。
 * 场景 A（进程内，bundle 级）：真实 ProjectAutomationBundle，真 CronManager（先停、真停不回滚）
 * + 注入 stop reject 的 alwaysOn。观察：stop() 把单失败原样抛出（不裹 AggregateError）；
 * finally 里 started=false + unpublish 照走；二次 stop() 是 no-op（失败组件永不再被停）；
 * 组件 start/stop 计数不回滚（cron 停一次、不被重启）。
 * 场景 B（child+driver，server 级）：同一 bundle 进真实五步 shutdown；alwaysOn 延迟 300ms reject
 * → 半停窗口（SIGTERM 后 +150ms）E1/E2 照常可服务；随后其余四步照走、退出码 0。
 */
import { spawn } from "node:child_process";
import { mkdirSync, writeSync } from "node:fs";
import { resolve as pathResolve, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import {
  httpProbe, wsConnect, tcpRefused, sleep, Recorder, makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p14-half-stop"), "p14_half_stop");
const CHILD = resolve(dirname(fileURLToPath(import.meta.url)), "p14_child.mjs");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 400)}`);
  rec.log(k, d);
};

const PORT = 39893;

/** 真实 CronManager 的记录包装（委托全部生命周期面；管理面直接走 real 引用）。 */
function wrapCron(real) {
  const w = {
    startCalls: 0,
    stopCalls: 0,
    cronStopDoneAt: 0,
    real,
    getTools: () => real.getTools(),
    bindAgentGateway: (g) => real.bindAgentGateway(g),
    start: async () => {
      w.startCalls += 1;
      await real.start();
    },
    stop: async () => {
      w.stopCalls += 1;
      await real.stop();
      w.cronStopDoneAt = Date.now();
    },
  };
  return w;
}

async function scenarioA() {
  const { ProjectAutomationBundle } = await import(`${BASE}/dist/src/cli/ProjectAutomationBundle.js`);
  const { createCronManager } = await import(`${BASE}/dist/src/cron/runtime/CronManager.js`);

  const cronReal = createCronManager({
    config: { enabled: true, timezone: "UTC", maxConcurrentRuns: 1, runTimeoutMinutes: 60 },
    pilotHome: makeHomeA(),
    logger: { info: () => {}, warn: () => {} },
  });
  const cron = wrapCron(cronReal);
  let aoStopEnteredAt = 0;
  let aoStopCalls = 0;
  const alwaysOn = {
    getTools: () => [],
    bindAgentGateway() {},
    start: async () => {},
    stop: () => {
      aoStopCalls += 1;
      aoStopEnteredAt = Date.now();
      return Promise.reject(new Error("inject: always-on stop failed"));
    },
  };
  const subsystemUpdates = [];
  const automation = new ProjectAutomationBundle({
    config: {
      alwaysOn: { projects: { probe: { enabled: true } } },
      cron: { enabled: true, timezone: "UTC", maxConcurrentRuns: 1, runTimeoutMinutes: 60 },
    },
    pilotHome: makeHomeA2(),
    createCronManager: () => cron,
    createAlwaysOnManager: () => alwaysOn,
  });
  automation.attach({
    agentGateway: {
      async *submitTurn() { yield { type: "error", code: "probe", message: "not fired" }; },
      async abortTurn() {},
      async closeSession() {},
    },
    updateSubsystems: (u) => subsystemUpdates.push(u),
  });
  const startStatus = await automation.start();
  // 真任务（远期 once，不触发）：让 cron.stop 有真实工作面
  await cronReal.createTask({
    projectKey: makeHomeA3(),
    message: "p14-half-stop",
    schedule: { type: "once", runAt: new Date(Date.now() + 3600_000).toISOString() },
  });

  const t0 = Date.now();
  const stopError = await automation.stop().then(
    () => null,
    (e) => e,
  );
  const stopMs = Date.now() - t0;
  log("A/half-stop", {
    startStatus,
    stopMs,
    error: stopError?.message,
    errorIsAggregate: stopError instanceof AggregateError,
    order: { cronStopDoneBeforeAoEntered: cron.cronStopDoneAt <= aoStopEnteredAt },
    counts: { cronStart: cron.startCalls, cronStop: cron.stopCalls },
    lastSubsystemUpdate: subsystemUpdates[subsystemUpdates.length - 1],
    note: "cron 先真停（不回滚：无人重启）；alwaysOn 失败原样抛出（单失败不裹 AggregateError）；finally 里 started=false + unpublish 照走",
  });

  // 二次 stop()：started 已 false → no-op，失败组件永不再被停（stop 计数不涨）
  const t1 = Date.now();
  const second = await automation.stop().then(
    () => ({ resolved: true }),
    (e) => ({ resolved: false, error: e.message }),
  );
  log("A/second-stop-noop", {
    second, secondMs: Date.now() - t1,
    countsAfterSecondStop: { cronStart: cron.startCalls, cronStop: cron.stopCalls, aoStopCalls },
    note: "二次 stop() 立即 resolve 且不再触碰任何组件——半停失败面无重试路径",
  });
}

// makeHome 辅助（隔离各自目录，避免与场景 B 混）
function makeHomeA() {
  const h = pathResolve(RAW_DIR, "homes", "p14-a-cron");
  mkdirSync(h, { recursive: true });
  return h;
}
function makeHomeA2() {
  const h = pathResolve(RAW_DIR, "homes", "p14-a-automation");
  mkdirSync(h, { recursive: true });
  return h;
}
function makeHomeA3() {
  const h = pathResolve(RAW_DIR, "homes", "p14-a-proj");
  mkdirSync(h, { recursive: true });
  return h;
}

async function waitHealthy(port, child, timeoutMs = 15000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (child.exitCode !== null || child.signalCode !== null) return false;
    const r = await httpProbe(port, "/health", { timeoutMs: 800 });
    if (r.ok && r.status === 200) return true;
    await sleep(150);
  }
  return false;
}

async function scenarioB() {
  const out = [];
  const child = spawn(process.execPath, [CHILD], {
    env: { ...process.env, A85_OUT: RAW_DIR, PROBE_PORT: String(PORT), PROBE_HOME_NAME: "p14-b" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (d) => out.push(d.toString().trim()));
  child.stderr.on("data", (d) => out.push(`[err] ${d.toString().trim()}`));
  const healthy = await waitHealthy(PORT, child);
  log("B/pre-sigterm-health", { healthy });
  if (!healthy) {
    log("B/child-output-before-kill", { out: out.join("\n").slice(0, 2000) });
    child.kill("SIGKILL");
    return;
  }

  child.kill("SIGTERM");
  // 半停窗口（alwaysOn 延迟 300ms 才失败）：+150ms 探测 E2（HTTP）与 E1（新 WS）
  await sleep(150);
  log("B/health-at-half-stop-window", await httpProbe(PORT, "/health"));
  let wsResult = "not-tried";
  try {
    const ws = await wsConnect(BASE, PORT, TOKEN);
    const hello = await ws.hello();
    wsResult = { helloType: hello?.type };
    ws.close(1000, "probe-done");
  } catch (e) {
    wsResult = `error: ${e?.message}`;
  }
  log("B/new-ws-at-half-stop-window", wsResult);
  await sleep(200); // 让探测 WS 的 close 帧在 closeServer 前完成

  const t0 = Date.now();
  while (child.exitCode === null && child.signalCode === null && Date.now() - t0 < 15000) {
    await sleep(100);
  }
  log("B/exit", { exitCode: child.exitCode, signal: child.signalCode, ms: Date.now() - t0 });
  log("B/port-after-exit", { refused: await tcpRefused(PORT) });
  const lines = out.join("\n").split("\n").filter((l) => l.startsWith("child-"));
  log("B/child-markers", { lines });
}

async function main() {
  await scenarioA();
  await scenarioB();
  rec.dump("p14_half_stop.json");
  say("P14 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P14 FAILED: ${e?.stack ?? e}`);
  rec.dump("p14_half_stop.json");
  process.exit(1);
});
