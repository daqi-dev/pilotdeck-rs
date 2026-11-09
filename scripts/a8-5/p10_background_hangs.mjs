/**
 * P10：#2251 条目 4 —— background start 未落定 / 快照写挂起两格（「SIGKILL 后 dispose 必然收敛」
 * 只对 running 且存储写不挂起成立的反例）。两格都有界观察（窗口 3s），到点登记不落定后收尾退出。
 *
 * 格 A（start 永不 settle）：注入 shell provider（options.shell）的 start() 挂起。
 *   rt.start 在 gate 通过后停在 provider 上（activeStarts=1）→ dispose() 同步置 draining 后
 *   卡 whenStartsDrained（无超时）→ dispose 永不落定、killAll 到不了。
 * 格 B（挂起的快照写）：真实 shell + 假 snapshotStore（write/flush 均挂起）。
 *   任务真实运行；dispose → killAll 的 TERM 即杀（默认 TERM）→ exit listener 同步置
 *   cancelled 后 settleEntry 停在 persistEntry 的 store.write（无二次超时）→ entry.done 不落定
 *   → dispose 卡 `await Promise.all(done)`，后段 output.close()/snapshotStore.flush() 到不了。
 */
import { writeSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { Recorder, makeHome, sleep, makePaths, parseProbeArgs } from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p10-background-hangs"), "p10_background_hangs");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 400)}`);
  rec.log(k, d);
};

const OBSERVE_MS = 3000;

function settleState(promiseRef) {
  return { settled: promiseRef.settled === true };
}

function tracked() {
  const state = { settled: false };
  state.promise = new Promise((res, rej) => {
    state.resolve = (v) => { state.settled = true; res(v); };
    state.reject = (e) => { state.settled = true; rej(e); };
  });
  return state;
}

async function cellA() {
  const { BackgroundTaskRuntime } = await import(`${BASE}/dist/src/task/runtime/BackgroundTaskRuntime.js`);
  const spill = resolve(RAW_DIR, "p10a-spill");
  mkdirSync(spill, { recursive: true });
  // 注入：shell provider 的 start() 永不 settle（无真实子进程派生）
  const hangShell = { start: () => new Promise(() => undefined) };
  const rt = new BackgroundTaskRuntime({ diskSpillDir: spill, maxTasks: 4, shell: hangShell });
  const cwd = makeHome(RAW_DIR, "p10a-cwd");

  const startState = tracked();
  const startPromise = rt.start({ command: "echo a-never", cwd }).then(
    (t) => { startState.resolve({ admitted: true, taskId: t.taskId, status: t.status }); return t; },
    (e) => { startState.reject({ admitted: false, error: e.message }); throw e; },
  );
  await sleep(300); // start 已进入 provider（gate 通过、activeStarts=1）
  const pendingEntry = rt.list().map((t) => ({ taskId: t.taskId, status: t.status, pid: t.pid }));
  log("A/start-in-flight", { startSettled: settleState(startState), listWhilePending: pendingEntry });

  const disposeState = tracked();
  const disposePromise = rt.dispose().then(
    () => disposeState.resolve(true),
    (e) => disposeState.reject({ error: e?.message }),
  );
  await sleep(OBSERVE_MS);
  const gateDuring = await rt.start({ command: "echo a-during", cwd }).then(
    (t) => ({ admitted: true, taskId: t.taskId }),
    (e) => ({ admitted: false, error: e.message }),
  );
  log("A/hang-window", {
    observeMs: OBSERVE_MS,
    disposeSettled: settleState(disposeState),
    gateDuringHang: gateDuring,
    note: "whenStartsDrained 无超时 ⇒ dispose 永不落定（右删失下界：3s 窗口内不落定）",
  });
  // 收尾：start/dispose 均挂起（注入所致，无真实子进程）——进程显式退出，无残留
  log("A/exit-note", { note: "两条 promise 均为注入挂起，无 OS 资源；process.exit 收尾" });
}

async function cellB() {
  const { BackgroundTaskRuntime } = await import(`${BASE}/dist/src/task/runtime/BackgroundTaskRuntime.js`);
  const spill = resolve(RAW_DIR, "p10b-spill");
  mkdirSync(spill, { recursive: true });
  // 注入：snapshotStore 的 write 自 store.hang=true 起挂起（快照写挂起）；
  // load 返回空（无历史快照可恢复）。任务起跑前的 admission 写必须放行，
  // 否则 start 卡在 spawn 前的持久化（那是另一个格，不在本格登记）。
  const hangStore = {
    hang: false,
    load: () => [],
    write: () => (hangStore.hang ? new Promise(() => undefined) : Promise.resolve()),
    flush: () => new Promise(() => undefined),
  };
  const rt = new BackgroundTaskRuntime({ diskSpillDir: spill, maxTasks: 4, snapshotStore: hangStore });
  const cwd = makeHome(RAW_DIR, "p10b-cwd");

  const task = await rt.start({ command: "echo b-start; sleep 60", cwd });
  await sleep(300);
  const out1 = rt.getOutput(task.taskId, 0, 200);
  log("B/task-running", { taskId: task.taskId, pid: task.pid, status: task.status, output: out1.content?.slice(0, 40) });

  hangStore.hang = true; // 快照写自此挂起（含 kill 后 settleEntry 的最终写）

  const disposeState = tracked();
  const t0 = Date.now();
  const disposePromise = rt.dispose().then(
    () => disposeState.resolve(true),
    (e) => disposeState.reject({ error: e?.message }),
  );
  await sleep(1500); // killAll 的 TERM 已发出（默认 TERM ⇒ 立即死）
  const taskNow = rt.get(task.taskId);
  const outDuring = rt.getOutput(task.taskId, 0, 200);
  log("B/after-kill", {
    msSinceDispose: Date.now() - t0,
    taskStatus: taskNow?.status,
    interrupted: taskNow?.interrupted,
    outputStillReadable: { content: outDuring.content?.slice(0, 40), totalBytes: outDuring.totalBytes },
  });
  await sleep(OBSERVE_MS - 1500);
  log("B/hang-window", {
    observeMs: OBSERVE_MS,
    disposeSettled: settleState(disposeState),
    note: "settleEntry 停在 store.write（无二次超时）⇒ entry.done 不落定 ⇒ dispose 卡 await done；output.close()/snapshotStore.flush() 更到不了",
  });
  // 收尾：任务子进程已被 TERM 杀掉（真实子进程已退出）；挂起的只是注入的 store promise
  const alive = task.pid ? await isAlive(task.pid) : null;
  log("B/exit-note", { childAlive: alive, note: "真实子进程已死于 TERM；process.exit 收尾" });
}

async function isAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function main() {
  await cellA();
  await cellB();
  rec.dump("p10_background_hangs.json");
  say("P10 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P10 FAILED: ${e?.stack ?? e}`);
  rec.dump("p10_background_hangs.json");
  process.exit(1);
});
