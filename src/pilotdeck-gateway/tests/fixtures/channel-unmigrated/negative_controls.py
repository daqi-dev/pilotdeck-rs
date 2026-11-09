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
    # M1: the reject table loses `telegram`; the telegram case must survive,
    # and the #1853 partition pin counts the rejection group at nine loader
    # keys after #1344 moved slack out (#1853 r2 review: the partition test's
    # cell is registered here).
    ('telegram-dropped', source / 'tests.rs',
     'const SYNTHETIC_PENDING_KEYS: &[&str] = &["probe_pending", "qq", "wecom"];',
     'const SYNTHETIC_PENDING_KEYS: &[&str] = &["qq", "wecom"];',
     [('synthetic_pending_guard_rejects_enabled_and_names_key', '#1386 unmigrated channel must reject startup')]),
    # M2: the rejection is silenced into a skip; any one of the 11 cases kills
    # (#1343: bluebubbles and #1344: slack left the table, so their rejection
    # cases are gone).
    ('rejection-silenced', source / 'loader.rs',
     'return Err(ChannelError::error(format!(\n'
     '                "[adapters] channel \\"{key}\\" 未迁移到 Rust，需继续使用 TS gateway"\n'
     '            )));',
     'continue;',
     [('synthetic_pending_guard_rejects_enabled_and_names_key', '#1386 unmigrated channel must reject startup')]),
    # M3: the error text loses the channel name; the naming assertion kills.
    ('rejection-message-loses-key', source / 'loader.rs',
     '"[adapters] channel \\"{key}\\" 未迁移到 Rust，需继续使用 TS gateway"',
     '"[adapters] 未迁移到 Rust，需继续使用 TS gateway"',
     'synthetic_pending_guard_rejects_enabled_and_names_key', '#1386 rejection names the channel key'),
    # M4: a subset key lands in the reject table; the subset control kills.
    ('subset-sms-rejected', source / 'loader.rs',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &[];',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &["sms"];',
     'unmigrated_subset_channels_stay_loadable', '#1386 subset channel must not be rejected'),
    # M4b (#1344): the implemented channel must not be back in the reject
    # table; re-inserting the key refuses startup for the subset the loader
    # test keeps loadable (the #1343 precedent
    # `reject-table-reinserts-bluebubbles`).
    ('reject-table-reinserts-slack', source / 'loader.rs',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &[];',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &["slack"];',
     'unmigrated_subset_channels_stay_loadable', '#1386 subset channel must not be rejected'),
    # M5: the loader-table truthiness gate is constant-true; the falsy-enabled
    # control kills. Anchor is the `else` arm of `rejects_enabled`, which is
    # the only place the nine loader keys read `js_truthy`.
    ('enabled-gate-always-true', source / 'loader.rs',
     '    } else {\n        js_truthy(cfg.get("enabled"))\n    }',
     '    } else {\n        true\n    }',
     'synthetic_pending_gate_respects_falsy_and_strict_boolean',
     '#1386 disabled unmigrated channel must not reject'),
    # M7 (#1579 P1): the two `ChannelAdapterBundle` keys fall back to JS
    # truthiness, so `enabled: 1` aborts startup where TS just skips the
    # channel. The strict-`true` control kills.
    ('qq-wecom-gate-loosened', source / 'loader.rs',
     'matches!(cfg.get("enabled"), Some(JsValue::Bool(true)))',
     'js_truthy(cfg.get("enabled"))',
     'synthetic_pending_gate_respects_falsy_and_strict_boolean',
     '#1386 bundle channel requires strict enabled === true'),
    # M8 (#1579 P1 sibling): the loader-table `else` arm is tightened to a
    # strict `=== true`, so `enabled: 1` stops rejecting where TS still loads
    # the channel. The paired js-truthiness control kills.
    ('loader-gate-strictened', source / 'loader.rs',
     '    } else {\n        js_truthy(cfg.get("enabled"))\n    }',
     '    } else {\n        matches!(cfg.get("enabled"), Some(JsValue::Bool(true)))\n    }',
     'synthetic_pending_loader_key_uses_js_truthiness',
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
     'synthetic_pending_gate_respects_falsy_and_strict_boolean',
     '#1386 disabled unmigrated channel must not reject'),
    # M6: iterating the loader table instead of the config-key table misses
    # `qq`/`wecom` (they are outside CHANNEL_LOADER_KEYS in TS too).
    ('loader-table-iteration', source / 'loader.rs',
     'for key in keys {',
     'for key in &CHANNEL_LOADER_KEYS {',
     [('synthetic_pending_guard_rejects_enabled_and_names_key', '#1386 unmigrated channel must reject startup')]),
    # M12 (#1853 r2 review, #2301 terminal): the loader table gains a key in
    # neither group; the partition covering cell must kill it — without this
    # row nothing pinned the fail-closed cell of
    # `loader_keys_partition_rejected_and_implemented` (M1 only covers the
    # nine-key count).
    ('loader-table-gains-ungrouped-key', source / 'loader.rs',
     'pub const CHANNEL_LOADER_KEYS: [&str; 15] = [\n    "telegram",',
     'pub const CHANNEL_LOADER_KEYS: [&str; 16] = [\n    "probe1853",\n    "telegram",',
     'loader_keys_partition_rejected_and_implemented',
     '#2301 every loader key is rejected or implemented'),
    # M13 (#1853 r2 r1 review, #2301 terminal): the loader table loses the
    # implemented key `sms`; the implemented-group reverse closure must kill
    # it (M1/M12 only touch the count and covering cells).
    ('loader-table-loses-silent-sms', source / 'loader.rs',
     'pub const CHANNEL_LOADER_KEYS: [&str; 15] = [\n    "telegram",\n    "discord",\n'
     '    "slack",\n    "matrix",\n    "mattermost",\n    "signal",\n    "whatsapp",\n'
     '    "bluebubbles",\n    "dingtalk",\n    "wecomCallback",\n    "email",\n    "sms",',
     'pub const CHANNEL_LOADER_KEYS: [&str; 14] = [\n    "telegram",\n    "discord",\n'
     '    "slack",\n    "matrix",\n    "mattermost",\n    "signal",\n    "whatsapp",\n'
     '    "bluebubbles",\n    "dingtalk",\n    "wecomCallback",\n    "email",',
     'loader_keys_partition_rejected_and_implemented',
     '#2301 every implemented loader key is a loader-table key'),
    # M14 (#1853 r2 r1 review): the rejection table gains a key that is
    # neither a loader key nor a strict bundle key; the rejection-side
    # reverse closure must kill it (the count stays nine — probe1853 is not a
    # loader key).
    ('rejection-table-gains-foreign-key', source / 'loader.rs',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &[];',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &["probe1853"];',
     'loader_keys_partition_rejected_and_implemented',
     '#1853 every rejection-table key is a loader key or a strict bundle key'),
    # M15 (#1853 r2 r1 review): the bundle table gains the silent key `sms`;
    # the bundle-outside-loader cell fires first (sms is a loader key), which
    # is the registered receipt for this row.
    ('bundle-gains-silent-sms', source / 'loader.rs',
     'pub const BUNDLE_CHANNEL_KEYS: &[&str] = &["weixin", "feishu", "qq", "wecom"];',
     'pub const BUNDLE_CHANNEL_KEYS: &[&str] = &["weixin", "feishu", "qq", "wecom", "sms"];',
     'loader_keys_partition_rejected_and_implemented',
     '#1853 bundle keys stay outside the loader table'),
    # M16 (#1853 r2 r3 review, #2301 terminal): the loader table loses the
    # implemented key `slack` — the covering cell only iterates the loader
    # table, so only the implemented group's membership closure catches it
    # (same family as M13 on a second key). The array length is decremented
    # alongside, exactly like a real regression would, so the mutant compiles
    # (same pattern as M13).
    ('native-mount-loses-loader-key', source / 'loader.rs',
     'pub const CHANNEL_LOADER_KEYS: [&str; 15] = [\n    "telegram",\n    "discord",\n'
     '    "slack",',
     'pub const CHANNEL_LOADER_KEYS: [&str; 14] = [\n    "telegram",\n    "discord",',
     'loader_keys_partition_rejected_and_implemented',
     '#2301 every implemented loader key is a loader-table key'),
    # M17 (#1853 r2 r3 review, #2301 terminal): a rejected key is duplicated
    # into the implemented group — behavior cells cannot catch a key in both
    # pinned groups, so the disjointness assertion is the only kill (it runs
    # before the whitelist closure for exactly this reason).
    ('implemented-gains-rejected-telegram', source / 'tests.rs',
     'const SYNTHETIC_IMPLEMENTED: [&str; 1] = ["native_ready"];',
     'const SYNTHETIC_IMPLEMENTED: [&str; 2] = ["native_ready", "probe_pending"];',
     'synthetic_partition_never_implements_a_pending_key',
     '#2301 every implemented loader key is outside the rejection table'),
    # M18 (#1853 r2 r3 review, #2301 terminal): the module subset whitelist
    # loses an implemented key; the implemented-group reverse closure must
    # kill it.
    ('subset-whitelist-loses-silent-sms', source / 'tests.rs',
     'const SUBSET_CHANNEL_KEYS: [&str; 19] = [\n    "wecom",\n    "discord",\n    "whatsapp",\n    "dingtalk",\n    "email",\n    "qq",\n    "telegram",\n    "matrix",\n    "signal",\n    "mattermost",\n    "homeassistant",\n    "weixin",\n    "feishu",\n    "apiServer",\n    "webhook",\n    "wecomCallback",\n    "sms",\n    "bluebubbles",\n    "slack",\n];',
     'const SUBSET_CHANNEL_KEYS: [&str; 18] = [\n    "wecom",\n    "discord",\n    "whatsapp",\n    "dingtalk",\n    "email",\n    "qq",\n    "telegram",\n    "matrix",\n    "signal",\n    "mattermost",\n    "homeassistant",\n    "weixin",\n    "feishu",\n    "apiServer",\n    "webhook",\n    "wecomCallback",\n    "bluebubbles",\n    "slack",\n];',
     'loader_keys_partition_rejected_and_implemented',
     '#2301 every implemented loader key is in the module subset whitelist'),
    # M19 (#1853 r2 r3 review, #2301 terminal): the module subset whitelist
    # gains a key in neither group; the subset reverse closure must kill it.
    ('subset-whitelist-gains-foreign-key', source / 'tests.rs',
     'const SUBSET_CHANNEL_KEYS: [&str; 19] = [\n    "wecom",',
     'const SUBSET_CHANNEL_KEYS: [&str; 20] = [\n    "probe1853",\n    "wecom",',
     'loader_keys_partition_rejected_and_implemented',
     '#2301 every module subset key is implemented or a bundle key'),
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
