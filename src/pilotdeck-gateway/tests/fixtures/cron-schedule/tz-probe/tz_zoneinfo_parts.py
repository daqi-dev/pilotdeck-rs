# #1324 A8.3-CRON-TZ-ALIAS-001 evidence script (moved in-repo from the
# round's _tmp directory so the diff entry's references persist; the
# original run was cb8, Python zoneinfo over tzdata 2025b).
# System-TZif side: same local parts via Python zoneinfo (reads the same
# /usr/share/zoneinfo files the tz-rs parser would), then diff against Node.
import json
import os
from datetime import datetime, timezone
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

zones = json.load(open(__import__('pathlib').Path(__file__).resolve().parents[4] / 'src/config/timezones.json'))
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

def parts(zone_path, ms):
    with open(zone_path, 'rb') as f:
        zi = ZoneInfo.from_file(f, key=zone_path)
    dt = datetime.fromtimestamp(ms // 1000, tz=zi)
    return (dt.month, dt.day, dt.hour, dt.minute, (dt.weekday() + 1) % 7)

node_rows = [line.split('\t') for line in open('/tmp/tz_node_parts.tsv').read().split('\n') if line]
assert len(node_rows) == len(zones) * len(instants), (len(node_rows), len(zones) * len(instants))

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
