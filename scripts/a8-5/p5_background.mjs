/**
 * P5：E5 background × 阶段矩阵。真实 BackgroundTaskRuntime（隔离 diskSpillDir）。
 * 观察：S0 提交/读取/单停、dispose 期间的接纳闸门（唯一显式 gate）、
 * dispose 的 kill/drain 顺序、输出在 dispose resolve 前可读、dispose 后提交被拒。
 */
import { writeSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { Recorder, makeHome, sleep, makePaths, parseProbeArgs } from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p5-background"), "p5_background");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 300)}`);
  rec.log(k, d);
};

async function main() {
  const { BackgroundTaskRuntime } = await import(`${BASE}/dist/src/task/runtime/BackgroundTaskRuntime.js`);
  const spill = resolve(RAW_DIR, "p5-spill");
  mkdirSync(spill, { recursive: true });
  const rt = new BackgroundTaskRuntime({ diskSpillDir: spill, maxTasks: 8 });

  // S0：提交 + 输出读取 + 单任务停
  const task = await rt.start({
    command: 'echo bg-probe-start; sleep 60',
    cwd: makeHome(RAW_DIR, "p5-cwd"),
    agentId: "probe-agent",
    sessionId: "probe-session",
  });
  log("S0/start", { taskId: task.taskId, status: task.status });
  await sleep(400);
  const out1 = rt.getOutput(task.taskId, 0, 200);
  log("S0/getOutput", { text: out1.text?.slice(0, 80), bytes: out1.bytes });
  const task2 = await rt.start({ command: "sleep 60", cwd: makeHome(RAW_DIR, "p5-cwd") });
  log("S0/second-task", { taskId: task2.taskId, status: task2.status });
  const stopOne = await rt.stop(task.taskId);
  log("S0/stop-one", { stopOne });

  // dispose：先置 draining（同步），期间新任务被拒；TERM 免疫任务把 draining 窗口拉到 graceMs(5s)
  const stubborn = await rt.start({
    command: "trap '' TERM; echo stubborn-start; sleep 60",
    cwd: makeHome(RAW_DIR, "p5-cwd"),
  });
  await sleep(300);
  const outStubborn = rt.getOutput(stubborn.taskId, 0, 200);
  log("S0/stubborn-output", { slice: JSON.stringify(outStubborn).slice(0, 160) });
  const disposePromise = rt.dispose();
  await sleep(300); // killAll 的 SIGTERM 已发、grace 计时中（state=draining）
  const during = await rt.start({ command: "echo never", cwd: makeHome(RAW_DIR, "p5-cwd") }).then(
    (t) => ({ admitted: true, taskId: t.taskId }),
    (e) => ({ admitted: false, error: e.message }),
  );
  log("during-dispose/start", during);
  const outDuring = rt.getOutput(stubborn.taskId, 0, 200);
  log("during-dispose/getOutput", { slice: JSON.stringify(outDuring).slice(0, 160) });
  const t0 = Date.now();
  await disposePromise;
  log("dispose-resolved", { ok: true, ms: Date.now() - t0, note: "grace=5000ms SIGKILL 兜底" });

  // dispose 后：读取/提交/停
  const after = await rt.start({ command: "echo never", cwd: makeHome(RAW_DIR, "p5-cwd") }).then(
    (t) => ({ admitted: true, taskId: t.taskId }),
    (e) => ({ admitted: false, error: e.message }),
  );
  log("after-dispose/start", after);
  const final2 = rt.get(task2.taskId);
  log("after-dispose/task2", { status: final2?.status, interrupted: final2?.interrupted });
  const listAfter = rt.list();
  log("after-dispose/list", { count: listAfter.length });

  rec.dump("p5_background.json");
  say("P5 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P5 FAILED: ${e?.stack ?? e}`);
  rec.dump("p5_background.json");
  process.exit(1);
});
