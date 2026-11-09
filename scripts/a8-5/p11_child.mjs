/**
 * P11 子进程：E5 在 S1–S3 窗口的接纳 + 提交连接两态去向（由 p11_driver.mjs 驱动）。
 * 真实 startPilotDeckServer（假 gateway、无 channel）+ 真实 BackgroundTaskRuntime 接进
 * disposeGateway（对齐 LocalGatewayLifecycleBundle 的 dispose 序）。信号接线照抄 pilotdeck.ts。
 * SIGTERM 后 400ms（automation.stop 已过、closeServer 挂在 server.close 的 closing 窗口内）
 * 由子进程自行 rt.start —— 测 E5 新任务在该窗口的接纳面；随后由 driver 控制 WS 去留。
 * PROBE_STATE=disconnect → 任务命令 sleep 61；hold → sleep 62。
 */
import { writeSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
import {
  makeFakeGateway, makeHome, makeStaticDir, Recorder, makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p11-e5-admission-window"), "p11_child");
const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const state = process.env.PROBE_STATE;
const port = Number(process.env.PROBE_PORT);
const markerSleep = state === "hold" ? "62" : "61";

async function main() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const { PilotDeckServerShutdownBundle } = await import(`${BASE}/dist/src/cli/PilotDeckServerShutdownBundle.js`);
  const { createShutdownAndExit } = await import(`${BASE}/dist/src/cli/shutdownCoordinator.js`);
  const { BackgroundTaskRuntime } = await import(`${BASE}/dist/src/task/runtime/BackgroundTaskRuntime.js`);

  const gw = makeFakeGateway();
  const server = await startPilotDeckServer({
    gateway: gw, port, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, `p11-${state}`),
  });
  const spill = resolve(RAW_DIR, `p11-${state}-spill`);
  mkdirSync(spill, { recursive: true });
  const rt = new BackgroundTaskRuntime({ diskSpillDir: spill, maxTasks: 4 });
  let sigtermAt = 0;
  let autoDoneAt = 0;
  let disposeEnteredAt = 0;

  const shutdown = new PilotDeckServerShutdownBundle({
    automation: {
      stop: async () => { autoDoneAt = Date.now(); rec.log("automation.stop", { done: true }); say("child-automation-stop-done"); },
    },
    closeServer: () => {
      rec.log("closeServer", { enter: true });
      say("child-close-server-enter");
      return server.close();
    },
    flushChannelState: async () => { rec.log("flush", { enter: true }); say("child-flush-enter"); },
    disposeGateway: async () => {
      disposeEnteredAt = Date.now();
      rec.log("disposeGateway", { enter: true });
      say("child-dispose-enter");
      await rt.dispose();
      say("child-rt-dispose-done");
    },
    telemetry: {
      snapshot: () => ({}),
      shutdown: async () => { rec.log("telemetry.shutdown", { ran: true }); say("child-telemetry-shutdown"); },
    },
    log: (m) => say(`child-bundle-log ${m}`),
  });
  const shutdownAndExit = createShutdownAndExit(() => shutdown.stop(), (code) => {
    rec.log("exit", { code });
    say(`child-exit code=${code}`);
    rec.dump(`p11_child_${state}_${port}.json`);
    process.exit(code);
  });
  process.on("SIGTERM", () => {
    sigtermAt = Date.now();
    say("child-sigterm");
    void shutdownAndExit(0);
  });
  process.on("SIGINT", () => {
    void shutdownAndExit(0);
  });
  process.on("unhandledRejection", () => undefined); // 探针自身不抬退出码
  process.on("uncaughtException", () => undefined);

  say(`child-listening ${server.url}`);
  // SIGTERM 后 400ms：closing 窗口内（closeServer 挂在 server.close）尝试 E5 新任务
  setTimeout(() => {
    const inWindow = sigtermAt > 0 && autoDoneAt > 0 && disposeEnteredAt === 0;
    rt.start({ command: `echo e5-window-${state}; sleep ${markerSleep}`, cwd: makeHome(RAW_DIR, `p11-${state}-cwd`) }).then(
      (t) => {
        say(`child-e5-admitted ${JSON.stringify({ taskId: t.taskId, pid: t.pid, status: t.status, msAfterSigterm: sigtermAt ? Date.now() - sigtermAt : -1, inClosingWindow: inWindow })}`);
        rec.log("e5-admitted", { pid: t.pid, msAfterSigterm: sigtermAt ? Date.now() - sigtermAt : -1, inClosingWindow: inWindow });
      },
      (e) => {
        say(`child-e5-rejected ${JSON.stringify({ error: e.message, msAfterSigterm: sigtermAt ? Date.now() - sigtermAt : -1, inClosingWindow: inWindow })}`);
        rec.log("e5-rejected", { error: e.message });
      },
    );
  }, 400);

  rec.log("ready", { state, port });
  await new Promise(() => undefined);
}

main().catch((e) => {
  say(`child-fatal ${e?.stack ?? e}`);
  process.exit(2);
});
