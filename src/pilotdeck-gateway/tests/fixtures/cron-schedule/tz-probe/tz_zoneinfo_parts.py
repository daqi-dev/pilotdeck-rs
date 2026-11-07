# #1324 A8.3-CRON-TZ-ALIAS-001 evidence script (moved in-repo from the
# round's _tmp directory so the diff entry's references persist; the
# original run was cb8, Python zoneinfo over tzdata 2025b).
#
# anchor-selfcheck: unsupported evidence probe script for the A8.3-CRON-TZ-ALIAS-001 diff entry, no product mutations
#
# System-TZif side: same local parts via Python zoneinfo (reads the same
# /usr/share/zoneinfo files the tz-rs parser would), then diff against Node.
#
# Usage: python3 tz_zoneinfo_parts.py <tz_node_parts.tsv> [expected-sha256]
#
# #1509 P2-1a: the TSV path is required (the old fixed
# `/tmp/tz_node_parts.tsv` let this side read whatever a *different* round
# left behind) and the bytes are pinned by sha256 — from the second argument
# or the `<tsv>.sha256` sidecar the Node side writes. The row count check is
# replaced by verifying the full expected grid (recorded zone list x probe
# instants), so a stale or truncated file cannot pass as "same shape".
import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

ROOT = '/usr/share/zoneinfo'


def resolve(name):
    # mirror transport timezone_tzif: exact component match first, then
    # case-insensitive; every component must exist
    path = ROOT
    for component in name.split('/'):
        exact = os.path.join(path, component)
        if os.path.exists(exact):
            path = exact
            continue
        found = None
        for entry in os.listdir(path):
            if entry.lower() == component:
                found = os.path.join(path, entry)
                break
        if found is None:
            raise KeyError(name)
        path = found
    return path


def expected_digest(tsv, override):
    if override:
        return override.strip().lower()
    sidecar = Path(str(tsv) + '.sha256')
    if not sidecar.is_file():
        raise SystemExit(
            f'{sidecar} missing: pass the expected sha256 as the second argument '
            '(the Node side writes the sidecar next to its output)')
    first = sidecar.read_text().split()
    if not first or not re.fullmatch(r'[0-9a-f]{64}', first[0]):
        raise SystemExit(f'{sidecar}: first field must be a sha256 hex digest')
    return first[0]


def check_grid(rows, zones, instants):
    """Every recorded zone once, with exactly the probe instants in order."""
    expected = [(zone, str(instant)) for zone in zones for instant in instants]
    got = [(row[0], row[1]) for row in rows]
    if got != expected:
        for index, (want, have) in enumerate(zip(expected, got)):
            if want != have:
                raise SystemExit(
                    f'row {index}: expected {want[0]} @ {want[1]}, got {have[0]} @ {have[1]}')
        raise SystemExit(
            f'row count {len(got)} != expected {len(expected)} '
            f'({len(zones)} zones x {len(instants)} instants)')


zones = json.load(open(Path(__file__).resolve().parents[4] / 'src/config/timezones.json'))
instants = [
    int(datetime(2026, 1, 15, tzinfo=timezone.utc).timestamp() * 1000),
    int(datetime(2026, 3, 8, 6, 30, tzinfo=timezone.utc).timestamp() * 1000),
    int(datetime(2026, 4, 5, 12, tzinfo=timezone.utc).timestamp() * 1000),
    int(datetime(2026, 7, 15, tzinfo=timezone.utc).timestamp() * 1000),
    int(datetime(2026, 10, 4, 1, tzinfo=timezone.utc).timestamp() * 1000),
    int(datetime(2026, 11, 1, 5, tzinfo=timezone.utc).timestamp() * 1000),
    int(datetime(2027, 3, 14, 0, 30, tzinfo=timezone.utc).timestamp() * 1000),
    int(datetime(2030, 6, 1, tzinfo=timezone.utc).timestamp() * 1000),
]

if len(sys.argv) < 2:
    raise SystemExit('usage: python3 tz_zoneinfo_parts.py <tz_node_parts.tsv> [expected-sha256]')
tsv = Path(sys.argv[1])
raw = tsv.read_bytes()
digest = hashlib.sha256(raw).hexdigest()
want = expected_digest(tsv, sys.argv[2] if len(sys.argv) > 2 else None)
if digest != want:
    raise SystemExit(f'{tsv}: sha256 {digest} != expected {want} (foreign or edited TSV)')

node_rows = [line.split('\t') for line in raw.decode().split('\n') if line]
check_grid(node_rows, zones, instants)


def parts(zone_path, ms):
    with open(zone_path, 'rb') as f:
        zi = ZoneInfo.from_file(f, key=zone_path)
    dt = datetime.fromtimestamp(ms // 1000, tz=zi)
    return (dt.month, dt.day, dt.hour, dt.minute, (dt.weekday() + 1) % 7)


mismatch = []
resolved = 0
for row in node_rows:
    z, t, mon, day, hour, minute, wd = row
    try:
        p = parts(resolve(z), int(t))
    except Exception as e:
        mismatch.append((z, t, 'UNRESOLVED', repr(e)))
        continue
    resolved += 1
    got = (int(mon), int(day), int(hour), int(minute), int(wd))
    if got != p:
        mismatch.append((z, t, got, p))
print('compared:', len(node_rows), 'resolved-in-zoneinfo:', resolved, 'mismatches:', len(mismatch))
for m in mismatch[:40]:
    print('MISMATCH', m)
