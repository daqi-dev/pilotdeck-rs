#!/bin/sh
# Measure peak RSS for the #409 shapes on macOS. One mode per process.
# Usage: sh tests/fixtures/html-markdown/rss-probe.sh [mode ...]
# Modes: deep-full-2.5 deep-bounded-2.5 deep-bounded-10 wide-bounded-10
#        flat-bounded-10 (default: all five)
set -eu
cd "$(dirname "$0")/../../../.."
modes=${*:-"deep-full-2.5 deep-bounded-2.5 deep-bounded-10 wide-bounded-10 flat-bounded-10"}
json=$(mktemp)
out=$(mktemp)
trap 'rm -f "$json" "$out"' EXIT
cargo test -p pilotdeck-gateway --release --test html_budget_probe --no-run \
  --message-format=json > "$json"
bin=$(python3 - "$json" <<'PY'
import json, sys
for line in open(sys.argv[1]):
    item = json.loads(line)
    if (item.get("reason") == "compiler-artifact"
            and item.get("target", {}).get("name") == "html_budget_probe"
            and item.get("executable")):
        print(item["executable"])
        break
PY
)
[ -n "$bin" ] || { echo "probe binary not found" >&2; exit 1; }
for mode in $modes; do
  echo "== $mode"
  # Take the child status first: `/usr/bin/time` prints `real` and the RSS
  # line even when the probe panics, so piping straight into grep would mask a
  # failed run and could record RSS evidence from a crash (gate4 P2).
  if ! PDRS_PROBE=$mode /usr/bin/time -l "$bin" --ignored --exact probe --nocapture \
    > "$out" 2>&1; then
    cat "$out" >&2
    echo "probe failed: $mode" >&2
    exit 1
  fi
  grep -E "mode=|elapsed_ms|maximum resident set size|real" "$out"
done
