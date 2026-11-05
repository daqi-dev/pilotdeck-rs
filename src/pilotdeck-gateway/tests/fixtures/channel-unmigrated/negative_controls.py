#!/usr/bin/env python3
"""#1386 A8.3 unmigrated-channel rejection controls; compile each mutant first.

OUT [mutation ...], --compile-only OUT, --check (also --check-anchors).
The shared runner owns source restoration, offline cargo, nonce receipts,
patch evidence, pending registrations and the complete result summary.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/channel'
MUTATIONS = [
    # M1: the reject table loses `telegram`; the telegram case must survive.
    ('telegram-dropped', source / 'loader.rs',
     '    "telegram",\n    "qq",',
     '    "qq",',
     'rejects_unmigrated_telegram', '#1386 unmigrated channel must reject startup'),
    # M2: the rejection is silenced into a skip; any one of the 13 cases kills.
    ('rejection-silenced', source / 'loader.rs',
     'return Err(ChannelError::error(format!(\n'
     '                "[adapters] channel \\"{key}\\" 未迁移到 Rust，需继续使用 TS gateway"\n'
     '            )));',
     'continue;',
     [('rejects_unmigrated_telegram', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_discord', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_slack', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_matrix', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_mattermost', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_signal', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_whatsapp', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_bluebubbles', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_dingtalk', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_email', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_homeassistant', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_qq', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_wecom', '#1386 unmigrated channel must reject startup')]),
    # M3: the error text loses the channel name; the naming assertion kills.
    ('rejection-message-loses-key', source / 'loader.rs',
     '"[adapters] channel \\"{key}\\" 未迁移到 Rust，需继续使用 TS gateway"',
     '"[adapters] 未迁移到 Rust，需继续使用 TS gateway"',
     'rejects_unmigrated_telegram', '#1386 rejection names the channel key'),
    # M4: a subset key lands in the reject table; the subset control kills.
    ('subset-sms-rejected', source / 'loader.rs',
     '    "qq",\n    "wecom",\n];',
     '    "qq",\n    "wecom",\n    "sms",\n];',
     'unmigrated_subset_channels_stay_loadable', '#1386 subset channel must not be rejected'),
    # M5: the enabled gate is constant-true; the falsy-enabled control kills.
    ('enabled-gate-always-true', source / 'loader.rs',
     'if js_truthy(cfg.get("enabled")) {',
     'if true {',
     'unmigrated_falsy_or_absent_enabled_does_not_reject',
     '#1386 disabled unmigrated channel must not reject'),
    # M6: iterating the loader table instead of the config-key table misses
    # `qq`/`wecom` (they are outside CHANNEL_LOADER_KEYS in TS too).
    ('loader-table-iteration', source / 'loader.rs',
     'for key in UNMIGRATED_CHANNEL_KEYS {',
     'for key in CHANNEL_LOADER_KEYS {',
     [('rejects_unmigrated_qq', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_wecom', '#1386 unmigrated channel must reject startup')]),
]


def main():
    guarded = list(source.glob('*.rs'))
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    return matrix_main(root, __file__, 'channel::tests::', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args,
                       any_cell=('rejection-silenced', 'loader-table-iteration'))


if __name__ == '__main__':
    raise SystemExit(main())
