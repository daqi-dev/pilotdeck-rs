/**
 * P9：#2251 条目 3 —— background 任务派生子进程：dispose resolve 后按进程树/cmdline 复核零残留。
 * 两个对照任务（TERM 免疫，逼出 5s grace 的组杀路径）：
 *   A 同组后代：`(trap '' TERM; exec sleep 301) &` —— 与任务同 pgid；
 *     预期 grace 到期的 kill(-pid, SIGKILL) 组杀覆盖 ⇒ 零残留。
 *   B 跨组逃逸后代：`(trap '' TERM; python3 -c "…setsid…exec sleep 302") &` —— setsid 后
 *     脱离任务进程组 ⇒ kill(-pid) 覆盖不到 ⇒ 残留（A11 的杀灭断言边界）。
 * 复核口径（主口径=杀前全 pid 快照 + 杀后存活差集；辅助=ppid 链/cmdline 枚举，
 * 演示「任务 pid 锚定的 ppid 链在 reparent 后漏检」）。收尾显式清理逃逸者并复核零残留。
 */
import { writeSync, mkdirSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { Recorder, makeHome, sleep, makePaths, parseProbeArgs } from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p9-background-descendants"), "p9_background_descendants");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 400)}`);
  rec.log(k, d);
};

function psTable() {
  const out = execFileSync("ps", ["-A", "-o", "pid=,ppid=,pgid=,command="], { encoding: "utf8" });
  return out.split("\n").filter((l) => l.trim()).map((line) => {
    const m = line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/);
    return m ? { pid: Number(m[1]), ppid: Number(m[2]), pgid: Number(m[3]), command: m[4] } : null;
  }).filter(Boolean);
}

function findByCommand(marker) {
  return psTable().filter((p) => p.command.includes(marker));
}

function descendantsByPpid(rootPid) {
  const table = psTable();
  const byPpid = new Map();
  for (const p of table) {
    if (!byPpid.has(p.ppid)) byPpid.set(p.ppid, []);
    byPpid.get(p.ppid).push(p);
  }
  const out = [];
  const queue = [rootPid];
  const seen = new Set();
  while (queue.length) {
    const pid = queue.shift();
    for (const child of byPpid.get(pid) ?? []) {
      if (seen.has(child.pid)) continue;
      seen.add(child.pid);
      out.push(child);
      queue.push(child.pid);
    }
  }
  return out;
}

async function main() {
  const { BackgroundTaskRuntime } = await import(`${BASE}/dist/src/task/runtime/BackgroundTaskRuntime.js`);
  const spill = resolve(RAW_DIR, "p9-spill");
  mkdirSync(spill, { recursive: true });
  const rt = new BackgroundTaskRuntime({ diskSpillDir: spill, maxTasks: 8 });
  const cwd = makeHome(RAW_DIR, "p9-cwd");

  // 任务 A：同组、TERM 免疫后代（exec 后 SIG_IGN 保持）
  const taskA = await rt.start({
    command: "trap '' TERM; echo a-shell-start; (trap '' TERM; exec sleep 301) & wait",
    cwd,
  });
  // 任务 B：跨组逃逸后代（python3 setsid 后 exec；TERM 也免疫）
  const taskB = await rt.start({
    command: [
      "trap '' TERM; echo b-shell-start;",
      "(trap '' TERM; python3 -c \"import os,signal; signal.signal(signal.SIGTERM, signal.SIG_IGN); os.setsid(); os.execvp('sleep', ['sleep','302'])\") & wait",
    ].join(" "),
    cwd,
  });
  await sleep(500); // 后代派生完成
  const treeA = descendantsByPpid(taskA.pid);
  const treeB = descendantsByPpid(taskB.pid);
  const markerA = findByCommand("sleep 301");
  const markerB = findByCommand("sleep 302");
  log("S0/tasks", {
    a: { taskId: taskA.taskId, pid: taskA.pid },
    b: { taskId: taskB.taskId, pid: taskB.pid },
  });
  log("S0/pre-dispose-trees", {
    aTree: treeA.map((p) => ({ pid: p.pid, ppid: p.ppid, pgid: p.pgid, cmd: p.command.slice(0, 60) })),
    bTree: treeB.map((p) => ({ pid: p.pid, ppid: p.ppid, pgid: p.pgid, cmd: p.command.slice(0, 60) })),
    markerA: markerA.map((p) => ({ pid: p.pid, pgid: p.pgid, pgidEqualsTaskPidA: p.pgid === taskA.pid })),
    markerB: markerB.map((p) => ({ pid: p.pid, pgid: p.pgid, pgidEqualsTaskPidB: p.pgid === taskB.pid })),
    note: "pgid==任务 pid（detached spawn ⇒ 任务为组长）；B 的逃逸后代 pgid 已脱离",
  });

  // 杀前 owned pid 快照（主口径：这些 pid 在杀后是否仍存活）
  const ownedBy = (p) => p.pgid === taskA.pid || p.pgid === taskB.pid
    || p.command.includes("sleep 301") || p.command.includes("sleep 302");
  const preDisposeOwned = psTable().filter(ownedBy).map((p) => ({ pid: p.pid, ppid: p.ppid, pgid: p.pgid, cmd: p.command.slice(0, 70) }));
  log("pre-dispose/owned-snapshot", { owned: preDisposeOwned });

  const t0 = Date.now();
  await rt.dispose();
  log("dispose-resolved", { ok: true, ms: Date.now() - t0, note: "两个 TERM 免疫任务 ⇒ ~grace(5s) 后组 SIGKILL" });
  await sleep(300);

  // 杀后：主口径 = 杀前 owned pid 仍存活者；辅助 = cmdline/任务 pid 锚定枚举
  const afterTable = psTable();
  const aliveNow = new Set(afterTable.map((p) => p.pid));
  log("post-dispose/snapshot-residue", {
    note: "主口径：杀前 owned pid 中杀后仍存活者",
    ownedAlive: preDisposeOwned.filter((p) => aliveNow.has(p.pid)),
  });
  log("post-dispose/by-marker", {
    inGroupSleep301: findByCommand("sleep 301"),
    escapeeSleep302: findByCommand("sleep 302").map((p) => ({ pid: p.pid, ppid: p.ppid, pgid: p.pgid })),
    taskPidTreeA: descendantsByPpid(taskA.pid), // 预期空（链已断/已死）
    taskPidTreeB: descendantsByPpid(taskB.pid), // 演示：reparent 后以任务 pid 锚定的链枚举漏检
  });

  // 收尾：显式清理逃逸者，复核零残留
  for (const p of findByCommand("sleep 302")) {
    process.kill(p.pid, "SIGKILL");
  }
  await sleep(200);
  log("cleanup", {
    sleep301: findByCommand("sleep 301").length,
    sleep302: findByCommand("sleep 302").length,
  });

  rec.dump("p9_background_descendants.json");
  say("P9 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P9 FAILED: ${e?.stack ?? e}`);
  rec.dump("p9_background_descendants.json");
  process.exit(1);
});
