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
    # M2: the rejection is silenced into a skip; any one of the 11 cases kills
    # (#1343: bluebubbles and #1344: slack left the table, so their rejection
    # cases are gone).
    ('rejection-silenced', source / 'loader.rs',
     'return Err(ChannelError::error(format!(\n'
     '                "[adapters] channel \\"{key}\\" 未迁移到 Rust，需继续使用 TS gateway"\n'
     '            )));',
     'continue;',
     [('rejects_unmigrated_telegram', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_discord', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_matrix', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_mattermost', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_signal', '#1386 unmigrated channel must reject startup'),
      ('rejects_unmigrated_whatsapp', '#1386 unmigrated channel must reject startup'),
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
    # M4b (#1344): the implemented channel must not be back in the reject
    # table; re-inserting the key refuses startup for the subset the loader
    # test keeps loadable (the #1343 precedent
    # `reject-table-reinserts-bluebubbles`).
    ('reject-table-reinserts-slack', source / 'loader.rs',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &[\n    "homeassistant",',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &[\n    "slack",\n    "homeassistant",',
     'unmigrated_subset_channels_stay_loadable', '#1386 subset channel must not be rejected'),
    # M5: the loader-table truthiness gate is constant-true; the falsy-enabled
    # control kills. Anchor is the `else` arm of `rejects_enabled`, which is
    # the only place the nine loader keys read `js_truthy`.
    ('enabled-gate-always-true', source / 'loader.rs',
     '    } else {\n        js_truthy(cfg.get("enabled"))\n    }',
     '    } else {\n        true\n    }',
     'unmigrated_falsy_or_absent_enabled_does_not_reject',
     '#1386 disabled unmigrated channel must not reject'),
    # M7 (#1579 P1): the two `ChannelAdapterBundle` keys fall back to JS
    # truthiness, so `enabled: 1` aborts startup where TS just skips the
    # channel. The strict-`true` control kills.
    ('qq-wecom-gate-loosened', source / 'loader.rs',
     'matches!(cfg.get("enabled"), Some(JsValue::Bool(true)))',
     'js_truthy(cfg.get("enabled"))',
     'unmigrated_bundle_channels_require_strict_boolean_true',
     '#1386 bundle channel requires strict enabled === true'),
    # M8 (#1579 P1 sibling): the loader-table `else` arm is tightened to a
    # strict `=== true`, so `enabled: 1` stops rejecting where TS still loads
    # the channel. The paired js-truthiness control kills.
    ('loader-gate-strictened', source / 'loader.rs',
     '    } else {\n        js_truthy(cfg.get("enabled"))\n    }',
     '    } else {\n        matches!(cfg.get("enabled"), Some(JsValue::Bool(true)))\n    }',
     'unmigrated_loader_channels_use_js_truthiness',
     '#1386 loader channel uses JS truthiness'),
    # M9 (#1579 r3 P1): the strict-key constant loses `qq`. Both parity suites
    # derive their key sets from that constant, so without the membership pin
    # every test stays green while qq silently moves to the JS-truthiness gate.
    ('strict-key-dropped', source / 'loader.rs',
     'const STRICT_TRUE_CHANNEL_KEYS: &[&str] = &["qq", "wecom"];',
     'const STRICT_TRUE_CHANNEL_KEYS: &[&str] = &["wecom"];',
     'strict_true_channel_keys_are_exactly_qq_and_wecom',
     '#1386 strict-true channel key set must be exactly qq and wecom'),
    # M10 (#1579 r3 P1): the constant gains a loader-table key; the same pin
    # catches it before the truthiness suite can quietly skip `telegram`.
    ('loader-key-made-strict', source / 'loader.rs',
     'const STRICT_TRUE_CHANNEL_KEYS: &[&str] = &["qq", "wecom"];',
     'const STRICT_TRUE_CHANNEL_KEYS: &[&str] = &["qq", "wecom", "telegram"];',
     'strict_true_channel_keys_are_exactly_qq_and_wecom',
     '#1386 strict-true channel key set must be exactly qq and wecom'),
    # M11 (#1579 r5 P2 sibling): the strict arm is constant-true, so a disabled
    # `qq`/`wecom` would refuse startup. The paired falsy/absent control kills;
    # without this row nothing proved the strict arm's false half is pinned
    # (M7 only loosens it, M5/M8 only touch the loader-table arm).
    ('strict-gate-always-true', source / 'loader.rs',
     'matches!(cfg.get("enabled"), Some(JsValue::Bool(true)))',
     'true',
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
