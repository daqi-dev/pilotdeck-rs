#!/bin/sh
# Measure peak RSS for the #409 shapes on macOS. One mode per process.
# Usage (from gateway crate): CARGO_TARGET_DIR=/absolute/per-tree/target \
#   sh tests/fixtures/html-markdown/rss-probe.sh [mode ...]
# Workspace inputs must equal committed HEAD; the caller supplies the target.
# Modes: deep-full-2.5 deep-bounded-2.5 deep-bounded-10 wide-bounded-10
#        flat-bounded-10 (default: all five)
set -eu
cd "$(dirname "$0")/../../../.."
modes=${*:-"deep-full-2.5 deep-bounded-2.5 deep-bounded-10 wide-bounded-10 flat-bounded-10"}
# Each invocation retains its own executable and build log under _tmp.
# The Python helper acquires the tree lock before checking HEAD/queuing cargo.
bin=$(python3 - <<'PYTHON'
from pathlib import Path
import sys
import tempfile
root = Path.cwd().parent
sys.path.insert(0, str(root / 'src/pilotdeck-gateway/tests/fixtures'))
from negative_process import build_snapshot
base = root / '_tmp/20261026-html-markdown-rss'
base.mkdir(parents=True, exist_ok=True)
run = Path(tempfile.mkdtemp(prefix='run-', dir=base))
print(build_snapshot(root, run / 'binary', package='pilotdeck-gateway',
                     target='html_budget_probe'))
PYTHON
)
out=$(mktemp)
trap 'rm -f "$out"' EXIT
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
