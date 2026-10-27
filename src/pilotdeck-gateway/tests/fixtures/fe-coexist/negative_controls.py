#!/usr/bin/env python3
"""#927 F3 coexistence negative controls: run every mutant, then summarize.

Each mutant is applied to the router/history boundary (ui-rs/shared/boundary.js),
the Option A patch assembly (scripts/fe-parity/coexist-deploy.mjs), the coexist
accounting (scripts/fe-parity/coexist-runner.mjs), the shared-key snapshot
(scripts/fe-parity/coexist-shared-keys.mjs) or the browser spec itself
(scripts/fe-parity/coexist-tests/coexist.spec.mjs), parse-checked with
``node --check``, run against the offline ``node --test`` oracle that must turn
red, and restored byte-for-byte. The runner never stops at the first failure:
it records KILLED / SURVIVED / INVALID with a reason for every row and exits
non-zero if any row is not KILLED. The named marker is the dedicated assertion
message that proves the intended check fired, not an unrelated parse error.

fix-r1 note: the old ``predicate-loose`` / ``switch-ignored`` rows mutated the
``isNewShellRequest`` mirror, which was NOT on the real deployment path (the
patched server runs the ``optionAPatch.prefixBranch`` template). The mirror and
its test were deleted; the behavioural oracle now executes the real template
(``executeOptionAPatchBranch``), and the switch/boundary mutants target the
template itself (``template-switch-ignored`` / ``template-new-dead`` /
``patch-prefix-loose``).

No cargo, no listen, no browser: the oracles are the offline node --test set
#927 ships (coexist-boundary / coexist-patch / coexist-accounting /
coexist-self-check .test.mjs).

Requires an already-committed tree (``mutating_sources`` refuses a dirty
baseline by design; the target files are #927 additions/edits, so this runner
is handed to the coordinator to execute on the host after commit, see
RESULT.md). Run from the worktree root:

    python3 src/pilotdeck-gateway/tests/fixtures/fe-coexist/negative_controls.py out/
"""
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned  # noqa: E402

ROOT = Path(__file__).resolve().parents[5]
BOUNDARY = ROOT / "ui-rs/shared/boundary.js"
DEPLOY = ROOT / "scripts/fe-parity/coexist-deploy.mjs"
RUNNER = ROOT / "scripts/fe-parity/coexist-runner.mjs"
SHARED_KEYS = ROOT / "scripts/fe-parity/coexist-shared-keys.mjs"
SPEC = ROOT / "scripts/fe-parity/coexist-tests/coexist.spec.mjs"
BOUNDARY_ORACLE = ROOT / "scripts/fe-parity/coexist-boundary.test.mjs"
PATCH_ORACLE = ROOT / "scripts/fe-parity/coexist-patch.test.mjs"
ACCOUNTING_ORACLE = ROOT / "scripts/fe-parity/coexist-accounting.test.mjs"
SELFCHECK_ORACLE = ROOT / "scripts/fe-parity/coexist-self-check.test.mjs"
NODE = "node"

# (name, path, before, after, oracle, marker)
mutations = [
    (
        "cross-dead",
        BOUNDARY,
        "    return target.origin === win.location.origin "
        "&& isNew(target.pathname) !== loadedShell ? target : null;",
        "    return null;",
        BOUNDARY_ORACLE,
        "cross pushState must call hardNavigate(assign)",
    ),
    (
        "cross-origin-dropped",
        BOUNDARY,
        "    return target.origin === win.location.origin "
        "&& isNew(target.pathname) !== loadedShell ? target : null;",
        "    return isNew(target.pathname) !== loadedShell ? target : null;",
        BOUNDARY_ORACLE,
        "different origin is router business, not a shell crossing",
    ),
    (
        "prefix-loose",
        BOUNDARY,
        "  const isNew = pathname => prefixes.some(prefix => pathname === prefix || pathname.startsWith(prefix + '/'));",
        "  const isNew = pathname => prefixes.some(prefix => pathname.startsWith(prefix));",
        BOUNDARY_ORACLE,
        "must stay in the old shell",
    ),
    (
        "push-passthrough",
        BOUNDARY,
        "    if (target) return hardNavigate(target.href, false);",
        "    if (target) return originalPush.call(this, state, title, url);",
        BOUNDARY_ORACLE,
        "cross pushState must call hardNavigate(assign)",
    ),
    (
        "pop-dead",
        BOUNDARY,
        "    if (target) hardNavigate(target.href, true);",
        "    if (false) hardNavigate(target.href, true);",
        BOUNDARY_ORACLE,
        "traversal must keep the history entry (replace, not assign)",
    ),
    (
        "dispose-noop",
        BOUNDARY,
        "    history.pushState = originalPush;",
        "    history.pushState = history.pushState;",
        BOUNDARY_ORACLE,
        "disposed boundary must not intercept",
    ),
    (
        "replace-flag-flipped",
        BOUNDARY,
        "    if (target) return hardNavigate(target.href, true);",
        "    if (target) return hardNavigate(target.href, false);",
        BOUNDARY_ORACLE,
        "cross replaceState 必须 hardNavigate(replace=true)（不产生重复历史项）",
    ),
    (
        "replace-passthrough",
        BOUNDARY,
        "    if (target) return hardNavigate(target.href, true);",
        "    if (target) return originalReplace.call(this, state, title, url);",
        BOUNDARY_ORACLE,
        "cross replaceState 必须 hardNavigate(replace=true)（不产生重复历史项）",
    ),
    (
        "default-prefix-dropped",
        BOUNDARY,
        "prefixes = ['/probe'])",
        "prefixes = [])",
        BOUNDARY_ORACLE,
        "默认 prefixes 必须保留 /probe 边界（exp007 语义）",
    ),
    (
        "exact-prefix-dropped",
        BOUNDARY,
        "pathname === prefix || pathname.startsWith(prefix + '/')",
        "pathname.startsWith(prefix + '/')",
        BOUNDARY_ORACLE,
        "exact prefix must cross",
    ),
    (
        "patch-prefix-loose",
        DEPLOY,
        "req.path.startsWith('${NEW_SHELL_PREFIX}/')",
        "req.path.startsWith('${NEW_SHELL_PREFIX}')",
        PATCH_ORACLE,
        "prefix branch must guard exactly the migrated prefix",
    ),
    (
        "template-switch-ignored",
        DEPLOY,
        "if (process.env.PILOTDECK_NEW_SHELL === '1' && (req.path === '${NEW_SHELL_PREFIX}' "
        "|| req.path.startsWith('${NEW_SHELL_PREFIX}/'))) {",
        "if ((req.path === '${NEW_SHELL_PREFIX}' || req.path.startsWith('${NEW_SHELL_PREFIX}/'))) {",
        PATCH_ORACLE,
        "开关 unset：/settings 必须回落旧 bundle（回退开关，字节等价旧行为）",
    ),
    (
        "template-new-dead",
        DEPLOY,
        "        return res.sendFile(path.join(__dirname, '../new-shell/index.html'));",
        "        return;",
        PATCH_ORACLE,
        "开关开：/settings 必须实际服务新壳入口",
    ),
    (
        "p0-error-folding-dropped",
        RUNNER,
        "report?.errors || []",
        "[]",
        ACCOUNTING_ORACLE,
        "P0：报告含执行错误时总状态不得 PASS（14 例全绿也不行）",
    ),
    (
        "p0-status-ignores-errors",
        RUNNER,
        "  const status = errors.length ? 'ERROR' : rows.length && rows.every(row => row.status === 'PASS') ? 'PASS' : 'FAIL';",
        "  const status = rows.length && rows.every(row => row.status === 'PASS') ? 'PASS' : 'FAIL';",
        ACCOUNTING_ORACLE,
        "P0：报告含执行错误时总状态不得 PASS（14 例全绿也不行）",
    ),
    (
        "catalog-drift-masked",
        RUNNER,
        "    'C3 共享键跨跳转不变',",
        "    'C3 共享键跨跳转不变（漂移注入）',",
        SELFCHECK_ORACLE,
        "coexist valid 注册与 COEXIST_CASES 必须逐条一致（离线抓漂移，不等到运行时 registration drift）",
    ),
    (
        "storage-local-only",
        SHARED_KEYS,
        "  for (const storage of [localStorage, sessionStorage]) {",
        "  for (const storage of [localStorage]) {",
        SELFCHECK_ORACLE,
        "C3 快照必须覆盖 sessionStorage：活动项目键在 sessionStorage（AppShellV2.tsx:163），只读 localStorage 会漏",
    ),
    (
        "active-project-unchecked",
        SPEC,
        "sessionStorage.getItem('pilotdeck-active-project')",
        "sessionStorage.getItem('auth-token')",
        SELFCHECK_ORACLE,
        "C3 必须先断言活动项目键在 sessionStorage 实际存在（评审 P1-2：漏验共享契约）",
    ),
    (
        "s1-marker-reverted",
        SPEC,
        "expect(migratedBody, '/settings 必须回新 shell 入口（新壳专属 probe 入口模块）').toContain(NEW_SHELL_ENTRY_MARKER);",
        "expect(migratedBody, '/settings 必须回新 shell 入口（新壳专属 probe 入口模块）').toContain('/probe/leptos/boundary-boot.js');",
        SELFCHECK_ORACLE,
        "S1 必须用新壳专属入口判据（/probe/leptos/probe.js；boundary-boot 双壳共用不能作判据）",
    ),
]


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = set(sys.argv[2:])
    unknown = selected - {row[0] for row in mutations}
    if unknown:
        print(f"unknown mutation names: {sorted(unknown)}", file=sys.stderr)
        return 2
    rows = [row for row in mutations if not selected or row[0] in selected]
    paths = sorted({row[1] for row in rows})
    results = []

    def record(name, status, reason=""):
        results.append({"name": name, "status": status, "reason": reason})
        print(f"{name}: {status} {reason}", flush=True)

    with mutating_sources(ROOT, paths) as snapshots:
        for name, path, before, after, oracle, marker in rows:
            original = snapshots[path]
            before_bytes = hashlib.sha256(original).hexdigest()
            try:
                content = original.decode("utf-8")
                count = content.count(before)
                if count != 1:
                    record(
                        name,
                        "INVALID",
                        f"stale anchor: expected 1 occurrence, found {count}",
                    )
                    continue
                path.write_text(content.replace(before, after, 1), encoding="utf-8")
                checked = run_owned([NODE, "--check", str(path)], cwd=ROOT, timeout=60)
                log = checked.stdout + checked.stderr
                (out / f"{name}.compile.log").write_text(log, encoding="utf-8")
                if checked.returncode != 0:
                    record(name, "INVALID", "mutant does not parse (node --check)")
                    continue
                result = run_owned(
                    [NODE, "--test", str(oracle)], cwd=ROOT, timeout=600
                )
                log = result.stdout + result.stderr
                (out / f"{name}.log").write_text(log, encoding="utf-8")
                if result.returncode == 0:
                    record(name, "SURVIVED", "offline oracle stayed green")
                elif marker in log:
                    record(name, "KILLED", "named assertion failed")
                else:
                    record(name, "INVALID", "oracle failed outside the named assertion")
            except Exception as error:
                # One mutant's bookkeeping/IO error must not hide every later
                # row; restore still runs in `finally`. SystemExit from a
                # restore mismatch deliberately escapes this handler.
                record(
                    name,
                    "INVALID",
                    f"runner error: {type(error).__name__}: {error}",
                )
            finally:
                path.write_bytes(original)
                if hashlib.sha256(path.read_bytes()).hexdigest() != before_bytes:
                    record(name, "INVALID", "restore mismatch")
                    raise SystemExit(2)

    (out / "summary.json").write_text(
        json.dumps(results, ensure_ascii=True, indent=2) + "\n", encoding="utf-8"
    )
    killed = sum(row["status"] == "KILLED" for row in results)
    print(f"{killed}/{len(results)} KILLED; see {out / 'summary.json'}", flush=True)
    return 0 if killed == len(results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
