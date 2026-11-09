/**
 * P3 驱动：spawn 真实子进程（p3_child.mjs），验证 drain 失败 / 挂起两种注入的 TS 表现。
 * fail：SIGTERM 后观察退出码、步骤推进（stdout 标记）、旧 owner 是否还在服务。
 * hang：SIGTERM 后旧 owner 是否仍可服务（health/WS），然后 SIGKILL 收尸。
 * 子进程经 env A85_OUT 继承同一证据目录。
 */
import { spawn } from "node:child_process";
import { writeSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { httpProbe, wsConnect, sleep, tcpRefused, Recorder, makePaths, parseProbeArgs, TOKEN } from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p3-drain-injection"), "p3_driver");
const CHILD = resolve(dirname(fileURLToPath(import.meta.url)), "p3_child.mjs");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d)}`);
  rec.log(k, d);
};

async function waitHealthy(port, child, timeoutMs = 15000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (child.exitCode !== null) return false;
    const r = await httpProbe(port, "/health", { timeoutMs: 800 });
    if (r.ok && r.status === 200) return true;
    await sleep(150);
  }
  return false;
}

async function runCase(mode, port, homeName) {
  const out = [];
  const child = spawn(process.execPath, [CHILD], {
    env: { ...process.env, A85_OUT: RAW_DIR, PROBE_MODE: mode, PROBE_PORT: String(port), PROBE_HOME_NAME: homeName },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (d) => out.push(`[out] ${d.toString().trim()}`));
  child.stderr.on("data", (d) => out.push(`[err] ${d.toString().trim()}`));
  const healthy = await waitHealthy(port, child);
  log(`${mode}/pre-sigterm-health`, { healthy });
  if (!healthy) {
    log(`${mode}/child-output-before-kill`, { out });
    child.kill("SIGKILL");
    return;
  }

  child.kill("SIGTERM");

  if (mode === "fail") {
    // 等子进程自然退出（上限 15s）
    const t0 = Date.now();
    while (child.exitCode === null && Date.now() - t0 < 15000) await sleep(100);
    await sleep(300);
    log(`${mode}/exit`, { exitCode: child.exitCode, signal: child.signalCode, ms: Date.now() - t0 });
    log(`${mode}/port-after-exit`, { refused: await tcpRefused(port) });
    log(`${mode}/child-stdout`, { lines: out.filter((l) => l.includes("child-")) });
    log(`${mode}/child-stderr-tail`, { lines: out.slice(-6) });
  } else {
    // hang：SIGTERM 2s 后旧 owner 是否仍可服务
    await sleep(2000);
    log(`${mode}/health-after-2s`, await httpProbe(port, "/health"));
    let wsResult = "not-tried";
    try {
      const ws = await wsConnect(BASE, port, TOKEN);
      const hello = await ws.hello();
      const resp = await ws.request("describe_server");
      wsResult = { helloType: hello?.type, respOk: resp?.ok };
      ws.close(1000, "probe-done");
    } catch (e) {
      wsResult = `error: ${e?.message}`;
    }
    log(`${mode}/ws-after-2s`, wsResult);
    log(`${mode}/child-stdout`, { lines: out.filter((l) => l.includes("child-")) });
    // 确认子进程确实还活着，然后 SIGKILL 收尸
    log(`${mode}/still-alive`, { exitCode: child.exitCode, signal: child.signalCode });
    child.kill("SIGKILL");
    const t1 = Date.now();
    while (child.exitCode === null && Date.now() - t1 < 5000) await sleep(100);
    log(`${mode}/after-kill`, { exitCode: child.exitCode, signal: child.signalCode });
  }
}

async function main() {
  await runCase("fail", 39881, "p3-fail");
  await runCase("hang", 39882, "p3-hang");
  rec.dump("p3_drain_injection.json");
  say("P3 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P3 FAILED: ${e?.stack ?? e}`);
  rec.dump("p3_drain_injection.json");
  process.exit(1);
});
