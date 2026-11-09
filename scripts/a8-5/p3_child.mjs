/**
 * P3 子进程：真实 startPilotDeckServer + PilotDeckServerShutdownBundle + 信号接线
 * （照抄 src/cli/pilotdeck.ts 的 uncaught/unhandled/SIGINT/SIGTERM 接法）。
 * PROBE_MODE=fail  → automation.stop() reject（drain 失败注入）
 * PROBE_MODE=hang  → automation.stop() 永不 resolve（drain 超时注入）
 * 由 p3_driver.mjs 驱动。
 */
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, Recorder, sleep, makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p3-drain-injection"), "p3_child");
const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const mode = process.env.PROBE_MODE;
const port = Number(process.env.PROBE_PORT);

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
    trackError: (e, meta) => rec.log("telemetry-trackError", { message: String(e), meta }),
    snapshot: () => ({ probe: true }),
    shutdown: async () => { rec.log("telemetry.shutdown", { ran: true }); say("child-telemetry-shutdown"); },
  };
  const shutdown = new PilotDeckServerShutdownBundle({
    automation: {
      stop: () => {
        rec.log("automation.stop", { mode });
        say(`child-automation-stop-enter mode=${mode}`);
        if (mode === "fail") {
          return sleep(30).then(() => Promise.reject(new Error("inject: automation drain failed")));
        }
        return new Promise(() => undefined); // hang
      },
    },
    closeServer: () => {
      rec.log("closeServer", { enter: true });
      say("child-close-server-enter");
      return server.close();
    },
    flushChannelState: async () => {
      rec.log("flush", { enter: true });
      say("child-flush-enter");
    },
    disposeGateway: async () => {
      rec.log("disposeGateway", { enter: true });
      say("child-dispose-enter");
      gw.dispose("local_gateway_disposed");
    },
    telemetry,
    log: (m) => say(`child-bundle-log ${m}`),
  });
  const shutdownAndExit = createShutdownAndExit(() => shutdown.stop(), (code) => {
    rec.log("exit", { code });
    say(`child-exit code=${code}`);
    rec.dump(`p3_${mode}_${port}.json`);
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
  rec.log("ready", { mode, port });
  await new Promise(() => undefined);
}

main().catch((e) => {
  say(`child-fatal ${e?.stack ?? e}`);
  process.exit(2);
});
