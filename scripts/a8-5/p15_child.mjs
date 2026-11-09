/**
 * P15 子进程（#2255 单飞格）：重复信号单飞 + 退出码抬升路径。
 * 真实 server + 真实 PilotDeckServerShutdownBundle + 真实 createShutdownAndExit，
 * 信号接线照抄 src/cli/pilotdeck.ts:266-280。automation 为快停 stub（计数标记——单飞的可观察面）；
 * closeServer 是真实 server.close()——driver 持有一条 WS 使其挂起（P2b 机制），构成「stop 挂起」窗口。
 * PROBE_REJECT=1：挂起窗口内（close-server-enter +800ms）child 自发 unhandledRejection →
 * requestedExitCode 抬到 1；PROBE_REJECT=0：对照（无 rejection，退出码 0）。由 p15_driver.mjs 驱动。
 */
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, Recorder, sleep, makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p15-single-flight"), "p15_child");
const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const port = Number(process.env.PROBE_PORT);
const reject = process.env.PROBE_REJECT === "1";

let automationStopCalls = 0;

async function main() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const { PilotDeckServerShutdownBundle } = await import(`${BASE}/dist/src/cli/PilotDeckServerShutdownBundle.js`);
  const { createShutdownAndExit } = await import(`${BASE}/dist/src/cli/shutdownCoordinator.js`);
  const gw = makeFakeGateway();
  const server = await startPilotDeckServer({
    gateway: gw, port, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, process.env.PROBE_HOME_NAME),
  });
  say(`child-listening ${server.url}`);

  const telemetry = {
    trackError: (e, meta) => {
      rec.log("telemetry-trackError", { message: String(e), meta });
      say("child-track-error");
    },
    snapshot: () => ({ probe: true }),
    shutdown: async () => { say("child-telemetry-shutdown"); },
  };
  const shutdown = new PilotDeckServerShutdownBundle({
    automation: {
      stop: async () => {
        automationStopCalls += 1;
        say(`child-automation-stop-enter n=${automationStopCalls}`);
        await sleep(30);
      },
    },
    closeServer: () => {
      say("child-close-server-enter");
      if (reject) {
        // 挂起窗口内（真实场景：closeServer 被 driver 的 WS 挂住）child 自发 late rejection
        setTimeout(() => {
          say("child-late-rejection-fired");
          Promise.reject(new Error("probe: late rejection during shutdown hang"));
        }, 800);
      }
      return server.close();
    },
    flushChannelState: async () => { say("child-flush-enter"); },
    disposeGateway: async () => { say("child-dispose-enter"); },
    telemetry,
    log: (m) => say(`child-bundle-log ${m}`),
  });
  const shutdownAndExit = createShutdownAndExit(() => shutdown.stop(), (code) => {
    rec.log("exit", { code, automationStopCalls, reject });
    say(`child-exit code=${code}`);
    rec.dump(`p15_child_${port}.json`);
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
  rec.log("ready", { port, reject });
  await new Promise(() => undefined);
}

main().catch((e) => {
  say(`child-fatal ${e?.stack ?? e}`);
  process.exit(2);
});
