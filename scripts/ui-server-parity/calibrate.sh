#!/usr/bin/env bash
# Run OUTSIDE the no-listen sandbox. All captures use the one fixed slot.
set -euo pipefail
B2_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# argparse handles read-only help/preflight before taking the capture lock.
if [[ "${1:-}" == --help || "${1:-}" == -h || "${1:-}" == --preflight ]]; then
  exec python3 "$B2_SCRIPT_DIR/calibrate.py" "$@"
fi
mkdir /tmp/pdrs-parity.lock || { echo 'REFUSED: /tmp/pdrs-parity.lock already held; do not remove another runner lock' >&2; exit 2; }
trap 'rmdir /tmp/pdrs-parity.lock' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
export PYTHONDONTWRITEBYTECODE=1
python3 "$B2_SCRIPT_DIR/calibrate.py" "$@"
