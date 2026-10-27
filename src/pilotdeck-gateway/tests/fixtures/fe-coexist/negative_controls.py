#!/usr/bin/env python3
"""#927 F3 coexistence negative controls: run every mutant, then summarize.

Each mutant is applied to the router/history boundary (ui-rs/shared/boundary.js)
or the Option A patch assembly (scripts/fe-parity/coexist-deploy.mjs),
parse-checked with ``node --check``, run against the offline ``node --test``
oracle that must turn red, and restored byte-for-byte. The runner never stops at
the first failure: it records KILLED / SURVIVED / INVALID with a reason for
every row and exits non-zero if any row is not KILLED. The named marker is the
dedicated assertion message that proves the intended check fired, not an
unrelated parse error.

No cargo, no listen, no browser: the oracle is the offline node --test pair
that #927 ships (coexist-boundary.test.mjs / coexist-patch.test.mjs).

Requires an already-committed tree (``mutating_sources`` refuses a dirty
baseline by design; both target files are #927 additions/edits, so this runner
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
BOUNDARY_ORACLE = ROOT / "scripts/fe-parity/coexist-boundary.test.mjs"
PATCH_ORACLE = ROOT / "scripts/fe-parity/coexist-patch.test.mjs"
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
        "patch-prefix-loose",
        DEPLOY,
        "req.path.startsWith('${NEW_SHELL_PREFIX}/')",
        "req.path.startsWith('${NEW_SHELL_PREFIX}')",
        PATCH_ORACLE,
        "prefix branch must guard exactly the migrated prefix",
    ),
    (
        "predicate-loose",
        DEPLOY,
        "  return newShell && (pathname === NEW_SHELL_PREFIX || pathname.startsWith(NEW_SHELL_PREFIX + '/'));",
        "  return newShell && (pathname === NEW_SHELL_PREFIX || pathname.startsWith(NEW_SHELL_PREFIX));",
        PATCH_ORACLE,
        "/settingsX stays old",
    ),
    (
        "switch-ignored",
        DEPLOY,
        "  return newShell && (pathname === NEW_SHELL_PREFIX || pathname.startsWith(NEW_SHELL_PREFIX + '/'));",
        "  return pathname === NEW_SHELL_PREFIX || pathname.startsWith(NEW_SHELL_PREFIX + '/');",
        PATCH_ORACLE,
        "switch off: byte-equivalent old behaviour",
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
