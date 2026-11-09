/**
 * P11 驱动：spawn p11_child.mjs，控制提交连接（WS）去留两态：
 *   disconnect —— WS 断开 ⇒ server.close settle ⇒ S3/S4 可达 ⇒ dispose 杀掉被接纳任务，
 *     子进程干净退出（码 0）+ 零残留。
 *   hold —— WS 保持 ⇒ close 永不 settle ⇒ S3/S4 不可达 ⇒ 任务不被 dispose 杀；
 *     SIGKILL 旧进程后 detached 任务成孤儿（存活、ppid→1、自有 pgid）。
 * 收尾显式清理孤儿并复核零残留。
 */
import { spawn } from "node:child_process";
import { writeSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { httpProbe, wsConnect, sleep, Recorder, makePaths, parseProbeArgs, TOKEN } from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p11-e5-admission-window"), "p11_driver");
const CHILD = resolve(dirname(fileURLToPath(import.meta.url)), "p11_child.mjs");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 320)}`);
  rec.log(k, d);
};

function psFind(marker) {
  const out = execFileSync("ps", ["-A", "-o", "pid=,ppid=,pgid=,command="], { encoding: "utf8" });
  return out.split("\n").filter((l) => l.includes(marker)).map((l) => {
    const m = l.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/);
    return m ? { pid: Number(m[1]), ppid: Number(m[2]), pgid: Number(m[3]), cmd: m[4].slice(0, 70) } : null;
  }).filter(Boolean);
}

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

async function runState(state, port) {
  const out = [];
  const child = spawn(process.execPath, [CHILD], {
    env: { ...process.env, A85_OUT: RAW_DIR, PROBE_STATE: state, PROBE_PORT: String(port) },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (d) => out.push(...d.toString().split("\n").filter((l) => l.trim())));
  child.stderr.on("data", (d) => out.push(`[err] ${d.toString().trim()}`));
  const healthy = await waitHealthy(port, child);
  log(`${state}/healthy`, { healthy });
  if (!healthy) {
    log(`${state}/child-output`, { out });
    child.kill("SIGKILL");
    return;
  }

  const ws = await wsConnect(BASE, port, TOKEN);
  await ws.hello();
  log(`${state}/ws-hello`, { ok: true });

  child.kill("SIGTERM");
  await sleep(1000); // closing 窗口内子进程已尝试 E5 提交（+400ms）
  log(`${state}/child-markers`, { lines: out.filter((l) => l.startsWith("child-")) });

  if (state === "disconnect") {
    ws.close(1000, "probe-release");
    const t0 = Date.now();
    while (child.exitCode === null && Date.now() - t0 < 15000) await sleep(100);
    log(`${state}/exit`, { exitCode: child.exitCode, signal: child.signalCode, ms: Date.now() - t0 });
    await sleep(300);
    log(`${state}/residue`, { sleep61: psFind("sleep 61") });
    log(`${state}/all-markers`, { lines: out.filter((l) => l.startsWith("child-")) });
  } else {
    await sleep(1500);
    log(`${state}/still-alive`, { exitCode: child.exitCode, note: "close 挂在 WS ⇒ S3/S4 不可达 ⇒ 任务不被 dispose 杀" });
    child.kill("SIGKILL");
    const t1 = Date.now();
    while (child.exitCode === null && child.signalCode === null && Date.now() - t1 < 5000) await sleep(100);
    log(`${state}/after-sigkill`, { exitCode: child.exitCode, signal: child.signalCode });
    await sleep(300);
    const orphans = psFind("sleep 62");
    log(`${state}/orphan-check`, { orphans, note: "detached 任务子进程不随父进程退出 ⇒ 孤儿" });
    for (const p of orphans) process.kill(p.pid, "SIGKILL");
    await sleep(200);
    log(`${state}/cleanup`, { sleep62: psFind("sleep 62").length });
  }
}

async function main() {
  await runState("disconnect", 39889);
  await runState("hold", 39890);
  rec.dump("p11_e5_admission_window.json");
  say("P11 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P11 FAILED: ${e?.stack ?? e}`);
  rec.dump("p11_e5_admission_window.json");
  process.exit(1);
});
