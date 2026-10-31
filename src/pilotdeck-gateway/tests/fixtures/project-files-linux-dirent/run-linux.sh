#!/usr/bin/env bash
# #133 / #117 Linux verification runner (host: cb8, Ubuntu 24.04 aarch64).
#
# Everything macOS cannot do for this lane happens here: the raw-byte dirent
# parity test, the node_io errno table check against fresh Node samples, and
# both negative controls. The positives and the compile check also run before
# the commit; the negative controls require the committed state
# (mutating_sources refuses untracked src/ files) and are therefore attempted
# but reported as SKIP when run pre-commit.
#
# Usage: run-linux.sh <pilotdeck-ts-baseline> [evidence-dir]
# Required caller environment: CARGO_TARGET_DIR (the caller-owned build tree).
set -u

baseline_ts=${1:?usage: run-linux.sh <pilotdeck-ts-baseline> [evidence-dir]}
repo=$(cd "$(dirname "$0")/../../../.." && pwd) # repository (worktree) root
evidence=${2:-$repo/_tmp/$(date +%Y%m%d-%H%M%S)-issue-133-cb8}
mkdir -p "$evidence"

# Locale contract first: LANG=C.UTF-8 must resolve en-US; a wrong locale is
# BLOCKED, not a parity failure. No LC_* override may survive.
export LANG=C.UTF-8
unset LC_ALL LC_MESSAGES LC_COLLATE LC_CTYPE LANGUAGE

# The pinned source-built Node (INTEGRATION.md); prefer it over any other.
pinned_node=$HOME/opt/node-v22.22.0-linux-arm64/bin
if [ -d "$pinned_node" ]; then export PATH="$pinned_node:$PATH"; fi

report() { printf '\n== %s\n' "$*"; }

report "environment contract"
if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  echo "BLOCKED: CARGO_TARGET_DIR must be supplied by the caller" >&2
  exit 2
fi
node --version
node -e 'console.log(JSON.stringify({node: process.version, uv: process.versions.uv, icu: process.versions.icu, platform: process.platform, locale: new Intl.Collator().resolvedOptions().locale}))' | tee "$evidence/node-versions.json"
node -e 'if (process.version !== "v22.22.0" || process.versions.icu !== "78.2") { console.error(`BLOCKED #133: requires Node v22.22.0 / ICU 78.2, found ${process.version} / ${process.versions.icu}`); process.exit(3); }'
node -e 'const l = new Intl.Collator().resolvedOptions().locale; if (l !== "en-US") { console.error(`BLOCKED #133: resolved locale ${l}; run with LANG=C.UTF-8 and no LC_* override`); process.exit(3); }'
git -C "$repo" rev-parse HEAD | tee "$evidence/rust-head.txt"

report "cargo test --no-run (whole tree compiles)"
bash "$repo/scripts/cargo-slots.sh"
( cd "$repo" && cargo test --manifest-path src/Cargo.toml --no-run ) 2>&1 | tee "$evidence/no-run.log"

report "#133 positive: raw-byte dirent parity vs the pinned TS baseline"
bash "$repo/scripts/cargo-slots.sh"
out133=$evidence/issue-133-positive
mkdir -p "$out133"
( cd "$repo" && PDRS_LINUX_DIRENT_CHECK=1 PDRS_BASELINE="$baseline_ts" PDRS_LINUX_DIRENT_OUT="$out133" \
    CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
    project_files::tests::linux_dirent::linux_dirent_matches_node_v22_on_raw_byte_names -- --exact ) \
  2>&1 | tee "$evidence/issue-133-positive.log"
rc133=$?
if [ "$rc133" -ne 0 ] || ! grep -q '^test project_files::tests::linux_dirent::linux_dirent_matches_node_v22_on_raw_byte_names \.\.\. ok$' "$evidence/issue-133-positive.log"; then
  echo "FAIL: #133 positive control did not pass" >&2
  failed=1
fi
python3 - "$out133/receipt.json" <<'PY'
import json, sys
receipt = json.load(open(sys.argv[1]))
assert receipt["cleanup"] is True, receipt
assert receipt["responses"] == 5, receipt
assert receipt["direntEntries"] == 21, receipt
assert receipt["wireItemCounts"] == [6, 5, 2, 4, 2], receipt
print("receipt:", json.dumps({k: receipt[k] for k in ("node", "icu", "baselineHead", "direntEntries", "wireItemCounts", "responses", "cleanup")}))
PY
[ $? -eq 0 ] || { echo "FAIL: #133 receipt incomplete or unexpected" >&2; failed=1; }

report "#117 positive: node_io errno table vs a fresh Linux Node sample"
bash "$repo/scripts/cargo-slots.sh"
( cd "$repo" && node --expose-internals src/pilotdeck-gateway/tests/fixtures/node-io/linux-errno-verify.mjs "$evidence/node-errno-sample.json" ) 2>&1 | tee "$evidence/issue-117-sampler.log"
( cd "$repo" && PDRS_NODE_IO_LINUX_JSON="$evidence/node-errno-sample.json" \
    CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib \
    node_io::tests::linux_node_errno_numbers_match_node_os_constants -- --exact ) \
  2>&1 | tee "$evidence/issue-117-positive.log"
rc117=$?
if [ "$rc117" -ne 0 ] || ! grep -q '^test node_io::tests::linux_node_errno_numbers_match_node_os_constants \.\.\. ok$' "$evidence/issue-117-positive.log"; then
  echo "FAIL: #117 positive control did not pass" >&2
  failed=1
fi

report "#117 generator --check (errors.json platform/errno fields differ by design)"
( cd "$repo" && node --expose-internals src/pilotdeck-gateway/tests/generate_node_io.mjs --check ) \
  > "$evidence/generator-check.log" 2>&1
gen_rc=$?
if grep -q "Generated output differs: .*node_io/generated.rs" "$evidence/generator-check.log"; then
  echo "FAIL: generated.rs differs on Linux — real table drift" >&2
  failed=1
elif [ "$gen_rc" -ne 0 ] && grep -q "Generated output differs: .*errors.json" "$evidence/generator-check.log"; then
  echo "expected: only errors.json differs (platform + darwin errno fields are platform data)"
else
  echo "unexpected generator --check result (rc=$gen_rc):" >&2
  cat "$evidence/generator-check.log" >&2
  [ "$gen_rc" -eq 0 ] || failed=1
fi

report "#133 negative controls (require the committed state; SKIP marks pre-commit)"
bash "$repo/scripts/cargo-slots.sh"
( cd "$repo" && CARGO_TARGET_DIR="$CARGO_TARGET_DIR" python3 -I src/pilotdeck-gateway/tests/fixtures/project-files-linux-dirent/negative_controls.py "$evidence/issue-133-negatives" ) \
  > "$evidence/issue-133-negatives.log" 2>&1
neg133=$?
if [ "$neg133" -eq 0 ]; then
  echo "KILLED all #133 mutants"
elif grep -q "untracked at HEAD" "$evidence/issue-133-negatives.log"; then
  echo "SKIP: uncommitted fixture files; rerun after the commit (coordinator)"
  neg133=0
else
  echo "FAIL: #133 negative controls (rc=$neg133)" >&2
  failed=1
fi

report "#117 negative controls (require the committed state; SKIP marks pre-commit)"
bash "$repo/scripts/cargo-slots.sh"
( cd "$repo" && CARGO_TARGET_DIR="$CARGO_TARGET_DIR" python3 -I src/pilotdeck-gateway/tests/fixtures/node-io/negative_controls.py "$evidence/issue-117-negatives" ) \
  > "$evidence/issue-117-negatives.log" 2>&1
neg117=$?
if [ "$neg117" -eq 0 ]; then
  echo "KILLED all #117 mutants"
elif grep -q "untracked at HEAD" "$evidence/issue-117-negatives.log"; then
  echo "SKIP: uncommitted fixture files; rerun after the commit (coordinator)"
  neg117=0
else
  echo "FAIL: #117 negative controls (rc=$neg117)" >&2
  failed=1
fi

report "summary"
python3 - "$evidence" <<'PY'
import json, pathlib, sys
evidence = pathlib.Path(sys.argv[1])
rows = []
for issue, name in (("#133", "issue-133-negatives"), ("#117", "issue-117-negatives")):
    summary = evidence / name / "summary.json"
    if summary.exists():
        rows.append((issue, [row["status"] for row in json.loads(summary.read_text())]))
print("negative controls:", rows or "not run (pre-commit)")
receipt = evidence / "issue-133-positive" / "receipt.json"
if receipt.exists():
    print("receipt:", json.dumps({k: json.loads(receipt.read_text())[k]
          for k in ("node", "icu", "baselineHead", "direntEntries", "wireItemCounts")}))
PY

if [ "${failed:-0}" -ne 0 ]; then
  echo "RESULT: FAILED" >&2
  exit 1
fi
echo "RESULT: positives verified; negatives per the logs above (SKIP = rerun post-commit)"
