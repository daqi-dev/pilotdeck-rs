/**
 * P15 驱动（#2255 单飞格）：spawn p15_child.mjs，驱动「重复信号单飞 + 退出码抬升」两态。
 * 场景 A（PROBE_REJECT=1）：持 WS → SIGTERM#1（stop 进入、closeServer 挂起）→ SIGTERM#2 + SIGINT
 * （单飞验证：automation-stop 只调一次）→ 挂起窗口内 late unhandledRejection → 断开 WS 放行 →
 * 退出码抬到 1（实测）。场景 B（对照，PROBE_REJECT=0）：同序无 rejection → 退出码 0。
 */
import { spawn } from "node:child_process";
import { writeSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import {
  httpProbe, wsConnect, tcpRefused, sleep, Recorder, makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p15-single-flight"), "p15_driver");
const CHILD = resolve(dirname(fileURLToPath(import.meta.url)), "p15_child.mjs");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 400)}`);
  rec.log(k, d);
};

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

/** 轮询等待 stdout 出现指定标记（有界）。 */
async function waitMarker(chunks, marker, timeoutMs = 10000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (chunks.join("").includes(marker)) return true;
    await sleep(50);
  }
  return false;
}

function countMarker(chunks, marker) {
  return chunks.join("").split(marker).length - 1;
}

async function runCase(name, reject, port, homeName) {
  const out = [];
  const child = spawn(process.execPath, [CHILD], {
    env: { ...process.env, A85_OUT: RAW_DIR, PROBE_PORT: String(port), PROBE_HOME_NAME: homeName, PROBE_REJECT: reject ? "1" : "0" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (d) => out.push(d.toString()));
  child.stderr.on("data", (d) => out.push(`[err] ${d.toString()}`));
  const healthy = await waitHealthy(port, child);
  log(`${name}/pre-sigterm-health`, { healthy });
  if (!healthy) {
    log(`${name}/child-output-before-kill`, { out: out.join("").slice(0, 2000) });
    child.kill("SIGKILL");
    return;
  }

  // 持一条 WS（closeServer 将挂在其上——P2b 机制）
  const ws = await wsConnect(BASE, port, TOKEN);
  const hello = await ws.hello();
  log(`${name}/hold-ws-hello`, { helloType: hello?.type });

  child.kill("SIGTERM"); // 信号 #1
  const closeEntered = await waitMarker(out, "child-close-server-enter");
  log(`${name}/after-sigterm1`, { closeEntered });

  // 重复信号：SIGTERM#2 + SIGINT——单飞验证（stop 不重入）
  child.kill("SIGTERM");
  child.kill("SIGINT");
  await sleep(500);
  log(`${name}/after-repeat-signals`, {
    stillAlive: child.exitCode === null && child.signalCode === null,
    automationStopCalls: countMarker(out, "child-automation-stop-enter"),
  });

  if (reject) {
    const fired = await waitMarker(out, "child-late-rejection-fired");
    log(`${name}/late-rejection`, { fired });
  }

  // 放行：断开持有 WS → closeServer settle → 五步走完 → exit
  ws.close(1000, "probe-release");
  const t0 = Date.now();
  while (child.exitCode === null && child.signalCode === null && Date.now() - t0 < 10000) {
    await sleep(50);
  }
  log(`${name}/exit`, { exitCode: child.exitCode, signal: child.signalCode, ms: Date.now() - t0 });
  log(`${name}/port-after-exit`, { refused: await tcpRefused(port) });
  log(`${name}/marker-counts`, {
    automationStopEnter: countMarker(out, "child-automation-stop-enter"),
    closeServerEnter: countMarker(out, "child-close-server-enter"),
    flushEnter: countMarker(out, "child-flush-enter"),
    disposeEnter: countMarker(out, "child-dispose-enter"),
    telemetryShutdown: countMarker(out, "child-telemetry-shutdown"),
    trackError: countMarker(out, "child-track-error"),
  });
}

async function main() {
  await runCase("A-elevate", true, 39894, "p15-a");
  await runCase("B-control", false, 39895, "p15-b");
  rec.dump("p15_single_flight.json");
  say("P15 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P15 FAILED: ${e?.stack ?? e}`);
  rec.dump("p15_single_flight.json");
  process.exit(1);
});
