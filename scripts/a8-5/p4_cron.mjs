/**
 * P4：E3 cron × 阶段矩阵。真实 CronManager（native storage，隔离 pilotHome）+ 假 agentGateway。
 * 观察：S0 管理方法/触发、stop() 的 abort drain（reason=system:cron_shutdown）、
 * abort 失败被吞、stop 后管理方法仍无闸门（含 runTaskNow 静默不触发）。
 */
import { writeSync, mkdirSync } from "node:fs";
import { Recorder, makeHome, sleep, makePaths, parseProbeArgs } from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p4-cron"), "p4_cron");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 300)}`);
  rec.log(k, d);
};

async function main() {
  const home = makeHome(RAW_DIR, "p4-cron");
  mkdirSync(home, { recursive: true });
  const projectKey = `${home}/proj`;
  mkdirSync(projectKey, { recursive: true });

  const gw = {
    submitTurnCalls: [],
    abortTurnCalls: [],
    closeSessionCalls: [],
    submitTurn(input) {
      gw.submitTurnCalls.push({ sessionKey: input.sessionKey, runId: input.runId, at: Date.now() });
      say(`[gw.submitTurn] ${JSON.stringify({ sessionKey: input.sessionKey, runId: input.runId })}`);
      return (async function* () {
        // 挂住直到 abort（CronFire 会因 abort 后流关闭而收尾）
        while (!gw.aborted) await sleep(50);
        yield { type: "error", code: "aborted", message: "probe abort" };
      })();
    },
    abortTurn(input) {
      gw.aborted = true;
      gw.abortTurnCalls.push({ ...input, at: Date.now() });
      say(`[gw.abortTurn] ${JSON.stringify(input)}`);
      return gw.abortMode === "fail" ? Promise.reject(new Error("inject: abort failed")) : Promise.resolve();
    },
    closeSession(input) {
      gw.closeSessionCalls.push(input);
      return Promise.resolve();
    },
    aborted: false,
    abortMode: "ok",
  };

  const { createCronManager } = await import(`${BASE}/dist/src/cron/runtime/CronManager.js`);
  const manager = createCronManager({
    config: { enabled: true, timezone: "UTC", maxConcurrentRuns: 1, runTimeoutMinutes: 60 },
    pilotHome: home,
    logger: { info: (m, d) => rec.log("cron-log", { m, d }), warn: (m, d) => rec.log("cron-warn", { m, d }) },
  });
  manager.bindAgentGateway(gw);
  await manager.start();
  log("started", { projects: 0 });

  // S0：管理方法
  const future = new Date(Date.now() + 3600_000).toISOString();
  const created = await manager.createTask({
    projectKey, message: "probe-run", schedule: { type: "once", runAt: future },
  });
  log("S0/createTask", { taskId: created.task.taskId, status: created.task.status });

  // S0：触发（runTaskNow 建立即刻到期副本 → poke → tick → submitTurn）
  const nowRun = await manager.runTaskNow({ taskId: created.task.taskId });
  log("S0/runTaskNow", nowRun);
  const t0 = Date.now();
  while (gw.submitTurnCalls.length === 0 && Date.now() - t0 < 5000) await sleep(50);
  log("S0/fired", { submitTurnCalls: gw.submitTurnCalls.length, ms: Date.now() - t0 });

  // S0：并发触发（在飞时再次 runTaskNow）
  const again = await manager.runTaskNow({ taskId: created.task.taskId });
  log("S0/runTaskNow-while-running", again);
  const listWhileRunning = await manager.listTasks({ projectKey, includeHistory: true });
  log("S0/listTasks", { statuses: listWhileRunning.tasks.map((t) => t.status) });

  // S1 等价（automation.stop 的第一步是 cron.stop）：真实 stop() 的 abort drain
  const stopResult = await manager.stop().then(
    () => ({ resolved: true }),
    (e) => ({ resolved: false, error: e.message }),
  );
  log("stop()", { stopResult, abortTurnCalls: gw.abortTurnCalls });

  // stop 后：管理方法仍无闸门（manager 层）
  const created2 = await manager.createTask({
    projectKey, message: "probe-after-stop", schedule: { type: "once", runAt: future },
  }).then(
    (r) => ({ ok: true, taskId: r.task.taskId }),
    (e) => ({ ok: false, error: e.message }),
  );
  log("after-stop/createTask", created2);
  const nowRun2 = await manager.runTaskNow({ taskId: created2.taskId ?? created.task.taskId });
  log("after-stop/runTaskNow", nowRun2);
  await sleep(600);
  log("after-stop/fired?", { submitTurnCallsAfterStop: gw.submitTurnCalls.length });

  // 注入：stop 时 abort 失败 → 被 CronRuntime 吞掉，stop 仍成功
  const manager2 = createCronManager({
    config: { enabled: true, timezone: "UTC", maxConcurrentRuns: 1, runTimeoutMinutes: 60 },
    pilotHome: makeHome(RAW_DIR, "p4-cron-2"),
    logger: { info: () => {}, warn: (m, d) => rec.log("cron2-warn", { m, d }) },
  });
  gw.abortMode = "fail";
  gw.aborted = false;
  const abortCountBefore = gw.abortTurnCalls.length;
  manager2.bindAgentGateway(gw);
  await manager2.start();
  const c2 = await manager2.createTask({
    projectKey: `${home}/proj2`, message: "probe2", schedule: { type: "once", runAt: future },
  });
  await manager2.runTaskNow({ taskId: c2.task.taskId });
  const t1 = Date.now();
  while (gw.submitTurnCalls.length < 2 && Date.now() - t1 < 5000) await sleep(50);
  log("injected-run-state", { submitTurnCalls: gw.submitTurnCalls.length, abortCountBefore });
  const stop2 = await manager2.stop().then(
    () => ({ resolved: true }),
    (e) => ({ resolved: false, error: e.message }),
  );
  log("stop-with-failing-abort", { stop2, newAbortTurnCalls: gw.abortTurnCalls.slice(abortCountBefore) });

  rec.dump("p4_cron.json");
  say("P4 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P4 FAILED: ${e?.stack ?? e}`);
  rec.dump("p4_cron.json");
  process.exit(1);
});
