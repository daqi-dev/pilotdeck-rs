#!/usr/bin/env bash
# #283 WorkspaceSnapshot non-utf8 verification runner (host: cb8, Ubuntu
# 24.04 aarch64, ext4).
#
# The non-utf8 gold scenario needs a byte-transparent filesystem to generate
# and replay, so everything macOS cannot do for this lane happens here: the
# dedicated Linux gate, the 28-scenario main replay, the regenerates-
# identically guard (Linux-only since #283), and the negative controls. The
# positives and the compile check also run before the commit; the negative
# controls require the committed state (mutating_sources refuses any tree
# that is not HEAD — tracked diffs or untracked build inputs) and are
# therefore attempted but reported as NOT_RUN when the running runner itself
# is not the committed one (pre-commit); post-commit refusals are FAILED.
#
# Usage: run-linux.sh <pilotdeck-ts-baseline> [evidence-dir]
# Required caller environment: CARGO_TARGET_DIR (the caller-owned build tree).
# Optional: PDRS_NODE (pinned Node binary; default
# ~/opt/node-v22.22.0-icu78-linux-arm64/bin/node). PATH is never trusted for
# node: the pinned binary's directory is prepended for this script and every
# child (the regen guard and the negative controls spawn `node`).
#
# Exit codes: 0 = positives verified and all negatives KILLED; 1 = FAILED;
# 2 = BLOCKED (caller environment); 3 = BLOCKED (pinned Node/locale gate);
# 77 = positives verified but negatives NOT_RUN (pre-commit only; rerun
# post-commit).
set -u
set -o pipefail

baseline_ts=${1:?usage: run-linux.sh <pilotdeck-ts-baseline> [evidence-dir]}
repo=$(cd "$(dirname "$0")/../../../../.." && pwd) # repository (worktree) root (five levels: fixtures dir → src/pilotdeck-gateway/tests/fixtures/<dir>)
# Guard the five-level count itself: a wrong repo root produces src/src/...
# manifest paths that only fail later, far from the cause (r2 review P1-2).
repo_toplevel=$(git -C "$repo" rev-parse --show-toplevel 2>/dev/null) || {
  echo "BLOCKED: $repo is not a git worktree" >&2
  exit 2
}
if [ "$repo_toplevel" != "$repo" ]; then
  echo "BLOCKED: $repo is not the worktree root (git says $repo_toplevel)" >&2
  exit 2
fi
evidence=${2:-$repo/_tmp/$(date +%Y%m%d-%H%M%S)-issue-283-cb8}
mkdir -p "$evidence"

# Locale contract first (the recorded provider-order case is locale-sensitive;
# the gold was generated under both C.UTF-8 and en_US.UTF-8 byte-identically,
# but a third locale is still BLOCKED, not a parity failure). No LC_* override
# may survive.
export LANG=C.UTF-8
unset LC_ALL LC_MESSAGES LC_COLLATE LC_CTYPE LANGUAGE
# Same normalization for TMPDIR: the dedicated gate's scratch hardcodes /tmp
# while Node's os.tmpdir()/Rust's temp_dir() follow $TMPDIR, so a caller
# override would make the byte-transparency probe below vouch for a
# filesystem the replay never writes its raw names to (r1 review P2).
unset TMPDIR

# The pinned source-built baseline Node (ICU 78.2 + libuv 1.52.1; the older
# ~/opt/node-v22.22.0-linux-arm64 is ICU 77.1 / libuv 1.5x and must not be
# picked up). Explicit path, never a PATH lookup.
node_bin=${PDRS_NODE:-$HOME/opt/node-v22.22.0-icu78-linux-arm64/bin/node}
if [ ! -x "$node_bin" ]; then
  echo "BLOCKED: pinned Node not executable at $node_bin; build it or set PDRS_NODE" >&2
  exit 2
fi
export PDRS_NODE="$node_bin"
export PATH="$(dirname "$node_bin"):$PATH"

report() { printf '\n== %s\n' "$*"; }

# scripts/cargo-slots.sh exits 1 while full (its header says callers wait and
# retry, like the Python side's wait_for_slot: 30s interval, 15-minute cap)
# and any other non-zero status means the check itself is broken — a missing
# script must never read as "slot free". Poll, then BLOCKED.
wait_for_cargo_slot() {
  local slots="$repo/scripts/cargo-slots.sh"
  local waited=0
  if [ ! -f "$slots" ]; then
    echo "BLOCKED: $slots missing; refusing to start cargo without the slot check" >&2
    exit 2
  fi
  while true; do
    bash "$slots"
    local rc=$?
    if [ "$rc" -eq 0 ]; then
      return 0
    fi
    if [ "$rc" -ne 1 ] || [ "$waited" -ge 900 ]; then
      echo "BLOCKED: no cargo slot within ${waited}s (cargo-slots.sh rc=$rc)" >&2
      exit 2
    fi
    sleep 30
    waited=$((waited + 30))
  done
}

report "environment contract"
if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  echo "BLOCKED: CARGO_TARGET_DIR must be supplied by the caller" >&2
  exit 2
fi
# The regen guard needs the baseline; the gated tests never spawn node but
# take the same contract for one place of truth.
export PDRS_BASELINE="$baseline_ts"
"$node_bin" --version
"$node_bin" -e 'console.log(JSON.stringify({node: process.version, uv: process.versions.uv, icu: process.versions.icu, platform: process.platform, locale: new Intl.Collator().resolvedOptions().locale}))' | tee "$evidence/node-versions.json"
"$node_bin" -e 'if (process.version !== "v22.22.0" || process.versions.icu !== "78.2" || process.versions.uv !== "1.52.1") { console.error(`BLOCKED #283: requires Node v22.22.0 / ICU 78.2 / libuv 1.52.1, found ${process.version} / ${process.versions.icu} / ${process.versions.uv}`); process.exit(3); }' || exit 3
# The recorded provider-order case is locale-sensitive: C.UTF-8 and
# en_US.UTF-8 record byte-identically (see the fixture README), but a third
# locale is BLOCKED, not a parity failure — assert it instead of only
# promising it in the header comment (r1 review P2, same gate as
# ../project-files-linux-dirent/run-linux.sh).
"$node_bin" -e 'const l = new Intl.Collator().resolvedOptions().locale; if (l !== "en-US") { console.error(`BLOCKED #283: resolved locale ${l}; run with LANG=C.UTF-8 and no LC_* override`); process.exit(3); }' || exit 3
# The byte-transparency of this host's filesystem is itself part of the
# contract: if it rejects non-UTF-8 names the whole lane is meaningless.
"$node_bin" -e 'const fs=require("fs"),os=require("os"),p=require("path"); const d=fs.mkdtempSync(p.join(os.tmpdir(),"pdrs-283-")); try { fs.writeFileSync(p.join(d, Buffer.concat([Buffer.from("probe-"),Buffer.from([0xff])])), ""); console.log("filesystem: accepts raw non-UTF-8 names"); } catch(e) { console.error(`BLOCKED #283: ${os.tmpdir()} rejects non-UTF-8 file names (${e.code}); this lane needs a byte-transparent filesystem`); process.exit(2); } finally { fs.rmSync(d,{recursive:true,force:true}); }' || exit 2
git -C "$repo" rev-parse HEAD | tee "$evidence/rust-head.txt"

report "cargo test --no-run (whole tree compiles)"
wait_for_cargo_slot
( cd "$repo" && cargo test --manifest-path src/Cargo.toml --no-run ) 2>&1 | tee "$evidence/no-run.log"
if [ $? -ne 0 ]; then
  echo "FAIL: cargo test --no-run did not compile (see no-run.log)" >&2
  failed=1
fi

report "#283 positive: the dedicated Linux gate"
wait_for_cargo_slot
( cd "$repo" && CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
    --test workspace_snapshot non_utf8_capture_fails_with_lossy_lstat_enoent_on_linux -- --exact ) \
  2>&1 | tee "$evidence/issue-283-gate.log"
rc_gate=$?
if [ "$rc_gate" -ne 0 ] || ! grep -q '^test non_utf8_capture_fails_with_lossy_lstat_enoent_on_linux \.\.\. ok$' "$evidence/issue-283-gate.log"; then
  echo "FAIL: #283 dedicated Linux gate did not pass" >&2
  failed=1
fi

report "#283 positive: the full 28-scenario replay (gold included)"
wait_for_cargo_slot
( cd "$repo" && CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
    --test workspace_snapshot ) \
  2>&1 | tee "$evidence/issue-283-replay.log"
rc_replay=$?
if [ "$rc_replay" -ne 0 ] || ! grep -q '^test scenarios_match_ts_outcomes_and_trees \.\.\. ok$' "$evidence/issue-283-replay.log"; then
  echo "FAIL: #283 main replay (including the non-utf8 scenario) did not pass" >&2
  failed=1
fi

report "#283 regenerates-identically guard (Linux-only; pinned Node/ICU)"
wait_for_cargo_slot
( cd "$repo" && PDRS_BASELINE="$baseline_ts" CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
    --test generator_provenance workspace_snapshot_regenerates_identically -- --exact --ignored ) \
  2>&1 | tee "$evidence/issue-283-regen.log"
rc_regen=$?
if [ "$rc_regen" -ne 0 ] || ! grep -q '^test workspace_snapshot_regenerates_identically \.\.\. ok$' "$evidence/issue-283-regen.log"; then
  echo "FAIL: #283 regen guard did not pass on this byte-transparent host (see issue-283-regen.log)" >&2
  failed=1
fi

report "#283 negative controls (require the committed state; NOT_RUN marks pre-commit)"
wait_for_cargo_slot
( cd "$repo" && CARGO_TARGET_DIR="$CARGO_TARGET_DIR" python3 -I src/pilotdeck-gateway/tests/fixtures/workspace-snapshot-nonutf8/negative_controls.py "$evidence/issue-283-negatives" ) \
  > "$evidence/issue-283-negatives.log" 2>&1
neg283=$?
negatives_not_run=0
if [ "$neg283" -eq 0 ]; then
  echo "KILLED all #283 mutants"
elif grep -qE '\((untracked build input at|differs from) HEAD\)' "$evidence/issue-283-negatives.log" \
  && ! git -C "$repo" show "HEAD:src/pilotdeck-gateway/tests/fixtures/workspace-snapshot-nonutf8/run-linux.sh" 2>/dev/null | cmp -s - "$0"; then
  # Pre-commit only, and provably so: mutating_sources refused the tree AND
  # the running runner's own bytes are not what HEAD carries, so an
  # uncommitted patch is executing (this round's edit, or an earlier round
  # that added this file outright — `untracked build input at` is
  # negative_process's actual wording, which the old `untracked at` pattern
  # never matched). Once this runner IS the committed one, a refusal means a
  # dirty tree — residue from a killed mutant or a hand edit — and is FAILED,
  # not a benign rerun-me-later (r1 review P2).
  echo "NOT_RUN: uncommitted/untracked product sources (mutating_sources refuses them); rerun after the commit (coordinator)"
  negatives_not_run=1
else
  echo "FAIL: #283 negative controls (rc=$neg283; see issue-283-negatives.log)" >&2
  failed=1
fi

report "summary"
python3 - "$evidence" <<'PY'
import json, pathlib, sys
evidence = pathlib.Path(sys.argv[1])
summary = evidence / "issue-283-negatives" / "summary.json"
if summary.exists():
    print("negative controls:", [(row["name"], row["status"]) for row in json.loads(summary.read_text())])
else:
    print("negative controls: not run (pre-commit)")
PY

if [ "${failed:-0}" -ne 0 ]; then
  echo "RESULT: FAILED" >&2
  exit 1
fi
if [ "${negatives_not_run:-0}" -ne 0 ]; then
  echo "RESULT: positives verified; negatives NOT_RUN (pre-commit; rerun post-commit; exit 77)"
  exit 77
fi
echo "RESULT: positives verified; negatives KILLED"
